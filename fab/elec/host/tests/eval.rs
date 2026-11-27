//! Document-backed evaluation, adoption and history ordering without a scheduler or window.

use fab_elec_board::{Board, BoardKeepout, BoardOp, CarriedKeepout, Factories, Item, Origin, PortRoom};
use fab_elec_circuit::{CircuitOp, End, QtyUnit, Quantity, Value};
use fab_elec_core::{mm, Angle, BBox, BoardId, Canon, CompRef, Diag, DocId, FeatureRef, ItemId, LayerId, LayerSet, PolySet, Sel, Side, P};
use fab_elec_design::{AssetBytes, BenchOp, By, Design, DesignOp, EntryKind, FileOp, PartRef, Place, ProbeOf, Resolved, TextEdit};
use fab_elec_parts::{BodySource, Forbid};
use fab_elec_host::{circuits_named, CircuitChange, Cmd, CASE_KEEPOUT, DocSet, Docs, Engine, EvalOut, Mailbox, Outcome, Plan, Sample, Step, TaskResult, Ticket, MISTAKE};
use fab_elec_lang::{board_doc::{self, Ledger}, evaluate, UnitState, SYSTEM_DOC};
use fab_elec_view::board::plan::label_ops;
use fab_elec_view::{StageFlags, StateOf};
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::mpsc;
use std::sync::Arc;

/// A real document-backed controller, matching the board-splice tests' setup.
fn setup() -> (Engine, Mailbox, Arc<AtomicU64>, Arc<AtomicU64>) {
    let sources = fab_elec_fixtures::docs("ledctl").expect("the fixture supplies controller documents");
    let (design, docs) = Docs::from_evaluated(sources, fab_elec_fixtures::ledctl_eval());
    let steps = Arc::new(AtomicU64::new(0));
    let logged = Arc::new(AtomicU64::new(0));
    let engine = Engine::new(design, Arc::new(Factories::new()), steps.clone(), logged.clone(), "eval".into()).with_docs(docs);
    (engine, Mailbox::default(), steps, logged)
}

/// Drain only evaluation jobs; completion, rather than time, determines when the design catches up.
fn run(engine: &mut Engine, mailbox: &Mailbox, mut step: Step) -> Step {
    loop {
        assert!(step.rejected.is_empty(), "evaluation and held commands are accepted: {:?}", step.rejected);
        if step.evals.is_empty() {
            return step;
        }
        assert_eq!(step.evals.len(), 1, "at most one evaluation is emitted per step");
        for job in step.evals {
            mailbox.result(job.run());
        }
        step = engine.step(mailbox.take(), &mut |_| {});
    }
}

/// Apply one inbox without needing a visible host.
fn step(engine: &mut Engine, mailbox: &Mailbox) -> Step {
    engine.step(mailbox.take(), &mut |_| {})
}

/// Compare document bytes independently of their Arc identities and revisions.
fn texts(engine: &Engine) -> Vec<(DocId, Arc<str>)> {
    engine.docs().expect("the engine retains documents").sources().docs().iter()
        .map(|doc| (doc.id, doc.text.clone())).collect()
}

/// The fixture's board, retained across value-only circuit evaluations.
fn board(engine: &Engine) -> &Arc<Board> {
    engine.design().unit(BoardId(0)).expect("the controller unit remains").board.as_ref().expect("the controller keeps its board")
}

/// Find the controller's boot resistor by its stable path after any lowering.
fn value(engine: &Engine) -> f64 {
    engine.design().unit(BoardId(0)).expect("the controller unit remains").circuit.comp("mcu.boot_r")
        .expect("the boot resistor remains").value.as_ref().and_then(|value| value.qty)
        .expect("the boot resistor has a resistance").si
}

/// The value edit used by both verification and concurrent board-preview checks.
fn set_value(engine: &Engine) -> Cmd {
    let unit = engine.design().unit(BoardId(0)).expect("the controller unit remains");
    Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::SetValue {
            comp: unit.circuit.comp("mcu.boot_r").expect("the boot resistor exists").id,
            value: Value { text: "4.7k".into(), qty: Some(Quantity { si: 4700.0, unit: QtyUnit::Ohm }), tol: None },
        } }],
        label: "value".into(), circuits: vec![], expect: None,
    }
}

/// Read an editor's revision from the same published view that supplies its text.
fn revision(engine: &Engine, doc: DocId) -> u64 {
    engine.snapshot().docs.as_ref().expect("the snapshot carries documents").texts.iter()
        .find(|text| text.doc == doc).expect("the editor document is published").rev
}

/// A board move gives text history a model fast path until its unit's epoch changes.
fn move_part(item: ItemId, by: P, label: &str) -> Cmd {
    Cmd::Commit { by: By::Person, ticket: None, place: vec![], ops: vec![DesignOp::Board { unit: BoardId(0), op: BoardOp::Move { item, by } }], label: label.into(), circuits: vec![], expect: None }
}

/// The ledger must lower the committed board text to the adopted model's exact content.
fn check_board(engine: &Engine) {
    let docs = engine.docs().expect("the engine retains documents");
    let doc = docs.sources().find("ledctl.pcb.splash").expect("the board document remains");
    let unit = engine.design().unit(BoardId(0)).expect("the controller unit remains");
    let board = board(engine);
    let ledger = Ledger::of_map(docs.map(), unit.id, doc, board.next_item);
    let lowered = board_doc::lower(doc, docs.text(doc).expect("the board has text"), &unit.circuit, Some(&ledger))
        .expect("the committed board text lowers with the followed ledger");
    assert_eq!(lowered.board.content_hash(), board.content_hash(), "ledger lowering matches the adopted board content");
}

/// Include every item kind in identity checks, including gaps left by prior allocation.
fn item_ids(board: &Board) -> Vec<ItemId> {
    (0..board.next_item).map(ItemId).filter(|id| board.item(*id).is_some()).collect()
}

// The probe fixture's power nets and resistor, reduced to one component.
const PROBE_CIRCUIT: &str = r#"use mod.elec.*
use mod.std.*
let e = mod.elec
let gnd = e.net({name: "GND" kind: @ground})
let v33 = e.net({name: "3V3" kind: @power volts: 3.3V})
let r = e.r(1kohm, v33, gnd, {package: "0402"})
"#;

const PROBE_BLOCK: &str = r#"let e = mod.elec
let load = e.block({
    name: "Load"
    ports: {top: e.port(@power_in) gnd: e.port(@ground)}
    params: {r: 1kohm}
    build: fn(p, o) {
        let r = e.r(o.r, p.top, p.gnd, {package: "0402"})
    }
})
"#;

fn proposal_fixture() -> (Engine, Mailbox) {
    let circuit = PROBE_CIRCUIT.replace("use mod.std.*", "use mod.project.*")
        .replace("e.r(1kohm, v33, gnd, {package: \"0402\"})", "load({top: v33 gnd: gnd}, {r: 1kohm})");
    let mut sources = fab_elec_lang::Sources::new();
    sources.add("load.blocks.splash", PROBE_BLOCK).unwrap();
    sources.add("probe.ckt.splash", circuit).unwrap();
    let libs = fab_elec_lang::std_libs();
    let ev = evaluate(&sources, &libs).expect("the one-resistor project block evaluates");
    let (design, docs) = Docs::from_evaluated(sources, ev);
    (Engine::new(design, Arc::default(), Arc::default(), Arc::default(), "probe".into()).with_docs(docs), Mailbox::default())
}

fn proposed_changes(engine: &Engine) -> Vec<DocSet> {
    let doc = engine.docs().unwrap().view().texts.iter().find(|doc| doc.name == "probe.ckt.splash").unwrap();
    vec![DocSet::Add { name: "brief.req.splash".into(), text: "require(\"two layers\", board(\"probe\").layers, {max: 2})\n".into() },
        DocSet::Replace { name: doc.name.clone(), base_rev: doc.rev, text: doc.text.replace("1kohm", "2kohm").into() }]
}

fn propose_probe(engine: &mut Engine, mail: &Mailbox) -> (fab_elec_host::ProposalId, u64, u64) {
    mail.push(Cmd::Propose { set: proposed_changes(engine), slot: "schematic".into(), label: "Probe schematic".into(),
        by: fab_elec_host::ProposalAuthor::Local(By::Ai { turn: 1, call: 1 }), ticket: Ticket(1) });
    let sent = step(engine, mail);
    assert_eq!(sent.evals.len(), 1, "a local proposal runs one independent verification");
    let verification = sent.evals[0].id;
    let shown = run(engine, mail, sent);
    let [(Ticket(1), Outcome::Proposed { id, shown, checks: Some(_), requirements: Some(_) })] = shown.outcomes.as_slice()
        else { panic!("the proposer gets the checked proposal identity") };
    (*id, *shown, verification)
}

#[derive(Debug, PartialEq)]
enum JournalNotice {
    Decision(Ticket, String, Vec<(String, String)>),
    Effect(Option<Ticket>, By),
    Done(Result<u64, String>),
}

fn record_decisions(engine: Engine) -> (Engine, mpsc::Receiver<JournalNotice>) {
    struct Sink(mpsc::Sender<JournalNotice>);
    type Reply = Box<dyn FnOnce(Result<u64, String>) + Send + Sync>;
    impl fab_elec_host::JournalSink for Sink {
        fn gesture(&self, _: Ticket, _: &str, _: &[(String, String)]) -> Option<Reply> {
            panic!("a proposal press must never bind a gesture");
        }
        fn decision(&self, ticket: Ticket, name: &str, args: &[(String, String)]) -> Option<Reply> {
            self.0.send(JournalNotice::Decision(ticket, name.into(), args.to_vec())).unwrap();
            let tx = self.0.clone();
            Some(Box::new(move |result| tx.send(JournalNotice::Done(result)).unwrap()))
        }
        fn receive(&self, event: fab_elec_host::JournalEvent<'_>) {
            if let fab_elec_host::JournalEvent::Effect { ticket, by, .. } = event {
                self.0.send(JournalNotice::Effect(ticket, by)).unwrap();
            }
        }
    }
    let (tx, rx) = mpsc::channel();
    (engine.with_journal(Some(Arc::new(Sink(tx)))), rx)
}

#[test]
fn held_proposal_accepts_the_shown_design_in_one_step() {
    let (engine, mail) = proposal_fixture();
    let (mut engine, journal) = record_decisions(engine);
    let before = engine.snapshot();
    let checks = |snap: &fab_elec_view::DesignSnapshot| fab_elec_host::edit_actions::check_list(snap, &[]).unwrap();
    let (ended, shown, _) = propose_probe(&mut engine, &mail);
    mail.push(decision(ended, shown, false));
    let decline = step(&mut engine, &mail);
    assert!(matches!(decline.outcomes.as_slice(), [(_, Outcome::Declined { .. })]), "the declined press ends the proposal");
    mail.push(decision(ended, shown, true));
    let refusal = step(&mut engine, &mail);
    assert!(matches!(refusal.outcomes.as_slice(), [(_, Outcome::Refused(_))]), "a press on the ended proposal is refused");
    assert!(journal.try_recv().is_err(), "proposing, declining and a refused press leave the journal empty");
    let (id, shown, verification) = propose_probe(&mut engine, &mail);
    let held = engine.snapshot();
    assert_eq!(held.proposals[0].id, id, "the outcome names the visible proposal");
    assert_eq!(held.proposals[0].shown, shown, "the outcome names the visible checks");
    assert_eq!(held.docs.as_ref().unwrap().texts, before.docs.as_ref().unwrap().texts, "a proposal preserves all committed texts and revisions");
    assert!(engine.log().entries.is_empty(), "the AI proposal creates no history");
    assert!(Arc::ptr_eq(&held.design, &before.design), "the committed design keeps its identity");
    assert!(held.preview.is_none(), "a proposal leaves the gesture slot empty");
    assert_eq!(checks(&held), checks(&before), "elec.check.list keeps reading the live design");
    let candidate = &held.proposals[0];
    let accept = |id, shown| Cmd::Decide { id, shown, accept: true, ticket: None };
    mail.push(decision(id, shown, true));
    let landed = step(&mut engine, &mail);
    assert!(landed.rejected.is_empty() && landed.evals.is_empty(), "accept adopts without evaluating again");
    assert!(Arc::ptr_eq(engine.design(), candidate.design.as_ref().unwrap()), "accept commits the exact design shown");
    let committed = engine.snapshot();
    assert!(committed.sheets.iter().all(|s| Arc::ptr_eq(&s.value, &candidate.sheets[&s.unit.unwrap()])),
        "accept installs the sheet indexes the person saw");
    let entries = &engine.log().entries;
    assert_eq!(entries.len(), 1, "the multi-document proposal takes one undo entry");
    assert_eq!(journal.try_iter().collect::<Vec<_>>(), [
        JournalNotice::Decision(Ticket(2), "elec.step.accept".into(), vec![("proposal".into(), id.0.to_string()),
            ("slot".into(), "schematic".into()), ("label".into(), "Probe schematic".into()), ("author".into(), "ai".into())]),
        JournalNotice::Effect(Some(Ticket(2)), By::Ai { turn: 1, call: 1 }),
        JournalNotice::Done(Ok(entries[0].seq)),
    ], "one decision binds the AI-authored effect to the press ticket and completes with its undo step");
    assert_eq!(entries[0].by, By::Ai { turn: 1, call: 1 }, "the accepted entry credits the local author");
    assert_eq!((entries[0].text.edits.len(), entries[0].text.present.len(), entries[0].text_inverse.edits.len(),
        entries[0].text_inverse.present.len()), (1, 1, 1, 1), "both history directions record the replacement and creation");
    let installed = texts(&engine);
    let docs = engine.docs().unwrap();
    for change in candidate.docs.iter() {
        assert_eq!(docs.sources().find(&change.name).and_then(|id| docs.text(id)), change.after.as_ref(), "accept installs the shown bytes for {}", change.name);
    }
    let first_id = docs.sources().find("brief.req.splash").unwrap();
    for (index, undo) in [true, false, true].into_iter().enumerate() {
        mail.push(if undo { Cmd::Undo } else { Cmd::Redo });
        let history = step(&mut engine, &mail);
        if index == 0 {
            assert_eq!(history.evals[0].id, verification + 1, "accept left the evaluation counter at the proposal verification");
        }
        run(&mut engine, &mail, history);
        if undo {
            let expected: Vec<_> = before.docs.as_ref().unwrap().texts.iter().map(|d| (d.doc, d.text.clone())).collect();
            assert_eq!(texts(&engine), expected, "one undo restores the circuit bytes and removes the requirements");
        } else {
            assert_eq!(texts(&engine), installed, "one redo restores both documents");
        }
    }
    let (again, shown, _) = propose_probe(&mut engine, &mail);
    mail.push(accept(again, shown));
    let landed = step(&mut engine, &mail);
    assert!(landed.rejected.is_empty(), "the same creation can be accepted after undo");
    let current = engine.docs().unwrap().sources();
    assert!(current.find("brief.req.splash").is_some_and(|id| id != first_id) && current.doc(first_id).is_none(),
        "reusing the absent name allocates a fresh id and leaves the undone document absent");
}

fn decision(id: fab_elec_host::ProposalId, shown: u64, accept: bool) -> Cmd {
    Cmd::Decide { id, shown, accept, ticket: Some(Ticket(2)) }
}

fn change_doc(engine: &Engine, name: &str, text: String) -> Cmd {
    let doc = engine.docs().unwrap().view().texts.iter().find(|d| d.name == name).unwrap();
    Cmd::Docs { set: vec![DocSet::Replace { name: name.into(), base_rev: doc.rev, text: text.into() }],
        by: By::Person, label: name.into(), ticket: None }
}

#[test]
fn held_proposal_declined_replaced_or_stale_applies_nothing() {
    use fab_elec_host::ProposalState;
    let layout = |unit| format!("board(\"{unit}\")\noutline([[[0mm, 0mm], [8mm, 0mm], [8mm, 8mm], [0mm, 8mm], [0mm, 0mm]]])\nzone(\"GND\", [\"F.Cu\"], @board)\n");
    let mut sources = fab_elec_lang::Sources::new();
    sources.add("probe.ckt.splash", PROBE_CIRCUIT).unwrap();
    sources.add("shared.ckt.splash", PROBE_CIRCUIT).unwrap();
    sources.add("shared.pcb.splash", layout("shared")).unwrap();
    let (design, docs) = Docs::load(sources, fab_elec_lang::std_libs());
    let mut engine = Engine::new_held(design, Arc::default(), Arc::default(), Arc::default(), "probe".into()).with_docs(docs).with_app_tools();
    let mail = Mailbox::default();
    let start = engine.snapshot().docs.clone().unwrap();
    let circuit = start.texts.iter().find(|d| d.name == "probe.ckt.splash").unwrap();
    let set = vec![
        DocSet::Replace { name: circuit.name.clone(), base_rev: circuit.rev,
            text: format!("{}let extra = e.r(2kohm, v33, gnd, {{package: \"0402\"}})\n", circuit.text).into() },
        DocSet::Add { name: "probe.pcb.splash".into(), text: layout("probe").into() },
    ];
    mail.push(Cmd::Propose { set, slot: "schematic".into(), label: "Probe layout".into(),
        by: fab_elec_host::ProposalAuthor::Local(By::Person), ticket: Ticket(1) });
    let checking = step(&mut engine, &mail);
    let [fill] = checking.fills.as_slice() else { panic!("only the shared board owes a committed zone refill") };
    mail.result(checking.evals[0].run());
    let ready = step(&mut engine, &mail);
    let [(Ticket(1), Outcome::Proposed { id: first, shown, .. })] = ready.outcomes.as_slice()
        else { panic!("the checked two-board proposal is ready: {:?}", ready.rejected) };
    let (first, shown) = (*first, *shown);
    let snap = engine.snapshot();
    let view = &snap.proposals[0];
    let design = view.design.as_ref().unwrap();
    let probe = design.unit_by_name("probe").unwrap();
    let shared = fill.task.unit;
    assert_eq!(view.boards.len(), 2, "both candidate boards publish fills");
    for unit in &design.units {
        let zone = unit.board.as_ref().unwrap().zones[0].id;
        let board = view.boards.iter().find(|b| b.unit == unit.id).unwrap();
        assert_eq!(board.fills[0].zone, zone, "the published fill belongs to the candidate unit's zone");
        assert_eq!(&*board.pending_zones, &[zone], "the candidate lists the zone whose fill is still owed");
    }
    let shared_board = view.boards.iter().find(|b| b.unit == shared).unwrap();
    assert!(Arc::ptr_eq(&shared_board.fills, &snap.of(&snap.fills, shared).unwrap().value), "the unchanged board shares its committed fills");
    assert!(Arc::ptr_eq(&view.sheets[&shared], &snap.of(&snap.sheets, shared).unwrap().value), "the unchanged circuit shares its committed sheet index");
    let extra = probe.circuit.comp("extra").expect("the candidate circuit has the added resistor");
    assert!(view.sheets[&probe.id].rows.comp(extra.id.0).is_some(), "the changed sheet has a row for its new resistor");

    // A results-dependent call consumes the fill early in the step, then publishes its refusal.
    mail.push(Cmd::Invoke { tool: "elec.zone.stitch".into(), args: vec![], preview: false, by: By::Person, ticket: Ticket(3) });
    mail.result(fill.run());
    let mut published = None;
    let settled = engine.step(mail.take(), &mut |snap| published = Some(snap));
    assert!(matches!(settled.outcomes.as_slice(), [(Ticket(3), Outcome::Refused(_))]), "the incomplete stitch call ends without editing the board");
    let after = published.expect("the shared fill publishes the updated proposal");
    let updated = &after.proposals[0];
    let board = updated.boards.iter().find(|b| b.unit == shared).unwrap();
    assert!(board.pending_zones.is_empty() && Arc::ptr_eq(&board.fills, &after.of(&after.fills, shared).unwrap().value)
        && !Arc::ptr_eq(&board.fills, &shared_board.fills), "the shared board republishes the settled fill and clears its debt");
    assert_eq!((&updated.checks, updated.shown), (&view.checks, shown), "settling geometry alone preserves the shown checks and revision");
    assert!(Arc::ptr_eq(&updated.sheets, &view.sheets), "a fill settlement shares the published sheets");
    mail.push(decision(first, shown, false));
    let declined = step(&mut engine, &mail);
    assert!(matches!(declined.outcomes.as_slice(), [(_, Outcome::Declined { id })] if *id == first), "decline answers its ticket");
    assert_eq!(engine.docs().unwrap().view().texts, start.texts, "decline preserves texts and revisions");
    assert!(engine.log().entries.is_empty(), "decline creates no history");
    let (second, _, _) = propose_probe(&mut engine, &mail);
    let (third, shown, _) = propose_probe(&mut engine, &mail);
    for ended in [first, second] {
        mail.push(decision(ended, shown, true));
        let refused = step(&mut engine, &mail);
        assert!(matches!(refused.outcomes.as_slice(), [(_, Outcome::Refused(r))] if r.text.contains("ended")),
            "a declined or replaced id refuses as ended");
    }
    let other = format!("// person's edit\n{}", circuit.text);
    mail.push(change_doc(&engine, &circuit.name, other.clone()));
    let edited = step(&mut engine, &mail);
    run(&mut engine, &mail, edited);
    let snap = engine.snapshot();
    let stale = &snap.proposals[0];
    assert!(matches!(stale.state, ProposalState::Stale(_)), "a change to the proposed circuit makes it stale");
    assert!(stale.design.is_none() && stale.boards.is_empty() && stale.sheets.is_empty(), "a stale proposal drops its drawable data");
    mail.push(decision(third, shown, true));
    let refused = step(&mut engine, &mail);
    assert!(matches!(refused.outcomes.as_slice(), [(_, Outcome::Refused(r))] if r.text.contains("changed")), "stale accept names the conflict");
    let docs = engine.docs().unwrap();
    assert_eq!(docs.text(circuit.doc).unwrap().as_ref(), other, "stale accept keeps the person's circuit");
    assert!(docs.sources().find("brief.req.splash").is_none(), "stale accept does not create the other document");
    assert_eq!(engine.log().entries.len(), 1, "only the person's edit entered history");
}

