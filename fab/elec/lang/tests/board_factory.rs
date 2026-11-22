use fab_elec_board::{BoardOp, Capabilities, Factories};
use fab_elec_core::Sel;
use fab_elec_design::{engines, DesignOp, SysOp, TextEdit};
use fab_elec_lang::{NO_CANCEL, board_doc, evaluate, evaluate_from, splice, splice_verified, std_libs, Sources, SpanEdit};
use std::sync::Arc;

#[test]
fn factory_headers_splice_and_evaluate_with_the_board() {
    let original = include_str!("../../fixtures/ledctl.pcb.splash");
    let board_line = "board(\"ledctl\")\n";
    let factory_line = "factory(\"fab-a\")\n";
    let at = board_line.len() as u32;
    let libs = std_libs();
    let mut sources = Sources::new();
    sources.add("ledctl.ckt.splash", include_str!("../../fixtures/ledctl.ckt.splash")).expect("setup: circuit document is added");
    let no_board = evaluate(&sources, &libs).expect("setup: circuit without a board document evaluates");
    let unit_id = no_board.design.units[0].id;
    let factory = |profile: Option<&str>| DesignOp::Sys(SysOp::SetFactory { unit: unit_id, profile: profile.map(str::to_string) });
    assert_eq!(no_board.design.units[0].factory, None, "missing board: the unit has no factory");
    for profile in [Some("fab-a"), None] {
        let error = splice(&sources, &no_board.map, &no_board.design, &[factory(profile)]).expect_err("missing board: factory edits are refused");
        assert_eq!(error.message, "the board has no document", "missing board: factory edits use the existing board refusal");
        assert_eq!(error.sel, vec![Sel::Unit(unit_id)], "missing board: the refusal selects the unit");
    }
    let doc = sources.add("ledctl.pcb.splash", original).expect("setup: board document is added");
    let before = evaluate_from(Some(&no_board), &sources, &libs, &NO_CANCEL).expect("setup: ledctl evaluates with its board document");
    let unit = &before.design.units[0];
    let board = unit.board.as_ref().expect("setup: ledctl has a board");
    assert_eq!(unit.factory, None, "unset: the fixture has no factory");
    assert!(!original.contains("stackup("), "combined setup: the fixture has no stackup statement");
    let caps = Capabilities {
        min_track: 400_000, min_clearance: 0, min_drill: 0, min_annular: 0, layers: vec![2],
        via_in_pad: false,
        board_min: (0, 0), board_max: (0, 0), thickness: Vec::new(), thickness_tol_ppm: 0,
    };
    let factories: Factories = [("fab-a".to_string(), Arc::new(caps))].into_iter().collect();
    assert!(engines::effective(unit, board, &factories).is_none(), "unset: no factory means no effective profile rules");
    let unset = splice(&sources, &before.map, &before.design, &[factory(None)]).expect("unset: clearing an absent factory succeeds");
    assert_eq!(unset, SpanEdit::default(), "unset: clearing an absent factory is a no-op");

    let set = splice_verified(&sources, &libs, &before, &[factory(Some("fab-a"))], &NO_CANCEL).expect("set: factory edit applies and evaluates");
    assert_eq!(set.edit.edits, vec![TextEdit { doc, span: at..at, text: factory_line.into() }], "set: exactly one factory line is inserted after board");
    assert!(!set.edit.reevaluate, "set: factory edits need no host reevaluation");
    let set_text = &set.sources.doc(doc).expect("set: board document remains").text;
    assert_eq!(set_text.as_ref(), original.replacen(board_line, &format!("{board_line}{factory_line}"), 1), "set: every original byte is preserved");
    let set_unit = &set.evaluated.design.units[0];
    assert_eq!(set_unit.factory.as_deref(), Some("fab-a"), "set: evaluation stores the factory on its unit");
    assert_eq!(board_doc::write_unit(set_unit), set_text.as_ref(), "writer: a canonical factory document is a fixed point");
    let set_board = set_unit.board.as_ref().expect("set: evaluated unit keeps its board");
    assert_eq!(fab_elec_lang::write(set_board, &set_unit.circuit), original, "importer writer: factory is omitted");
    assert_eq!(board_doc::write_with(set_board, &set_unit.circuit, &|_| None), original, "custom code writer: factory is omitted");
    let effective = engines::effective(set_unit, set_board, &factories).expect("effective: the evaluated unit resolves its factory");
    assert_eq!(effective.origin, "fab-a", "effective: the rules name their factory profile");
    assert_eq!(effective.rules.base.track, Some(400_000), "effective: the factory raises the track minimum");
    assert_eq!(set_board.rules, board.rules, "effective: the board's authored rules are unchanged");
    let inverse = set.edit.inverse(&sources, &before.map);
    let mut restored = set.sources.clone();
    restored.apply(&inverse.edits).expect("inverse: the factory insertion can be undone");
    assert_eq!(restored, sources, "inverse: every document is restored byte for byte");
    let reused = evaluate_from(Some(&set.evaluated), &set.sources, &libs, &NO_CANCEL).expect("reuse: unchanged documents evaluate");
    assert!(reused.units[0].reused, "reuse: unchanged board text reuses its unit");
    assert_eq!(reused.design.units[0].factory.as_deref(), Some("fab-a"), "reuse: the factory stays with the board text");
    let same = splice(&set.sources, &set.evaluated.map, &set.evaluated.design, &[factory(Some("fab-a"))]).expect("same: setting the current factory succeeds");
    assert_eq!(same, SpanEdit::default(), "same: setting the current profile makes no edit");
    let last = splice(&set.sources, &set.evaluated.map, &set.evaluated.design, &[factory(Some("fab-b")), factory(Some("fab-a"))])
        .expect("commit order: multiple factory ops combine");
    assert_eq!(last, SpanEdit::default(), "commit order: the last factory op determines the profile");

    let change = splice_verified(&set.sources, &libs, &set.evaluated, &[factory(Some("fab-b"))], &NO_CANCEL).expect("change: factory edit applies and evaluates");
    let token = at + "factory(".len() as u32;
    assert_eq!(change.edit.edits, vec![TextEdit { doc, span: token..token + "\"fab-a\"".len() as u32, text: "\"fab-b\"".into() }], "change: only the complete string token is replaced");
    assert!(!change.edit.reevaluate, "change: token replacement needs no host reevaluation");
    assert_eq!(change.evaluated.design.units[0].factory.as_deref(), Some("fab-b"), "change: evaluation stores the replacement profile");
    let clear = splice_verified(&change.sources, &libs, &change.evaluated, &[factory(None)], &NO_CANCEL).expect("clear: factory deletion applies and evaluates");
    assert_eq!(clear.edit.edits, vec![TextEdit { doc, span: at..at + factory_line.len() as u32, text: String::new() }], "clear: exactly the factory line is deleted");
    assert!(!clear.edit.reevaluate, "clear: line deletion needs no host reevaluation");
    assert_eq!(clear.sources.doc(doc).expect("clear: board document remains").text.as_ref(), original, "clear: text equals the original fixture");
    assert_eq!(clear.evaluated.design.units[0].factory, None, "clear: evaluation clears the unit's profile");

    let mut stackup = board.stackup.clone();
    stackup.thickness += 200_000;
    let combined = splice_verified(&sources, &libs, &before, &[
        factory(Some("fab-a")),
        DesignOp::Board { unit: unit_id, op: BoardOp::SetStackup(stackup.clone()) },
    ], &NO_CANCEL).expect("combined: factory and stackup edits apply together and evaluate");
    assert_eq!(combined.edit.edits.len(), 1, "combined: headers sharing an offset form one insertion");
    assert_eq!(combined.edit.edits[0].span, at..at, "combined: both headers are inserted after board");
    assert!(combined.edit.edits[0].text.starts_with(&format!("{factory_line}stackup(")), "combined: factory precedes stackup canonically");
    assert!(!combined.edit.reevaluate, "combined: both headers need no host reevaluation");
    let combined_unit = &combined.evaluated.design.units[0];
    assert_eq!(combined_unit.factory.as_deref(), Some("fab-a"), "combined: evaluation stores the factory");
    assert_eq!(combined_unit.board.as_ref().expect("combined: board remains").stackup, stackup, "combined: evaluation stores the stackup");
    assert_eq!(board_doc::write_unit(combined_unit), combined.sources.doc(doc).expect("combined: document remains").text.as_ref(), "combined: both headers have canonical order and spelling");

    for (headers, statement, message) in [
        ("factory(1)\n", "factory(1)", "profile must be a string"),
        ("factory()\n", "factory()", "expected 1 positional arguments"),
        ("factory(\"fab-a\", \"fab-b\")\n", "factory(\"fab-a\", \"fab-b\")", "expected 1 positional arguments"),
        ("factory(\"fab-a\")\nfactory(\"fab-b\")\n", "factory(\"fab-b\")", "duplicate factory statement"),
    ] {
        let document = original.replacen(board_line, &format!("{board_line}{headers}"), 1);
        let errors = board_doc::lower(doc, &document, &unit.circuit, None).expect_err("lowering: invalid factory headers are rejected");
        assert_eq!(errors.len(), 1, "lowering: {statement} has one diagnostic");
        assert!(errors[0].message.contains(message), "lowering: {statement} reports its failed property");
        let start = at + headers.rfind(statement).expect("lowering: the bad statement is present") as u32;
        assert_eq!(errors[0].sel, vec![Sel::Source { doc, start, end: start + statement.len() as u32 }], "lowering: {statement} selects its complete statement");
    }
    for document in [format!("{factory_line}{original}"), format!("{original}{factory_line}")] {
        let lowered = board_doc::lower(doc, &document, &unit.circuit, None).expect("ordering: factory follows existing permissive header lowering");
        assert_eq!(lowered.factory.as_deref(), Some("fab-a"), "ordering: canonical header order is enforced only by writing");
    }
    sources.ensure_system().expect("mount setup: the project has an empty system document");
    let other = splice(&sources, &before.map, &before.design, &[DesignOp::Sys(SysOp::ClearMount(unit_id))]).expect("mount ops: clearing a default mount succeeds");
    assert_eq!(other, SpanEdit::default(), "mount ops: clearing a default mount leaves the factory and system documents unchanged");
}
