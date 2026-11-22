//! Unchanged unit inputs reuse their lowering, sites, diagnostics and warnings.

use fab_elec_design::TextEdit;
use fab_elec_lang::{NO_CANCEL, DocId, DocKind, Evaluated, Libs, Sources, UnitState, evaluate, evaluate_from, std_libs};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CIRCUIT: &str = include_str!("../../fixtures/ledctl.ckt.splash");
const BENCH: &str = include_str!("../../fixtures/ledctl.bench.splash");
const SMALL: &str = concat!(
    "use mod.elec.*\nlet e = mod.elec\n",
    "let a = e.net({name: \"A\"})\nlet r = e.r(1kohm, a, a)\n",
    "sheet({\"missing\": {}})\n",
);

fn edit(sources: &mut Sources, doc: DocId, from: &str, to: &str) {
    let text = &sources.doc(doc).expect("the edited document exists").text;
    let start = text.find(from).expect("the edited text exists") as u32;
    sources.apply(&[TextEdit { doc, span: start..start + from.len() as u32, text: to.into() }])
        .expect("the single text edit applies");
}

fn same_result(actual: &Evaluated, expected: &Evaluated, why: &str) {
    let normalize = |ev: &Evaluated| {
        let mut ev = ev.clone();
        for unit in &mut ev.units {
            unit.took = Duration::ZERO;
            unit.reused = false;
            unit.phases = None;
        }
        ev
    };
    assert_eq!(normalize(actual), normalize(expected), "{why}: every result field except took/reused/phases must match");
}

fn reused(prev: &Evaluated, next: &Evaluated, index: usize, why: &str) {
    let (before, after) = (&prev.units[index], &next.units[index]);
    assert!(after.reused, "{why}: the unit must be reused");
    assert_eq!(after.state, before.state, "{why}: reuse must retain the state and diagnostics");
    assert_eq!(after.phases, before.phases, "{why}: reuse must retain the previous build phases");
    assert_eq!(after.docs, before.docs, "{why}: reuse must retain document ids");
    assert_eq!(after.texts.len(), after.docs.len(), "{why}: every document must have a cached text");
    for (a, b) in after.texts.iter().zip(&before.texts) {
        assert!(Arc::ptr_eq(a, b), "{why}: reuse must retain the cached text allocations");
    }
    for doc in &before.docs {
        assert_eq!(next.map.of_doc(*doc), prev.map.of_doc(*doc), "{why}: reuse must retain document map entries");
    }
    assert_eq!(next.map.value_sites_of(after.id), prev.map.value_sites_of(before.id), "{why}: reuse must retain value sites");
    assert_eq!(next.map.pin_sites_of(after.id), prev.map.pin_sites_of(before.id), "{why}: reuse must retain pin sites");
    let before = prev.design.units.iter().find(|u| u.id == before.id);
    let after = next.design.units.iter().find(|u| u.id == after.id);
    match (before, after) {
        (Some(a), Some(b)) => {
            assert!(Arc::ptr_eq(&a.circuit, &b.circuit), "{why}: reuse must share the circuit allocation");
            assert!(Arc::ptr_eq(a.board.as_ref().expect("previous board"), b.board.as_ref().expect("reused board")),
                "{why}: reuse must share the board allocation");
        }
        (None, None) => assert!(matches!(next.units[index].state, UnitState::Failed(_)), "{why}: only failed units have no lowering"),
        _ => panic!("{why}: reuse changed whether a lowering exists"),
    }
}