#[test]
fn architecture_proposal_reports_block_checks_in_check_list_and_req_summary() {
    use fab_elec_circuit::CheckKind::{OpenPort, RailBudget, VoltageDomain};
    use fab_elec_core::Severity::{Error, Warning};
    let mut sources = fab_elec_lang::Sources::new();
    sources.add("device.req.splash", r#"require("ERC clean", checks(@erc).errors, {max: 0})"#).unwrap();
    let (design, docs) = Docs::load(sources, fab_elec_lang::std_libs());
    let mut engine = Engine::new_held(design, Arc::default(), Arc::default(), Arc::default(), "device".into()).with_docs(docs);
    let mail = Mailbox::default();
    let definitions = r#"let e = mod.elec
let reg = e.block({name: "Source" planned: true ports: {out: e.port(@power_out, {volts: 5V current: 20mA})}})
let load = e.block({name: "Load" planned: true role: "Controller" ports: {
    v: e.port(@power_in, {volts: 3.3V current: 30mA}) io: e.port(@in)}})
"#;
    let circuit = r#"use mod.project.*
let e = mod.elec
let rail = e.net({name: "VDD" kind: @power})
let src = reg({out: rail})
let controller = load({v: rail})
"#;
    for fixed in [false, true] {
        let defs = "device.blocks.splash";
        let set = if fixed {
            let view = engine.docs().unwrap().view();
            let rev = view.texts.iter().find(|d| d.name == defs).expect("the accepted blocks file must be present").rev;
            vec![DocSet::Replace { name: defs.into(), base_rev: rev, text: definitions.replace("3.3V", "5V").replace("30mA", "20mA").into() }]
        } else {
            vec![DocSet::Add { name: defs.into(), text: definitions.into() },
                DocSet::Add { name: "device.ckt.splash".into(), text: circuit.into() }]
        };
        mail.push(Cmd::Propose { slot: "architecture".into(), label: "Block architecture".into(),
            by: fab_elec_host::ProposalAuthor::Local(By::Ai { turn: 1, call: 1 }), set, ticket: Ticket(1) });
        let sent = step(&mut engine, &mail);
        let checked = run(&mut engine, &mail, sent);
        let [(_, Outcome::Proposed { id, shown, checks: Some(c), requirements: Some(r) })] = checked.outcomes.as_slice()
            else { panic!("verification must return both the block findings and the requirements verdict"); };
        assert_eq!(c.counts.get(&(OpenPort, Warning)), Some(&1), "the unbound signal must remain a warning");
        for kind in [RailBudget, VoltageDomain] {
            assert_eq!(c.counts.get(&(kind.clone(), Error)).copied().unwrap_or(0), usize::from(!fixed),
                "{kind:?} must follow the rail in the held candidate");
        }
        assert_eq!((r.fail,r.pass), (usize::from(!fixed),usize::from(fixed)), "ERC clean must follow the candidate's errors");
        if !fixed {
            mail.push(Cmd::Decide { id: *id, shown: *shown, accept: true, ticket: None });
            let accepted = step(&mut engine, &mail);
            assert!(accepted.rejected.is_empty(), "a person must be able to accept the reviewed design");
            let text = fab_elec_host::edit_actions::check_list(&engine.snapshot(), &[("only".into(), "erc".into())]).unwrap();
            for expected in ["VoltageDomain", "RailBudget", "OpenPort", "controller"] {
                assert!(text.contains(expected), "the committed check list must name {expected}: {text}");
            }
            assert!(!text.contains("Block("), "the check list must use readable block paths: {text}");
            let read = fab_elec_host::edit_actions::design_read(engine.design(), &[]).unwrap();
            for expected in ["blocks: [", "planned: true", "Controller", "power_in", "VDD"] {
                assert!(read.contains(expected), "design.read must expose {expected} for the accepted architecture");
            }
        }
    }
}

#[test]
fn held_board_proposal_judges_the_whole_brief_and_binds_its_verdict() {
    use fab_elec_host::req_actions::{design_facts, report};
    use fab_elec_req::Status;
    let brief = r#"brief("Build a probe that fits the opening")
require("fits the opening", board("probe").size, {max: [10mm 10mm]})
require("probe routing", board("probe").unrouted, {max: 100})
require("shared routing", board("shared").unrouted, {max: 100})
require("shared checks", checks(@drc, {unit: "shared"}).errors, {max: 100})
require("project DRC", checks(@drc).warnings, {max: 100})
require("project ERC", checks(@erc).errors, {max: 100})
require("fits the case", case("probe").fits)
require("starts up", bench("power"))
require("voltage", bench("power").expect("rail"))
require("print time", print.time, {max: 3h})
require("print mass", print.filament, {max: 40g})
"#;
    let layout = |name, edge| format!("board(\"{name}\")\noutline([[[0mm, 0mm], [{edge}mm, 0mm], [{edge}mm, {edge}mm], [0mm, {edge}mm], [0mm, 0mm]]])\n");
    let mut sources = fab_elec_lang::Sources::new();
    sources.add("probe.ckt.splash", PROBE_CIRCUIT).unwrap();
    sources.add("shared.ckt.splash", PROBE_CIRCUIT).unwrap();
    let board = sources.add("probe.pcb.splash", layout("probe", 8)).unwrap();
    sources.add("shared.pcb.splash", layout("shared", 8) + "zone(\"GND\", [\"F.Cu\"], @board)\n").unwrap();
    sources.add("targets.req.splash", brief).unwrap();
    sources.add("waivers.req.splash", r#"waive("print mass", "prototype")"#).unwrap();
    let (design, docs) = Docs::load(sources, fab_elec_lang::std_libs());
    let mut engine = Engine::new_held(design, Arc::default(), Arc::default(), Arc::default(), "probe".into()).with_docs(docs);
    let mail = Mailbox::default();
    mail.push(Cmd::Propose { set: vec![DocSet::Replace { name: "probe.pcb.splash".into(),
        base_rev: revision(&engine, board), text: layout("probe", 12).into() }],
        slot: "layout".into(), label: "Wider board".into(), by: fab_elec_host::ProposalAuthor::Local(By::Person), ticket: Ticket(1) });
    let started = step(&mut engine, &mail);
    mail.result(started.evals[0].run());
    let ready = step(&mut engine, &mail);
    let [(Ticket(1), Outcome::Proposed { id, .. })] = ready.outcomes.as_slice() else { panic!("the board-only proposal verifies: {:?}", ready.rejected) };
    let snap = engine.snapshot();
    let req = snap.proposals[0].requirements.as_ref().expect("the proposal inherits the complete brief");
    assert!(req.fail == 1 && req.failures[0].0 == "fits the opening" && req.failures[0].1.contains("12") && req.failures[0].1.contains("max 10"),
        "the untouched size requirement names the candidate measurement and the bound it breaks");
    let (live, _) = report(snap.docs.as_ref().unwrap(), &design_facts(&snap));
    assert_eq!(live.row("fits the opening").unwrap().status, Status::Pass, "the committed board still fits the opening");
    for name in ["fits the case", "starts up", "voltage", "print time"] {
        assert!(req.unjudged.iter().any(|(n, reason)| n == name && reason == "not judged for a proposal"),
            "{name} is unknown because its app producer has not judged the candidate");
    }
    assert_eq!(req.waived, 1, "the person's waiver in another candidate document still applies");
    for name in ["shared routing", "shared checks", "project DRC"] {
        assert!(req.unjudged.iter().any(|(n, reason)| n == name && reason.contains("pending")), "{name} depends on the outstanding copper work");
    }
    assert_eq!(req.pass, 2, "pending copper on the shared board allows current ERC and the probe's routing to pass");

    let [fill] = started.fills.as_slice() else { panic!("only the shared board has a fill in flight") };
    mail.result(fill.run());
    let filled = step(&mut engine, &mail);
    assert_eq!(engine.snapshot().proposals[0].requirements.as_ref(), Some(req), "a refill alone leaves whole-board checks unjudged");
    let [check] = filled.checks.as_slice() else { panic!("the shared board now runs its whole-board check") };
    mail.result(check.run());
    step(&mut engine, &mail);
    let settled = engine.snapshot();
    let held = &settled.proposals[0];
    let judged = held.requirements.as_ref().unwrap();
    assert_eq!((judged.pass, judged.fail, judged.unknown, judged.waived), (5, 1, 4, 1),
        "settled copper republishes the scoped and whole-project verdicts, keeping app measures unknown");

    // Keep the same failing row and counts, changing only the bound in its explanation.
    mail.push(change_doc(&engine, "targets.req.splash", brief.replace("10mm", "9mm")));
    let edit = step(&mut engine, &mail);
    run(&mut engine, &mail, edit);
    let latest = engine.snapshot();
    let shown = &latest.proposals[0];
    assert_eq!(shown.checks, held.checks, "a requirement-only edit leaves the candidate check summary identical");
    assert!(shown.shown == held.shown + 1 && shown.requirements.as_ref().unwrap().failures[0].1.contains("max 9"),
        "a changed failure reason alone advances the decision revision");
    mail.push(decision(*id, held.shown, true));
    let obsolete = step(&mut engine, &mail);
    assert!(matches!(obsolete.outcomes.as_slice(), [(_, Outcome::Refused(r))] if r.text.contains("requirement results changed")),
        "Accept on the old revision explains the changed requirements");
    assert_eq!(engine.docs().unwrap().text(board).unwrap().as_ref(), layout("probe", 8), "the obsolete Accept leaves the committed board untouched");

    let removal = |name: &str| Cmd::Propose { slot: "cleanup".into(), label: "Remove document".into(),
        set: vec![DocSet::Remove { name: name.into(), base_rev: revision(&engine, engine.docs().unwrap().sources().find(name).unwrap()) }],
        by: fab_elec_host::ProposalAuthor::Peer { label: "Partner".into() }, ticket: Ticket(2) };
    let ordinary = removal("waivers.req.splash");
    mail.push(removal("targets.req.splash"));
    let refused = step(&mut engine, &mail);
    assert!(matches!(refused.outcomes.as_slice(), [(_, Outcome::Refused(r))] if r.text.contains("targets.req.splash:1: the brief is the person's own words")),
        "removing the brief's whole document is refused at the original brief before a peer proposal is held");
    mail.push(ordinary);
    let held = step(&mut engine, &mail);
    assert!(matches!(held.outcomes.as_slice(), [(_, Outcome::Proposed { .. })]),
        "a peer can still propose removing a requirements document without a brief");
}

#[test]
fn held_proposal_rechecks_dependencies_and_parks_the_press() {
    let (mut engine, mail) = proposal_fixture();
    let (id, shown, _) = propose_probe(&mut engine, &mail);
    let first = engine.snapshot();
    let initial = first.proposals[0].checks.clone().unwrap();
    let extra = PROBE_BLOCK.replace("let r = e.r", "let parallel = e.r(o.r, p.top, p.gnd, {package: \"0402\"})\n        let r = e.r");
    mail.push(change_doc(&engine, "load.blocks.splash", extra));
    let changed = step(&mut engine, &mail);
    assert_eq!(changed.evals.len(), 1, "the committed block evaluates first");
    let waiting_clock = engine.snapshot().proposals[0].run.clone();
    mail.push(decision(id, shown - 1, false));
    let obsolete = step(&mut engine, &mail);
    assert!(matches!(obsolete.outcomes.as_slice(), [(_, Outcome::Refused(_))])
        && waiting_clock.started().is_none() && Arc::ptr_eq(&waiting_clock, &engine.snapshot().proposals[0].run),
        "republishing while the committed block evaluates preserves the waiting clock");
    mail.result(changed.evals[0].run());
    let recheck = step(&mut engine, &mail);
    assert_eq!(recheck.evals.len(), 1, "the candidate verifies against the new block");
    let snap = engine.snapshot();
    let checking = &snap.proposals[0];
    assert!(checking.design.is_none() && checking.boards.is_empty() && checking.sheets.is_empty(), "a recheck clears the previous candidate geometry");
    let clock = checking.run.clone();
    assert!(first.proposals[0].run.started().is_some() && !Arc::ptr_eq(&clock, &first.proposals[0].run)
        && clock.started().is_none(), "a queued re-verification has its own unstarted clock");
    mail.push(decision(id, shown, true));
    let parked = step(&mut engine, &mail);
    assert!(parked.outcomes.is_empty() && parked.evals.is_empty(), "a press during re-verification waits for the existing job");
    mail.result(recheck.evals[0].run());
    let refused = step(&mut engine, &mail);
    let snap = engine.snapshot();
    let current = &snap.proposals[0];
    assert!(clock.started().is_some() && Arc::ptr_eq(&current.run, &clock), "publishing checks preserves that verification's clock");
    assert_ne!(current.checks.as_ref().unwrap().counts, initial.counts, "the block's second resistor changes the candidate ERC");
    assert_eq!(current.shown, shown + 1, "changed checks advance the shown revision once");
    assert!(matches!(refused.outcomes.as_slice(), [(_, Outcome::Refused(r))] if r.text.contains("SinglePinNet")),
        "the parked press is refused with the changed check");
    assert!(engine.docs().unwrap().sources().find("brief.req.splash").is_none(), "the obsolete press applies nothing");
    mail.push(decision(id, current.shown, true));
    let accepted = step(&mut engine, &mail);
    assert!(accepted.evals.is_empty() && accepted.rejected.is_empty(), "accepting the new checks adopts directly");
    let docs = engine.docs().unwrap();
    let fresh = evaluate(docs.sources(), docs.libs()).unwrap();
    let expected = Design { generation: engine.design().generation, ..fresh.design };
    assert_eq!(engine.design().as_ref(), &expected, "the committed design equals a fresh evaluation at the same generation");

    mail.push(Cmd::Undo);
    let undo = step(&mut engine, &mail);
    run(&mut engine, &mail, undo);
    let (id, shown, _) = propose_probe(&mut engine, &mail);
    mail.push(Cmd::Docs { set: vec![DocSet::Add { name: "other.req.splash".into(), text: "// other brief".into() }],
        label: "Other requirements".into(), by: By::Person, ticket: None });
    let changed = step(&mut engine, &mail);
    run(&mut engine, &mail, changed);
    assert_eq!(engine.snapshot().proposals[0].shown, shown, "an unrelated document keeps the shown revision");
    mail.push(decision(id, shown, true));
    assert!(step(&mut engine, &mail).rejected.is_empty(), "the same press accepts after an unrelated change");

    let doc = engine.docs().unwrap().view().texts.iter().find(|d| d.name == "brief.req.splash").unwrap();
    mail.push(Cmd::Propose { slot: "requirements".into(), label: "Another requirement".into(), by: fab_elec_host::ProposalAuthor::Local(By::Person),
        set: vec![DocSet::Replace { name: doc.name.clone(), base_rev: doc.rev, text: "// deferred requirement".into() }], ticket: Ticket(3) });
    let started = step(&mut engine, &mail);
    run(&mut engine, &mail, started);
    let waiting = engine.snapshot().proposals[0].id;
    let unit = &engine.design().units[0];
    let ends = unit.circuit.nets.iter().take(2).map(|n| End::Net(n.id)).collect();
    mail.preview(Sample { seq: 1, tag: 9, place: vec![], label: "resistor".into(), by: By::Person, proposal: None, notes: vec![],
        circuits: vec![], expect: None, ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::AddPart {
            stmt: "let r = e.r(1kohm, _n0, _n1, {package: \"0402\"})".into(), after: None, ends, hint: None } }] });
    let gesture = step(&mut engine, &mail);
    run(&mut engine, &mail, gesture);
    assert!(engine.snapshot().preview.is_some(), "the gesture gets a live preview alongside the held proposal");
    mail.push(Cmd::CommitPreview { seq: 1, tag: 9 });
    let gesture = step(&mut engine, &mail);
    run(&mut engine, &mail, gesture);
    let snap = engine.snapshot();
    assert!(snap.preview.is_none() && snap.proposals[0].id == waiting && snap.proposals[0].design.is_some(),
        "committing the gesture leaves the independent proposal ready on its new base");

    // Only the proposed circuit uses draft_load. Losing its definition keeps the committed
    // project valid but introduces a mistake into the held proposal.
    let block_text = engine.docs().unwrap().view().texts.iter().find(|d| d.name == "load.blocks.splash").unwrap().text.clone();
    mail.push(change_doc(&engine, "load.blocks.splash", format!("{block_text}\nlet draft_load = load\n")));
    let block = step(&mut engine, &mail);
    run(&mut engine, &mail, block);
    let doc = engine.docs().unwrap().view().texts.iter().find(|d| d.name == "probe.ckt.splash").unwrap();
    mail.push(Cmd::Propose { slot: "draft".into(), label: "Draft block".into(), by: fab_elec_host::ProposalAuthor::Local(By::Person),
        set: vec![DocSet::Replace { name: doc.name.clone(), base_rev: doc.rev, text: doc.text.replace("= load(", "= draft_load(").into() }],
        ticket: Ticket(4) });
    let draft = step(&mut engine, &mail);
    run(&mut engine, &mail, draft);
    assert_eq!(engine.snapshot().proposals.len(), 2, "distinct slots keep independent verified candidates");
    mail.push(change_doc(&engine, "load.blocks.splash", block_text.to_string()));
    let block = step(&mut engine, &mail);
    run(&mut engine, &mail, block);
    let snap = engine.snapshot();
    assert!(matches!(&snap.proposals[1].state, fab_elec_host::ProposalState::Refused(reason) if reason.contains("draft_load")),
        "a mistake introduced only on re-verification leaves the refusal and its diagnostic on the row");
    let refused = &snap.proposals[1];
    assert!(refused.design.is_none() && refused.boards.is_empty() && refused.sheets.is_empty(), "a refused proposal has no drawable data");
    assert!(snap.proposals[0].design.is_some(), "failure of one slot leaves the other ready");
    mail.push(Cmd::Undo);
    let undo = step(&mut engine, &mail);
    run(&mut engine, &mail, undo);
    assert!(Arc::ptr_eq(&snap.proposals[1], &engine.snapshot().proposals[1]),
        "restoring the dependency leaves the refused proposal untouched");
    mail.push(Cmd::Propose { slot: "draft".into(), label: "Replacement".into(), set: vec![DocSet::Add {
        name: "replacement.req.splash".into(), text: "// next draft".into() }],
        by: fab_elec_host::ProposalAuthor::Local(By::Person), ticket: Ticket(5) });
    let replaced = step(&mut engine, &mail);
    let newest = engine.snapshot();
    assert!(newest.proposals.len() == 2 && newest.proposals[1].id != snap.proposals[1].id
        && newest.proposals[1].state == fab_elec_host::ProposalState::Checking,
        "a new proposal replaces the refused row in the same slot on arrival");
    assert!(matches!(run(&mut engine, &mail, replaced).outcomes.as_slice(), [(Ticket(5), Outcome::Proposed { .. })]),
        "the replacement ticket receives its own verification result");
}

#[test]
fn held_proposal_requires_a_local_press_to_check_peer_text() {
    use fab_elec_host::{ProposalAuthor, ProposalState};
    let (engine, mail) = proposal_fixture();
    let (mut engine, journal) = record_decisions(engine);
    for name in ["elec.step.accept", "elec.step.decline", "elec.step.check"] {
        assert!(fab_elec_design::action::find(name).is_none(), "a tool cannot invoke the decision {name}");
    }
    let ai = ProposalAuthor::Local(By::Ai { turn: 3, call: 1 });
    for (name, text, reason) in [("waive.req.splash", "waive(\"test\", \"because\")", "only the person waives"),
        ("program.fw.splash", "", "firmware")] {
        mail.push(Cmd::Propose { set: vec![DocSet::Add { name: name.into(), text: text.into() }],
            slot: "stage".into(), label: "Stage".into(), by: ai.clone(), ticket: Ticket(4) });
        let result = step(&mut engine, &mail);
        assert!(matches!(result.outcomes.as_slice(), [(Ticket(4), Outcome::Refused(r))] if r.text.contains(reason)),
            "proposing {name} returns the specific refusal");
        assert!(result.evals.is_empty() && engine.snapshot().proposals.is_empty() && engine.log().entries.is_empty(),
            "a planning refusal starts no job, shows no proposal and logs no edit");
    }

    let name = "lost.pcb.splash";
    mail.push(Cmd::Propose { set: vec![DocSet::Add { name: name.into(), text: "".into() }],
        slot: "stage".into(), label: "Stage".into(), by: ai, ticket: Ticket(4) });
    let mut published = None;
    let arrival = engine.step(mail.take(), &mut |s| published = Some(s));
    let snap = published.expect("arrival publishes the planned text before verification");
    let [local] = snap.proposals.as_slice() else { panic!("arrival shows exactly one planned proposal") };
    assert!(local.state == ProposalState::Checking && local.checks.is_none() && local.design.is_none(),
        "a local arrival shows its checking state before checks or a design exist");
    assert!(matches!(local.docs.as_slice(), [doc] if doc.name == name && doc.before.is_none() && doc.after.as_deref() == Some("")),
        "the arrival carries the exact planned document");
    assert!(arrival.evals.len() == 1 && arrival.outcomes.is_empty() && local.run.started().is_none(),
        "a queued job has neither a start time nor a verification outcome");
    let last_eval = arrival.evals[0].id;
    let done = arrival.evals[0].run();
    assert!(local.run.started().is_some(), "the pool starts the published clock before its result is delivered");
    mail.result(done);
    let result = step(&mut engine, &mail);
    let failed = engine.snapshot();
    let reason = match &result.outcomes[..] {
        [(Ticket(4), Outcome::Refused(r))] if r.text.contains(name) => &r.text,
        _ => panic!("the proposer's ticket receives the specific verification refusal"),
    };
    assert!(matches!(&failed.proposals[..], [v] if v.id == local.id && v.state == ProposalState::Refused(reason.to_string())
        && v.design.is_none() && v.checks.is_none() && Arc::ptr_eq(&v.docs, &local.docs)), "a refusal keeps the text and ticket reason on the row");
    mail.push(decision(local.id, local.shown, true));
    assert!(matches!(step(&mut engine, &mail).outcomes.as_slice(), [(_, Outcome::Refused(r))] if r.text == "this proposal is not usable"),
        "accepting a refused proposal explains why it cannot be used");
    mail.push(decision(local.id, local.shown, false));
    assert!(matches!(step(&mut engine, &mail).outcomes.as_slice(), [(_, Outcome::Declined { id })] if *id == local.id)
        && engine.snapshot().proposals.is_empty() && engine.log().entries.is_empty(), "dismiss ends the refused row without an edit");

    let peer = ProposalAuthor::Peer { label: "Design partner".into() };
    mail.push(Cmd::Propose { set: vec![DocSet::Add { name: "waive.req.splash".into(), text: "waive(\"test\", \"because\")".into() }],
        slot: "stage".into(), label: "Stage".into(), by: peer.clone(), ticket: Ticket(4) });
    let waived = step(&mut engine, &mail);
    assert!(matches!(waived.outcomes.as_slice(), [(_, Outcome::Refused(r))] if r.text.contains("only the person waives")),
        "a peer's text may propose a waiver but never grant one");
    assert!(waived.evals.is_empty() && engine.snapshot().proposals.is_empty(), "a peer's refused waiver starts no evaluation and is never shown");
    mail.push(Cmd::Propose { slot: "stage".into(), set: proposed_changes(&engine), label: "Probe".into(), by: peer.clone(), ticket: Ticket(4) });
    let held = step(&mut engine, &mail);
    let [(Ticket(4), Outcome::Proposed { id, shown, checks: None, requirements: None })] = held.outcomes.as_slice()
        else { panic!("the peer proposal answers immediately with no checks") };
    let (id, shown) = (*id, *shown);
    let snap = engine.snapshot();
    let v = &snap.proposals[0];
    assert!(held.evals.is_empty() && v.by == peer && v.state == ProposalState::Unchecked, "receiving peer text starts no evaluation");
    assert!(v.design.is_none() && v.checks.is_none() && v.docs.len() == 2, "only the peer's document diffs are available");
    mail.push(decision(id, shown, true));
    let refused = step(&mut engine, &mail);
    assert!(matches!(refused.outcomes.as_slice(), [(_, Outcome::Refused(r))] if r.text.contains("check it first")),
        "the person must check a peer proposal before accepting");
    assert!(refused.evals.is_empty() && engine.log().entries.is_empty(), "an unchecked accept starts no work and applies nothing");
    mail.push(Cmd::CheckProposal { id, ticket: None });
    let check = step(&mut engine, &mail);
    assert_eq!(check.evals.len(), 1, "the person's check press starts the first evaluation of the peer's text");
    assert_eq!(check.evals[0].id, last_eval + 1, "the unchecked proposal and its refused accept left the evaluation counter unchanged");
    mail.push(decision(id, shown, true));
    let parked = step(&mut engine, &mail);
    assert!(parked.outcomes.is_empty(), "a press while checking waits for the first peer verification");
    mail.result(check.evals[0].run());
    let first = step(&mut engine, &mail);
    assert!(matches!(first.outcomes.as_slice(), [(_, Outcome::Refused(r))] if r.text.contains("first verified checks")),
        "a press on unchecked text cannot accept the newly shown checks");
    let checked = engine.snapshot();
    let v = &checked.proposals[0];
    assert!(v.state == ProposalState::Ready && v.design.is_some() && v.checks.is_some(), "the checked peer candidate is ready");
    assert!(journal.try_recv().is_err(), "checking peer text and refusing the parked press bind no journal call");
    mail.push(decision(id, v.shown, true));
    let accepted = step(&mut engine, &mail);
    assert!(accepted.rejected.is_empty() && engine.log().entries.len() == 1, "the checked peer proposal installs one entry");
    assert_eq!(engine.log().entries[0].by, By::Person, "the person owns the locally accepted peer edit");
    assert_eq!(journal.try_iter().collect::<Vec<_>>(), [
        JournalNotice::Decision(Ticket(2), "elec.step.accept".into(), vec![("proposal".into(), id.0.to_string()),
            ("slot".into(), "stage".into()), ("label".into(), "Probe".into()), ("author".into(), "peer".into()),
            ("peer".into(), "Design partner".into())]),
        JournalNotice::Effect(Some(Ticket(2)), By::Person), JournalNotice::Done(Ok(engine.log().entries[0].seq)),
    ], "the decision names the peer and its linked effect belongs to the person who accepted");
}

#[test]
fn document_set_plans_verify_before_install() {
    use fab_elec_lang::{docs_verified, NO_CANCEL};

    let mut before = fab_elec_lang::Sources::new();
    let probe = before.add("probe.ckt.splash", PROBE_CIRCUIT).expect("the probe fixture is a circuit document");
    let prev = evaluate(&before, &fab_elec_lang::std_libs()).expect("the one-resistor probe evaluates");
    let (_, mut docs) = Docs::from_evaluated(before.clone(), prev.clone());
    let view = docs.view().clone();
    let circuit = &view.texts[0];
    let replacement: Arc<str> = PROBE_CIRCUIT.replace("1kohm", "2kohm").into();
    let requirement: Arc<str> = "require(\"two layers\", board(\"probe\").layers, {max: 2})\n".into();
    let changes = |name: &str, text| [
        DocSet::Add { name: name.into(), text },
        DocSet::Replace { name: circuit.name.clone(), base_rev: circuit.rev, text: replacement.clone() },
    ];
    let set = changes("brief.req.splash", requirement.clone());
    let revision = docs.revision();
    let (planned, edit) = docs.plan_set(&set).expect("the document changes plan together");
    assert_eq!(docs.sources(), &before, "planning preserves the installed documents and their texts");
    assert_eq!(docs.revision(), revision, "planning preserves the document revisions");
    assert_eq!(docs.view().texts, view.texts, "planning preserves each published text and revision");
    let added = planned.find("brief.req.splash").expect("the new requirements document is present in the plan");
    assert_eq!(planned.doc(added).unwrap().text, requirement, "the plan contains the full requirements text");
    assert_eq!(planned.doc(probe).unwrap().text, replacement, "the plan contains the replaced circuit text");
    let verified = docs_verified(planned, &prev.libs, &prev, edit, &NO_CANCEL)
        .expect("the valid document set verifies");

    // An unmatched board produces a project error note located only in the new document.
    let unmatched = "unmatched.pcb.splash";
    let invalid_set = changes(unmatched, "board(\"unmatched\")\n".into());
    let (invalid, edit) = docs.plan_set(&invalid_set).expect("the unmatched board can be planned");
    let board = invalid.find(unmatched).expect("the invalid plan includes the new board");
    let error = docs_verified(invalid, &prev.libs, &prev, edit, &NO_CANCEL)
        .expect_err("a new error in a created document refuses the plan");
    assert!(error.diags().iter().any(|d| d.message.contains(unmatched)
        && d.sel.iter().any(|s| matches!(s, Sel::Source { doc, .. } if *doc == board))),
        "the refusal names and locates the created board: {error:?}");

    docs.doc_set(&set).expect("the valid document set installs");
    assert_eq!(docs.sources(), &verified.sources, "installation preserves every planned document and its exact bytes");
}

