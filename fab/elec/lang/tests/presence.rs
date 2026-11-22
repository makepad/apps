//! Whole-document removal leaves ids reserved and contributes no evaluation input.

use fab_elec_core::{BoardId, CompRef, DocId, NetRef, Sel, Severity};
use fab_elec_design::TextEdit;
use fab_elec_lang::{NO_CANCEL, evaluate, evaluate_from, std_libs, BenchScope, Evaluated, Sources, SourceRange, SpanEdit, UnitState};

const CIRCUIT: &str = r#"let e = mod.elec
let signal = e.net({name: "SIGNAL"})
let c1 = e.part("example:C1", {pins: {B: signal C: signal E: signal}})
"#;

const PART: &str = r#"let e = mod.elec
let f = e.f
e.define("example:C1", {
    kind: @transistor prefix: "Q"
    symbol: f.ic_symbol("C1", {left: [["B", @passive], ["C", @passive], ["E", @passive]]})
    footprint: f.footprint({
        pads: [
            f.smd("1", [-1mm, 0mm], [0.6mm, 1mm]),
            f.smd("2", [1mm, 0mm], [0.6mm, 1mm]),
            f.smd("3", [0mm, 1mm], [0.5mm, 0.5mm])
        ]
        body: [[-1mm, -1mm], [1mm, 1mm]] mount: @smd text: 0.6mm graphics: []
    })
    pads: {B: "1" C: "2" E: "3"}
})
"#;