#[test]
fn unchanged_units_reuse_their_last_evaluation() {
    let libs = std_libs();
    let mut sources = Sources::new();
    let ledctl = sources.add("ledctl.ckt.splash", CIRCUIT).expect("fixture circuit name");
    let bench = sources.add("ledctl.bench.splash", BENCH).expect("fixture bench name");
    let small = sources.add("small.ckt.splash", SMALL).expect("second circuit name");
    let board = sources.add("ledctl.pcb.splash", "board(\"ledctl\")\nhole(\"mount\", [0mm, 0mm], 3mm)\n")
        .expect("fixture board name");
    let start = Instant::now();
    let ev = evaluate(&sources, &libs).expect("both circuits build");
    let scratch_took = start.elapsed();
    assert_eq!(ev.units.len(), 2, "the setup must evaluate two units");
    assert!(ev.units.iter().all(|u| u.state == UnitState::Built && !u.reused), "from-scratch units must be newly built");
    assert_eq!(ev.notes.len(), 1, "the missing sheet path must warn, to exercise warning reuse");

    let same_sources = sources.clone();
    let same_libs = libs.clone();
    assert!(libs.same(&same_libs), "cloning libraries must preserve their identity");
    let start = Instant::now();
    let all = evaluate_from(Some(&ev), &same_sources, &same_libs, &NO_CANCEL).expect("unchanged inputs evaluate");
    let reused_took = start.elapsed();
    for i in 0..2 {
        reused(&ev, &all, i, "unchanged inputs");
    }
    same_result(&all, &ev, "all-reused evaluation versus the from-scratch evaluation of the same sources");

    edit(&mut sources, bench, "3.4V", "3.5V");
    for doc in [ledctl, small, board] {
        assert!(Arc::ptr_eq(&sources.doc(doc).unwrap().text, &same_sources.doc(doc).unwrap().text),
            "a bench edit must preserve every untouched document's text allocation");
    }
    assert!(!Arc::ptr_eq(&sources.doc(bench).unwrap().text, &same_sources.doc(bench).unwrap().text),
        "a bench edit must replace the edited document's text allocation");
    let start = Instant::now();
    let bench_edit = evaluate_from(Some(&all), &sources, &libs, &NO_CANCEL).expect("the bench-only edit evaluates");
    let bench_took = start.elapsed();
    eprintln!("evaluate reuse: from scratch {scratch_took:?}; all reused {reused_took:?}; bench-only edit {bench_took:?}");
    for i in 0..2 {
        reused(&all, &bench_edit, i, "bench-only edit");
    }
    assert_eq!(bench_edit.benches[0].state, UnitState::Built, "the edited bench must build");
    let bound = bench_edit.benches[0].expects.iter().find(|e| e.name == "3V3 up").expect("the rail expectation remains");
    assert_eq!(bound.range().map(|(_, max)| max), Some(3.5), "the bench must evaluate its new expectation bound");

    edit(&mut sources, small, "1kohm", "2kohm");
    let changed = evaluate_from(Some(&bench_edit), &sources, &libs, &NO_CANCEL).expect("the circuit value edit evaluates");
    reused(&bench_edit, &changed, 0, "editing only the second circuit");
    assert!(!changed.units[1].reused, "the changed circuit must be rebuilt");
    assert_eq!(changed.units[1].state, UnitState::Built, "the changed circuit must build successfully");
    assert!(!Arc::ptr_eq(&changed.design.units[1].circuit, &bench_edit.design.units[1].circuit),
        "the changed circuit must have a new allocation");
    assert_eq!(changed.design.units[1].circuit.comps[0].value.as_ref().and_then(|v| v.qty).map(|q| q.si), Some(2000.0),
        "the changed resistor must evaluate to 2 kohm");
    let fresh = evaluate(&sources, &libs).expect("the edited sources build from scratch");
    same_result(&changed, &fresh, "selective rebuild versus from scratch");

    let good_sources = sources.clone();
    edit(&mut sources, small, "e.r(2kohm, a, a)", "e.r(");
    let kept = evaluate_from(Some(&changed), &sources, &libs, &NO_CANCEL).expect("a broken circuit keeps its lowering");
    assert!(matches!(&kept.units[1].state, UnitState::Kept(d) if !d.is_empty()), "the broken circuit must be kept with diagnostics");
    assert!(!kept.units[1].reused, "the first broken evaluation must attempt a build");
    assert!(Arc::ptr_eq(&kept.design.units[1].circuit, &changed.design.units[1].circuit), "the broken circuit must retain its good lowering");
    assert!(Arc::ptr_eq(&kept.units[1].texts[0], &sources.doc(small).unwrap().text), "a kept unit must cache the broken text it attempted");
    let kept_again = evaluate_from(Some(&kept), &sources, &libs, &NO_CANCEL).expect("the same broken text reuses the kept unit");
    for i in 0..2 {
        reused(&kept, &kept_again, i, "unchanged broken text");
    }
    same_result(&kept_again, &kept, "reusing a kept unit");

    let failed = evaluate(&sources, &libs).expect("the fixture builds beside a failed second circuit");
    assert!(matches!(&failed.units[1].state, UnitState::Failed(d) if !d.is_empty()), "a circuit without a previous lowering must fail");
    let failed_again = evaluate_from(Some(&failed), &sources, &libs, &NO_CANCEL).expect("an unchanged failed unit is reused");
    reused(&failed, &failed_again, 1, "unchanged failed text");
    same_result(&failed_again, &failed, "reusing a failed unit");

    let mut reallocated = Libs::new();
    for (i, doc) in libs.docs().iter().enumerate() {
        let text = if i == 0 { Arc::<str>::from(doc.text.as_ref()) } else { doc.text.clone() };
        reallocated.add(doc.lib.as_deref().expect("a library name"), &doc.name, text).expect("the same library document name");
    }
    assert_eq!(reallocated, libs, "reallocated libraries must retain identical content and names");
    assert!(!libs.same(&reallocated), "one reallocated library text must invalidate library identity");
    let rebuilt = evaluate_from(Some(&changed), &good_sources, &reallocated, &NO_CANCEL).expect("the reallocated library still builds");
    for i in 0..2 {
        assert!(!rebuilt.units[i].reused, "reallocating one library text must rebuild every unit");
        assert!(!Arc::ptr_eq(&rebuilt.design.units[i].circuit, &changed.design.units[i].circuit), "library invalidation must allocate every circuit again");
    }
    same_result(&rebuilt, &fresh, "rebuilding after library reallocation");

    let mut board_sources = good_sources.clone();
    edit(&mut board_sources, board, "3mm", "4mm");
    let board_edit = evaluate_from(Some(&changed), &board_sources, &libs, &NO_CANCEL).expect("the board-only edit builds");
    assert!(!board_edit.units[0].reused, "a board text edit must invalidate its unit");
    assert!(!Arc::ptr_eq(board_edit.design.units[0].board.as_ref().unwrap(), changed.design.units[0].board.as_ref().unwrap()),
        "a board text edit must lower a new board");
    reused(&changed, &board_edit, 1, "editing only the other unit's board");
    let added_board = board_sources.add("small.pcb.splash", "board(\"small\")\n").expect("a new board document");
    let with_board = evaluate_from(Some(&board_edit), &board_sources, &libs, &NO_CANCEL).expect("adding a board builds");
    assert!(!with_board.units[1].reused, "adding a board must invalidate a unit that had no board document");
    assert_eq!(with_board.units[1].docs, [small, added_board], "the new board must join the cached unit documents");
    let without_board = evaluate_from(Some(&with_board), &good_sources, &libs, &NO_CANCEL).expect("removing board input builds");
    assert!(!without_board.units[1].reused, "removing a board must invalidate its unit");
    same_result(&without_board, &fresh, "removing a board returns to the from-scratch result");
}