#[test]
fn whole_documents_from_other_writers_are_atomic_and_undoable() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    let docs = engine.docs().expect("the fixture has documents");
    let controller = docs.sources().find("ledctl.ckt.splash").expect("the controller circuit exists");
    let controller_text = docs.text(controller).expect("the controller has text").clone();
    let rev = |engine: &Engine, doc: DocId| {
        engine.docs().expect("documents remain").view().texts.iter()
            .find(|text| text.doc == doc).expect("the present document has a revision").rev
    };
    let circuit: Arc<str> = PROBE_CIRCUIT.into();
    let layout: Arc<str> = "board(\"probe\")\noutline([[[0mm, 0mm], [10mm, 0mm], [10mm, 10mm], [0mm, 10mm], [0mm, 0mm]]])\n".into();
    // Revisions count from the load's first one (each load starts above every earlier one).
    let first = rev(&engine, controller);
    mailbox.push(Cmd::Docs {
        set: vec![
            DocSet::Add { name: "probe.ckt.splash".into(), text: circuit.clone() },
            DocSet::Add { name: "probe.pcb.splash".into(), text: layout.clone() },
        ],
        label: "add probe documents".into(), by: By::Person, ticket: Some(Ticket(40)),
    });
    let added = step(&mut engine, &mailbox);
    assert!(added.rejected.is_empty(), "another writer's document additions are accepted");
    assert_eq!(engine.log().entries.len(), 1, "adding both documents logs one undo entry");
    assert_eq!(engine.log().entries[0].kind, EntryKind::Eval, "whole documents use evaluation history");
    assert_eq!(added.evals.len(), 1, "documents added after the last evaluation start a new evaluation");
    assert!(matches!(added.outcomes.as_slice(), [(Ticket(40), Outcome::Done { label, created, sites, .. })]
        if label == "add probe documents" && created.is_empty() && sites.is_empty()),
        "the writer receives one completion for the whole document set");
    let docs = engine.docs().expect("documents remain after add");
    let original = docs.sources().find("probe.ckt.splash").expect("the added circuit is present");
    let original_board = docs.sources().find("probe.pcb.splash").expect("the added board is present");
    assert_eq!((docs.text(original), docs.text(original_board)), (Some(&circuit), Some(&layout)),
        "adding documents preserves every byte supplied by the writer");
    assert_eq!((rev(&engine, original), rev(&engine, original_board)), (first + 1, first + 1), "each added document starts one revision past the load's");
    run(&mut engine, &mailbox, added);
    assert_eq!(engine.design().units.len(), 2, "evaluation includes the added unit alongside the controller");
    let unit = engine.design().units.iter().find(|unit| unit.name == "probe").expect("the added circuit evaluates").id;
    let with_probe = texts(&engine);

    mailbox.push(Cmd::Undo);
    let undo = step(&mut engine, &mailbox);
    assert!(engine.docs().expect("documents remain").behind(), "undoing an add is behind before evaluation even though its text is unchanged");
    assert!(engine.docs().expect("documents remain").sources().docs().iter()
        .all(|doc| doc.id != original && doc.id != original_board), "undo makes both added documents absent");
    run(&mut engine, &mailbox, undo);
    assert!(engine.design().unit(unit).is_none(), "undo evaluation removes the added unit");
    assert_eq!(texts(&engine), loaded, "undo of add preserves every controller document byte");
    mailbox.push(Cmd::Redo);
    let redo = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, redo);
    assert_eq!(engine.design().units.iter().find(|unit| unit.name == "probe").map(|unit| unit.id), Some(unit),
        "redo restores the original BoardId");
    assert_eq!(texts(&engine), with_probe, "redo restores the same document ids and bytes");
    assert_eq!((rev(&engine, original), rev(&engine, original_board)), (first + 3, first + 3), "undo and redo each advance both presence revisions");

    let replacement: Arc<str> = controller_text.replace("let boot_r = e.r(10kohm", "let boot_r = e.r(4.7kohm").into();
    assert_ne!(replacement, controller_text, "the replacement changes the fixture's boot resistor");
    let base_rev = rev(&engine, controller);
    let entries = engine.log().entries.len();
    mailbox.push(Cmd::Docs {
        set: vec![
            DocSet::Add { name: "pending.ckt.splash".into(), text: circuit.clone() },
            DocSet::Replace { name: "ledctl.ckt.splash".into(), base_rev: base_rev + 1, text: replacement.clone() },
        ],
        label: "stale replacement".into(), by: By::Person, ticket: Some(Ticket(41)),
    });
    let refused = step(&mut engine, &mailbox);
    assert_eq!(refused.rejected.len(), 1, "a stale writer revision refuses the entire set");
    assert!(matches!(refused.outcomes.as_slice(), [(Ticket(41), Outcome::Refused(refusal))]
        if refusal.text == "the document changed since the writer read it"
            && refusal.site == Some(Sel::Source { doc: controller, start: 0, end: 0 })),
        "a stale writer receives the located revision refusal");
    assert!(refused.evals.is_empty(), "a refused set starts no evaluation");
    assert_eq!(engine.log().entries.len(), entries, "a stale replacement logs nothing");
    assert_eq!(texts(&engine), with_probe, "a stale replacement rolls back earlier additions and preserves every byte");
    assert_eq!(rev(&engine, controller), base_rev, "a refused replacement leaves its revision unchanged");

    mailbox.push(Cmd::Docs {
        set: vec![DocSet::Replace { name: "ledctl.ckt.splash".into(), base_rev, text: replacement.clone() }],
        label: "replace controller".into(), by: By::Person, ticket: None,
    });
    let replaced = step(&mut engine, &mailbox);
    assert_eq!(engine.log().entries.len(), entries + 1, "a current replacement logs one entry");
    assert_eq!(replaced.evals.len(), 1, "a current replacement starts evaluation");
    run(&mut engine, &mailbox, replaced);
    let resistance = engine.design().unit(BoardId(0)).expect("the controller remains").circuit.comp("mcu.boot_r")
        .expect("the boot resistor remains").value.as_ref().and_then(|value| value.qty).expect("the resistor has a quantity").si;
    assert_eq!(resistance, 4700.0, "evaluation adopts the replacement circuit's resistor value");
    assert_eq!(engine.docs().expect("documents remain").text(controller), Some(&replacement), "the replacement keeps every supplied byte");
    mailbox.push(Cmd::Undo);
    let undo = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undo);
    assert_eq!(texts(&engine), with_probe, "undo of replacement restores every original document byte");

    let head = engine.log().head;
    mailbox.push(Cmd::Docs {
        set: vec![DocSet::Remove { name: "probe.ckt.splash".into(), base_rev: rev(&engine, original) }],
        label: "remove probe".into(), by: By::Person, ticket: None,
    });
    let removed = step(&mut engine, &mailbox);
    assert_eq!(engine.log().head, head + 1, "removing a circuit and its board logs one entry");
    assert!(engine.docs().expect("documents remain").sources().docs().iter()
        .all(|doc| doc.id != original && doc.id != original_board), "removing the circuit also removes its board document");
    run(&mut engine, &mailbox, removed);
    assert!(engine.design().unit(unit).is_none(), "remove evaluates without the removed unit");
    assert_eq!(texts(&engine), loaded, "remove leaves the controller's document bytes untouched");
    mailbox.push(Cmd::Undo);
    let undo = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undo);
    assert_eq!(engine.design().units.iter().find(|unit| unit.name == "probe").map(|unit| unit.id), Some(unit),
        "undo of remove restores the original BoardId");
    assert_eq!(texts(&engine), with_probe, "undo of remove restores the circuit and board bytes and document ids");

    let head = engine.log().head;
    mailbox.push(Cmd::Docs {
        set: vec![DocSet::Remove { name: "probe.ckt.splash".into(), base_rev: rev(&engine, original) }],
        label: "remove before reuse".into(), by: By::Person, ticket: None,
    });
    let removed = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, removed);
    let other_text: Arc<str> = circuit.replace("1kohm", "2.2kohm").into();
    mailbox.push(Cmd::Docs {
        set: vec![DocSet::Add { name: "probe.ckt.splash".into(), text: other_text.clone() }],
        label: "reuse probe name".into(), by: By::Person, ticket: None,
    });
    let reused = step(&mut engine, &mailbox);
    assert_eq!(engine.log().entries.len(), head + 2, "remove and name reuse are both logged");
    run(&mut engine, &mailbox, reused);
    let new_doc = engine.docs().expect("documents remain").sources().find("probe.ckt.splash").expect("the reused name is present");
    assert_ne!(new_doc, original, "name reuse allocates a new document id");
    let new_unit = engine.design().units.iter().find(|unit| unit.name == "probe").expect("the new document evaluates").id;
    assert_ne!(new_unit, unit, "the new document does not take the original unit's BoardId");
    for _ in 0..2 {
        mailbox.push(Cmd::Undo);
        let undo = step(&mut engine, &mailbox);
        run(&mut engine, &mailbox, undo);
    }
    assert_eq!(texts(&engine), with_probe, "undo twice restores the original document ids and every original byte after name reuse");
    assert_eq!(engine.design().units.iter().find(|unit| unit.name == "probe").map(|unit| unit.id), Some(unit),
        "undo twice after name reuse restores the original unit's BoardId");
    for _ in 0..2 {
        mailbox.push(Cmd::Redo);
        let redo = step(&mut engine, &mailbox);
        run(&mut engine, &mailbox, redo);
    }
    let docs = engine.docs().expect("documents remain");
    assert!(docs.sources().docs().iter().any(|doc| doc.id == new_doc && doc.text == other_text),
        "redo twice restores the new document with its other text");
    assert!(docs.sources().docs().iter().all(|doc| doc.id != original && doc.id != original_board),
        "redo twice leaves the original circuit and board absent");
    assert_eq!(engine.design().units.iter().find(|unit| unit.name == "probe").map(|unit| unit.id), Some(new_unit),
        "redo of name reuse retains the new document's BoardId too");

    let before_duplicate = texts(&engine);
    let entries = engine.log().entries.len();
    let base_rev = rev(&engine, new_doc);
    mailbox.push(Cmd::Docs {
        set: vec![
            DocSet::Replace { name: "probe.ckt.splash".into(), base_rev, text: circuit.clone() },
            DocSet::Remove { name: "probe.ckt.splash".into(), base_rev },
        ],
        label: "duplicate document".into(), by: By::Person, ticket: None,
    });
    let duplicate = step(&mut engine, &mailbox);
    assert_eq!(duplicate.rejected, vec![("duplicate document".into(), "one change per document".into())],
        "two changes naming the same document refuse the set");
    assert_eq!(engine.log().entries.len(), entries, "a duplicate-document refusal logs nothing");
    assert_eq!(texts(&engine), before_duplicate, "a duplicate-document refusal leaves all text and presence unchanged");
    assert_eq!(rev(&engine, new_doc), base_rev, "a duplicate-document refusal leaves the revision unchanged");
}

/// The new component is selected from the verified map before adoption consumes it.
#[test]
fn an_added_part_reports_only_new_selections_inside_its_statement() {
    let (mut engine, mailbox, ..) = setup();
    let before = engine.docs().expect("the fixture has documents").map().clone();
    mailbox.push(Cmd::Commit {
        ops: vec![add_resistor(&engine)],
        place: vec![], label: "add resistor".into(), circuits: vec![], expect: None,
        by: By::Ai { turn: 1, call: 2 }, ticket: Some(Ticket(7)),
    });
    let requested = step(&mut engine, &mailbox);
    assert_eq!(requested.evals.len(), 1, "AddPart goes through one pool verification");
    assert!(requested.outcomes.is_empty(), "the commit waits for adoption before answering");
    let done = requested.evals[0].run();
    let TaskResult::Eval(result) = &done else { panic!("AddPart produces an evaluation result") };
    let EvalOut::Verified(Ok((verified, _))) = &result.out else { panic!("the fixture's placeholder-based AddPart verifies") };
    let written = verified.edit.written.clone();
    assert_eq!(written.len(), 1, "AddPart reports one newly written statement");
    mailbox.result(done);
    let adopted = step(&mut engine, &mailbox);
    assert!(adopted.rejected.is_empty(), "the verified AddPart is adopted");
    let unit = &engine.design().units[0];
    let part = unit.circuit.comp("r_btn").expect("the splice names the new part from its BTN net");
    let selection = Sel::Comp(CompRef { board: unit.id, comp: part.id });
    let snapshot = engine.snapshot();
    let committed = snapshot.last_commit.as_ref().expect("adoption publishes the new commit");
    assert!(committed.created.contains(&selection), "Committed.created includes the new resistor component");
    assert!(committed.created.iter().all(|sel| before.locate(sel).is_none()),
        "Committed.created excludes every selection that existed before the commit");
    let after = engine.docs().expect("adoption retains documents");
    assert!(committed.created.iter().all(|sel| after.map().locate(sel).is_some_and(|range|
        written.iter().any(|at| at.doc == range.doc && at.span.start <= range.span.start && range.span.end <= at.span.end))),
        "every created selection lies inside a written statement");
    let sites: Vec<_> = written.iter().map(|at| Sel::Source { doc: at.doc, start: at.span.start, end: at.span.end }).collect();
    assert!(matches!(adopted.outcomes.as_slice(), [(Ticket(7), Outcome::Done { seq, created, sites: actual, .. })]
        if *seq == committed.seq && *created == committed.created && *actual == sites),
        "the verified commit's caller gets its sequence, created selections and written ranges");
    let at = &written[0];
    let text = after.text(at.doc).expect("the written document remains");
    assert_eq!(&text[at.span.start as usize..at.span.end as usize], "let r_btn = e.r(10kohm, key, v33, {package: \"0402\" added: 1})",
        "the reported site contains the inserted statement with its placeholders resolved");
}

/// The same footprint-bearing statement used by creation and placement checks.
fn add_resistor(engine: &Engine) -> DesignOp {
    let unit = &engine.design().units[0];
    let nets = ["BTN", "3V3"].map(|name| unit.circuit.nets.iter().find(|net| net.name == name)
        .expect("the controller supplies the placeholder's net").id).to_vec();
    DesignOp::Circuit { unit: unit.id, op: CircuitOp::AddPart {
        stmt: "let r = e.r(10kohm, _n0, _n1, {package: \"0402\"})".into(),
        after: None, ends: nets.into_iter().map(End::Net).collect(), hint: None,
    } }
}

#[test]
fn an_added_part_is_placed_in_one_commit_and_undo_restores_both_documents() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    let unit = engine.design().units[0].id;
    let at = P::mm(18.0, 12.0);
    mailbox.push(Cmd::Commit {
        ops: vec![add_resistor(&engine)],
        place: vec![Place { unit, part: PartRef::Added(0), at, rot: None, side: None }],
        label: "add and place resistor".into(), circuits: vec![], expect: None, by: By::Person, ticket: Some(Ticket(8)),
    });
    let requested = step(&mut engine, &mailbox);
    assert_eq!(requested.evals.len(), 1, "adding and placing share one pool job");
    assert_eq!(texts(&engine), loaded, "the pending placement changes no document bytes");
    let adopted = run(&mut engine, &mailbox, requested);
    let part = engine.design().unit(unit).expect("the unit remains").circuit.comp("r_btn").expect("the resistor was added");
    let placed: Vec<_> = board(&engine).placed.iter().filter(|placed| placed.comp == part.id).collect();
    assert_eq!(placed.len(), 1, "the new resistor has exactly one placement");
    assert_eq!((placed[0].at, placed[0].rot, placed[0].side), (at, Angle::ZERO, Side::Top),
        "the new footprint uses the requested position and default rotation and side");
    let component = Sel::Comp(CompRef { board: unit, comp: part.id });
    let item = Sel::Item(unit, placed[0].id);
    assert_eq!(engine.log().entries.len(), 1, "adding and placing create one history entry");
    assert_eq!(engine.log().entries[0].kind, EntryKind::Eval, "adding and placing use evaluation history");
    let snapshot = engine.snapshot();
    let committed = snapshot.last_commit.as_ref().expect("adoption publishes the commit");
    assert!(committed.created.contains(&component) && committed.created.contains(&item),
        "Committed.created includes both the component and its placed board item");
    assert!(matches!(adopted.outcomes.as_slice(), [(Ticket(8), Outcome::Done { created, circuit: CircuitChange::Structure, .. })]
        if *created == committed.created), "adding and placing a part reports both new selections and a structural change");
    let committed_texts = texts(&engine);
    assert_eq!(loaded.iter().zip(&committed_texts).filter(|(before, after)| before != after).count(), 2,
        "the commit changes both the circuit and board documents");
    check_board(&engine);
    mailbox.push(Cmd::Undo);
    let undo = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undo);
    assert_eq!(texts(&engine), loaded, "one undo restores every original circuit and board byte");
    mailbox.push(Cmd::Redo);
    let redo = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, redo);
    assert_eq!(texts(&engine), committed_texts, "one redo restores every committed circuit and board byte");
    check_board(&engine);
}

#[test]
fn an_added_part_preview_places_without_changing_documents_and_carries_the_placement() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    let unit = engine.design().units[0].id;
    let at = P::mm(18.0, 12.0);
    mailbox.preview(Sample {
        seq: 1, tag: 9, ops: vec![add_resistor(&engine)],
        place: vec![Place { unit, part: PartRef::Added(0), at, rot: None, side: None }],
        label: "preview resistor placement".into(), by: By::Person, proposal: None, notes: vec![], circuits: vec![], expect: None,
    });
    let requested = step(&mut engine, &mailbox);
    assert_eq!(requested.evals.len(), 1, "the added-part preview starts one evaluation job");
    run(&mut engine, &mailbox, requested);
    let snapshot = engine.snapshot();
    let preview = snapshot.preview.as_ref().expect("the evaluated placement is previewed");
    let preview_unit = preview.unit(unit).expect("the preview retains the unit");
    let part = preview_unit.circuit.comp("r_btn").expect("the preview adds the resistor");
    let placed = preview_unit.board.as_ref().expect("the preview retains the board").placed.iter()
        .find(|placed| placed.comp == part.id).expect("the preview places its new resistor");
    assert_eq!(placed.at, at, "the preview footprint is at the requested anchor");
    let item = Sel::Item(unit, placed.id);
    assert!(snapshot.created.contains(&item), "the preview's created selections include the placed board item");
    // Fable H1/H2 (11-16): the preview publishes the row index of its own circuit, which covers every
    // preview item, keeps every committed item's row and issues new rows past the committed count.
    let committed = engine.design().unit(unit).expect("the committed unit remains").circuit.clone();
    let committed_index = snapshot.sheets.iter().find(|sheet| sheet.unit == Some(unit) && sheet.of == StateOf::Committed)
        .expect("the committed rows are published").value.clone();
    let shown = snapshot.of(&snapshot.sheets, unit).expect("the preview's unit has rows");
    assert_eq!(shown.of, StateOf::Preview, "a circuit preview publishes its own row index");
    let index = shown.value.clone();
    for comp in &preview_unit.circuit.comps {
        let row = index.rows.comp(comp.id.0).unwrap_or_else(|| panic!("the preview index has a row for {}", comp.path));
        match committed.comp(&comp.path) {
            Some(old) => assert_eq!(Some(row), committed_index.rows.comp(old.id.0), "{} keeps its committed row in the preview", comp.path),
            None => assert!(row >= committed_index.count, "{} takes a row at or past the committed count", comp.path),
        }
    }
    assert_eq!(texts(&engine), loaded, "previewing changes no document bytes");
    assert!(engine.log().entries.is_empty(), "previewing creates no history entry");
    assert!(engine.design().unit(unit).expect("the committed unit remains").circuit.comp("r_btn").is_none(),
        "the new preview part is absent from the committed circuit");
    mailbox.push(Cmd::CommitPreview { seq: 1, tag: 9 });
    let dropped = step(&mut engine, &mailbox);
    assert!(dropped.rejected.is_empty() && dropped.evals.is_empty(), "the preview placement carries without a second job");
    assert_eq!(engine.log().entries.len(), 1, "dropping the placement preview logs one entry");
    assert_eq!(engine.log().entries[0].kind, EntryKind::Eval, "the carried placement uses evaluation history");
    assert!(Arc::ptr_eq(engine.design(), preview), "the carried commit adopts the exact preview design");
    let carried = engine.snapshot();
    let sheet = carried.of(&carried.sheets, unit).expect("the carried unit has rows");
    assert!(sheet.of == StateOf::Committed && Arc::ptr_eq(&sheet.value, &index),
        "the carried commit publishes the preview's index Arc, so no view lays the sheet out again");
    assert!(engine.snapshot().last_commit.as_ref().expect("the drop publishes its commit").created.contains(&item),
        "the carried commit retains the placed item's created selection");
    check_board(&engine);
}

#[test]
fn invalid_placement_references_refuse_the_whole_verified_commit_at_a_site() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    let design = engine.design().clone();
    let unit = &design.units[0];
    for (part, message) in [
        (PartRef::Added(1), "Added(1) names no AddPart of this commit".to_string()),
        (PartRef::Path("missing_part".into()), format!("missing_part is not a part of {}", unit.name)),
    ] {
        mailbox.push(Cmd::Commit {
            ops: vec![add_resistor(&engine)],
            place: vec![Place { unit: unit.id, part, at: P::mm(18.0, 12.0), rot: None, side: None }],
            label: "invalid placement".into(), by: By::Person, ticket: Some(Ticket(10)), circuits: vec![], expect: None,
        });
        let requested = step(&mut engine, &mailbox);
        assert_eq!(requested.evals.len(), 1, "placement references are resolved after the circuit evaluates");
        mailbox.result(requested.evals[0].run());
        let refused = step(&mut engine, &mailbox);
        assert_eq!(refused.rejected, vec![("invalid placement".into(), message.clone())], "the invalid reference reports its precise reason");
        let snapshot = engine.snapshot();
        let refusal = snapshot.last_refusal.as_ref().expect("the invalid reference publishes a refusal");
        assert!(matches!(refusal.site, Some(Sel::Source { .. })), "the invalid placement refusal has a source location");
        assert!(matches!(refused.outcomes.as_slice(), [(Ticket(10), Outcome::Refused(answer))] if answer.text == message),
            "the caller receives the placement refusal");
        assert!(Arc::ptr_eq(engine.design(), &design), "the refused placement preserves the committed design");
        assert_eq!(texts(&engine), loaded, "the refused placement preserves every document byte");
        assert!(engine.log().entries.is_empty(), "the refused placement commits neither the part nor its placement");
    }
}

#[test]
fn a_tray_path_previews_and_commits_through_the_board_path() {
    let (mut engine, mailbox, ..) = setup();
    let unit = engine.design().units[0].id;
    let part = board(&engine).placed[0].clone();
    mailbox.push(Cmd::Commit {
        ops: vec![DesignOp::Board { unit, op: BoardOp::Remove(part.id) }], place: vec![],
        label: "return part to tray".into(), by: By::Person, ticket: None, circuits: vec![], expect: None,
    });
    let removed = step(&mut engine, &mailbox);
    assert!(removed.rejected.is_empty() && removed.evals.is_empty(), "removing the footprint uses the board path");
    assert!(board(&engine).placed.iter().all(|placed| placed.comp != part.comp), "the removed footprint's component waits in the tray");
    let before = texts(&engine);
    let entries = engine.log().entries.len();
    let at = P::mm(18.0, 12.0);
    let place = vec![Place { unit, part: PartRef::Path(part.path.clone()), at, rot: None, side: None }];
    mailbox.preview(Sample {
        seq: 1, tag: 11, ops: vec![], place: place.clone(), label: "preview tray part".into(),
        by: By::Person, proposal: None, notes: vec![], circuits: vec![], expect: None,
    });
    let previewed = step(&mut engine, &mailbox);
    assert!(previewed.rejected.is_empty() && previewed.evals.is_empty(), "a tray placement preview needs no evaluation job");
    let snapshot = engine.snapshot();
    let preview = snapshot.preview.as_ref().expect("the tray placement is previewed");
    let placed = preview.unit(unit).expect("the preview has the unit").board.as_ref().expect("the preview has its board").placed.iter()
        .find(|placed| placed.comp == part.comp).expect("the preview places the tray part");
    assert_eq!(placed.at, at, "the tray preview uses the requested anchor");
    assert!(snapshot.created.contains(&Sel::Item(unit, placed.id)), "the tray preview reports its created board item");
    assert_eq!(texts(&engine), before, "the tray preview preserves all document bytes");
    assert_eq!(engine.log().entries.len(), entries, "the tray preview adds no history entry");
    mailbox.push(Cmd::CancelPreview { seq: 1 });
    step(&mut engine, &mailbox);
    mailbox.push(Cmd::Commit {
        ops: vec![], place, label: "place tray part".into(), by: By::Person, ticket: None, circuits: vec![], expect: None,
    });
    let committed = step(&mut engine, &mailbox);
    assert!(committed.rejected.is_empty() && committed.evals.is_empty(), "a Path placement commits without an evaluation job");
    assert_eq!(engine.log().entries.len(), entries + 1, "the Path placement adds exactly one history entry");
    assert_eq!(engine.log().entries.last().expect("the placement is logged").kind, EntryKind::Board, "the tray placement uses Board history");
    let placed = board(&engine).placed.iter().find(|placed| placed.comp == part.comp).expect("the tray part is placed again");
    assert_eq!(placed.at, at, "the committed tray footprint uses the requested anchor");
    assert!(engine.snapshot().last_commit.as_ref().expect("the placement publishes a commit").created.contains(&Sel::Item(unit, placed.id)),
        "the board commit reports the placed item's selection");
    check_board(&engine);
}