#[test]
fn absent_documents_contribute_nothing_and_presence_round_trips() {
    let libs = std_libs();
    let mut sources = Sources::new();
    let a = sources.add("a.ckt.splash", CIRCUIT).expect("add circuit a");
    let board = sources.add("a.pcb.splash", "board(\"a\")\nhole(\"mount\", [0mm, 0mm], 3mm)\n")
        .expect("add a's board");
    let b = sources.add("b.ckt.splash", CIRCUIT).expect("add circuit b");
    let part = sources.add("parts/example/C1.part.splash", PART).expect("add the shared part");
    let bench = sources.add("checks.bench.splash", "bench(\"check_b\", {circuit: \"b\"}, fn(b, c) {})\n")
        .expect("add a bench naming b");
    let system = sources.ensure_system().expect("add the implicit system document");
    let baseline_sources = sources.clone();
    let baseline = evaluate(&sources, &libs).expect("the complete project evaluates");
    assert!(sources.all_docs().iter().all(|d| d.present), "added and implicit documents start present");
    assert_eq!(baseline.units.iter().map(|u| (u.name.as_str(), u.id)).collect::<Vec<_>>(),
        [("a", BoardId(0)), ("b", BoardId(1))], "initial unit ids follow circuit document order");
    assert_eq!(baseline.unit_ids, [(a, BoardId(0)), (b, BoardId(1))], "the initial ledger holds both circuit ids");
    assert!(baseline.units.iter().all(|u| u.state == UnitState::Built), "both initial units build");
    assert!(baseline.notes.is_empty(), "the complete project has no unowned errors or warnings");
    assert_eq!(baseline.benches.len(), 1, "the present bench contributes one result");
    assert_eq!(baseline.benches[0].state, UnitState::Built, "the bench naming b builds");
    assert_eq!(baseline.benches[0].scope, BenchScope::Design(BoardId(1)), "the bench binds to b's unit id");

    let remove = |sources: &mut Sources, previous: &mut Evaluated, id: DocId| {
        let text = sources.doc(id).expect("the document being removed is present").text.clone();
        sources.set_present(id, false).expect("a project document can become absent");
        assert_eq!(sources.all_docs()[id.ix()].text, text, "presence alone leaves document {}'s text untouched", id.0);
        let edits = vec![TextEdit { doc: id, span: 0..text.len() as u32, text: String::new() }];
        sources.apply(&edits).expect("apply can clear an absent document");
        previous.map.follow(&SpanEdit { edits, ..SpanEdit::default() });
    };
    let mut previous = baseline.clone();
    remove(&mut sources, &mut previous, a);
    let mut removed = evaluate_from(Some(&previous), &sources, &libs, &NO_CANCEL).expect("b survives removing a");
    assert_eq!(removed.units.iter().map(|u| (u.name.as_str(), u.id)).collect::<Vec<_>>(),
        [("b", BoardId(1))], "removing a leaves b's evaluation id unchanged");
    assert_eq!(removed.design.units.iter().map(|u| u.id).collect::<Vec<_>>(),
        [BoardId(1)], "the absent circuit contributes no design unit");
    assert_eq!(removed.unit_ids, baseline.unit_ids, "the ledger retains the absent circuit's id");
    for id in [a, board] {
        assert!(removed.map.of_doc(id).is_empty(), "a's document {} contributes no map entries without its circuit", id.0);
    }
    assert_eq!(sources.docs().iter().map(|d| d.id).collect::<Vec<_>>(),
        [board, b, part, bench, system], "docs skips the absent circuit and preserves id order");
    assert_eq!(sources.find("a.ckt.splash"), None, "find skips an absent document");
    assert_eq!(sources.doc(a), None, "doc skips an absent id");
    assert_eq!(sources.line_col(a, 0), None, "line_col skips an absent id");
    assert_eq!(sources.describe(&libs, &SourceRange { doc: a, span: 0..0 }), format!("document {}", a.0),
        "describe does not resolve an absent document's name");
    assert_eq!(sources.all_docs().len(), baseline_sources.all_docs().len(), "removal retains every allocated document");
    assert!(!sources.all_docs()[a.ix()].present, "all_docs exposes the removed circuit as absent");
    assert_eq!(sources.all_docs()[a.ix()].text.as_ref(), "", "the host's text edit leaves the absent document empty");
    assert_eq!(removed.notes.len(), 1, "the orphan board contributes exactly one note");
    assert_eq!(removed.notes[0].severity, Severity::Error, "the orphan board note is an error");
    assert_eq!(removed.notes[0].message, "a.pcb.splash has no circuit document a.ckt.splash", "the orphan error names its missing circuit");
    assert_eq!(removed.notes[0].sel, [Sel::Source { doc: board, start: 0, end: 0 }], "the orphan error is located at the board's start");
    assert_eq!(removed.benches[0].scope, BenchScope::Design(BoardId(1)), "removing a preserves the bench's binding to b");
    assert!(removed.units.iter().all(|u| !u.docs.contains(&a) && !u.docs.contains(&board)),
        "a's documents contribute no remaining unit reuse inputs");

    remove(&mut sources, &mut removed, board);
    let mut no_board = evaluate_from(Some(&removed), &sources, &libs, &NO_CANCEL).expect("b evaluates with both a documents absent");
    assert!(no_board.notes.is_empty(), "an absent board contributes no orphan error");
    remove(&mut sources, &mut no_board, part);
    let mut no_part = evaluate_from(Some(&no_board), &sources, &libs, &NO_CANCEL).expect("b keeps its last good lowering when its part is absent");
    assert!(matches!(no_part.units[0].state, UnitState::Kept(_)), "removing the used part must fail b's rebuild");
    let error = no_part.units[0].state.diags().iter().find(|d| d.message.contains("no part named example:C1"))
        .expect("the absent part produces a no-part-named error");
    assert_eq!(error.severity, Severity::Error, "the missing part is an error");
    let call = "e.part(\"example:C1\", {pins: {B: signal C: signal E: signal}})";
    let start = CIRCUIT.find(call).expect("the fixture contains the part use") as u32;
    assert_eq!(error.sel, [Sel::Source { doc: b, start, end: start + call.len() as u32 }],
        "the absent part's error points to the use in b");
    assert_eq!(no_part.units[0].docs, [b], "absent parts contribute no reuse document or text");
    assert_eq!(no_part.units[0].texts, [sources.doc(b).expect("b remains present").text.clone()],
        "the remaining reuse key contains only b's text");
    assert!(no_part.map.of_doc(part).is_empty(), "a kept unit retains no map entries in the absent part");
    assert_eq!(no_part.unit_ids, baseline.unit_ids, "a failed rebuild keeps the complete unit ledger");

    for id in [bench, system] {
        remove(&mut sources, &mut no_part, id);
    }
    let mut no_aux = evaluate_from(Some(&no_part), &sources, &libs, &NO_CANCEL).expect("absent bench and system documents do not fail a kept unit");
    assert!(no_aux.benches.is_empty(), "an absent bench contributes no result");
    assert_eq!(no_aux.system.doc, None, "an absent system document contributes no system input");
    assert!(no_aux.system.diags.is_empty(), "an absent system document contributes no diagnostics");
    for id in [a, board, part, bench, system] {
        assert!(no_aux.map.of_doc(id).is_empty(), "absent document {} contributes no map entries", id.0);
        let edits = vec![TextEdit { doc: id, span: 0..0, text: baseline_sources.all_docs()[id.ix()].text.to_string() }];
        sources.apply(&edits).expect("apply restores an absent document's text before its presence");
        no_aux.map.follow(&SpanEdit { edits, ..SpanEdit::default() });
        sources.set_present(id, true).expect("each original document can be restored");
    }
    assert_eq!(sources, baseline_sources, "restoring text then presence recovers every original byte and document");
    let mut restored = evaluate_from(Some(&no_aux), &sources, &libs, &NO_CANCEL).expect("the restored project evaluates");
    assert_eq!(restored.unit_ids, baseline.unit_ids, "restoration recovers the original unit ledger");
    assert_eq!(restored.units.iter().map(|u| u.id).collect::<Vec<_>>(), [BoardId(0), BoardId(1)], "restoration recovers both unit ids");
    assert_eq!(restored.design, baseline.design, "restoration recovers the original design");
    for unit in &baseline.design.units {
        let selections = unit.circuit.comps.iter().map(|c| Sel::Comp(CompRef { board: unit.id, comp: c.id }))
            .chain(unit.circuit.nets.iter().map(|n| Sel::Net(NetRef { board: unit.id, net: n.id })));
        for sel in selections {
            let at = baseline.map.locate(&sel).expect("each baseline component and net has a source location");
            assert_eq!(restored.map.locate(&sel), Some(at), "restoration recovers the location of {sel:?}");
        }
    }

    remove(&mut sources, &mut restored, a);
    let removed_again = evaluate_from(Some(&restored), &sources, &libs, &NO_CANCEL).expect("removing a again retains b");
    let next = DocId(sources.all_docs().len() as u32);
    let replacement = sources.add("a.ckt.splash", CIRCUIT).expect("an absent document does not reserve its name");
    assert_eq!(replacement, next, "a replacement document takes a new id after every retained document");
    assert_eq!(sources.find("a.ckt.splash"), Some(replacement), "find resolves the present replacement");
    let replaced = evaluate_from(Some(&removed_again), &sources, &libs, &NO_CANCEL).expect("the replacement circuit evaluates");
    assert_eq!(replaced.units.iter().map(|u| (u.name.as_str(), u.id)).collect::<Vec<_>>(),
        [("b", BoardId(1)), ("a", BoardId(2))], "the replacement's unit never takes the removed unit's id");
    assert_eq!(replaced.unit_ids, [(a, BoardId(0)), (b, BoardId(1)), (replacement, BoardId(2))],
        "the ledger appends the replacement after the original assignments");
    assert!(replaced.notes.is_empty(), "the replacement circuit owns the present board");
    let before_refusal = sources.clone();
    let error = sources.set_present(a, true).expect_err("a restored document cannot collide with a present name");
    assert_eq!(error.severity, Severity::Error, "a restoration name collision is an error");
    assert_eq!(error.message, "a document named a.ckt.splash is in the project already", "the restoration refusal names the collision");
    assert_eq!(error.sel, [Sel::Source { doc: a, start: 0, end: 0 }], "the restoration refusal is located in the old document");
    assert_eq!(sources, before_refusal, "a refused restoration changes neither presence nor text");
    for id in [DocId(sources.all_docs().len() as u32), libs.docs()[0].id] {
        for present in [false, true] {
            let error = sources.set_present(id, present).expect_err("unknown and library ids cannot change project presence");
            assert_eq!(error.severity, Severity::Error, "an invalid presence target is an error");
            assert!(error.sel.is_empty(), "an invalid presence target has no source selection");
        }
    }
    assert_eq!(sources, before_refusal, "invalid ids leave the sources unchanged");
    sources.set_present(system, false).expect("the old implicit system can become absent");
    let next = DocId(sources.all_docs().len() as u32);
    assert_eq!(sources.ensure_system().expect("a missing system is ensured"), next, "ensure_system allocates instead of reusing an absent system");
    assert!(!sources.all_docs()[system.ix()].present, "ensuring a system leaves the absent system absent");
    assert_eq!(sources.all_docs()[system.ix()].name, baseline_sources.all_docs()[system.ix()].name,
        "ensuring a system never renames an absent document");
    assert!(sources.doc(next).expect("the new implicit system is present").text.is_empty(), "the new implicit system starts empty");
}
