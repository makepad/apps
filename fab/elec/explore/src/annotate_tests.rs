use super::*;
use crate::sheet::PartPlace;
use fab_elec_lang::{evaluate, splice, splice_verified, std_libs, Sources, NO_CANCEL};

#[test]
fn annotate_stores_sheet_order_and_preserves_held_refs() {
    let source = r#"let e = mod.elec
let hidden = e.c(1uF)
let lower = e.r(1kohm)
let fixed = e.r(2kohm, nil, nil, {ref: "R2"})
let cell = e.block({name: "Cell" ports: {} build: fn(p, o) {
    let r = e.r(3kohm)
}})
let a = cell({})
let b = cell({})
let upper_right = e.r(4kohm)
let upper_left = e.r(5kohm)
let missing = e.c(2uF)
"#;
    let mut sources = Sources::new();
    sources.add("rows.ckt.splash", source).expect("the sheet's circuit document");
    let libs = std_libs();
    let ev = evaluate(&sources, &libs).expect("the sheet's circuit evaluates");
    let circuit = &ev.design.units[0].circuit;
    let comp = |path: &str| circuit.comp(path).unwrap_or_else(|| panic!("{path} exists"));
    let mut sheet = SheetLayout {
        parts: vec![PartPlace::default(); comp("missing").id.ix()],
        ..SheetLayout::default()
    };
    for (path, at) in [
        ("upper_right", [20, 7]), ("upper_left", [0, 0]), ("lower", [-20, -1]),
        ("fixed", [0, -20]), ("a.r", [10, -20]), ("b.r", [20, -20]),
    ] {
        sheet.parts[comp(path).id.ix()] = PartPlace { at, placed: true, ..PartPlace::default() };
    }
    let Planned::Edit(edit) = annotate(circuit, &sheet) else { panic!("automatic references produce a plan") };
    let set = |path: &str, reference: &str| DesignOp::Circuit {
        unit: circuit.unit, op: CircuitOp::SetRef { comp: comp(path).id, to: Some(reference.into()) },
    };
    assert_eq!(edit.label, "Annotate 5 parts (2 keep automatic refs)", "the history label counts written and shared parts separately");
    assert_eq!(edit.ops, [
        set("upper_left", "R1"), set("upper_right", "R5"), set("lower", "R6"),
        set("hidden", "C1"), set("missing", "C2"),
    ], "the 7-step row reads left to right, 8 steps starts a new row, and undrawn parts follow in source order");

    let edits = splice(&sources, &ev.map, &ev.design, &edit.ops).expect("all planned references splice together");
    sources.apply(&edits.edits).expect("the references are written in one step");
    let after = evaluate(&sources, &libs).expect("the annotated source evaluates");
    let annotated = &after.design.units[0].circuit;
    for (path, reference, kind) in [
        ("fixed", "R2", RefKind::Stored), ("a.r", "R3", RefKind::Shared), ("b.r", "R4", RefKind::Shared),
        ("upper_right", "R5", RefKind::Stored),
    ] {
        let c = annotated.comp(path).unwrap_or_else(|| panic!("{path} survives annotation"));
        assert_eq!((c.refdes.as_str(), c.ref_kind), (reference, kind), "{path}: the expected reference and its source survive evaluation");
    }
    assert_eq!(annotate(annotated, &sheet), Planned::Nothing {
        chip: "2 parts keep automatic refs: each is made by a shared or library call".into(),
    }, "a second annotation explains the two references that remain automatic");
}

#[test]
fn drum16_annotation_writes_every_planned_reference() {
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16's project documents");
    let libs = std_libs();
    let ev = evaluate(&sources, &libs).expect("drum16 evaluates");
    let unit = ev.design.units.iter().find(|u| u.name == "drum16").expect("the drum16 board");
    let circuit = &unit.circuit;
    let Planned::Edit(edit) = annotate(circuit, &SheetLayout::default()) else { panic!("drum16 has references to store") };
    let written = splice_verified(&sources, &libs, &ev, &edit.ops, &NO_CANCEL).expect("the host accepts the complete annotation");
    for (path, kind) in [("sg.top", RefKind::Shared), ("usb.j", RefKind::Shared), ("rva", RefKind::Auto)] {
        let comp = circuit.comp(path).unwrap_or_else(|| panic!("drum16 has {path}"));
        assert_eq!(comp.ref_kind, kind, "{path}: only its own writable call can store a reference");
    }
    let auto = circuit.comps.iter().filter(|c| c.ref_kind == RefKind::Auto).count();
    let shared = circuit.comps.iter().filter(|c| c.ref_kind == RefKind::Shared).count();
    assert_eq!(edit.ops.len(), auto, "one edit per independently writable drum16 part");
    assert_eq!(edit.label, format!("Annotate {auto} parts ({shared} keep automatic refs)"), "the strip reports both counts");

    let after = &written.evaluated.design.unit(unit.id).expect("the annotated drum16 board").circuit;
    for comp in &circuit.comps {
        let got = &after.comps[comp.id.ix()];
        let kind = if comp.ref_kind == RefKind::Auto { RefKind::Stored } else { comp.ref_kind };
        assert_eq!(got.ref_kind, kind, "{}: the source stores exactly the writable references", comp.path);
        assert_eq!(got.refdes, comp.refdes, "{}: component-order annotation preserves the number", comp.path);
    }
}