/// A placement lands with its labels: an LED put back in ledctl's tight row, whose label's default
/// spot lies on its neighbour's pad, takes its row's label spot in the placement's own undo step.
#[test]
fn a_placement_arranges_the_labels_it_disturbs_in_its_own_step() {
    let (mut engine, mailbox, ..) = setup();
    let unit = engine.design().units[0].id;
    let part = board(&engine).placed.iter().find(|placed| placed.path == "lamps[3].d").expect("ledctl has its fourth LED").clone();
    mailbox.push(Cmd::Commit {
        ops: vec![DesignOp::Board { unit, op: BoardOp::Remove(part.id) }], place: vec![],
        label: "return part to tray".into(), by: By::Person, ticket: None, circuits: vec![], expect: None,
    });
    step(&mut engine, &mailbox);
    let entries = engine.log().entries.len();
    mailbox.push(Cmd::Commit {
        ops: vec![],
        place: vec![Place { unit, part: PartRef::Path(part.path.clone()), at: part.at, rot: Some(part.rot), side: Some(part.side) }],
        label: "place tray part".into(), by: By::Person, ticket: None, circuits: vec![], expect: None,
    });
    let committed = step(&mut engine, &mailbox);
    assert!(committed.rejected.is_empty(), "the placement and its labels commit: {:?}", committed.rejected);
    assert_eq!(engine.log().entries.len(), entries + 1, "the placement and its labels are one undo step");
    let board = board(&engine);
    let placed = board.placed.iter().find(|placed| placed.comp == part.comp).expect("the LED is placed again");
    assert!(placed.ref_label.is_some(), "the LED's label left its default spot on its neighbour's pad");
    assert!(label_ops(unit, board, &[], &[placed.id]).is_empty(), "nothing is left for key A to arrange around the placed LED");
    check_board(&engine);
}

/// A value token changes on the pool and the already-requested undo follows its adoption in order.
#[test]
fn a_value_edit_changes_one_token_keeps_the_board_and_undo_waits_its_turn() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    let loaded_board = board(&engine).clone();
    let loaded_circuit = engine.design().units[0].circuit.clone();
    let loaded_value = value(&engine);
    mailbox.push(set_value(&engine));
    let mut published = Vec::new();
    let requested = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(requested.evals.len(), 1, "a canvas value edit emits one verification job");
    assert!(requested.waiting, "the host stays busy during verification");
    assert!(published.last().expect("starting verification publishes a snapshot").stale.has(StageFlags::EVAL), "verification marks the published snapshot stale");
    assert_eq!(texts(&engine), loaded, "verification changes no text before adoption");
    mailbox.push(Cmd::Undo);
    let held = step(&mut engine, &mailbox);
    assert!(held.waiting && held.evals.is_empty(), "undo waits behind the existing job without emitting another");
    assert_eq!(engine.log().head, 0, "the held undo cannot move the history head yet");

    let done = requested.evals[0].run();
    let TaskResult::Eval(result) = &done else { panic!("a verification job returns an evaluation result") };
    let EvalOut::Verified(Ok((verified, _))) = &result.out else { panic!("the value edit verifies") };
    assert_eq!(verified.edit.edits.len(), 1, "the value edit changes exactly one source span");
    let edit = &verified.edit.edits[0];
    let original = &loaded.iter().find(|(doc, _)| *doc == edit.doc).expect("the edited document was loaded").1;
    assert_eq!(&original[edit.span.start as usize..edit.span.end as usize], "10kohm", "the source span is the original value token");
    assert_eq!(edit.text, "4.7kohm", "the replacement keeps the original unit suffix");
    for (doc, text) in &loaded {
        let expected = if *doc == edit.doc {
            let mut expected = text.to_string();
            expected.replace_range(edit.span.start as usize..edit.span.end as usize, &edit.text);
            expected
        } else { text.to_string() };
        assert_eq!(verified.sources.doc(*doc).expect("verification keeps every document").text.as_ref(), expected,
            "the verified texts differ only in that one value token");
    }
    mailbox.result(done);
    let adopted = step(&mut engine, &mailbox);
    assert!(!Arc::ptr_eq(&loaded_circuit, &engine.design().units[0].circuit), "adoption swaps the circuit Arc");
    assert_eq!(value(&engine), 4700.0, "the adopted circuit contains the new resistance before undo evaluates");
    assert!(Arc::ptr_eq(&loaded_board, board(&engine)), "the value edit keeps the loaded board Arc");
    assert!(adopted.fills.is_empty(), "a value-only adoption creates no refill debt");
    assert_eq!(engine.log().entries.len(), 1, "the verified commit creates one history entry");
    assert_eq!(engine.log().entries[0].kind, EntryKind::Eval, "the canvas circuit edit takes the evaluation history path");
    assert_eq!(engine.log().head, 0, "the held undo runs in the adoption step");
    assert_eq!(adopted.evals.len(), 1, "the held undo evaluates its restored text");
    let finished = run(&mut engine, &mailbox, adopted);
    assert!(!finished.waiting, "the host becomes idle after the held undo's evaluation");
    assert_eq!(texts(&engine), loaded, "undo restores every loaded byte");
    assert_eq!(value(&engine), loaded_value, "undo restores the original resistance");
    assert!(Arc::ptr_eq(&loaded_board, board(&engine)), "undo also keeps the loaded board Arc");
    assert!(!engine.snapshot().stale.has(StageFlags::EVAL), "the completed undo clears evaluation staleness");
}

/// An AI burst: calls pushed back to back each resolve on the design the call before them left,
/// so a second circuit edit is not refused for a circuit the first one replaced. Each commit
/// answers its own call with its author, and a text commit names the ranges it wrote.
#[test]
fn ai_calls_in_a_burst_each_commit_and_answer_with_their_author_and_sites() {
    fn boot_value(design: &Design, _docs: Option<&Docs>, _tool: &str, args: &[(String, String)]) -> Result<Plan, Diag> {
        let unit = &design.units[0];
        let arg = |name: &str| args.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone()).expect("the test passes both arguments");
        let si: f64 = arg("si").parse().expect("the test passes a number");
        Ok(Plan::Edit(Resolved {
            ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::SetValue {
                comp: unit.circuit.comp("mcu.boot_r").expect("the boot resistor exists").id,
                value: Value { text: arg("text"), qty: Some(Quantity { si, unit: QtyUnit::Ohm }), tol: None },
            } }],
            place: vec![], label: format!("boot_r to {}", arg("text")),
        }))
    }
    let (engine, mailbox, ..) = setup();
    let boot = engine.design().units[0].circuit.comp("mcu.boot_r").expect("the boot resistor exists").id;
    let mut engine = engine.with_resolver("test.boot", boot_value);
    let call = |call, text: &str, si: &str| Cmd::Invoke {
        tool: "test.boot".into(), args: vec![("text".into(), text.into()), ("si".into(), si.into())],
        preview: false, by: By::Ai { turn: 0, call }, ticket: Ticket(call),
    };
    mailbox.push(call(1, "4.7k", "4700"));
    mailbox.push(call(2, "22k", "22000"));
    let mut outcomes = Vec::new();
    let mut step = engine.step(mailbox.take(), &mut |_| {});
    loop {
        outcomes.append(&mut step.outcomes);
        if step.evals.is_empty() {
            break;
        }
        for job in step.evals {
            mailbox.result(job.run());
        }
        step = engine.step(mailbox.take(), &mut |_| {});
    }
    let answered: Vec<_> = outcomes.iter().map(|(ticket, outcome)| match outcome {
        Outcome::Done { label, by, targets, .. } => {
            assert_eq!(targets, &[Sel::Comp(CompRef { board: BoardId(0), comp: boot })], "a value commit targets its part");
            (ticket.0, Ok((label.clone(), *by)))
        }
        Outcome::Refused(refusal) => (ticket.0, Err(refusal.text.clone())),
        Outcome::Previewed { .. } => (ticket.0, Err("previewed".into())),
        Outcome::Proposed { .. } | Outcome::Declined { .. } => panic!("direct edits never return proposal decisions"),
    }).collect();
    assert_eq!(answered, vec![
        (1, Ok(("boot_r to 4.7k".into(), By::Ai { turn: 0, call: 1 }))),
        (2, Ok(("boot_r to 22k".into(), By::Ai { turn: 0, call: 2 }))),
    ], "each call of the burst commits once and answers with its own author");
    assert_eq!(engine.log().entries.len(), 2, "the burst logs one entry per call");
    assert_eq!(value(&engine), 22000.0, "the second call applies over the first");

    let docs = engine.docs().expect("the fixture has documents");
    let doc = docs.sources().find("ledctl.ckt.splash").expect("the circuit document exists");
    let text = docs.text(doc).expect("the circuit has text").to_string();
    let at = text.find("let mcu   = mcu_core(").expect("the fixture hooks up its MCU") as u32;
    let by = By::Ai { turn: 0, call: 3 };
    mailbox.push(Cmd::Text {
        doc, base_rev: revision(&engine, doc), group: None, label: "two notes".into(), tag: None, by, ticket: Some(Ticket(3)),
        edits: vec![
            TextEdit { doc, span: 0..0, text: "// first\n".into() },
            TextEdit { doc, span: at..at, text: "// second\n".into() },
        ],
    });
    let typed = engine.step(mailbox.take(), &mut |_| {});
    let [(Ticket(3), Outcome::Done { by: answered_by, sites, .. })] = typed.outcomes.as_slice() else {
        panic!("the text commit answers its call once with Done: {:?}", typed.outcomes.len())
    };
    assert_eq!(*answered_by, by, "the text commit answers with its author");
    let first = "// first\n".len() as u32;
    let second = at + first;
    assert_eq!(sites, &[
        Sel::Source { doc, start: 0, end: first },
        Sel::Source { doc, start: second, end: second + "// second\n".len() as u32 },
    ], "the text commit names each written range in the text after it");
    let after = engine.docs().expect("the commit keeps its documents").text(doc).expect("the circuit has text").to_string();
    assert_eq!(&after[second as usize..second as usize + 10], "// second\n", "the second site holds the second edit's text");
    run(&mut engine, &mailbox, typed);
}

/// The bench pane follows the snapshot's bench views: an edit that leaves every bench's view as it
/// was keeps their Arc, a bench edit publishes the bench's new view, and undo brings the loaded
/// views back.
#[test]
fn the_snapshot_keeps_its_bench_views_until_a_bench_changes() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = engine.snapshot().benches.clone();
    assert_eq!(loaded.iter().map(|view| view.out.name.as_str()).collect::<Vec<_>>(), ["bringup"], "the loaded snapshot carries the controller's bench view");
    assert!(Arc::ptr_eq(&loaded, &engine.snapshot().benches), "snapshots between evaluations share the views");

    mailbox.push(set_value(&engine));
    let requested = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, requested);
    assert_eq!(value(&engine), 4700.0, "the circuit edit is adopted");
    assert!(Arc::ptr_eq(&loaded, &engine.snapshot().benches), "a circuit edit that leaves the bench's view equal keeps the Arc");

    let rail = ProbeOf::v("3V3");
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Bench { bench: "bringup".into(), op: BenchOp::AddProbe { of: rail.clone(), label: "Rail".into() } }],
        label: "probe".into(), circuits: vec![], expect: None,
    });
    let requested = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, requested);
    let probed = engine.snapshot().benches.clone();
    assert!(!Arc::ptr_eq(&loaded, &probed), "a bench edit publishes new views");
    let probes = &probed[0].out.probes;
    assert_eq!(probes.len(), loaded[0].out.probes.len() + 1, "the bench's view has one probe more");
    assert!(probes.iter().any(|probe| probe.of == rail && probe.label == "Rail"), "the new view carries the written probe");

    mailbox.push(Cmd::Undo);
    let undone = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undone);
    assert_eq!(*engine.snapshot().benches, *loaded, "undo restores the loaded views");
}

/// A refused pool verification consumes its reservation without logging or killing redo.
#[test]
fn a_refused_verified_commit_preserves_redo() {
    let (mut engine, mailbox, steps, logged) = setup();
    let loaded = texts(&engine);
    mailbox.push(set_value(&engine));
    let requested = step(&mut engine, &mailbox);
    let committed = run(&mut engine, &mailbox, requested);
    let committed_texts = texts(&engine);
    assert_ne!(committed_texts, loaded, "the accepted value edit changes the source texts");
    let seq = committed.undo.expect("the adopted value edit has an undo number");
    mailbox.push(Cmd::Undo);
    let undone = step(&mut engine, &mailbox);
    let undone = run(&mut engine, &mailbox, undone);
    assert_eq!(undone.redo, Some(seq), "undo exposes the committed value edit for redo");
    assert_eq!(texts(&engine), loaded, "undo restores the loaded texts before the refusal");
    let mark = logged.load(SeqCst);
    let before = steps.load(SeqCst);

    let mut invalid = set_value(&engine);
    let Cmd::Commit { ops, .. } = &mut invalid else { panic!("the value helper returns a commit") };
    let DesignOp::Circuit { op: CircuitOp::SetValue { value: proposed, .. }, .. } = &mut ops[0] else {
        panic!("the value helper edits a circuit value")
    };
    proposed.qty.as_mut().expect("the resistor value has a quantity").si = f64::INFINITY;
    mailbox.push(invalid);
    let accepted = step(&mut engine, &mailbox);
    assert!(accepted.rejected.is_empty(), "the circuit commit is accepted before pool verification");
    assert_eq!(accepted.evals.len(), 1, "the accepted circuit commit emits one verification job");
    assert_eq!(steps.load(SeqCst), before + 1, "acceptance reserves a step number before the job runs");
    assert_eq!(logged.load(SeqCst), mark, "acceptance logs no entry before verification");
    let done = accepted.evals[0].run();
    let TaskResult::Eval(result) = &done else { panic!("the pool job returns an evaluation result") };
    let EvalOut::Verified(Err(diag)) = &result.out else { panic!("pool verification refuses the infinite value") };
    assert!(diag.message.contains("not a finite value"), "the pool refuses a non-finite resistor value");
    mailbox.result(done);
    let refused = step(&mut engine, &mailbox);
    assert_eq!(refused.rejected.len(), 1, "adoption reports the pool verification refusal");
    assert!(!refused.waiting && refused.evals.is_empty(), "the refusal leaves no held command or evaluation");
    assert_eq!(steps.load(SeqCst), before + 1, "the refused reservation remains a gap in the step numbers");
    assert_eq!(logged.load(SeqCst), mark, "the refused verification logs no entry");
    assert_eq!(engine.log().entries.len(), 1, "the refused commit retains the undone entry without adding history");
    assert_eq!(engine.log().head, 0, "the refused commit leaves the history head undone");
    assert_eq!(refused.redo, Some(seq), "redo survives the refused reservation");
    assert_eq!(texts(&engine), loaded, "the refused verification changes no source text");

    mailbox.push(Cmd::Redo);
    let redone = step(&mut engine, &mailbox);
    let redone = run(&mut engine, &mailbox, redone);
    assert_eq!(redone.undo, Some(seq), "redo restores the original committed step number");
    assert_eq!(texts(&engine), committed_texts, "redo after the refusal restores the committed value edit's texts");
    assert_eq!(value(&engine), 4700.0, "redo after the refusal restores the evaluated resistance");
    assert_eq!(steps.load(SeqCst), before + 1, "redo consumes no further step number");
    assert_eq!(logged.load(SeqCst), mark, "redo does not log a new entry");
}

/// A model edit logged during verification must sort after the accepted circuit commit.
#[test]
fn a_circuit_reservation_precedes_a_model_entry_logged_before_adoption() {
    let (mut engine, mailbox, steps, logged) = setup();
    mailbox.push(set_value(&engine));
    let requested = step(&mut engine, &mailbox);
    assert!(requested.rejected.is_empty(), "the circuit commit is accepted for verification");
    assert_eq!(requested.evals.len(), 1, "the circuit verification remains in flight");
    let reserved = steps.load(SeqCst);
    assert_eq!(reserved, 1, "the circuit commit reserves its number at acceptance");
    assert_eq!(logged.load(SeqCst), 0, "the accepted circuit commit is not logged yet");
    let model_seq = steps.fetch_add(1, SeqCst) + 1;
    logged.fetch_max(model_seq, SeqCst);
    let adopted = run(&mut engine, &mailbox, requested);
    assert_eq!(engine.log().entries.len(), 1, "adoption logs the single circuit commit");
    assert_eq!(adopted.undo, Some(reserved), "adoption retains the circuit commit's reserved number");
    assert!(engine.log().entries[0].seq < model_seq, "the circuit entry sorts before the model entry logged during verification");
    assert_eq!(steps.load(SeqCst), model_seq, "adoption consumes no extra number when only the other history logged");
    assert_eq!(logged.load(SeqCst), model_seq, "adopting an older reservation never decreases the logged counter");
}

/// Re-lowering preserves identities, while old board inverses switch to text when epochs change.
#[test]
fn circuit_edits_relower_the_board_with_its_ids_and_an_older_move_undoes_by_text() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    let part = board(&engine).placed.iter().find(|part| part.path == "mcu.dbg").expect("the debug header is placed").clone();
    mailbox.push(move_part(part.id, P::mm(1.0, 0.0), "first move"));
    let moved = step(&mut engine, &mailbox);
    assert!(moved.rejected.is_empty() && moved.evals.is_empty(), "the first board move applies without evaluation");
    assert_eq!(engine.log().entries[0].epochs, vec![(BoardId(0), 0)], "the first move records epoch zero");
    let moved_board = board(&engine).clone();
    let comp = engine.design().units[0].circuit.comp("mcu.dbg").expect("the header has a component").id;
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Circuit { unit: BoardId(0), op: CircuitOp::Rename {
            what: Sel::Comp(CompRef { board: BoardId(0), comp }), to: "debug_header".into(),
        } }], label: "rename".into(), circuits: vec![], expect: None,
    });
    let rename = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, rename);
    let renamed = board(&engine);
    assert!(!Arc::ptr_eq(&moved_board, renamed), "the renamed placement replaces its board Arc");
    assert_eq!(renamed.generation, moved_board.generation + 1, "one adoption advances board generation by one");
    assert_eq!(renamed.placed.iter().map(|p| (p.id, p.at, p.rot, p.side)).collect::<Vec<_>>(),
        moved_board.placed.iter().map(|p| (p.id, p.at, p.rot, p.side)).collect::<Vec<_>>(),
        "renaming preserves every placement id, position, rotation and side");
    assert_eq!(renamed.placed_item(part.id).expect("the header keeps its item id").path, "mcu.debug_header",
        "the renamed placement keeps its containing block's path prefix");
    assert!(engine.docs().expect("the engine retains documents").epoch(BoardId(0)) > 0, "a replaced board advances the unit epoch");
    check_board(&engine);
    mailbox.push(move_part(part.id, P::mm(0.0, 1.0), "second move"));
    let moved = step(&mut engine, &mailbox);
    assert!(moved.rejected.is_empty() && moved.evals.is_empty(), "a move of the renamed placement still uses the board splice");
    let docs = engine.docs().expect("the engine retains documents");
    let doc = docs.sources().find("ledctl.pcb.splash").expect("the board document remains");
    let at = docs.map().locate(&Sel::Item(BoardId(0), part.id)).expect("the header has a statement");
    assert!(docs.text(doc).expect("the board has text")[at.span.start as usize..at.span.end as usize].contains("\"mcu.debug_header\""),
        "the moved statement uses the renamed component path");
    check_board(&engine);

    let before_add = board(&engine).clone();
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Circuit { unit: BoardId(0), op: CircuitOp::AddPart {
            stmt: "let r9 = e.r(1kohm, gnd, e.net({name: \"TP1\"}), {package: \"0402\"})".into(), after: None,
            ends: vec![], hint: None,
        } }], label: "add part".into(), circuits: vec![], expect: None,
    });
    let added = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, added);
    assert_ne!(board(&engine).content_hash(), before_add.content_hash(), "adding a net changes board content");
    assert_eq!(board(&engine).nets.len(), before_add.nets.len() + 1, "the new component introduces one board net");
    assert_eq!(item_ids(board(&engine)), item_ids(&before_add), "adding an unplaced component preserves every board item id");
    check_board(&engine);

    for label in ["add part", "second move", "rename"] {
        assert_eq!(engine.log().entries[engine.log().head - 1].label, label, "undo order follows the committed operations");
        mailbox.push(Cmd::Undo);
        let undone = step(&mut engine, &mailbox);
        run(&mut engine, &mailbox, undone);
    }
    mailbox.push(Cmd::Undo);
    let first_move = step(&mut engine, &mailbox);
    assert_eq!(first_move.evals.len(), 1, "the first move's old epoch forces text undo and evaluation");
    run(&mut engine, &mailbox, first_move);
    assert_eq!(engine.log().head, 0, "all four entries have been undone");
    assert_eq!(texts(&engine), loaded, "all undos restore every loaded document byte");
    let docs = engine.docs().expect("the engine retains documents");
    let fresh = evaluate(docs.sources(), docs.libs()).expect("the restored documents evaluate from scratch");
    for unit in &engine.design().units {
        let expected = fresh.design.unit(unit.id).expect("a fresh evaluation has every retained unit");
        assert_eq!(unit.circuit.content_hash(), expected.circuit.content_hash(), "unit {:?}: restored circuit equals a fresh evaluation", unit.id);
        assert_eq!(unit.board.as_ref().map(|b| b.content_hash()), expected.board.as_ref().map(|b| b.content_hash()),
            "unit {:?}: restored board equals a fresh evaluation", unit.id);
    }
}

/// Why: a case's light block reserves its board space (11-17). The engine holds it as a derived
/// keepout: applied once and then in place, an unchanged publication changes nothing (no board
/// generation), a re-lowering of the board from its text keeps it, the case gone removes it, and
/// it never enters the history or a document.
#[test]
fn a_case_keepout_is_derived_kept_through_a_relowering_and_goes_with_the_case() {
    let (engine, mailbox, ..) = setup();
    let mut engine = engine.with_app_tools();
    let loaded = texts(&engine);
    let unit = BoardId(0);
    let name = "Light pipe block D1 D2";
    let block = |x: f64| Item::Keepout(BoardKeepout {
        id: Default::default(), area: PolySet::rect(BBox::new(P::mm(x, 10.0), P::mm(x + 6.0, 14.4))),
        layers: LayerSet::of(&[LayerId::F_CU]), forbid: Forbid::FOOTPRINTS, reason: name.into(),
        origin: Default::default(), z: Some((mm(1.2), mm(9.0))),
    });
    let held = |engine: &Engine| board(engine).keepouts.iter()
        .filter(|k| matches!(&k.origin, Origin::Derived { from: FeatureRef::Panel(from) } if from.starts_with(CASE_KEEPOUT)))
        .cloned().collect::<Vec<_>>();
    let comp = board(&engine).placed[0].comp;
    let set = format!("@elec:port:{}:{}:0/{name}", unit.0, comp.0);
    let port = |width| PortRoom {
        comp, room: Some(PolySet::rect(BBox::new(P::ZERO, P::mm(width, 2.0)))),
        keepout: Some(CarriedKeepout {
            from: FeatureRef::Panel(format!("{CASE_KEEPOUT}{set}")), area: PolySet::rect(BBox::new(P::ZERO, P::mm(6.0, 4.4))),
            band: (mm(1.2), mm(9.0)), side: Side::Top,
        }),
    };
    let publish = |generation, x: Option<f64>, rooms| Cmd::CaseKeepouts {
        unit, generation, plan: Ok(None), sets: x.map(|x| (set.clone(), vec![block(x)])).into_iter().collect(), rooms,
    };

    let evaluated = board(&engine).generation;
    assert!(engine.case_rooms(unit).is_none());
    mailbox.push(Cmd::Invoke {
        tool: "elec.place".into(), args: tool_args([("mode", "@place"), ("all", "true")]),
        by: By::Person, preview: false, ticket: Ticket(1),
    });
    let started = step(&mut engine, &mailbox);
    let [start] = started.routes.as_slice() else { panic!("expected a place job: {:?}", started.rejected) };
    assert!(matches!(&start.job, fab_elec_host::Job::Place(job) if job.rooms.is_empty()), "a job before the first publication carries no rooms");
    mailbox.push(publish(evaluated, Some(10.0), vec![port(1.0)]));
    let first = step(&mut engine, &mailbox);
    assert!(first.rejected.is_empty() && first.evals.is_empty(), "a case publication applies without an evaluation");
    assert_eq!(engine.case_rooms(unit), Some((evaluated, [port(1.0)].as_slice())), "the room belongs to the generation the case read");
    let [kept] = held(&engine).try_into().expect("the board holds one case keepout");
    assert_eq!(kept.origin, Origin::Derived { from: FeatureRef::Panel(format!("{CASE_KEEPOUT}{set}")) }, "the engine preserves the opaque set key");
    assert_eq!(kept.reason, name);
    assert!(engine.log().entries.is_empty(), "a case publication is not an undo step");
    assert_eq!(texts(&engine), loaded, "a case keepout is written to no document");
    let generation = board(&engine).generation;
    let design = engine.design().clone();
    mailbox.push(publish(evaluated, Some(10.0), vec![port(1.0)]));
    step(&mut engine, &mailbox);
    assert_eq!(board(&engine).generation, generation, "the same publication again leaves the board generation");
    assert!(Arc::ptr_eq(engine.design(), &design), "and the committed design as it was");

    mailbox.push(publish(generation, Some(10.0), vec![port(2.0)]));
    step(&mut engine, &mailbox);
    assert_eq!(engine.case_rooms(unit), Some((generation, [port(2.0)].as_slice())), "the new publication replaces the room");
    assert_eq!(board(&engine).generation, generation, "a room-only change leaves the board generation alone");
    assert!(Arc::ptr_eq(engine.design(), &design) && engine.log().entries.is_empty(), "only the held rooms changed");

    mailbox.push(publish(generation, Some(12.0), vec![port(2.0)]));
    step(&mut engine, &mailbox);
    let [moved] = held(&engine).try_into().expect("the board still holds one case keepout");
    assert_eq!(moved.id, kept.id, "a moved block replaces its keepout in place");
    assert_eq!(moved.area, PolySet::rect(BBox::new(P::mm(12.0, 10.0), P::mm(18.0, 14.4))), "at the block's new place");

    // A rename re-lowers the board from its text: the keepout stays until the case says otherwise.
    let before = board(&engine).clone();
    let comp = engine.design().units[0].circuit.comp("mcu.dbg").expect("the header has a component").id;
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Circuit { unit, op: CircuitOp::Rename { what: Sel::Comp(CompRef { board: unit, comp }), to: "debug_header".into() } }],
        label: "rename".into(), circuits: vec![], expect: None,
    });
    let rename = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, rename);
    assert!(!Arc::ptr_eq(&before, board(&engine)), "the rename replaced the board");
    let [relowered] = held(&engine).try_into().expect("the re-lowered board keeps the case keepout");
    assert_eq!((relowered.area, relowered.z, relowered.origin), (moved.area, moved.z, moved.origin), "as it was");

    let evaluated = board(&engine).generation;
    mailbox.push(publish(evaluated, None, Vec::new()));
    step(&mut engine, &mailbox);
    assert!(held(&engine).is_empty(), "a publication without the block (the case gone) removes its keepout");
    assert_eq!(engine.case_rooms(unit), Some((evaluated, [].as_slice())), "an empty publication clears the port rooms");
    assert_eq!(engine.log().entries.len(), 1, "only the rename is in the history");
    check_bring_history();
}