#[test]
fn block_bench_identity_and_generation_follow_only_its_circuit() {
    use fab_elec_core::{BoardId, BENCH_SCOPE_BASE};
    use fab_elec_lang::{BenchOut, BenchScope};

    fn own(bench: &BenchOut) -> (&Arc<fab_elec_circuit::Circuit>, u64) {
        match &bench.scope {
            BenchScope::Own {
                circuit,
                generation,
            } => (circuit, *generation),
            BenchScope::Design(_) => {
                panic!("{}: a block bench must retain an Own scope", bench.name)
            }
        }
    }
    fn half(ev: &Evaluated) -> &BenchOut {
        ev.benches
            .iter()
            .find(|b| b.name == "divider.half")
            .expect("the original bench remains present")
    }
    fn unchanged(before: &BenchOut, after: &BenchOut, why: &str) {
        let (a, ga) = own(before);
        let (b, gb) = own(after);
        assert!(
            Arc::ptr_eq(a, b),
            "{why}: an unchanged circuit must retain its Arc"
        );
        assert_eq!(
            ga, gb,
            "{why}: an unchanged circuit must retain its generation"
        );
        assert_eq!(
            before.scope_id(),
            after.scope_id(),
            "{why}: the bench name must retain its scope id"
        );
    }

    let text = r#"
use mod.std.*
let e = mod.elec
bench("divider.half", {
    circuit: fn() {
        let gnd = e.net({name: "GND" kind: @ground})
        let top = e.net({name: "TOP"})
        let out = e.net({name: "OUT"})
        let div = divider({top: top out: out gnd: gnd ratio: 0.5 r_total: 100kohm})
    }
    run: {stop: 2ms}
}, fn(b, c) {
    b.vsrc(c.net("TOP"), c.net("GND"), {dc: 5V})
    b.expect("half", b.mean(b.v(c.net("OUT")), {window: 1ms after: 0.5ms}), {min: 2.4V max: 2.6V})
})
"#;
    let libs = std_libs();
    let mut sources = Sources::new();
    let doc = sources
        .add("divider.bench.splash", text)
        .expect("block bench document");
    let unit = sources
        .add(
            "unit.ckt.splash",
            "let e = mod.elec\nlet n = e.net({})\nlet r = e.r(1kohm, n, n)\n",
        )
        .expect("independent unit document");
    let first = evaluate(&sources, &libs).expect("the unit and block bench evaluate");
    assert_eq!(
        half(&first).state,
        UnitState::Built,
        "the initial block bench must build"
    );
    assert_eq!(
        half(&first).scope_id(),
        BoardId(BENCH_SCOPE_BASE),
        "the first block bench must take the first reserved id"
    );

    edit(&mut sources, doc, "2.6V", "2.7V");
    let stimulus =
        evaluate_from(Some(&first), &sources, &libs, &NO_CANCEL).expect("the stimulus edit evaluates");
    assert_eq!(
        half(&stimulus).state,
        UnitState::Built,
        "a stimulus edit must build"
    );
    unchanged(half(&first), half(&stimulus), "stimulus-only edit");
    assert_eq!(
        half(&stimulus).expects[0].range().map(|(_, max)| max),
        Some(2.7),
        "reusing the circuit must still evaluate the edited stimulus"
    );

    edit(&mut sources, doc, "ratio: 0.5", "ratio: 0.4");
    let changed =
        evaluate_from(Some(&stimulus), &sources, &libs, &NO_CANCEL).expect("the circuit edit evaluates");
    assert_eq!(
        half(&changed).state,
        UnitState::Built,
        "a circuit parameter edit must build"
    );
    let (old, old_generation) = own(half(&stimulus));
    let (new, new_generation) = own(half(&changed));
    assert!(
        !Arc::ptr_eq(old, new),
        "changing the divider ratio must allocate a new circuit"
    );
    assert!(
        new_generation > old_generation,
        "changing the circuit must advance its generation"
    );
    assert_eq!(
        half(&changed).scope_id(),
        half(&stimulus).scope_id(),
        "a circuit edit must keep the bench scope id"
    );

    let inserted = "bench(\"second\", {circuit: fn() {\nlet gnd = e.net({kind: @ground})\n}}, fn(b, c) {})\nbench(\"divider.half\",";
    edit(&mut sources, doc, "bench(\"divider.half\",", inserted);
    let added =
        evaluate_from(Some(&changed), &sources, &libs, &NO_CANCEL).expect("inserting a block bench evaluates");
    assert!(
        added.benches.iter().all(|b| b.state == UnitState::Built),
        "both block benches must build after insertion: {:?}",
        added.benches
    );
    unchanged(
        half(&changed),
        half(&added),
        "inserting a bench above the first",
    );
    assert_eq!(
        added.benches[0].name, "second",
        "the inserted bench must evaluate before the original"
    );
    assert_eq!(
        added.benches[0].scope_id(),
        BoardId(BENCH_SCOPE_BASE + 1),
        "a new name must take the next id even when encountered first"
    );
    assert!(
        own(&added.benches[0]).1 > new_generation,
        "the inserted circuit must get a generation distinct from every previous circuit"
    );

    edit(&mut sources, doc, "run: {stop: 2ms}", "run: {");
    let kept =
        evaluate_from(Some(&added), &sources, &libs, &NO_CANCEL).expect("syntax errors keep prior benches");
    assert!(
        matches!(&half(&kept).state, UnitState::Kept(diags) if !diags.is_empty()),
        "a syntax error must keep the block bench with diagnostics"
    );
    unchanged(half(&added), half(&kept), "syntax error");
    assert_eq!(
        kept.map.of_doc(doc),
        added.map.of_doc(doc),
        "kept Own benches must retain their circuit source-map entries"
    );

    edit(&mut sources, doc, "run: {", "run: {stop: 2ms}");
    edit(&mut sources, unit, "1kohm", "2kohm");
    let unit_edit = evaluate_from(Some(&kept), &sources, &libs, &NO_CANCEL)
        .expect("the fixed bench and changed unit evaluate");
    assert_eq!(
        half(&unit_edit).state,
        UnitState::Built,
        "fixing the bench syntax must rebuild its body"
    );
    assert!(
        !Arc::ptr_eq(
            &kept.design.units[0].circuit,
            &unit_edit.design.units[0].circuit
        ),
        "the independent unit edit must actually rebuild its circuit"
    );
    unchanged(half(&kept), half(&unit_edit), "editing an independent unit");

    edit(&mut sources, doc, "ratio: 0.4", "ratio: missing_ratio");
    let runtime_error = evaluate_from(Some(&unit_edit), &sources, &libs, &NO_CANCEL)
        .expect("a circuit-function runtime error keeps the previous block bench");
    assert!(
        matches!(&half(&runtime_error).state, UnitState::Kept(diags) if !diags.is_empty()),
        "a circuit-function runtime error must keep the block bench with diagnostics"
    );
    assert!(
        half(&runtime_error)
            .state
            .diags()
            .iter()
            .any(|d| d.message.contains("parameter ratio") && !d.sel.is_empty()),
        "the circuit-function runtime diagnostic must name the ratio parameter and be located: {:?}",
        half(&runtime_error).state.diags()
    );
    unchanged(
        half(&unit_edit),
        half(&runtime_error),
        "circuit-function runtime error",
    );
    let scope_id = half(&unit_edit).scope_id();
    let previous_map = unit_edit
        .map
        .of_doc(doc)
        .into_iter()
        .filter(|(sel, _)| sel.unit() == Some(scope_id))
        .collect::<Vec<_>>();
    assert!(
        !previous_map.is_empty(),
        "the previously built block bench must have circuit source-map entries to retain"
    );
    assert_eq!(
        runtime_error
            .map
            .of_doc(doc)
            .into_iter()
            .filter(|(sel, _)| sel.unit() == Some(scope_id))
            .collect::<Vec<_>>(),
        previous_map,
        "a circuit-function runtime error must retain its scope's previous source-map entries"
    );

    edit(&mut sources, doc, "ratio: missing_ratio", "ratio: 0.4");
    let recovered = evaluate_from(Some(&runtime_error), &sources, &libs, &NO_CANCEL)
        .expect("fixing the circuit-function runtime error evaluates");
    assert_eq!(
        half(&recovered).state,
        UnitState::Built,
        "fixing the circuit-function runtime error must build the block bench"
    );
    unchanged(
        half(&unit_edit),
        half(&recovered),
        "fixing the circuit-function runtime error",
    );
}