/// Keep, split and discard an actual tray bring, including its document and selection ledger.
fn check_bring_history() {
    let unit = BoardId(0);
    let mut sources = fab_elec_fixtures::docs("ledctl").unwrap();
    let doc = sources.find("ledctl.pcb.splash").unwrap();
    let stock = &sources.doc(doc).unwrap().text;
    let without_lamps = stock.lines().filter(|l| l.starts_with("board(") || l.starts_with("outline(")
        || l.starts_with("place(\"mcu.") || l.starts_with("hole(")).collect::<Vec<_>>().join("\n");
    sources.apply(&[TextEdit::trimmed(doc, stock, &without_lamps)]).unwrap();
    for (discard, intervene) in [(false, false), (false, true), (true, false), (true, true)] {
        let (design, docs) = Docs::load(sources.clone(), fab_elec_lang::std_libs());
        let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::default(), Arc::default(), "place".into())
            .with_docs(docs).with_app_tools();
        let mailbox = Mailbox::default();
        let before = texts(&engine);
        let before_board = board(&engine).content_hash();
        let generation = board(&engine).generation;
        let ticket = Ticket(10);
        mailbox.push(Cmd::CaseFindings { unit, generation, run: 1, cased: true, findings: vec![] });
        mailbox.push(Cmd::Invoke { tool: "elec.place".into(), args: tool_args([("mode", "@place"), ("parts", "D1,D2")]),
            by: By::Person, preview: false, ticket });
        let mut done = step(&mut engine, &mailbox);
        let bring = engine.log().entries.last().expect("the tray bring committed").seq;
        let tray = texts(&engine);
        let tray_board = board(&engine).content_hash();
        assert_ne!(tray_board, before_board);
        let led = board(&engine).placed.iter().find(|p| p.refdes == "D1").unwrap().id;
        let standing = board(&engine).placed[0].id;
        let (waiting, id, owner) = done.parked.expect("the tray bring is visible");
        assert_eq!((waiting, owner), (ticket, unit));
        let stamp = engine.design().generation;
        mailbox.push(Cmd::CaseOutcome { unit, stamp: stamp - 1, reason: None });
        loop {
            assert!(done.routes.is_empty() && done.rejected.is_empty());
            for fill in done.fills { mailbox.result(fill.run()); }
            for check in done.checks { mailbox.result(check.run()); }
            done = step(&mut engine, &mailbox);
            if done.checks.is_empty() && done.fills.is_empty() { break }
        }
        assert!(done.routes.is_empty(), "an exact board and yesterday's publication still wait");
        let tag = id.tag();
        // Discard before release, including after an intervening edit. The kept trials
        // exercise an evaluation at the bring and one that supersedes it.
        if !discard {
            let ports: Vec<_> = board(&engine).placed.iter().filter(|p| ["D1", "D2"].contains(&p.refdes.as_str()))
                .map(|p| PortRoom { comp: p.comp, room: Some(PolySet::rect(BBox::new(P::ZERO, P::mm(1., 2.)))), keepout: None }).collect();
            if !intervene {
                mailbox.push(Cmd::CaseKeepouts { unit, generation: board(&engine).generation, sets: vec![], rooms: ports.clone(), plan: Ok(None) });
            }
            mailbox.push(Cmd::CaseOutcome { unit, stamp: stamp + u64::from(intervene),
                reason: intervene.then(|| "failed to build the roof".into()) });
            let released = step(&mut engine, &mailbox);
            let [start] = released.routes.as_slice() else { panic!("the stamped outcome releases the job") };
            assert_eq!(start.job.id(), id);
            let fab_elec_host::Job::Place(job) = &start.job else { panic!("expected a place job") };
            assert!(job.add.is_empty() && job.scope.len() == 2);
            if !intervene { assert_eq!(job.rooms, ports); }
        }
        if intervene {
            mailbox.push(move_part(standing, P::mm(1., 0.), "adjust MCU"));
            assert!(step(&mut engine, &mailbox).rejected.is_empty());
        }
        let between = texts(&engine);
        if discard {
            mailbox.push(Cmd::DiscardJob { seq: 11, tag });
            let discarded = step(&mut engine, &mailbox);
            assert_eq!(discarded.redo, None);
            if intervene {
                assert!(matches!(discarded.outcomes.as_slice(), [(_, Outcome::Refused(reason))] if reason.text.contains("adjust MCU")));
                assert_eq!(texts(&engine), between);
                assert_eq!(engine.log().head, 2);
            } else {
                assert_eq!(texts(&engine), before);
                assert_eq!(board(&engine).content_hash(), before_board);
                assert!(engine.log().entries.is_empty());
                check_board(&engine);
            }
            continue;
        }
        let ops = [standing, led].into_iter().map(|item| DesignOp::Board { unit, op: BoardOp::Move { item, by: P::mm(1.0, -4.0) } }).collect();
        mailbox.preview(Sample { seq: 11, tag, ops, place: vec![], label: "keep placement".into(), by: By::Person,
            proposal: None, notes: vec![], circuits: vec![], expect: None });
        mailbox.push(Cmd::CommitPreview { seq: 11, tag });
        let kept = step(&mut engine, &mailbox);
        assert!(kept.rejected.is_empty(), "{:?}", kept.rejected);
        assert_eq!(engine.log().head, if intervene { 3 } else { 1 });
        assert_eq!(engine.log().entries.last().unwrap().label, "keep placement");
        assert!(matches!(kept.outcomes.as_slice(), [(_, Outcome::Done { seq, notes, .. })]
            if (*seq == bring) == !intervene && notes.iter().any(|(_, v)| v.contains("adjust MCU")) == intervene));
        assert!(matches!(kept.outcomes.as_slice(), [(_, Outcome::Done { notes, .. })]
            if notes.iter().any(|(key, value)| key == "case" && value.contains(if intervene { "failed to build the roof" } else { "waited for the case" }))));
        let after = texts(&engine);
        let after_board = board(&engine).content_hash();
        check_board(&engine);
        mailbox.push(Cmd::Undo);
        assert!(step(&mut engine, &mailbox).rejected.is_empty());
        if intervene {
            assert_eq!(texts(&engine), between);
            mailbox.push(Cmd::Undo);
            step(&mut engine, &mailbox);
            assert_eq!(texts(&engine), tray);
            assert_eq!(board(&engine).content_hash(), tray_board);
            mailbox.push(Cmd::Redo);
            step(&mut engine, &mailbox);
        } else {
            assert_eq!(texts(&engine), before);
            assert_eq!(board(&engine).content_hash(), before_board);
            check_board(&engine);
        }
        mailbox.push(Cmd::Redo);
        assert!(step(&mut engine, &mailbox).rejected.is_empty());
        assert_eq!(texts(&engine), after);
        assert_eq!(board(&engine).content_hash(), after_board);
        check_board(&engine);
    }

}

/// Broken text keeps the usable unit; repairing the same gesture keeps one undo entry.
#[test]
fn broken_text_keeps_the_unit_refuses_canvas_edits_and_typing_merges() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    let circuit = engine.design().units[0].circuit.clone();
    let docs = engine.docs().expect("the engine retains documents");
    let doc = docs.sources().find("ledctl.ckt.splash").expect("the circuit document exists");
    let end = docs.text(doc).expect("the circuit has text").len() as u32;
    let broken = "let x = (";
    mailbox.push(Cmd::Text {
        by: By::Person, ticket: None, tag: None,
        doc, base_rev: revision(&engine, doc), edits: vec![TextEdit { doc, span: end..end, text: broken.into() }],
        group: Some(1), label: "typing".into(),
    });
    let typed = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, typed);
    let Some(UnitState::Kept(diags)) = engine.docs().expect("the engine retains documents").unit_state(BoardId(0)) else {
        panic!("broken text keeps the previously built unit");
    };
    assert!(diags.iter().any(|diag| diag.sel.iter().any(|site| matches!(site, Sel::Source { doc: at, .. } if *at == doc))),
        "the kept unit's diagnostic points into the edited circuit document");
    assert!(Arc::ptr_eq(&circuit, &engine.design().units[0].circuit), "broken text keeps the last good circuit Arc");
    mailbox.push(set_value(&engine));
    let refused = step(&mut engine, &mailbox);
    assert_eq!(refused.rejected.len(), 1, "a canvas edit of broken text is refused immediately");
    assert!(refused.evals.is_empty(), "refusing a kept unit starts no verification job");
    assert!(engine.snapshot().last_refusal.as_ref().and_then(|refusal| refusal.site.as_ref()).is_some(), "the refusal carries a source site");
    mailbox.push(Cmd::Text {
        by: By::Person, ticket: None, tag: None,
        doc, base_rev: revision(&engine, doc), edits: vec![TextEdit { doc, span: end..end + broken.len() as u32, text: String::new() }],
        group: Some(1), label: "typing".into(),
    });
    let repaired = step(&mut engine, &mailbox);
    assert_eq!(engine.log().entries.len(), 1, "two edits in one gesture merge despite a refused canvas command");
    assert_eq!(engine.log().entries[0].group, Some(1), "the merged entry retains its gesture id");
    run(&mut engine, &mailbox, repaired);
    assert_eq!(engine.docs().expect("the engine retains documents").unit_state(BoardId(0)), Some(&UnitState::Built), "repaired text builds the unit");
    assert_eq!(texts(&engine), loaded, "repair restores every loaded text byte");
    mailbox.push(Cmd::Undo);
    let undone = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undone);
    assert_eq!(engine.log().entries.len(), 1, "undo retains the gesture as a single redoable entry");
    assert_eq!(engine.log().head, 0, "undo moves past the whole gesture");
    assert_eq!(texts(&engine), loaded, "the net-zero gesture's undo restores the same original bytes");

    let system = engine.docs().expect("the engine retains documents").sources().find(SYSTEM_DOC).expect("the system document exists");
    let base_rev = revision(&engine, system);
    mailbox.push(Cmd::Text {
        by: By::Person, ticket: None, tag: None,
        doc: system, base_rev, edits: vec![TextEdit { doc: system, span: 0..0, text: "// note\n".into() }],
        group: None, label: "system note".into(),
    });
    let note = step(&mut engine, &mailbox);
    assert!(note.rejected.is_empty(), "typed system text is accepted");
    assert_eq!(engine.log().entries[0].kind, EntryKind::System, "system text uses the reserved system history branch");
    assert!(note.evals.is_empty() && !note.waiting, "system text needs no pool job");
    assert_eq!(note.docs.as_ref().and_then(|written| written.docs.get(SYSTEM_DOC)).map(|text| text.as_ref()), Some("// note\n"), "the step hands the system text to the plate");
    assert!(!engine.docs().expect("the engine retains documents").behind(), "system text never marks the design behind");
    mailbox.push(Cmd::Text {
        by: By::Person, ticket: None, tag: None,
        doc: system, base_rev, edits: vec![TextEdit { doc: system, span: 0..0, text: "// stale\n".into() }],
        group: None, label: "stale editor".into(),
    });
    let stale = step(&mut engine, &mailbox);
    assert_eq!(stale.rejected.len(), 1, "an editor using the old revision is refused");
    assert!(stale.rejected[0].1.contains("the document changed since the editor read it"), "the refusal asks the stale editor to read again");
    assert!(stale.docs.is_none(), "a refused stale revision changes no document");
    assert_eq!(engine.log().entries.len(), 1, "a refused stale revision adds no history entry");

    // The pre-save check: typed system text the model does not follow is a located note.
    let mismatch = |engine: &Engine| engine.docs().expect("the engine retains documents").view().notes.iter()
        .find(|note| note.message.contains("mount differs")).cloned();
    assert!(mismatch(&engine).is_none(), "a comment keeps the system text in step with the model");
    let mount = "mount(ledctl, {at: [1mm, 0mm, 0mm]})\n";
    mailbox.push(Cmd::Text {
        by: By::Person, ticket: None, tag: None,
        doc: system, base_rev: revision(&engine, system), edits: vec![TextEdit { doc: system, span: 0..0, text: mount.into() }],
        group: None, label: "typed mount".into(),
    });
    let typed = step(&mut engine, &mailbox);
    assert!(typed.rejected.is_empty(), "typed system text is accepted before the model follows it");
    let note = mismatch(&engine).expect("a typed mount the model does not follow is a pre-save mismatch");
    assert_eq!(note.message, "ledctl's mount differs: text at [1000000, 0, 0], model at [0, 0, 0]", "the note names the unit and both translations");
    assert!(note.sel.iter().any(|sel| matches!(sel, Sel::Source { doc, .. } if *doc == system)), "the mismatch is located in the system document");
    mailbox.push(Cmd::Undo);
    step(&mut engine, &mailbox);
    assert!(mismatch(&engine).is_none(), "undoing the typed mount clears the mismatch");

    // The AI's text (`fab.elec_text`) is checked before it commits, where typing is not: a change
    // that adds a mistake commits nothing and says where it is; one that adds none is one AI step.
    let checked = |engine: &mut Engine, text: &str, ticket: Ticket| -> Step {
        let base_rev = revision(engine, doc);
        mailbox.push(Cmd::CheckedText {
            doc, base_rev, edits: vec![TextEdit { doc, span: end..end, text: text.into() }], label: "AI text".into(), by: By::Ai { turn: 1, call: ticket.0 }, ticket,
        });
        let mut done = step(engine, &mailbox);
        while done.outcomes.is_empty() && !done.evals.is_empty() {
            for job in done.evals {
                mailbox.result(job.run());
            }
            done = step(engine, &mailbox);
        }
        done
    };
    let before = (texts(&engine), revision(&engine, doc), engine.log().entries.len(), engine.log().head);
    let refused = checked(&mut engine, broken, Ticket(7));
    let [(Ticket(7), Outcome::Refused(refusal))] = refused.outcomes.as_slice() else {
        panic!("checked text with a mistake answers its caller once with a refusal")
    };
    assert!(refusal.text.starts_with(MISTAKE), "the refusal says the change adds a mistake: {}", refusal.text);
    assert!(matches!(refusal.site, Some(Sel::Source { doc: at, .. }) if at == doc), "the mistake is located in the changed document");
    assert_eq!((texts(&engine), revision(&engine, doc), engine.log().entries.len(), engine.log().head), before,
        "a refused checked change leaves every text, the revision and the history as they were");
    assert_eq!(engine.docs().expect("the engine retains documents").unit_state(BoardId(0)), Some(&UnitState::Built), "the unit stays built");

    let done = checked(&mut engine, "\nlet pull = e.r(10kohm, key, v33, {package: \"0402\"})\n", Ticket(8));
    let [(Ticket(8), Outcome::Done { by, created, circuit, .. })] = done.outcomes.as_slice() else {
        panic!("checked text that adds no mistake answers its caller once with Done: {:?}", done.rejected)
    };
    assert!(matches!(by, By::Ai { .. }) && *circuit == CircuitChange::Structure, "it is the AI's circuit step");
    let unit = &engine.design().units[0];
    let pull = unit.circuit.comp("pull").expect("the part the text defines is in the design");
    assert!(created.contains(&Sel::Comp(CompRef { board: unit.id, comp: pull.id })), "Done names the part the text defines");
    assert_eq!(revision(&engine, doc), before.1 + 1, "the document moves one revision on");
    assert_eq!(engine.log().head, before.3 + 1, "the change is one step");
    assert_eq!(engine.log().entries[before.3].label, "AI text", "the step is the call's");
    mailbox.push(Cmd::Undo);
    let undone = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undone);
    assert_eq!(texts(&engine), before.0, "one undo restores every byte");
}

/// A live drag remains responsive and its committed text forces an in-flight value edit to retry.
#[test]
fn a_board_drag_keeps_previewing_while_an_evaluation_runs() {
    let (mut engine, mailbox, steps, logged) = setup();
    let loaded = texts(&engine);
    let part = board(&engine).placed.first().expect("the board has a placed part").clone();
    mailbox.push(set_value(&engine));
    let value_job = step(&mut engine, &mailbox);
    assert_eq!(value_job.evals.len(), 1, "the value commit starts one evaluation");
    assert_eq!(steps.load(SeqCst), 1, "the value commit reserves one number before verification");
    assert_eq!(logged.load(SeqCst), 0, "the value reservation does not log an entry");
    for seq in 1..=3 {
        mailbox.preview(Sample {
            tag: 0, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: None,
            seq, ops: vec![DesignOp::Board { unit: BoardId(0), op: BoardOp::Move { item: part.id, by: P::mm(seq as f64, 0.0) } }],
            label: "move".into(),
        });
        let mut published = Vec::new();
        let previewed = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
        assert!(previewed.rejected.is_empty(), "preview sample {seq} is accepted during evaluation");
        let snap = published.last().expect("each preview sample publishes a snapshot");
        let preview = snap.preview.as_ref().expect("the snapshot carries a live preview");
        let board = preview.unit(BoardId(0)).expect("the preview has the controller").board.as_ref().expect("the preview has its board");
        assert_eq!(board.placed_item(part.id).expect("the moved part stays present").at, part.at.add(P::mm(seq as f64, 0.0)),
            "preview sample {seq} reaches the sampled position");
        assert!(snap.stale.has(StageFlags::EVAL), "preview sample {seq} retains evaluation staleness");
    }
    mailbox.push(Cmd::CommitPreview { seq: 3, tag: 0 });
    let moved = step(&mut engine, &mailbox);
    assert!(moved.rejected.is_empty(), "the drag commits while the value evaluation runs");
    assert_eq!(engine.log().entries.len(), 1, "the board commit applies at once when no command is held");
    assert_eq!(engine.log().entries[0].label, "move", "the board move precedes the unadopted value in history");
    assert_eq!(engine.log().entries[0].seq, 2, "the board preview commit numbers when it logs after the value reservation");
    let moved_circuit = engine.design().units[0].circuit.clone();
    mailbox.result(value_job.evals[0].run());
    let retried = step(&mut engine, &mailbox);
    assert_eq!(retried.evals.len(), 1, "changed board text emits a replacement verification job");
    assert!(retried.evals[0].verify.is_some(), "the stale commit is re-verified, not just evaluated");
    assert!(Arc::ptr_eq(&moved_circuit, &engine.design().units[0].circuit), "the stale result never replaces the current circuit");
    assert_eq!(engine.log().entries.len(), 1, "a stale result creates no history entry");
    assert_eq!(steps.load(SeqCst), 2, "retry keeps the value commit's original reservation");
    assert_eq!(logged.load(SeqCst), 2, "retry logs no entry beyond the board move");
    let finished = run(&mut engine, &mailbox, retried);
    assert!(!finished.waiting, "both edits finish without held commands");
    assert_eq!(value(&engine), 4700.0, "the retried value reaches the model");
    assert_eq!(board(&engine).placed_item(part.id).expect("the moved part remains").at, part.at.add(P::mm(3.0, 0.0)),
        "value adoption preserves the committed drag position");
    assert_eq!(engine.log().entries.iter().map(|entry| entry.label.as_str()).collect::<Vec<_>>(), vec!["move", "value"],
        "history records adoption order: move, then value");
    assert_eq!(engine.log().entries.iter().map(|entry| entry.seq).collect::<Vec<_>>(), vec![2, 3],
        "adoption takes a fresh number after a newer entry in the same history");
    assert_eq!(steps.load(SeqCst), 3, "adoption after the board move consumes exactly one replacement number");
    assert_eq!(logged.load(SeqCst), 3, "adoption advances the logged counter to its replacement number");
    let docs = engine.docs().expect("the engine retains documents");
    let circuit_doc = docs.sources().find("ledctl.ckt.splash").expect("the circuit document remains");
    assert!(docs.text(circuit_doc).expect("the circuit has text").contains("let boot_r = e.r(4.7kohm"), "the value is written into the circuit text");
    let board_doc = docs.sources().find("ledctl.pcb.splash").expect("the board document remains");
    assert_ne!(docs.text(board_doc), loaded.iter().find(|(id, _)| *id == board_doc).map(|(_, text)| text), "the drag is written into board text");
    check_board(&engine);
}

/// A pane-like value sample keeps its sequence separate from its published tag.
fn value_sample(engine: &Engine, seq: u64, si: f64) -> Sample {
    let Cmd::Commit { mut ops, place, label, circuits, expect, by, .. } = set_value(engine) else {
        panic!("the value helper returns a commit")
    };
    let DesignOp::Circuit { op: CircuitOp::SetValue { value, .. }, .. } = &mut ops[0] else {
        panic!("the value helper edits the resistor")
    };
    value.text = si.to_string();
    value.qty.as_mut().expect("the resistor value has a quantity").si = si;
    Sample { seq, tag: 100 + seq, ops, place, label, circuits, expect, by, proposal: None, notes: vec![] }
}

/// Read the evaluated preview rather than the committed circuit or the requested op.
fn preview_value(engine: &Engine) -> f64 {
    engine.snapshot().preview.as_ref().expect("an evaluated preview is shown")
        .unit(BoardId(0)).expect("the preview keeps the controller").circuit.comp("mcu.boot_r")
        .expect("the preview keeps the boot resistor").value.as_ref().and_then(|value| value.qty)
        .expect("the preview resistor has a resistance").si
}

#[test]
fn a_circuit_sample_shows_evaluated_values_without_changing_documents_or_history() {
    let (mut engine, mailbox, steps, logged) = setup();
    let loaded = texts(&engine);
    let original = value(&engine);
    let committed = engine.design().clone();
    let docs = engine.docs().expect("the fixture has documents");
    let map = docs.map().clone();
    let view = docs.view().clone();
    let epoch = docs.epoch(BoardId(0));
    mailbox.preview(value_sample(&engine, 1, 4700.0));
    let mut published = Vec::new();
    let requested = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(requested.rejected.is_empty(), "the circuit sample is accepted for verification");
    assert_eq!(requested.evals.len(), 1, "the first sample starts one pool job");
    assert_eq!(requested.evals[0].preview, Some(1), "the job identifies its preview sample");
    assert!(!requested.waiting, "a preview job does not make undo busy");
    let pending = published.last().expect("starting evaluation publishes its stale flag");
    assert!(pending.stale.has(StageFlags::EVAL), "the snapshot is evaluation-stale while the sample runs");
    assert!(pending.preview.is_none(), "the first sample shows nothing until it evaluates");
    let done = requested.evals[0].run();
    let TaskResult::Eval(result) = &done else { panic!("a preview job returns an evaluation") };
    assert_eq!(result.preview, Some(1), "the pool result echoes the preview sequence");
    mailbox.result(done);
    let shown = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(shown.rejected.is_empty() && shown.evals.is_empty(), "the evaluated sample is shown without another job");
    assert_eq!(preview_value(&engine), 4700.0, "the preview circuit contains the evaluated resistance");
    assert_eq!(value(&engine), original, "the committed circuit retains its original resistance");
    assert!(Arc::ptr_eq(engine.design(), &committed), "a preview preserves the committed design Arc");
    assert_eq!(texts(&engine), loaded, "a preview changes no document bytes");
    assert!(engine.log().entries.is_empty() && engine.log().head == 0, "a preview adds no history entry");
    assert_eq!((steps.load(SeqCst), logged.load(SeqCst)), (0, 0), "a preview reserves and logs no step number");
    let docs = engine.docs().expect("previewing retains documents");
    assert!(Arc::ptr_eq(&map, docs.map()) && Arc::ptr_eq(&view, docs.view()), "previewing preserves the map and document view");
    assert_eq!(docs.epoch(BoardId(0)), epoch, "previewing does not advance the unit epoch");
    assert!(shown.fills.is_empty(), "a value-only preview creates no refill debt");
    let snapshot = published.last().expect("the evaluated preview is published");
    assert_eq!(snapshot.preview_tag, Some(101), "the evaluated preview echoes the sample tag");
    assert!(!snapshot.stale.has(StageFlags::EVAL), "the completed preview clears evaluation staleness");
    let rows = snapshot.of(&snapshot.sheets, BoardId(0)).expect("the preview has sheet rows");
    assert_eq!(rows.of, StateOf::Preview, "changed circuits publish preview-tagged sheet rows");
    let committed_rows = snapshot.sheets.iter().find(|rows| rows.unit == Some(BoardId(0)) && rows.of == StateOf::Committed)
        .expect("the snapshot also keeps committed rows");
    assert_eq!(rows.value.rows, committed_rows.value.rows, "a value-only preview preserves every sheet row identity");
    let again = engine.snapshot();
    assert!(Arc::ptr_eq(&rows.value, &again.of(&again.sheets, BoardId(0)).expect("the next snapshot has rows").value),
        "successive snapshots share the rows made once for this preview");
}

#[test]
fn circuit_samples_coalesce_to_the_newest_without_reversing_the_shown_sequence() {
    let (mut engine, mailbox, ..) = setup();
    let mut shown = Vec::new();
    mailbox.preview(value_sample(&engine, 1, 4700.0));
    let first = engine.step(mailbox.take(), &mut |snapshot| shown.extend(snapshot.preview_tag));
    assert_eq!(first.evals.len(), 1, "the first sample occupies the preview job slot");
    let mut jobs = first.evals.len();
    for (seq, si) in [(2, 6800.0), (3, 8200.0)] {
        mailbox.preview(value_sample(&engine, seq, si));
        let queued = engine.step(mailbox.take(), &mut |snapshot| shown.extend(snapshot.preview_tag));
        jobs += queued.evals.len();
        assert!(queued.evals.is_empty() && queued.rejected.is_empty(), "sample {seq} waits without a parallel preview job or refusal");
        assert!(!queued.waiting, "queued sample {seq} does not block undo");
    }
    mailbox.result(first.evals[0].run());
    let next = engine.step(mailbox.take(), &mut |snapshot| shown.extend(snapshot.preview_tag));
    jobs += next.evals.len();
    assert_eq!(preview_value(&engine), 4700.0, "the first evaluated sample remains visible while the newest runs");
    assert_eq!(next.evals.len(), 1, "completion immediately starts the waiting sample");
    assert_eq!(next.evals[0].preview, Some(3), "only the newest waiting sample evaluates next");
    assert!(engine.snapshot().stale.has(StageFlags::EVAL), "the shown first sample stays stale while the next runs");
    mailbox.result(next.evals[0].run());
    let finished = engine.step(mailbox.take(), &mut |snapshot| shown.extend(snapshot.preview_tag));
    jobs += finished.evals.len();
    assert_eq!(jobs, 2, "three samples produce exactly two evaluation jobs");
    assert_eq!(preview_value(&engine), 8200.0, "the final preview shows the newest evaluated value");
    assert_eq!(shown.last(), Some(&103), "the final published tag identifies sample three");
    assert!(shown.windows(2).all(|tags| tags[0] <= tags[1]), "the published sample sequence never goes backwards");
    assert!(!shown.contains(&102), "the coalesced middle sample is never shown");
    assert!(finished.rejected.is_empty(), "coalescing never refuses a valid sample");
    assert!(!engine.snapshot().stale.has(StageFlags::EVAL), "the final result clears evaluation staleness");
}

#[test]
fn a_commit_under_a_circuit_drag_reissues_the_newest_sample_on_the_committed_text() {
    let (mut engine, mailbox, ..) = setup();
    let part = board(&engine).placed.first().expect("the fixture has a placed part").clone();
    mailbox.preview(value_sample(&engine, 1, 4700.0));
    let first = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, first);
    let shown = engine.snapshot().preview.as_ref().expect("the first sample is live").clone();
    mailbox.push(move_part(part.id, P::mm(1.0, 0.0), "move under preview"));
    let moved = step(&mut engine, &mailbox);
    assert!(moved.rejected.is_empty(), "a commit under a live circuit preview does not refuse it");
    assert_eq!(moved.evals.len(), 1, "the live sample reissues immediately on committed board text");
    assert_eq!(moved.evals[0].preview, Some(1), "rebasing retains the live sample sequence");
    assert!(Arc::ptr_eq(&shown, engine.snapshot().preview.as_ref().expect("the old preview stays visible")),
        "the last evaluated preview stays visible during re-evaluation");
    mailbox.preview(value_sample(&engine, 2, 6800.0));
    // The commit slot starts while a preview is already running; adoption moves its base again.
    mailbox.push(set_value(&engine));
    let concurrent = step(&mut engine, &mailbox);
    assert!(concurrent.rejected.is_empty(), "a newer sample and circuit commit are accepted during preview evaluation");
    assert_eq!(concurrent.evals.len(), 1, "the commit starts without waiting for the preview job");
    assert_eq!(concurrent.evals[0].preview, None, "the independent job belongs to the commit slot");
    assert_ne!(concurrent.evals[0].id, moved.evals[0].id, "commit and preview jobs share unique identities");
    mailbox.result(concurrent.evals[0].run());
    let adopted = step(&mut engine, &mailbox);
    assert!(adopted.rejected.is_empty(), "adopting the value commit does not refuse the running or waiting preview");
    assert_eq!(value(&engine), 4700.0, "the independent commit is adopted before preview completion");
    let committed_texts = texts(&engine);
    mailbox.result(moved.evals[0].run());
    let retry = step(&mut engine, &mailbox);
    assert!(retry.rejected.is_empty(), "a result based on old text retries without refusal");
    assert_eq!(retry.evals.len(), 1, "a stale result reissues the newest sample in the same step");
    assert_eq!(retry.evals[0].preview, Some(2), "retry chooses the waiting sample over the older job's sample");
    run(&mut engine, &mailbox, retry);
    let snapshot = engine.snapshot();
    let preview_board = snapshot.preview.as_ref().expect("the newest preview is shown")
        .unit(BoardId(0)).expect("the preview keeps the controller").board.as_ref().expect("the preview keeps its board");
    assert_eq!(preview_value(&engine), 6800.0, "the rebased preview shows the newest sample value");
    assert_eq!(preview_board.placed_item(part.id).expect("the moved part remains").at, part.at.add(P::mm(1.0, 0.0)),
        "the preview also contains the committed board move");
    assert_eq!(texts(&engine), committed_texts, "re-evaluating the preview leaves committed text unchanged");
    assert_eq!(engine.log().entries.len(), 2, "only the board and value commits enter history");
    assert!(snapshot.last_refusal.is_none(), "a commit under a circuit drag leaves no refusal");
    assert_eq!(snapshot.preview_tag, Some(102), "the reissued preview keeps the newest sample tag");
}

#[test]
fn cancelling_circuit_samples_preserves_every_byte_and_drops_late_results() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    let design = engine.design().clone();
    let map = engine.docs().expect("the fixture has documents").map().clone();
    mailbox.preview(value_sample(&engine, 1, 4700.0));
    let first = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, first);
    mailbox.preview(value_sample(&engine, 2, 6800.0));
    let running = step(&mut engine, &mailbox);
    assert_eq!(running.evals.len(), 1, "a newer sample starts behind the shown preview");
    mailbox.preview(value_sample(&engine, 3, 8200.0));
    let queued = step(&mut engine, &mailbox);
    assert!(queued.evals.is_empty(), "the third sample waits behind the running job");
    mailbox.preview(value_sample(&engine, 4, 10000.0));
    mailbox.push(Cmd::CancelPreview { seq: 4 });
    let cancelled = step(&mut engine, &mailbox);
    assert!(cancelled.rejected.is_empty() && cancelled.evals.is_empty(), "cancel clears the live, running, queued and inbox samples");
    assert!(engine.snapshot().preview.is_none(), "cancel ends the visible circuit preview");
    assert!(!engine.snapshot().stale.has(StageFlags::EVAL), "cancel clears the preview's evaluation flag");
    mailbox.result(running.evals[0].run());
    let late = step(&mut engine, &mailbox);
    assert!(late.rejected.is_empty() && late.evals.is_empty(), "a cancelled result neither refuses nor starts another sample");
    assert!(engine.snapshot().preview.is_none(), "a late result cannot resurrect a cancelled preview");
    assert_eq!(texts(&engine), loaded, "cancelling and receiving a late result preserve every document byte");
    assert!(Arc::ptr_eq(engine.design(), &design), "cancelling preserves the committed design Arc");
    assert!(Arc::ptr_eq(engine.docs().expect("documents remain").map(), &map), "cancelling preserves the committed source map");
    assert!(engine.log().entries.is_empty(), "cancel and late completion create no history entry");
}

#[test]
fn a_pool_refused_circuit_sample_reports_its_tag_and_ends_the_live_preview() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    mailbox.preview(value_sample(&engine, 1, 4700.0));
    let first = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, first);
    mailbox.preview(value_sample(&engine, 2, f64::INFINITY));
    let requested = step(&mut engine, &mailbox);
    assert!(requested.rejected.is_empty(), "the non-finite sample reaches pool verification");
    assert_eq!(requested.evals.len(), 1, "the non-finite sample starts a verification job");
    assert_eq!(preview_value(&engine), 4700.0, "the last good preview remains visible until verification finishes");
    mailbox.result(requested.evals[0].run());
    let refused = step(&mut engine, &mailbox);
    assert_eq!(refused.rejected.len(), 1, "the pool refusal is reported once");
    let snapshot = engine.snapshot();
    let refusal = snapshot.last_refusal.as_ref().expect("the snapshot carries the sample refusal");
    assert_eq!(refusal.sample, Some(102), "the refusal echoes the sample tag rather than its sequence");
    assert_eq!(refusal.op, "circuit", "the refusal identifies the circuit operation");
    assert!(refusal.text.contains("not a finite value"), "the refusal retains the pool's diagnostic");
    assert!(refusal.site.is_some(), "the refusal keeps the diagnostic's source location");
    assert!(snapshot.preview.is_none(), "a refused circuit sample ends the live preview");
    assert!(!snapshot.stale.has(StageFlags::EVAL), "a refused sample clears the completed job's evaluation flag");
    assert_eq!(texts(&engine), loaded, "a refused preview changes no document bytes");
    assert!(engine.log().entries.is_empty(), "a refused preview creates no history entry");
}

#[test]
fn a_shown_circuit_preview_commits_without_a_second_evaluation_and_round_trips_text() {
    let (mut engine, mailbox, steps, logged) = setup();
    let loaded = texts(&engine);
    let original = value(&engine);
    let sample = value_sample(&engine, 7, 6800.0);
    mailbox.preview(sample.clone());
    let requested = step(&mut engine, &mailbox);
    assert_eq!(requested.evals.len(), 1, "showing the sample needs one preview evaluation");
    let done = requested.evals[0].run();
    let TaskResult::Eval(result) = &done else { panic!("the preview job returns an evaluation") };
    let EvalOut::Verified(Ok((verified, _))) = &result.out else { panic!("the value sample verifies") };
    let expected: Vec<_> = verified.sources.docs().iter().map(|doc| (doc.id, doc.text.clone())).collect();
    mailbox.result(done);
    let shown = step(&mut engine, &mailbox);
    assert!(shown.rejected.is_empty() && shown.evals.is_empty(), "the verified sample is shown without another job");
    assert_eq!(preview_value(&engine), 6800.0, "the shown preview has the sampled value");
    let preview = engine.snapshot().preview.as_ref().expect("the verified sample is live").clone();
    let reserved = steps.load(SeqCst) + 1;
    mailbox.push(Cmd::CommitPreview { seq: sample.seq, tag: sample.tag });
    let dropped = step(&mut engine, &mailbox);
    assert!(dropped.evals.is_empty(), "carrying the shown preview starts no second EvalJob");
    assert!(dropped.rejected.is_empty() && !dropped.waiting, "the carried drop is committed immediately");
    assert_eq!(steps.load(SeqCst), reserved, "the drop reserves exactly one number at acceptance");
    assert_eq!(logged.load(SeqCst), reserved, "the carried commit logs its reserved number");
    assert_eq!(engine.log().entries.len(), 1, "carrying the preview logs exactly one entry");
    let entry = &engine.log().entries[0];
    assert_eq!(entry.kind, EntryKind::Eval, "a carried circuit preview uses evaluation history");
    assert_eq!(entry.seq, reserved, "the history entry uses the acceptance reservation, not the sample sequence");
    assert_eq!(entry.by, sample.by, "the carried entry preserves sample authorship");
    assert_eq!(entry.label, sample.label, "the carried entry preserves the sample label");
    assert_eq!(entry.ops, sample.ops, "the carried entry retains the exact sampled operations");
    assert!(Arc::ptr_eq(engine.design(), &preview), "the drop adopts the exact shown design Arc");
    assert_eq!(value(&engine), 6800.0, "the sample value becomes committed without evaluation");
    assert_eq!(texts(&engine), expected, "committed documents are exactly the preview's verified texts");
    let snapshot = engine.snapshot();
    let committed = snapshot.last_commit.as_ref().expect("the carried drop publishes a commit");
    assert_eq!(committed.tag, Some(sample.tag), "Committed.tag identifies the dropped sample");
    assert_eq!(committed.seq, reserved, "Committed.seq identifies the reserved history entry");
    assert!(snapshot.preview.is_none(), "carrying the sample ends the preview");
    mailbox.push(Cmd::Undo);
    let undo = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undo);
    assert_eq!(texts(&engine), loaded, "undo restores every original document byte");
    assert_eq!(value(&engine), original, "undo restores the original committed value");
    mailbox.push(Cmd::Redo);
    let redo = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, redo);
    assert_eq!(texts(&engine), expected, "redo restores every carried document byte");
    assert_eq!(value(&engine), 6800.0, "redo restores the sampled value");
}

#[test]
fn typed_text_before_a_preview_drop_requires_verification_again() {
    let (mut engine, mailbox, ..) = setup();
    mailbox.preview(value_sample(&engine, 1, 6800.0));
    let requested = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, requested);
    assert_eq!(preview_value(&engine), 6800.0, "the value sample is shown before typing");
    let docs = engine.docs().expect("the fixture has documents");
    let before = docs.sources().clone();
    let doc = before.find("ledctl.ckt.splash").expect("the circuit document exists");
    let end = docs.text(doc).expect("the circuit has text").len() as u32;
    let comment = "\n// keep this comment through the drop\n";
    mailbox.push(Cmd::Text {
        doc, base_rev: revision(&engine, doc), edits: vec![TextEdit { doc, span: end..end, text: comment.into() }],
        group: Some(1), label: "comment".into(), tag: None, by: By::Person, ticket: None,
    });
    mailbox.push(Cmd::CommitPreview { seq: 1, tag: 101 });
    let dropped = step(&mut engine, &mailbox);
    assert!(!engine.docs().expect("documents remain").same_texts(&before), "typing invalidates the preview's source identities");
    assert_eq!(dropped.evals.len(), 1, "changed text forces an evaluation before the drop can land");
    assert!(dropped.waiting && dropped.rejected.is_empty(), "the drop waits for current text without refusing the sample");
    assert!(engine.snapshot().preview.is_none(), "the drop ends the outdated preview");
    run(&mut engine, &mailbox, dropped);
    assert_eq!(value(&engine), 6800.0, "the sample lands after verifying against the typed text");
    assert!(engine.docs().expect("documents remain").text(doc).expect("the circuit remains").ends_with(comment),
        "the verified drop preserves the intervening comment byte for byte");
    assert_eq!(engine.log().entries.len(), 2, "typing and the verified drop each log once");
}

#[test]
fn assets_before_a_preview_drop_require_verification_again() {
    let (mut engine, mailbox, ..) = setup();
    mailbox.preview(value_sample(&engine, 1, 6800.0));
    let requested = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, requested);
    assert_eq!(preview_value(&engine), 6800.0, "the sample is shown before asset registration");
    let loaded = texts(&engine);
    let before = engine.docs().expect("the fixture has documents").sources().clone();
    mailbox.push(Cmd::Docs {
        set: vec![DocSet::Asset { name: "x.glb".into(), key: "asset:0/x.glb".into() }],
        label: "asset".into(), by: By::Person, ticket: None,
    });
    mailbox.push(Cmd::CommitPreview { seq: 1, tag: 101 });
    let dropped = step(&mut engine, &mailbox);
    assert_eq!(texts(&engine), loaded, "registering an asset leaves all document text unchanged");
    assert!(!engine.docs().expect("documents remain").same_texts(&before), "the asset list alone invalidates the carry");
    assert_eq!(dropped.evals.len(), 1, "an asset change forces evaluation before committing the sample");
    assert!(dropped.waiting && dropped.rejected.is_empty(), "the asset change defers the drop without refusing it");
    run(&mut engine, &mailbox, dropped);
    assert_eq!(value(&engine), 6800.0, "the sample lands after verification with the new asset list");
    assert!(engine.docs().expect("documents remain").sources().assets().iter().any(|name| name == "x.glb"),
        "the drop retains the registered asset");
    assert_eq!(engine.log().entries.len(), 1, "only the verified drop enters history");
}

#[test]
fn new_factories_before_a_preview_drop_require_verification_again() {
    let (mut engine, mailbox, ..) = setup();
    mailbox.preview(value_sample(&engine, 1, 6800.0));
    let requested = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, requested);
    assert_eq!(preview_value(&engine), 6800.0, "the sample is shown with the original factories");
    let before = engine.docs().expect("the fixture has documents").sources().clone();
    let design = engine.design().clone();
    mailbox.push(Cmd::SetFactories(Arc::new(Factories::new())));
    mailbox.push(Cmd::CommitPreview { seq: 1, tag: 101 });
    let dropped = step(&mut engine, &mailbox);
    assert!(engine.docs().expect("documents remain").same_texts(&before), "factory replacement leaves source identities and assets unchanged");
    assert!(Arc::ptr_eq(engine.design(), &design), "the factory change alone leaves the committed design identity unchanged");
    assert_eq!(dropped.evals.len(), 1, "a different factories Arc forces a second evaluation");
    assert!(dropped.evals[0].verify.is_some() && dropped.evals[0].preview.is_none(), "the drop uses commit verification with the new factories");
    assert!(engine.log().entries.is_empty(), "the stale carry does not log before verification");
    run(&mut engine, &mailbox, dropped);
    assert_eq!(value(&engine), 6800.0, "the sample lands after verification under the new factories");
    assert_eq!(engine.log().entries.len(), 1, "the verified drop logs once after the factory change");
}

#[test]
fn a_preview_drop_waits_for_an_earlier_value_commit_and_verifies_again() {
    let (mut engine, mailbox, steps, logged) = setup();
    let sample = value_sample(&engine, 7, 6800.0);
    mailbox.preview(sample.clone());
    let requested = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, requested);
    assert_eq!(preview_value(&engine), 6800.0, "the sample is shown before the earlier commit starts");
    mailbox.push(set_value(&engine));
    let earlier = step(&mut engine, &mailbox);
    assert_eq!(earlier.evals.len(), 1, "the earlier accepted value commit has an evaluation in flight");
    mailbox.push(Cmd::CommitPreview { seq: sample.seq, tag: sample.tag });
    let dropped = step(&mut engine, &mailbox);
    assert!(dropped.rejected.is_empty() && dropped.waiting && dropped.evals.is_empty(), "the drop waits behind the earlier evaluation without refusal");
    assert_eq!((steps.load(SeqCst), logged.load(SeqCst)), (2, 0), "both commits reserve numbers before either is logged");
    assert!(engine.log().entries.is_empty(), "the shown preview cannot overtake the earlier accepted commit");
    assert!(engine.snapshot().preview.is_none(), "an ordered drop ends the shown preview immediately");
    mailbox.result(earlier.evals[0].run());
    let verified = step(&mut engine, &mailbox);
    assert_eq!(value(&engine), 4700.0, "the earlier value is committed before the dropped sample");
    assert_eq!(verified.evals.len(), 1, "the dropped sample is verified again after the earlier commit lands");
    assert!(verified.evals[0].verify.is_some() && verified.evals[0].preview.is_none(), "the later job verifies a commit, not a preview");
    run(&mut engine, &mailbox, verified);
    assert_eq!(value(&engine), 6800.0, "the dropped sample is the final committed value");
    assert_eq!(engine.log().entries.iter().map(|entry| entry.seq).collect::<Vec<_>>(), vec![1, 2],
        "the two commits log in acceptance order with their reserved numbers");
    assert_eq!(engine.snapshot().last_commit.as_ref().expect("the sample commits").tag, Some(sample.tag),
        "the queued commit preserves the sample tag");
}

#[test]
fn a_drop_during_its_preview_job_commits_and_ignores_the_late_preview_result() {
    let (mut engine, mailbox, steps, logged) = setup();
    let sample = value_sample(&engine, 7, 6800.0);
    mailbox.preview(sample.clone());
    let preview = step(&mut engine, &mailbox);
    assert_eq!(preview.evals.len(), 1, "the sample has a running preview job");
    mailbox.push(Cmd::CommitPreview { seq: sample.seq, tag: sample.tag });
    let dropped = step(&mut engine, &mailbox);
    assert!(dropped.rejected.is_empty(), "dropping an unfinished sample is accepted");
    assert_eq!(dropped.evals.len(), 1, "an early drop starts verified commit evaluation");
    assert!(dropped.evals[0].verify.is_some() && dropped.evals[0].preview.is_none(), "the early drop uses the verified commit path");
    assert_eq!((steps.load(SeqCst), logged.load(SeqCst)), (1, 0), "the early drop reserves before evaluation without logging yet");
    run(&mut engine, &mailbox, dropped);
    assert_eq!(value(&engine), 6800.0, "the sample's value lands despite its unfinished preview job");
    let committed = texts(&engine);
    mailbox.result(preview.evals[0].run());
    let late = step(&mut engine, &mailbox);
    assert!(late.rejected.is_empty() && late.evals.is_empty(), "the late preview result is ignored without starting another job");
    assert!(engine.snapshot().preview.is_none(), "a committed sample's late preview result shows nothing");
    assert_eq!(texts(&engine), committed, "the late preview result changes no committed text");
    assert_eq!(engine.log().entries.len(), 1, "the early drop and late result produce only one history entry");
    assert_eq!(engine.snapshot().last_commit.as_ref().expect("the early drop commits").tag, Some(sample.tag),
        "the early drop retains its sample tag");
}

#[test]
fn a_queued_preview_drop_lands_behind_held_commands_and_clears_older_jobs() {
    let (mut engine, mailbox, ..) = setup();
    mailbox.preview(value_sample(&engine, 1, 6800.0));
    let preview = step(&mut engine, &mailbox);
    assert_eq!(preview.evals.len(), 1, "the first sample occupies the preview job slot");
    mailbox.preview(value_sample(&engine, 2, 8200.0));
    let queued = step(&mut engine, &mailbox);
    assert!(queued.evals.is_empty() && queued.rejected.is_empty(), "the second sample waits in the preview queue");
    mailbox.push(set_value(&engine));
    let earlier = step(&mut engine, &mailbox);
    assert_eq!(earlier.evals.len(), 1, "an earlier value commit runs independently of the preview");
    mailbox.push(Cmd::Undo);
    mailbox.push(Cmd::CommitPreview { seq: 2, tag: 102 });
    let dropped = step(&mut engine, &mailbox);
    assert!(dropped.rejected.is_empty() && dropped.waiting && dropped.evals.is_empty(),
        "the queued sample's drop waits behind the held undo without refusal");
    mailbox.result(preview.evals[0].run());
    let late = step(&mut engine, &mailbox);
    assert!(late.evals.is_empty() && late.rejected.is_empty(), "the older preview job is cleared even while commands are held");
    assert!(engine.snapshot().preview.is_none(), "the cleared job's late result cannot show a preview");
    run(&mut engine, &mailbox, earlier);
    assert_eq!(value(&engine), 8200.0, "the queued sample lands after the earlier commit and held undo");
    assert_eq!(engine.snapshot().last_commit.as_ref().expect("the queued sample commits").tag, Some(102),
        "the held drop commits the queued sample's tag, not the running sample's tag");
    assert!(engine.snapshot().preview.is_none(), "the queued sample is committed without reviving its preview");
}