#[test]
fn project_blocks_build_in_units_and_benches_and_invalidate_reuse() {
    use fab_elec_core::Sel;
    use fab_elec_lang::BenchScope;

    fn own(ev: &Evaluated) -> (&Arc<fab_elec_circuit::Circuit>, u64) {
        match &ev.benches[0].scope {
            BenchScope::Own { circuit, generation } => (circuit, *generation),
            BenchScope::Design(_) => panic!("the project block bench must have an Own circuit"),
        }
    }

    let libs = std_libs();
    let mut sources = Sources::new();
    let blocks = sources.add("power.blocks.splash", r#"
let e = mod.elec
let half = e.block({
    name: "Half"
    ports: {top: e.port(@power_in)  out: e.port(@out)  gnd: e.port(@ground)}
    params: {r: 10kohm}
    build: fn(p, o) {
        let r_top = e.r(o.r, p.top, p.out, {package: "0402"})
        let r_bottom = e.r(o.r, p.out, p.gnd, {package: "0402"})
    }
})
"#).expect("the project blocks document is added");
    let unit = sources.add("main.ckt.splash", r#"
use mod.project.*
let e = mod.elec
let gnd = e.net({name: "GND" kind: @ground})
let vin = e.net({name: "VIN"})
let vout = e.net({name: "VOUT"})
let h = half({top: vin out: vout gnd: gnd})
"#).expect("the unit document is added");
    let bench = sources.add("checks.bench.splash", r#"
use mod.project.*
let e = mod.elec
bench("half", {
    circuit: fn() {
        let gnd = e.net({name: "GND" kind: @ground})
        let vin = e.net({name: "VIN"})
        let vout = e.net({name: "VOUT"})
        let dut = half({top: vin out: vout gnd: gnd})
    }
    run: {stop: 2ms}
}, fn(b, c) {
    b.vsrc(c.net("VIN"), c.net("GND"), {dc: 5V})
    b.expect("half", b.mean(b.v(c.net("VOUT")), {window: 1ms after: 0.5ms}), {min: 2.4V max: 2.6V})
})
"#).expect("the block bench document is added");

    let first = evaluate_from(None, &sources, &libs, &NO_CANCEL).expect("the project block builds in both VMs");
    assert_eq!(first.units.len(), 1, "the project must have one design unit");
    assert_eq!(first.units[0].state, UnitState::Built, "the unit must build from the project block");
    assert!(!first.units[0].reused, "the first evaluation must build the unit");
    assert_eq!(first.units[0].docs, [unit, blocks], "the blocks document must join the unit's reuse inputs");
    assert_eq!(
        first.design.units[0].circuit.comps.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        ["h.r_top", "h.r_bottom"],
        "the unit must instantiate both project block parts under h"
    );
    assert_eq!(first.benches.len(), 1, "the project must have one block bench");
    assert_eq!(first.benches[0].state, UnitState::Built, "the project block bench must build");
    let (first_circuit, first_generation) = own(&first);
    assert_eq!(
        first_circuit.comps.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        ["dut.r_top", "dut.r_bottom"],
        "the bench must instantiate both project block parts under dut"
    );
    assert_eq!(first.benches[0].spec.sources.len(), 1, "the block bench must record its voltage source");
    assert_eq!(first.benches[0].expects.len(), 1, "the block bench must record its expectation");
    assert_eq!(first.benches[0].expects[0].range(), Some((2.4, 2.6)), "the initial expectation must retain both bounds");

    edit(&mut sources, blocks, "10kohm", "20kohm");
    let changed = evaluate_from(Some(&first), &sources, &libs, &NO_CANCEL).expect("the project block value edit evaluates");
    assert_eq!(changed.units[0].state, UnitState::Built, "the unit must build after a project block value edit");
    assert!(!changed.units[0].reused, "a project block edit must invalidate unit reuse");
    assert!(
        !Arc::ptr_eq(&first.design.units[0].circuit, &changed.design.units[0].circuit),
        "a project block edit must allocate a new unit circuit"
    );
    assert_eq!(changed.benches[0].state, UnitState::Built, "the block bench must build after a project block value edit");
    let (changed_circuit, changed_generation) = own(&changed);
    assert!(!Arc::ptr_eq(first_circuit, changed_circuit), "a project block value edit must allocate a new bench circuit");
    assert!(changed_generation > first_generation, "a project block value edit must advance the bench circuit generation");
    for circuit in [&changed.design.units[0].circuit, changed_circuit] {
        assert_eq!(
            circuit.comps.iter().map(|c| c.value.as_ref().and_then(|v| v.qty).map(|q| q.si)).collect::<Vec<_>>(),
            [Some(20000.0), Some(20000.0)],
            "both VMs must use the edited project block resistor value"
        );
    }

    edit(&mut sources, bench, "2.6V", "2.7V");
    let bounds = evaluate_from(Some(&changed), &sources, &libs, &NO_CANCEL).expect("the bench bound edit evaluates");
    reused(&changed, &bounds, 0, "editing only a project block bench's bound");
    assert_eq!(bounds.benches[0].state, UnitState::Built, "the edited block bench must build");
    let (bounds_circuit, bounds_generation) = own(&bounds);
    assert!(Arc::ptr_eq(changed_circuit, bounds_circuit), "a bench bound edit must retain its circuit Arc");
    assert_eq!(bounds_generation, changed_generation, "a bench bound edit must retain its circuit generation");
    assert_eq!(bounds.benches[0].expects[0].range(), Some((2.4, 2.7)), "circuit reuse must still evaluate the new bench bound");

    edit(&mut sources, blocks, "e.r(o.r, p.top", "e.r(missing_resistance, p.top");
    let broken = evaluate_from(Some(&bounds), &sources, &libs, &NO_CANCEL).expect("a broken project block keeps the last good unit");
    assert!(!broken.units[0].reused, "an error in an edited project block must attempt to rebuild the unit");
    assert!(
        matches!(&broken.units[0].state, UnitState::Kept(diags) if !diags.is_empty()),
        "an unknown name in the block build must fail the rebuild and keep the last good unit"
    );
    assert!(
        broken.units[0].state.diags().iter().any(|d| {
            d.message.contains("missing_resistance")
                && d.sel.iter().any(|s| matches!(s, Sel::Source { doc, .. } if *doc == blocks))
        }),
        "the unknown-name diagnostic must be located in power.blocks.splash: {:?}",
        broken.units[0].state.diags()
    );
}

#[test]
fn firmware_documents_are_never_evaluated_and_their_edits_keep_the_design() {
    let libs = std_libs();
    let mut plain = Sources::new();
    plain.add("small.ckt.splash", SMALL).expect("add the circuit");
    let without = evaluate(&plain, &libs).expect("the circuit evaluates");
    let mut sources = plain.clone();
    // Not circuit Splash at all: evaluating it as a circuit would fail.
    let fw = sources.add("small.fw.splash", "let chase_ms = 125\nevery chase_ms ms { ) }\n").expect("add the firmware");
    assert_eq!(sources.doc(fw).expect("the firmware document is in the project").kind, DocKind::Firmware,
        "a .fw.splash document is firmware");
    let with = evaluate(&sources, &libs).expect("a firmware document never fails the circuit");
    assert_eq!(with.design, without.design, "a firmware document adds nothing to the design");
    assert_eq!(with.notes, without.notes, "a firmware document adds no notes");
    assert!(with.units.iter().all(|unit| !unit.docs.contains(&fw)), "no unit reads the firmware document");
    assert!(with.map.of_doc(fw).is_empty(), "the firmware document has no source map sites");
    edit(&mut sources, fw, "125", "20");
    let edited = evaluate_from(Some(&with), &sources, &libs, &NO_CANCEL).expect("the edited firmware evaluates nothing");
    reused(&with, &edited, 0, "a firmware edit");
    same_result(&edited, &with, "a firmware edit");
}