#[test]
fn dropping_a_pool_refused_preview_reuses_its_refusal_without_a_job_or_log() {
    let (mut engine, mailbox, steps, logged) = setup();
    let loaded = texts(&engine);
    mailbox.preview(value_sample(&engine, 7, f64::INFINITY));
    let requested = step(&mut engine, &mailbox);
    assert_eq!(requested.evals.len(), 1, "the non-finite sample reaches pool verification");
    mailbox.result(requested.evals[0].run());
    let refused = step(&mut engine, &mailbox);
    assert_eq!(refused.rejected.len(), 1, "the pool refuses the non-finite sample once");
    let earlier = engine.snapshot();
    let refusal = earlier.last_refusal.as_ref().expect("the earlier snapshot holds the pool refusal");
    mailbox.push(Cmd::CommitPreview { seq: 7, tag: 107 });
    let mut published = Vec::new();
    let dropped = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(dropped.evals.is_empty() && !dropped.waiting, "dropping a refused sample starts no evaluation job");
    assert_eq!(dropped.rejected, vec![(refusal.label.clone(), refusal.text.clone())], "the drop answers the original refusal label and text");
    let snapshot = published.last().expect("the drop republishes the sample refusal");
    assert!(Arc::ptr_eq(refusal, snapshot.last_refusal.as_ref().expect("the refusal remains visible")),
        "the drop reuses the same refusal Arc as the earlier snapshot");
    assert!(engine.log().entries.is_empty(), "dropping a refused sample logs nothing");
    assert_eq!((steps.load(SeqCst), logged.load(SeqCst)), (0, 0), "a refused drop neither reserves nor logs a commit number");
    assert_eq!(texts(&engine), loaded, "a refused drop changes no document bytes");
    mailbox.push(Cmd::CommitPreview { seq: 7, tag: 107 });
    let repeated = step(&mut engine, &mailbox);
    assert!(repeated.evals.is_empty() && repeated.rejected.len() == 1, "a second drop starts nothing and is answered once");
    let again = engine.snapshot().last_refusal.clone().expect("the second drop publishes its answer");
    assert!(!Arc::ptr_eq(refusal, &again) && again.sample == Some(107) && again.text.contains("ended"),
        "the first drop consumed the stored refusal: the second is refused as ended, under the same tag");
}

/// Drain evaluation jobs whatever the step refused; returns every rejection on the way.
fn drain(engine: &mut Engine, mailbox: &Mailbox, mut step: Step) -> Vec<(String, String)> {
    let mut rejected = std::mem::take(&mut step.rejected);
    while !step.evals.is_empty() {
        for job in step.evals {
            mailbox.result(job.run());
        }
        step = engine.step(mailbox.take(), &mut |_| {});
        rejected.append(&mut step.rejected);
    }
    rejected
}

/// Fable H4 (11-16): circuit ids are positions in the circuit they were planned on. A typed part
/// before `mcu` takes mcu.u's id, so a release that crosses that commit would wire the new part's
/// pins. The host refuses such a sample (commit or preview) with a reason and evaluates nothing;
/// a board-only commit in between leaves the circuit, and the sample still commits.
#[test]
fn a_circuit_sample_crossing_another_circuit_commit_is_refused_not_rewired() {
    let (mut engine, mailbox, ..) = setup();
    let unit = engine.design().units[0].id;
    let planned = engine.design().clone();
    let mcu = planned.unit(unit).expect("the controller unit").circuit.comp("mcu.u").expect("the fixture has its MCU").clone();
    let pin = |name: &str| mcu.part.pin(name).unwrap_or_else(|| panic!("the MCU has {name}"));
    let wire = vec![DesignOp::Circuit { unit, op: CircuitOp::ConnectPins { a: (mcu.id, pin("PA8")), b: (mcu.id, pin("PA9")) } }];
    let stamp = circuits_named(&planned, &wire);
    assert_eq!(stamp.len(), 1, "the sample's circuit op stamps its unit's circuit");

    // Another author types a part before `mcu`; evaluation numbers it where mcu.u was.
    let docs = engine.docs().expect("the fixture has documents");
    let doc = docs.sources().find("ledctl.ckt.splash").expect("the circuit document exists");
    let at = docs.text(doc).expect("the circuit has text").find("let mcu   = mcu_core(").expect("the fixture hooks up its MCU") as u32;
    mailbox.push(Cmd::Text {
        doc, base_rev: revision(&engine, doc), group: None, label: "typed spare".into(), tag: None, by: By::Ai { turn: 1, call: 1 }, ticket: None,
        edits: vec![TextEdit { doc, span: at..at, text: "let spare = e.part(\"MCU_20_TSSOP\", {})\n".into() }],
    });
    let typed = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, typed);
    let spare = engine.design().unit(unit).expect("the unit remains").circuit.comp("spare").expect("the typed part is in the circuit").id;
    assert_eq!(spare, mcu.id, "precondition: the typed part now has mcu.u's planned id");
    let after_typing = texts(&engine);
    let entries = engine.log().entries.len();
    let spare_wired = |engine: &Engine| engine.design().unit(unit).expect("the unit remains").circuit.comp("spare")
        .expect("the spare remains").conns.iter().any(|(p, _)| *p == pin("PA8") || *p == pin("PA9"));

    // 1. The release crosses the typed commit: CommitPreview of a sample planned before it.
    mailbox.preview(Sample {
        seq: 1, tag: 21, ops: wire.clone(), place: vec![], label: "wire PA8 to PA9".into(),
        by: By::Person, proposal: None, notes: vec![], circuits: stamp.clone(), expect: None,
    });
    mailbox.push(Cmd::CommitPreview { seq: 1, tag: 21 });
    let released = step(&mut engine, &mailbox);
    let evaluated = released.evals.len();
    drain(&mut engine, &mailbox, released);
    assert!(!spare_wired(&engine), "the sample planned on mcu.u does not wire the spare's PA8 and PA9");
    assert_eq!(evaluated, 0, "a sample whose circuit changed is not evaluated again");
    let refusal = engine.snapshot().last_refusal.clone().expect("the crossing release is refused");
    assert_eq!(refusal.sample, Some(21), "the refusal names the released sample's tag");
    assert!(refusal.text.contains("circuit changed"), "the refusal says why: {}", refusal.text);
    assert_eq!(texts(&engine), after_typing, "the refused release changes no document byte");
    assert_eq!(engine.log().entries.len(), entries, "the refused release logs nothing");

    // 2. A preview planned before the typed commit is refused when it arrives, not evaluated.
    mailbox.preview(Sample {
        seq: 2, tag: 22, ops: wire.clone(), place: vec![], label: "wire PA8 to PA9".into(),
        by: By::Person, proposal: None, notes: vec![], circuits: stamp, expect: None,
    });
    let previewed = step(&mut engine, &mailbox);
    assert!(previewed.evals.is_empty(), "a stale circuit preview starts no evaluation");
    assert_eq!(engine.snapshot().last_refusal.as_ref().and_then(|refusal| refusal.sample), Some(22), "the stale preview is refused by its tag");
    assert!(engine.snapshot().preview.is_none(), "the stale preview is not shown");

    // 3. A board-only commit between planning and release keeps the circuit: the sample commits.
    let current = engine.design().clone();
    let rewire = vec![DesignOp::Circuit { unit, op: CircuitOp::ConnectPins { a: (spare, pin("PA8")), b: (spare, pin("PA9")) } }];
    let via = board(&engine).vias[0].id;
    mailbox.push(Cmd::Commit {
        ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item: via, by: P::mm(0.5, 0.0) } }], place: vec![],
        label: "move via".into(), circuits: vec![], expect: None, by: By::Ai { turn: 1, call: 2 }, ticket: None,
    });
    mailbox.preview(Sample {
        seq: 3, tag: 23, ops: rewire.clone(), place: vec![], label: "wire the spare".into(),
        by: By::Person, proposal: None, notes: vec![], circuits: circuits_named(&current, &rewire), expect: None,
    });
    mailbox.push(Cmd::CommitPreview { seq: 3, tag: 23 });
    let kept = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, kept);
    assert!(spare_wired(&engine), "after a board-only commit the sample planned on the same circuit commits");
}

/// Firmware text belongs to the firmware language: typing and undoing it reach the plate as text
/// and never start an evaluation or touch the design.
#[test]
fn firmware_text_saves_and_undoes_without_an_evaluation() {
    let mut sources = fab_elec_fixtures::docs("ledctl").expect("the fixture supplies controller documents");
    let fw = sources.add("ledctl.fw.splash", "let chase_ms = 125\n").expect("the firmware document is added");
    let (design, docs) = Docs::from_evaluated(sources, fab_elec_fixtures::ledctl_eval());
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "eval".into())
        .with_docs(docs);
    let mailbox = Mailbox::default();
    let design = engine.design().clone();
    let firmware = |step: &Step| step.docs.as_ref().and_then(|written| written.docs.get("ledctl.fw.splash")).map(|text| text.to_string());
    let at = "let chase_ms = ".len() as u32;
    for (span, text, want) in [(at..at + 3, "2", "let chase_ms = 2\n"), (at + 1..at + 1, "0", "let chase_ms = 20\n")] {
        mailbox.push(Cmd::Text {
            by: By::Person, ticket: None, tag: None,
            doc: fw, base_rev: revision(&engine, fw), edits: vec![TextEdit { doc: fw, span, text: text.into() }],
            group: Some(1), label: "typing".into(),
        });
        let typed = step(&mut engine, &mailbox);
        assert!(typed.rejected.is_empty(), "typed firmware text is accepted: {:?}", typed.rejected);
        assert!(typed.evals.is_empty() && !typed.waiting, "firmware text starts no evaluation");
        assert_eq!(firmware(&typed).as_deref(), Some(want), "the step hands the firmware text to the plate");
        assert!(!engine.docs().expect("the engine retains documents").behind(), "firmware text never marks the design behind");
    }
    assert_eq!(engine.log().entries.len(), 1, "one typing gesture is one history entry");
    let entry = &engine.log().entries[0];
    assert!(!entry.text.reevaluate && !entry.text_inverse.reevaluate, "the merged firmware entry never re-evaluates either way");
    mailbox.push(Cmd::Undo);
    let undone = step(&mut engine, &mailbox);
    assert!(undone.rejected.is_empty() && undone.evals.is_empty() && !undone.waiting, "undoing firmware text starts no evaluation");
    assert_eq!(firmware(&undone).as_deref(), Some("let chase_ms = 125\n"), "undo restores the firmware text");
    assert!(Arc::ptr_eq(engine.design(), &design), "firmware text and its undo leave the design as it was");
}

/// A project part the Library panel lists once its document is added.
const SENSOR_PART: &str = r#"let e = mod.elec
e.define("SENSOR_X", {
    kind: @ic prefix: "U"
    fields: {title: "Sensor" mpn: "SENSOR_X"}
    symbol: e.f.ic_symbol("Sensor", {left: [["1", @in], ["VCC", @power_in]] right: [["OUT", @out], ["GND", @power_in]]})
})
"#;

/// The Library panel's entries ride every document-backed snapshot: the load's, the same Arc
/// across a circuit-only commit, a new one listing a part a person adds, and a failed load's.
#[test]
fn snapshots_carry_the_catalog_and_a_definition_document_replaces_it() {
    let (mut engine, mailbox, ..) = setup();
    let named = |catalog: &fab_elec_lang::Catalog, name: &str| catalog.entries.iter().any(|entry| entry.name == name);
    let loaded = engine.snapshot().catalog.clone().expect("a document-backed snapshot carries the Library panel's catalog");
    assert!(named(&loaded, "usb_c_power"), "the loaded catalog lists the installed library's blocks");

    mailbox.push(set_value(&engine));
    let committed = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, committed);
    assert_eq!(value(&engine), 4700.0, "the circuit-only commit is adopted");
    let after_value = engine.snapshot().catalog.clone().expect("the commit's snapshot carries the catalog");
    assert!(Arc::ptr_eq(&loaded, &after_value), "a circuit-only commit keeps the catalog Arc, so the panel does not rebuild");

    mailbox.push(Cmd::Docs {
        set: vec![DocSet::Add { name: "parts/sensor.part.splash".into(), text: SENSOR_PART.into() }],
        label: "define a sensor".into(), by: By::Person, ticket: None,
    });
    let added = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, added);
    let defined = engine.snapshot().catalog.clone().expect("the definition's snapshot carries the catalog");
    assert!(!Arc::ptr_eq(&loaded, &defined), "an added part document replaces the catalog");
    assert!(named(&defined, "SENSOR_X") && named(&defined, "usb_c_power"), "the new catalog lists the part beside the library");

    let mut broken = fab_elec_lang::Sources::new();
    broken.add("broken.ckt.splash", "let e = mod.elec\nlet r = e.r(\n").expect("the broken circuit has a valid name");
    broken.add("parts/sensor.part.splash", SENSOR_PART).expect("the part has a valid name");
    let libs = engine.docs().expect("documents remain").libs().as_ref().clone();
    let (design, docs) = Docs::load(broken, libs);
    assert!(design.units.is_empty(), "the broken circuit fails the load");
    assert!(named(docs.catalog(), "SENSOR_X") && named(docs.catalog(), "usb_c_power"), "a failed load still lists its definitions and the library");
}

/// An imported part's copy as the importer writes it: its document in the parts folder, its body
/// naming the mesh beside it, and the key under which the host holds the mesh's bytes.
const PART_DOC: &str = "parts/test/T1.part.splash";
const PART_MESH: &str = "parts/test/T1.glb";
const PART_KEY: &str = "asset:00/T1.glb";
const PART_TEXT: &str = r#"let __test_t1_e = mod.elec
let __test_t1_f = mod.elec.f
let test_t1 = fn() {
    let e = __test_t1_e
    let f = __test_t1_f
    let existing = e.lookup("test:T1")
    if existing != nil { return existing }
    return e.define("test:T1", {
        kind: @resistor
        symbol: f.two_terminal([["1", @passive], ["2", @passive]])
        footprint: f.footprint({
            pads: [
                f.smd("1", [-0.5mm, 0mm], [0.5mm, 0.3mm], {shape: @rect rot: 0deg})
                f.smd("2", [0.5mm, 0mm], [0.5mm, 0.3mm], {shape: @rect rot: 0deg})
            ]
            body: [[-1mm, -0.5mm], [1mm, 0.5mm]]
            mount: @smd
        })
        body: {mesh: "T1.glb" offset: [0mm, 0mm, 0mm] rotation: [0deg, 0deg, 0deg] height: 0.5mm}
        pads: [["1", "1"], ["2", "2"]]
        prefix: "R"
        fields: {}
    })
}
test_t1()
"#;

/// The mesh's bytes as the importer read them: one allocation the tests follow by pointer.
fn mesh_bytes() -> AssetBytes {
    AssetBytes(Arc::from(&b"glTF test mesh bytes"[..]))
}

/// The importer's commit: the copy's document and mesh, then the part that uses them.
fn import(engine: &Engine, text: &str, key: &str, bytes: &AssetBytes) -> Vec<DesignOp> {
    vec![
        DesignOp::File(FileOp::AddDoc { name: PART_DOC.into(), text: text.into() }),
        DesignOp::File(FileOp::AddAsset { name: PART_MESH.into(), key: key.into(), bytes: bytes.clone() }),
        DesignOp::Circuit { unit: engine.design().units[0].id, op: CircuitOp::AddPart {
            stmt: "let imp = e.part(\"test:T1\", {})".into(), after: None, ends: vec![], hint: None,
        } },
    ]
}

fn commit(ops: Vec<DesignOp>, label: &str) -> Cmd {
    Cmd::Commit { ops, place: vec![], label: label.into(), circuits: vec![], expect: None, by: By::Person, ticket: None }
}

/// The committed controller's imported parts, by the mesh each body names.
fn imported(engine: &Engine) -> Vec<Option<BodySource>> {
    engine.design().units[0].circuit.comps.iter().filter(|comp| comp.part.name == "test:T1")
        .map(|comp| comp.part.body.as_ref().map(|body| body.source.clone())).collect()
}

fn part_doc(engine: &Engine) -> Option<DocId> {
    engine.docs().expect("the engine retains documents").sources().find(PART_DOC)
}

fn has_mesh(engine: &Engine) -> bool {
    engine.docs().expect("the engine retains documents").sources().assets().iter().any(|name| name == PART_MESH)
}

/// Step and run evaluations until the engine answers; returns the step that refused or the last one.
fn until_answer(engine: &mut Engine, mailbox: &Mailbox) -> Step {
    let mut step = step(engine, mailbox);
    while step.rejected.is_empty() && !step.evals.is_empty() {
        for job in step.evals {
            mailbox.result(job.run());
        }
        step = engine.step(mailbox.take(), &mut |_| {});
    }
    step
}

#[test]
fn an_imported_part_its_document_and_its_mesh_are_one_commit_and_one_undo_step() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    assert!(part_doc(&engine).is_none() && !has_mesh(&engine), "the fixture holds no copy of the part");
    let bytes = mesh_bytes();
    let shared = |written: Option<&fab_elec_host::Written>| written.and_then(|written| written.assets.get(PART_MESH).cloned())
        .is_some_and(|held| held.is_some_and(|held| Arc::ptr_eq(&held, &bytes.0)));
    mailbox.push(commit(import(&engine, PART_TEXT, PART_KEY, &bytes), "import T1"));
    let requested = step(&mut engine, &mailbox);
    assert_eq!(requested.evals.len(), 1, "the copy and the part are verified in one pool job");
    let adopted = run(&mut engine, &mailbox, requested);
    assert!(adopted.docs.as_ref().is_some_and(|written| written.docs.contains_key(PART_DOC)) && shared(adopted.docs.as_ref()),
        "the plate's write carries the copy's document and the importer's own bytes, not a copy");
    let doc = part_doc(&engine).expect("the commit adds the copy's document");
    assert!(has_mesh(&engine), "the commit makes the copy's mesh available");
    assert_eq!(imported(&engine), vec![Some(BodySource::Mesh(PART_KEY.into()))], "the new part's body names the host's bytes");
    assert_eq!(engine.log().entries.len(), 1, "document, mesh and part are one history entry");
    let committed = texts(&engine);
    mailbox.push(Cmd::Undo);
    let undo = step(&mut engine, &mailbox);
    assert!(undo.docs.as_ref().is_some_and(|written| !written.docs.contains_key(PART_DOC) && !written.assets.contains_key(PART_MESH)),
        "the undo's write takes the copy's document and mesh off the plate");
    run(&mut engine, &mailbox, undo);
    assert_eq!(texts(&engine), loaded, "one undo restores every document and takes the copy's document out");
    assert!(!has_mesh(&engine), "one undo makes the mesh unavailable");
    assert!(imported(&engine).is_empty(), "one undo removes the part");
    mailbox.push(Cmd::Redo);
    let redo = step(&mut engine, &mailbox);
    assert!(shared(redo.docs.as_ref()), "the redo's write brings the history's allocation back, not a copy");
    run(&mut engine, &mailbox, redo);
    assert_eq!(texts(&engine), committed, "one redo brings every document back");
    assert_eq!(part_doc(&engine), Some(doc), "the copy's document keeps its id");
    assert_eq!(imported(&engine), vec![Some(BodySource::Mesh(PART_KEY.into()))], "redo names the same bytes");
    // The same copy again: only the part is added, and undoing it keeps the copy the first part uses.
    mailbox.push(commit(import(&engine, PART_TEXT, PART_KEY, &mesh_bytes()), "import T1 again"));
    let again = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, again);
    assert_eq!(imported(&engine).len(), 2, "a second import adds a second part");
    let entry = engine.log().entries.last().expect("the second import is logged");
    assert!(entry.text.present.is_empty() && entry.text.assets.is_empty(), "a copy the project holds is not added again");
    mailbox.push(Cmd::Undo);
    let undo = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undo);
    assert_eq!((imported(&engine).len(), part_doc(&engine), has_mesh(&engine)), (1, Some(doc), true),
        "undoing the second import keeps the copy the first part uses");
}

#[test]
fn an_import_naming_another_copy_or_other_bytes_is_refused_whole() {
    let (mut engine, mailbox, ..) = setup();
    let bytes = mesh_bytes();
    mailbox.push(commit(import(&engine, PART_TEXT, PART_KEY, &bytes), "import T1"));
    let first = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, first);
    let committed = texts(&engine);
    let other_text = PART_TEXT.replace("height: 0.5mm", "height: 0.6mm");
    for (ops, case) in [
        (import(&engine, &other_text, PART_KEY, &bytes), "another copy under the document's name"),
        (import(&engine, PART_TEXT, "asset:01/T1.glb", &mesh_bytes()), "other bytes under the mesh's name"),
    ] {
        mailbox.push(commit(ops, case));
        let answer = until_answer(&mut engine, &mailbox);
        assert_eq!(answer.rejected.len(), 1, "{case}: the import is refused");
        assert_eq!(texts(&engine), committed, "{case}: nothing of the import is applied");
        assert_eq!((engine.log().entries.len(), imported(&engine).len()), (1, 1), "{case}: a refused import logs and adds nothing");
    }
    // Undone, the mesh's name is free for other bytes; redo then refuses rather than name them.
    mailbox.push(Cmd::Undo);
    let undo = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undo);
    mailbox.push(Cmd::Docs {
        set: vec![DocSet::Asset { name: PART_MESH.into(), key: "asset:01/T1.glb".into() }],
        label: "other bytes".into(), by: By::Person, ticket: None,
    });
    let registered = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, registered);
    mailbox.push(Cmd::Redo);
    let redo = step(&mut engine, &mailbox);
    assert_eq!(redo.rejected.len(), 1, "redo of an import whose mesh name now holds other bytes is refused");
    assert!(part_doc(&engine).is_none() && imported(&engine).is_empty(), "the refused redo applies nothing");
}

#[test]
fn an_import_preview_shows_the_part_and_its_drop_commits_the_copy_in_the_same_step() {
    let (mut engine, mailbox, ..) = setup();
    let loaded = texts(&engine);
    let unit = engine.design().units[0].id;
    mailbox.preview(Sample {
        seq: 1, tag: 11, ops: import(&engine, PART_TEXT, PART_KEY, &mesh_bytes()), place: vec![],
        label: "preview import".into(), by: By::Person, proposal: None, notes: vec![], circuits: vec![], expect: None,
    });
    let requested = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, requested);
    let snapshot = engine.snapshot();
    let preview = snapshot.preview.as_ref().expect("the import is previewed");
    let part = preview.unit(unit).expect("the preview retains the unit").circuit.comps.iter()
        .find(|comp| comp.part.name == "test:T1").expect("the preview shows the imported part");
    assert_eq!(part.part.body.as_ref().map(|body| body.source.clone()), Some(BodySource::Mesh(PART_KEY.into())),
        "the preview's body names the host's bytes");
    assert_eq!(texts(&engine), loaded, "previewing adds no document");
    assert!(!has_mesh(&engine) && engine.log().entries.is_empty(), "previewing adds no asset and no history");
    mailbox.push(Cmd::CommitPreview { seq: 1, tag: 11 });
    let dropped = step(&mut engine, &mailbox);
    assert!(dropped.rejected.is_empty() && dropped.evals.is_empty(), "the shown import commits without a second evaluation");
    assert!(part_doc(&engine).is_some() && has_mesh(&engine), "the drop adds the copy's document and mesh");
    assert_eq!(imported(&engine), vec![Some(BodySource::Mesh(PART_KEY.into()))], "the committed part names the host's bytes");
    assert_eq!(engine.log().entries.len(), 1, "the drop is one history entry");
    mailbox.push(Cmd::Undo);
    let undo = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undo);
    assert_eq!(texts(&engine), loaded, "one undo of the drop takes the copy's document out");
    assert!(!has_mesh(&engine) && imported(&engine).is_empty(), "one undo of the drop removes the mesh and the part");
}

fn tool_args<const N: usize>(fields: [(&str, &str); N]) -> Vec<(String, String)> {
    fields.into_iter().map(|(name, value)| (name.into(), value.into())).collect()
}

#[test]
fn value_set_plans_the_card_edit_and_commits_it_once_for_the_caller() {
    use fab_elec_host::edit_actions::value_set;
    use fab_elec_lang::units::parse_value;

    let (engine, mailbox, ..) = setup();
    let mut engine = engine.with_app_tools();
    let unit = &engine.design().units[0];
    let part = unit.circuit.comp("mcu.boot_r").expect("the fixture has the boot resistor");
    let value_text = "4.7kohm";
    let parsed = parse_value(value_text, part.value.as_ref().expect("the resistor has a value"), &part.part.kind).expect("the card accepts the resistance");
    let card = Resolved {
        label: format!("Set {} to {}", part.refdes, parsed.text),
        ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::SetValue { comp: part.id, value: parsed } }],
        place: vec![],
    };
    let tool = "elec.value.set";
    for name in [&part.refdes, &part.path] {
        let args = tool_args([("comp", name), ("value", value_text)]);
        let Plan::Edit(edit) = value_set(engine.design(), engine.docs(), tool, &args).expect("both names resolve") else {
            panic!("a value set must plan a design edit");
        };
        assert_eq!(edit, card, "setting {name} makes exactly the card's op and undo label");
    }
    let by = By::Ai { turn: 0, call: 1 };
    let invoke = |args, ticket| Cmd::Invoke { tool: tool.into(), args, preview: false, by, ticket: Ticket(ticket) };
    let loaded = value(&engine);
    mailbox.push(invoke(tool_args([("comp", &part.path), ("value", value_text)]), 1));
    let done = until_answer(&mut engine, &mailbox);
    assert!(matches!(done.outcomes.as_slice(), [(Ticket(1), Outcome::Done { label, by: author, circuit: CircuitChange::Values, .. })]
        if *author == by && *label == card.label), "the caller gets one value change with its ticket, author and card label");
    assert_eq!(engine.log().entries.len(), 1, "one AI value edit creates one undo step");
    assert_eq!(value(&engine), 4700.0, "the evaluated circuit adopts the resistor's new value");
    mailbox.push(Cmd::Undo);
    let undone = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, undone);
    assert_eq!(value(&engine), loaded, "one undo takes the AI's edit back");
    mailbox.push(Cmd::Redo);
    let redone = step(&mut engine, &mailbox);
    run(&mut engine, &mailbox, redone);
    assert_eq!((value(&engine), engine.log().entries[0].by), (4700.0, by), "redo brings it back, still the AI's");

    for (args, reason) in [
        (tool_args([("comp", "missing"), ("value", "1k")]), "comp: no part missing; elec.design.read lists them"),
        (tool_args([("comp", "mcu.boot_r"), ("value", "2.2uF")]), "value: a resistance needs ohms: 2.2uF"),
        (tool_args([("comp", "mcu.boot_r"), ("value", "1k"), ("extra", "")]), "extra: unknown field"),
    ] {
        mailbox.push(invoke(args, 2));
        let refused = step(&mut engine, &mailbox);
        assert!(matches!(refused.outcomes.as_slice(), [(Ticket(2), Outcome::Refused(r))] if r.text == reason),
            "the engine returns the field's refusal: {reason}");
    }
    assert_eq!(engine.log().entries.len(), 1, "refused edits leave the undo history alone");
}

#[test]
fn part_place_plans_the_panes_drag_or_tray_drop_and_places_the_footprint() {
    use fab_elec_host::edit_actions::part_place;

    let (engine, mailbox, ..) = setup();
    let mut engine = engine.with_app_tools();
    let unit = engine.design().units[0].id;
    let placed = board(&engine).placed.iter().find(|p| p.path == "mcu.boot_r").expect("the boot resistor is placed").clone();
    let tool = "elec.part.place";
    let at = P::mm(34.0, 25.0);
    let args = tool_args([("part", &placed.refdes), ("at", "[3.4cm 25]")]);
    let plan = |engine: &Engine, args: &[(String, String)]| {
        let Plan::Edit(edit) = part_place(engine.design(), engine.docs(), tool, args)? else {
            panic!("placing or moving a part must plan a design edit");
        };
        Ok::<_, Diag>(edit)
    };
    let mut ops = vec![DesignOp::Board { unit, op: BoardOp::Move { item: placed.id, by: at.sub(placed.at) } }];
    ops.extend(label_ops(unit, board(&engine), &ops, &[placed.id]));
    let mut exact = args.clone();
    exact.push(("legalise".into(), "@off".into()));
    assert_eq!(plan(&engine, &exact).expect("the placed resistor can move"),
        Resolved { ops, place: vec![], label: format!("Move {}", placed.refdes) },
        "moving a placed part plans the pane's displacement and affected labels together");
    for (field, value, reason) in [
        ("at", "[20.5mm 7.5mm]", format!("at: {} is there already", placed.refdes)),
        ("at", "[90deg 1mm]", "at: expected a point in mm, as [12mm 30mm]".into()),
        ("rot", "0deg", format!("rot: {} is placed and keeps its rotation; only at moves it", placed.refdes)),
        ("side", "@bottom", format!("side: {} is placed and keeps its side; only at moves it", placed.refdes)),
    ] {
        let mut fields = args.clone();
        fields.retain(|(key, _)| key != field);
        fields.push((field.into(), value.into()));
        assert_eq!(plan(&engine, &fields).expect_err("the invalid move is refused").message, reason,
            "the move reports which coordinate, rotation or side it cannot apply");
    }

    mailbox.push(commit(vec![DesignOp::Board { unit, op: BoardOp::Remove(placed.id) }], "Return boot resistor to tray"));
    let removed = until_answer(&mut engine, &mailbox);
    assert!(removed.rejected.is_empty() && !board(&engine).placed.iter().any(|p| p.comp == placed.comp),
        "removing its placement returns the resistor to the tray");
    let drop = Resolved {
        ops: vec![],
        place: vec![Place { unit, part: PartRef::Path(placed.path.clone()), at, rot: None, side: None }],
        label: format!("Place {}", placed.refdes),
    };
    assert_eq!(plan(&engine, &exact).expect("the tray resistor can be dropped"), drop, "an ordinary drop keeps the pane's placement defaults");
    let args = tool_args([("part", &placed.path), ("at", "[34mm, 25mm]"), ("rot", "-90deg"), ("side", "@bottom")]);
    let requested = Place { rot: Some(Angle::deg(-90.0)), side: Some(Side::Bottom), ..drop.place[0].clone() };
    assert_eq!(plan(&engine, &args).expect("a tray drop accepts a rotation and side"),
        Resolved { place: vec![requested], ..drop }, "explicit rotation and side travel on the same placement");
    mailbox.push(Cmd::Invoke { tool: tool.into(), args, preview: false, by: By::Ai { turn: 0, call: 1 }, ticket: Ticket(1) });
    let done = until_answer(&mut engine, &mailbox);
    assert!(matches!(done.outcomes.as_slice(), [(Ticket(1), Outcome::Done { circuit: CircuitChange::None, .. })]),
        "the engine acknowledges one committed placement with no circuit change");
    let after = board(&engine).placed.iter().find(|p| p.comp == placed.comp).expect("the committed footprint is on the board");
    assert_eq!((after.at, after.rot, after.side), (at, Angle::deg(-90.0), Side::Bottom), "the board adopts the requested anchor, rotation and side");
}

#[test]
fn part_place_rotate_and_tidy_report_the_correction_from_the_requested_pose() {
    use fab_elec_board::placement::{body_at, placed_courtyard, PlacementIndex};

    let (engine, mailbox, ..) = setup();
    let mut engine = engine.with_app_tools();
    let placed = |engine: &Engine, path: &str| board(engine).placed.iter().find(|p| p.path == path)
        .expect("the fixture part is placed").clone();
    let part = placed(&engine, "mcu.boot_r");
    let mcu = placed(&engine, "mcu.u");
    let point = |at: P| format!("[{}mm {}mm]", f64::from(at.x) / 1e6, f64::from(at.y) / 1e6);
    let invoke = |engine: &mut Engine, tool: &str, args| {
        let ticket = Ticket(engine.log().entries.len() as u64 + 1);
        mailbox.push(Cmd::Invoke { tool: tool.into(), args, preview: false, by: By::Ai { turn: 1, call: ticket.0 }, ticket });
        let done = until_answer(engine, &mailbox);
        assert!(done.rejected.is_empty(), "{tool} commits its requested pose: {:?}", done.rejected);
        let [(answered, Outcome::Done { notes, .. })] = done.outcomes.as_slice() else { panic!("{tool} must answer once") };
        assert_eq!(*answered, ticket, "the placement report belongs to the committing call");
        notes.clone()
    };
    let clear = |engine: &Engine| PlacementIndex::new(board(engine), None).courtyard_pairs().is_empty();

    // Drop from the tray first, then exercise the existing part's Move op on the same board.
    mailbox.push(commit(vec![DesignOp::Board { unit: board(&engine).unit, op: BoardOp::Remove(part.id) }], "Return resistor to tray"));
    let removed = until_answer(&mut engine, &mailbox);
    assert!(removed.rejected.is_empty() && board(&engine).placed_of(part.comp).is_none(), "the resistor starts in the tray");
    let wish = point(mcu.at);
    let args = tool_args([("part", part.path.as_str()), ("at", &wish)]);
    let notes = invoke(&mut engine, "elec.part.place", args.clone());
    let landed = placed(&engine, &part.path);
    assert_ne!(landed.at, mcu.at, "a tray drop onto the MCU moves the resistor off its anchor");
    assert!(clear(&engine), "the legalised drop leaves no courtyard pair");
    assert_eq!(placed(&engine, &mcu.path).at, mcu.at, "the MCU remains fixed");
    let row = format!(r#"[{{part: "{}" moved_mm: {:.3}}}]"#, part.refdes, landed.at.dist(mcu.at) / 1e6);
    assert_eq!(notes, [("legalised".into(), row)], "the committed answer names the resistor and its displacement in mm to three decimals");

    let mut exact = args.clone();
    exact.push(("legalise".into(), "@off".into()));
    let notes = invoke(&mut engine, "elec.part.place", exact.clone());
    assert_eq!(placed(&engine, &part.path).at, mcu.at, "off keeps the exact requested anchor");
    assert!(notes.is_empty() && !clear(&engine), "off leaves the requested overlap and has no correction to report");
    invoke(&mut engine, "elec.part.place", args);
    assert_eq!(placed(&engine, &part.path).at, landed.at, "moving the placed resistor takes the same nearest spot as its tray drop");

    // Alongside the last lamp resistor, leave a gap that the boot resistor's long side would cross.
    let neighbour = placed(&engine, "lamps[7].r");
    let turned = landed.rot.add(Angle::deg(90.0));
    let reach = |rot| -body_at(&landed, P::ZERO, rot, landed.side, false).bbox().min.y;
    let edge = placed_courtyard(&neighbour).bbox().max.y;
    let overlap = P::new(neighbour.at.x, edge + reach(landed.rot) - mm(0.01));
    let near = tool_args([("part", &part.path), ("at", &point(overlap))]);
    let plan = fab_elec_host::edit_actions::part_place(engine.design(), engine.docs(), "elec.part.place", &near);
    assert!(matches!(plan, Ok(Plan::Noted { notes, .. }) if notes.iter().any(|(key, _)| key == "legalised")),
        "the resolver corrects a 0.01 mm courtyard overlap and says so");

    let beside = P::new(neighbour.at.x, edge + (reach(landed.rot) + reach(turned)) / 2);
    let notes = invoke(&mut engine, "elec.part.place", tool_args([("part", &part.path), ("at", &point(beside))]));
    assert_eq!(placed(&engine, &part.path).at, beside, "a legal request lands exactly beside the neighbour");
    assert!(notes.is_empty() && clear(&engine), "the legal move has no legalised entry or courtyard overlap");
    assert!(body_at(&landed, beside, turned, landed.side, false).bbox().intersects(&placed_courtyard(&neighbour).bbox()),
        "the uncorrected turn would overlap the neighbouring resistor");
    let notes = invoke(&mut engine, "elec.part.rotate", tool_args([("parts", &part.path)]));
    let rotated = placed(&engine, &part.path);
    assert_eq!((rotated.rot, rotated.side), (turned, landed.side), "the corrected turn keeps the requested angle and side");
    assert!(rotated.at != beside && clear(&engine), "rotation about the anchor adds a move to clear the neighbour");
    assert!(notes.iter().any(|(key, _)| key == "legalised"), "the rotation answer reports its correction");

    invoke(&mut engine, "elec.part.place", exact.clone());
    let overlap = board(&engine).clone();
    assert!(!clear(&engine), "the exact move gives Tidy an overlapping pair");
    let notes = invoke(&mut engine, "elec.place", vec![]);
    assert!(clear(&engine), "tidying the whole board clears every courtyard pair");
    let tidied = placed(&engine, &part.path);
    let row = format!(r#"[{{part: "{}" moved_mm: {:.3}}}]"#, part.refdes, tidied.at.dist(mcu.at) / 1e6);
    assert_eq!(notes, [("legalised".into(), row)], "Tidy reports the corrected part and its displacement");
    for old in overlap.placed.iter().filter(|p| p.comp != part.comp) {
        assert_eq!(board(&engine).placed_of(old.comp).unwrap().at, old.at, "Tidy moved {}", old.refdes);
    }
    let history = engine.log().entries.len();
    mailbox.push(Cmd::Invoke { tool: "elec.place".into(), args: vec![], by: By::Person, preview: false, ticket: Ticket(90) });
    let refused = until_answer(&mut engine, &mailbox);
    assert!(matches!(refused.outcomes.as_slice(), [(Ticket(90), Outcome::Refused(why))]
        if why.text == "Every part already sits on a legal spot."), "a legal board explains why Tidy did nothing");
    assert_eq!(engine.log().entries.len(), history, "a second Tidy adds no undo step");

    invoke(&mut engine, "elec.part.place", exact);
    // Only the MCU is in scope. The smaller resistor keeps its spot.
    invoke(&mut engine, "elec.place", tool_args([("parts", &mcu.path)]));
    assert_eq!(placed(&engine, &part.path).at, mcu.at, "a named scope leaves even a smaller overlapping neighbour fixed");
    mailbox.push(Cmd::Undo);
    let undone = until_answer(&mut engine, &mailbox);
    assert!(undone.rejected.is_empty() && !clear(&engine), "one undo restores the overlapping pair");
    let both = format!("{},{}", part.refdes, mcu.refdes);
    invoke(&mut engine, "elec.place", tool_args([("parts", &both), ("pin", &part.path)]));
    assert!(placed(&engine, &part.path).at == mcu.at && clear(&engine), "the pinned resistor settles before the larger MCU");
    mailbox.push(Cmd::Undo);
    assert!(until_answer(&mut engine, &mailbox).rejected.is_empty(), "the pin-prioritised move undoes");
    let resistor = placed(&engine, &part.path);
    mailbox.push(commit(vec![DesignOp::Board { unit: overlap.unit, op: BoardOp::SetField {
        item: resistor.id, field: fab_elec_board::Field::Locked(true),
    } }], "Lock the overlapping resistor"));
    assert!(until_answer(&mut engine, &mailbox).rejected.is_empty(), "the lock commits");
    invoke(&mut engine, "elec.place", tool_args([("parts", &both), ("pin", &mcu.path), ("mode", "@legalise")]));
    assert_eq!(placed(&engine, &part.path).at, resistor.at, "a named locked resistor stays fixed even against a pinned MCU");
    assert!(placed(&engine, &mcu.path).at != mcu.at && clear(&engine), "the named MCU clears the lock");
}

#[test]
fn design_read_lists_the_boot_resistor_and_filters_both_directions_of_its_net() {
    use fab_elec_host::edit_actions::{design_read, register_actions};
    use makepad_widgets_core::makepad_script::syntax::{parse_value, Node};

    fn rows<'a>(object: &'a Node, name: &str) -> &'a [Node] {
        object.field(name).and_then(|f| f.value.items()).unwrap_or_else(|| panic!("{name} must be an array"))
    }
    fn text<'a>(object: &'a Node, name: &str) -> &'a str {
        object.field(name).and_then(|f| f.value.string()).unwrap_or_else(|| panic!("{name} must be text"))
    }

    let design = fab_elec_fixtures::ledctl_pair();
    register_actions();
    let read = |args: &[(String, String)]| {
        let reply = design_read(&design, args).expect("the design query resolves");
        parse_value(&reply).expect("the design reply parses as one Splash object")
    };
    let unit = &design.units[0];
    let boot = unit.circuit.comp("mcu.boot_r").expect("the fixture has the boot resistor");
    let mcu = unit.circuit.comp("mcu.u").expect("the fixture has the MCU");
    let (pin, net) = boot.conns.iter().find(|(_, n)| unit.circuit.nets[n.ix()].name != "GND").expect("the resistor has a boot net");
    let net = &unit.circuit.nets[net.ix()].name;
    let boot_pin = format!("{}.{}", boot.refdes, boot.part.pins[pin.ix()].name);
    let mcu_pin = format!("{}.BOOT0", mcu.refdes);
    let all = read(&[]);
    let listed = &rows(&all, "units")[0];
    assert_eq!(text(listed, "unit"), unit.name, "the full read names the controller unit");
    assert_eq!(listed.field("board").and_then(|f| f.value.ident()), Some("true"), "the controller reports its board");
    assert_eq!(rows(listed, "parts").len(), unit.circuit.comps.len(), "the full read includes every controller part");
    let resistor = rows(listed, "parts").iter().find(|p| text(p, "path") == boot.path).expect("the full read lists the resistor path");
    assert_eq!((text(resistor, "ref"), text(resistor, "kind"), text(resistor, "value")), (boot.refdes.as_str(), "resistor", "10k"),
        "the resistor has its reference, canonical kind and displayed value");
    assert_eq!(resistor.field("placed").and_then(|f| f.value.ident()), Some("true"), "the boot resistor reports its placement");

    let filtered = read(&tool_args([("only", &boot.path)]));
    let listed = &rows(&filtered, "units")[0];
    assert_eq!(rows(listed, "parts").iter().map(|p| text(p, "ref")).collect::<Vec<_>>(), [&boot.refdes],
        "only a part shows that part alone");
    let nets = rows(listed, "nets");
    assert_eq!(nets.len(), 2, "the part filter retains both boot resistor nets");
    assert!(nets.iter().any(|n| text(n, "net") == "GND"), "the part filter includes the ground connection");
    let connected = nets.iter().find(|n| text(n, "net") == net).expect("the part filter includes the boot signal");
    let pins: Vec<_> = rows(connected, "pins").iter().map(|p| p.string().expect("a pin is ref.pin text")).collect();
    assert!(pins.len() == 2 && pins.contains(&boot_pin.as_str()) && pins.contains(&mcu_pin.as_str()),
        "the boot net keeps both the resistor and MCU pin even in a part-only read");

    let filtered = read(&tool_args([("unit", &unit.name), ("only", net)]));
    let listed = &rows(&filtered, "units")[0];
    assert_eq!(rows(listed, "nets").iter().map(|n| text(n, "net")).collect::<Vec<_>>(), [net], "only a net shows that net alone");
    let parts: Vec<_> = rows(listed, "parts").iter().map(|p| text(p, "ref")).collect();
    assert!(parts.len() == 2 && parts.contains(&boot.refdes.as_str()) && parts.contains(&mcu.refdes.as_str()),
        "only a net includes exactly the parts on its pins");

    let filtered = read(&tool_args([("only", "r")]));
    let listed = rows(&filtered, "units");
    assert_eq!(listed.len(), 2, "the resistor prefix matches the controller and probe without a unit qualifier");
    for (selected, full) in listed.iter().zip(rows(&all, "units")) {
        assert_eq!(text(selected, "unit"), text(full, "unit"), "the prefix preserves unit order");
        let resistors: Vec<_> = rows(full, "parts").iter().filter(|p| text(p, "kind") == "resistor").map(|p| text(p, "ref")).collect();
        assert_eq!(rows(selected, "parts").iter().map(|p| text(p, "ref")).collect::<Vec<_>>(), resistors,
            "the lowercase prefix selects every resistor in this unit");
        let nets: Vec<_> = rows(full, "nets").iter().filter(|n| rows(n, "pins").iter().any(|pin| {
            pin.string().and_then(|pin| pin.split_once('.')).is_some_and(|(part, _)| resistors.contains(&part))
        })).map(|n| text(n, "net")).collect();
        assert_eq!(rows(selected, "nets").iter().map(|n| text(n, "net")).collect::<Vec<_>>(), nets,
            "the prefix keeps exactly the nets that touch a selected resistor in this unit");
    }
    let refusal = design_read(&design, &tool_args([("only", "ZZ9")])).expect_err("the filter matches no part, prefix or net").message;
    assert!(refusal.contains("no part, prefix or net ZZ9") && refusal.contains("D1–D8") && refusal.contains("SW1")
        && refusal.lines().count() == 1 && refusal.chars().count() <= 200, "the refusal names reference ranges in one short line: {refusal}");
}

/// `elec.part.body.set` (EKMODELS), the part card's body switch as an action: a part with an LCSC
/// number switches to its EasyEDA model in one undo step that adds the unit's models document with
/// the part's `e.body`; going back to its plain body removes the override; a source the part lacks,
/// or EasyEDA while it is off, is refused with why; undo takes the document back.
#[test]
fn body_set_switches_a_parts_body_as_its_card_does_in_one_undo_step() {
    use fab_elec_host::model_actions::body_set;
    use fab_elec_parts::model_board::{self, SOURCE_DOWNLOAD, SOURCE_EASYEDA, SOURCE_LOCAL};

    let fixture = fab_elec_fixtures::docs("ledctl").expect("the fixture supplies controller documents");
    let mut sources = fab_elec_lang::Sources::new();
    for doc in fixture.docs() {
        let text = doc.text.replace(r#"let boot_r = e.r(10kohm, u.BOOT0, p.gnd, {package: "0402"})"#,
            r#"let boot_r = e.r(10kohm, u.BOOT0, p.gnd, {package: "0402" fields: {LCSC: "C25744"}})"#);
        sources.add(doc.name.clone(), text).expect("a fixture document");
    }
    let evaluated = evaluate(&sources, &fab_elec_lang::std_libs()).expect("the controller evaluates");
    let (design, docs) = Docs::from_evaluated(sources, evaluated);
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "body".into())
        .with_docs(docs).with_app_tools();
    let mailbox = Mailbox::default();
    let tool = "elec.part.body.set";
    let args = |source: &str| tool_args([("part", "mcu.boot_r"), ("source", source)]);
    let plan = |engine: &Engine, source: &str| body_set(engine.design(), engine.docs(), tool, &args(source));
    let models = fab_elec_lang::models_doc_name(&engine.design().units[0].name);
    let models_text = |engine: &Engine| {
        let sources = engine.docs().expect("the engine retains documents").sources();
        sources.find(&models).and_then(|id| sources.doc(id)).filter(|doc| doc.present).map(|doc| doc.text.to_string())
    };
    let boot = |engine: &Engine| engine.design().units[0].circuit.comp("mcu.boot_r").expect("the boot resistor").part.body.clone().expect("a body");

    let original = boot(&engine);
    let part = engine.design().units[0].circuit.comp("mcu.boot_r").expect("the boot resistor").part.name.clone();
    model_board::set_sources(SOURCE_LOCAL | SOURCE_DOWNLOAD);
    for (source, why) in [("easyeda", "EasyEDA off"), ("kicad", "no KiCad model")] {
        let refused = plan(&engine, source).expect_err("the switch is refused");
        assert!(refused.message.contains(why), "{source} says why in a word: {}", refused.message);
    }
    model_board::set_sources(SOURCE_LOCAL | SOURCE_DOWNLOAD | SOURCE_EASYEDA);
    let Plan::Edit(edit) = plan(&engine, "easyeda").expect("an LCSC number, and EasyEDA on") else { panic!("a design edit") };
    assert!(matches!(edit.ops.as_slice(), [DesignOp::File(FileOp::AddDoc { name, .. }), DesignOp::Circuit { op: CircuitOp::SetBody { part: named, body: Some(body) }, .. }]
        if *name == models && *named == part && body == r#"{mesh: "parts/lcsc/C25744.glb"}"#), "the models document comes first: {:?}", edit.ops);
    let by = By::Ai { turn: 0, call: 1 };
    let invoke = |source: &str, ticket| Cmd::Invoke { tool: tool.into(), args: args(source), preview: false, by, ticket: Ticket(ticket) };
    mailbox.push(invoke("easyeda", 1));
    let done = until_answer(&mut engine, &mailbox);
    assert!(matches!(done.outcomes.as_slice(), [(Ticket(1), Outcome::Done { label, .. })] if *label == edit.label), "the AI's switch is done under the card's label");
    let line = format!("e.body(\"{part}\", {{mesh: \"parts/lcsc/C25744.glb\"}})\n");
    assert!(models_text(&engine).is_some_and(|text| text.ends_with(&line)), "the models document holds the override: {:?}", models_text(&engine));
    assert_eq!(boot(&engine).missing_mesh.as_deref(), Some("parts/lcsc/C25744.glb"), "the part names the EasyEDA model and waits for it");
    assert_eq!(engine.log().entries.len(), 1, "the switch and its new document are one undo step");
    let Plan::Edit(back) = plan(&engine, "plain").expect("the plain body is offered") else { panic!("a design edit") };
    assert!(matches!(back.ops.as_slice(), [DesignOp::Circuit { op: CircuitOp::SetBody { body: None, .. }, .. }]),
        "plain is the resistor's own generated body: the override goes: {:?}", back.ops);
    mailbox.push(invoke("plain", 2));
    let done = until_answer(&mut engine, &mailbox);
    assert!(matches!(done.outcomes.as_slice(), [(Ticket(2), Outcome::Done { .. })]), "the switch back is done");
    assert_eq!(boot(&engine), original, "the part draws its own body again");
    assert!(models_text(&engine).is_some_and(|text| !text.contains("e.body(")), "the override was removed: {:?}", models_text(&engine));
    let refused = plan(&engine, "plain").expect_err("plain is drawn already");
    assert!(refused.message.contains("plain body already"), "{}", refused.message);
    model_board::set_sources(SOURCE_LOCAL | SOURCE_DOWNLOAD);
    for _ in 0..2 {
        mailbox.push(Cmd::Undo);
        let undone = step(&mut engine, &mailbox);
        run(&mut engine, &mailbox, undone);
    }
    assert_eq!((models_text(&engine), boot(&engine)), (None, original), "two undos take the document and the overrides back");
}
