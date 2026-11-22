//! L1: every controller item is mapped to where it is written, and mistakes are located
//! errors naming the binding.

use fab_elec_circuit::RefKind;
use fab_elec_core::{BlockId, BoardId, CompRef, Diag, NetRef, Sel, Severity};
use fab_elec_lang::{NO_CANCEL, evaluate, evaluate_from, source::line_col, std_libs, Sources, UnitState};

const LEDCTL: &str = include_str!("../../fixtures/ledctl.ckt.splash");

fn sources(text: &str) -> Sources {
    let mut s = Sources::new();
    s.add("ledctl.ckt.splash", text.to_string()).expect("a circuit document name");
    s
}

fn shown(errors: &[Diag], text: &str) -> String {
    errors
        .iter()
        .map(|d| match d.sel.first() {
            Some(Sel::Source { start, .. }) => format!("{:?}: {}", line_col(text, *start), d.message),
            _ => d.message.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ledctl_is_mapped_to_the_calls_that_made_it() {
    let libs = std_libs();
    let src = sources(LEDCTL);
    let ev = evaluate(&src, &libs).unwrap_or_else(|e| panic!("the fixture evaluates:\n{}", shown(&e, LEDCTL)));
    let got = &ev.design.units[0].circuit;

    // Every component, net and block instance is mapped to the call that made it.
    let unit = BoardId(0);
    let at = |sel: &Sel| {
        let r = ev.map.locate(sel).unwrap_or_else(|| panic!("{sel:?} is mapped"));
        &LEDCTL[r.span.start as usize..r.span.end as usize]
    };
    assert_eq!(at(&Sel::Unit(unit)), LEDCTL, "the unit is the whole document");
    for c in &got.comps {
        let text = at(&Sel::Comp(CompRef { board: unit, comp: c.id }));
        assert!(text.starts_with("e."), "{}: mapped to its builder call, not {text:?}", c.path);
    }
    for n in &got.nets {
        at(&Sel::Net(NetRef { board: unit, net: n.id }));
    }
    for b in got.tree.iter().skip(1) {
        let text = at(&Sel::Block(unit, b.id));
        assert!(text.ends_with(')'), "{}: mapped to its instance call, not {text:?}", b.path);
    }
    let comp = |path: &str| got.comps.iter().find(|c| c.path == path).unwrap_or_else(|| panic!("{path} exists")).id;
    assert!(at(&Sel::Comp(CompRef { board: unit, comp: comp("reg.c_in") })).starts_with("e.c(o.c_in"), "reg.c_in: its own call");
    assert_eq!(
        ev.map.locate(&Sel::Comp(CompRef { board: unit, comp: comp("lamps[2].r") })),
        ev.map.locate(&Sel::Comp(CompRef { board: unit, comp: comp("lamps[5].r") })),
        "every instance of a block body maps to the one call"
    );
    assert!(at(&Sel::Block(unit, BlockId(4))).starts_with("led_driver("), "lamps[0]: the instance call in the loop");
}

#[test]
fn stored_refs_reserve_numbers_and_select_invalid_values() {
    let source = r#"let e = mod.elec
let first = e.r(1kohm)
let second = e.r(2kohm)
let fixed = e.r(3kohm, nil, nil, {ref: "R02"})
let cap = e.c(1uF, nil, nil, {ref: "R4A"})
let third = e.r(4kohm)
let lamp = e.led({color: @green ref: "Panel.D"})
let sw = e.part(mod.std.tact_switch(), {ref: "Panel.D"})
let auto_cap = e.c(2uF)
"#;
    let libs = std_libs();
    let ev = evaluate(&sources(source), &libs).expect("duplicate stored references are allowed during evaluation");
    let refs: Vec<_> = ev.design.units[0].circuit.comps.iter().map(|c| c.refdes.as_str()).collect();
    assert_eq!(refs, ["R1", "R3", "R02", "R4A", "R4", "Panel.D", "Panel.D", "C2"],
        "later numeric refs reserve their number, a stored part keeps its count (so `auto_cap` stays C2), other text stays verbatim and each prefix counts independently");

    let bad = source.replace("\"R02\"", "\"\"").replace("\"R4A\"", "12").replace("ref: \"Panel.D\"}", "ref: \"two words\"}");
    let errors = evaluate(&sources(&bad), &libs).expect_err("invalid references are refused");
    assert_eq!(errors.len(), 4, "each invalid reference gets one diagnostic: {errors:?}");
    for (path, value) in [("fixed", "\"\""), ("cap", "12"), ("lamp", "\"two words\""), ("sw", "\"two words\"")] {
        let diag = errors.iter().find(|d| d.message.starts_with(&format!("{path}: ref: expected text like \"R12\", got ")))
            .unwrap_or_else(|| panic!("{path}: missing reference diagnostic in {errors:?}"));
        let Some(Sel::Source { start, end, .. }) = diag.sel.first() else { panic!("{path}: the invalid reference is located") };
        assert_eq!(&bad[*start as usize..*end as usize], value, "{path}: the diagnostic selects the invalid ref value");
    }
}

#[test]
fn added_parts_follow_the_count_and_their_written_order() {
    let source = r#"let e = mod.elec
let late = e.c(1pF, opts: {added: 30})
let first = e.c(2pF)
let cell = e.block({name: "Cell" ports: {} build: fn(p, o) { let cap = e.c(3pF) }})
let left = cell({})
let right = cell({})
let counted = e.c(4pF, opts: {ref: "C07"})
let early = e.c(5pF, opts: {added: 10})
let fixed = e.c(6pF, opts: {added: 20 ref: "C09"})
let tied = e.c(7pF, opts: {added: 10})
"#;
    let libs = std_libs();
    let ev = evaluate(&sources(source), &libs).expect("the marked and counted capacitors evaluate");
    let circuit = &ev.design.units[0].circuit;
    for (path, reference, kind) in [
        ("first", "C1", RefKind::Auto), ("left.cap", "C2", RefKind::Shared), ("right.cap", "C3", RefKind::Shared),
        ("counted", "C07", RefKind::Stored), ("early", "C8", RefKind::Auto), ("fixed", "C09", RefKind::Stored),
        ("tied", "C10", RefKind::Auto), ("late", "C11", RefKind::Auto),
    ] {
        let part = circuit.comp(path).unwrap_or_else(|| panic!("the evaluated circuit contains {path}"));
        assert_eq!((part.refdes.as_str(), part.ref_kind), (reference, kind),
            "{path}: counted refs stay in place, additions follow their order past C07 and skip the held C09");
    }

    let bad = r#"let e = mod.elec
let zero = e.r(1kohm, opts: {added: 0})
let fraction = e.c(1pF, 1%, rest: {added: 1.5})
let string = e.led({color: @green added: "two"})
let large = e.part(mod.std.tact_switch(), {added: 4294967296})
"#;
    let errors = evaluate(&sources(bad), &libs).expect_err("each invalid addition order is refused");
    assert_eq!(errors.len(), 4, "each bad value produces one diagnostic: {errors:?}");
    for (path, value) in [("zero", "0"), ("fraction", "1.5"), ("string", r#""two""#), ("large", "4294967296")] {
        let diag = errors.iter().find(|d| d.message == format!("{path}: added: expected a positive whole number, got {value}"))
            .unwrap_or_else(|| panic!("{path}: missing addition order diagnostic in {errors:?}"));
        let Some(Sel::Source { start, end, .. }) = diag.sel.first() else { panic!("{path}: the diagnostic needs a source range") };
        assert_eq!(&bad[*start as usize..*end as usize], value, "{path}: the diagnostic selects the invalid value");
    }
}

#[test]
fn reference_kind_follows_the_component_call() {
    let source = r#"let e = mod.elec
let single = e.r(1kohm)
let pair = e.block({name: "Pair" ports: {} build: fn(p, o) {
    let numbered = e.r(2kohm)
    let fixed = e.r(3kohm, nil, nil, {ref: "R20"})
}})
let left = pair({})
let right = pair({})
"#;
    let ev = evaluate(&sources(source), &std_libs()).expect("the two block instances evaluate");
    let circuit = &ev.design.units[0].circuit;
    let kind = |path: &str| circuit.comp(path).unwrap_or_else(|| panic!("{path} exists")).ref_kind;
    assert_eq!(kind("single"), RefKind::Auto, "a single unwritten reference can be annotated");
    for path in ["left.numbered", "right.numbered"] {
        assert_eq!(kind(path), RefKind::Shared, "{path}: both instances share the unwritten reference's call");
    }
    for path in ["left.fixed", "right.fixed"] {
        assert_eq!(kind(path), RefKind::Stored, "{path}: an explicit reference takes precedence on a shared call");
    }
}

#[test]
fn automatic_refs_follow_project_wide_call_counts_and_library_ownership() {
    let mut src = Sources::new();
    src.add("cell.blocks.splash", r#"let e = mod.elec
let cell = e.block({name: "Cell" ports: {} build: fn(p, o) {
    let r = e.r(2kohm)
}})
"#).expect("the project block document");
    src.add("panel.ckt.splash", r#"let e = mod.elec
let single = e.r(1kohm)
let branch = mod.project.cell({})
let gnd = e.net({name: "GND" kind: @ground})
let vbus = e.net({name: "VBUS" kind: @power volts: 5V})
let usb = mod.std.usb_c_power({vbus: vbus gnd: gnd})
"#).expect("the panel circuit document");
    let libs = std_libs();
    let panel = evaluate(&src, &libs).expect("the panel and its library block evaluate");
    let circuit = &panel.design.units[0].circuit;
    let branch = circuit.comp("branch.r").expect("the panel's project-block resistor");
    assert_eq!(branch.ref_kind, RefKind::Auto, "one instance of a project block can store its reference");
    assert_eq!(circuit.comp("usb.j").expect("the one USB connector").ref_kind, RefKind::Shared,
        "a single call inside a read-only library keeps its automatic reference");

    let second = src.add("voice.ckt.splash", "let branch = mod.project.cell({})\n").expect("the voice circuit document");
    let both = evaluate_from(Some(&panel), &src, &libs, &NO_CANCEL).expect("the voice shares the panel's project block");
    assert_eq!(both.design.units.len(), 2, "both circuit documents build");
    for unit in &both.design.units {
        let comp = unit.circuit.comp("branch.r").expect("both units instantiate the block");
        assert_eq!(comp.ref_kind, RefKind::Shared, "{}: the call makes one resistor in each unit", unit.name);
    }
    let reused = &both.design.units[0].circuit;
    assert_eq!(reused.comp("branch.r").expect("the shared panel resistor").refdes, branch.refdes,
        "sharing the call preserves the panel resistor's number");
    assert_eq!(reused.comp("single").expect("the panel's own resistor").ref_kind, RefKind::Auto,
        "the single call in the panel document stays writable");

    src.set_present(second, false).expect("remove the voice circuit");
    let alone = evaluate_from(Some(&both), &src, &libs, &NO_CANCEL).expect("the panel survives removal of the voice");
    let restored = alone.design.units[0].circuit.comp("branch.r").expect("the panel keeps its resistor");
    assert_eq!((restored.ref_kind, &restored.refdes), (RefKind::Auto, &branch.refdes),
        "removing the other unit makes the reused resistor writable again with its original number");
}

#[test]
fn netclass_via_in_pad_options_are_typed() {
    let libs = std_libs();
    let source = r#"use mod.elec.*
netclass("allowed", {via_in_pad: true})
netclass("forbidden", {via_in_pad: false})
netclass("omitted", {})
netclass("unset", {via_in_pad: nil})
"#;
    let ev = evaluate(&sources(source), &libs).expect("boolean and unset via-in-pad options evaluate");
    let classes = &ev.design.units[0].circuit.classes;
    for (name, expected) in [("allowed", Some(true)), ("forbidden", Some(false)), ("omitted", None), ("unset", None)] {
        let class = classes.iter().find(|class| class.name == name).expect("each declared net class is lowered");
        assert_eq!(class.rules.via_in_pad, expected, "netclass {name}: the via-in-pad option keeps its boolean or unset value");
    }

    let bad = "use mod.elec.*\nnetclass(\"invalid\", {via_in_pad: 1})\n";
    let errors = evaluate(&sources(bad), &libs).expect_err("a numeric via-in-pad option is refused");
    assert_eq!(errors.len(), 1, "a numeric via-in-pad option produces one diagnostic");
    assert_eq!(
        errors[0].message, "netclass invalid: via_in_pad: expected true or false, got a number",
        "the via-in-pad diagnostic names the class, expected booleans and actual value type"
    );
    assert_eq!(loc_in(&errors[0], bad), Some((2, 1)), "the via-in-pad diagnostic points to the netclass call");
}

#[test]
fn mistakes_are_located_and_name_the_binding() {
    let libs = std_libs();
    let bad = LEDCTL
        .replace("{package: \"0603\" volts: 10V})\n        let c_out", "{package: \"0603\" volt: 10V})\n        let c_out")
        .replace("class: \"power\"})\nlet v33", "class: \"pwr\"})\nlet v33")
        .replace("let btn   = button({out: key gnd: gnd})", "let btn   = button({out: key})");
    let errors = evaluate(&sources(&bad), &libs).expect_err("three mistakes");
    let text = shown(&errors, &bad);
    let line_of = |needle: &str| bad.lines().position(|l| l.contains(needle)).expect("the line exists") as u32 + 1;
    let find = |what: &str| errors.iter().find(|d| d.message.contains(what)).unwrap_or_else(|| panic!("an error says {what:?}:\n{text}"));
    let loc = |d: &Diag| match d.sel.first() {
        Some(Sel::Source { start, .. }) => line_col(&bad, *start),
        _ => None,
    };
    let opt = find("reg.c_in: unknown option volt");
    assert_eq!(loc(opt).map(|l| l.0), Some(line_of("let c_in  = e.c(")), "the option error is at the e.c call:\n{text}");
    let port = find("btn: port gnd of Button is not connected");
    assert_eq!(loc(port), Some((line_of("let btn   = button("), 13)), "the port error is at the instance call:\n{text}");
    // Lowering runs only on a clean run, so the class error waits for the other two.
    let fixed = bad.replace("volt: 10V", "volts: 10V").replace("button({out: key})", "button({out: key gnd: gnd})");
    let errors = evaluate(&sources(&fixed), &libs).expect_err("one mistake");
    let text = shown(&errors, &fixed);
    assert_eq!(errors.len(), 1, "one error:\n{text}");
    assert!(errors[0].message.starts_with("VBUS: no net class has the name \"pwr\""), "{text}");
    assert_eq!(loc_in(&errors[0], &fixed).map(|l| l.0), Some(line_of("let vbus = e.net(")), "{text}");

    for (opts, message) in [
        (r#"{color: @violet package: "0603" current: 5mA}"#, "current: no forward-voltage curve is known for violet"),
        (r#"{color: @green package: "0603" current: 0mA}"#, "current: expected a positive current in amperes, got a number"),
    ] {
        let bad = format!("let e = mod.elec\nlet d = e.led({opts})\n");
        let errors = evaluate(&sources(&bad), &libs).expect_err("invalid LED drive current");
        let text = shown(&errors, &bad);
        assert_eq!(errors.len(), 1, "one LED error:\n{text}");
        assert_eq!(errors[0].message, format!("d: {message}"), "the error names the LED binding:\n{text}");
        assert_eq!(loc_in(&errors[0], &bad), Some((2, 9)), "the current error is at the e.led call:\n{text}");
    }
}

#[test]
fn a_missing_property_passed_to_a_native_is_one_diagnostic() {
    let source = r#"let e = mod.elec
e.define("Two pins", {
    kind: @resistor
    symbol: e.f.two_terminal([["A", @passive], ["B", @passive]])
})
let vcc = e.net({name: "VCC" kind: @power volts: 5V})
let n = {a: 1}
let r = e.r(10kohm, n.missing, vcc)
let part = e.part("Two pins", {pins: {A: n.missing}})
e.connect(n.missing, vcc)
"#;
    let errors = evaluate(&sources(source), &std_libs()).expect_err("three missing properties");
    let text = shown(&errors, source);
    assert_eq!(errors.len(), 3, "one diagnostic per missing property:\n{text}");
    for error in &errors {
        assert_eq!(error.severity, Severity::Error, "the missing property is an error:\n{text}");
        assert!(error.message.contains("property missing not found in prototype chain"),
            "only the VM's missing-property error is reported:\n{text}");
    }
    for call in ["e.r(10kohm", "e.part(\"Two pins\"", "e.connect(n.missing"] {
        let line = source.lines().position(|line| line.contains(call)).expect("the call exists") as u32 + 1;
        assert_eq!(errors.iter().filter(|error| loc_in(error, source).map(|loc| loc.0) == Some(line)).count(), 1,
            "{call}: exactly one diagnostic at the call:\n{text}");
    }
}

fn loc_in(d: &Diag, text: &str) -> Option<(u32, u32)> {
    match d.sel.first() {
        Some(Sel::Source { start, .. }) => line_col(text, *start),
        _ => None,
    }
}

#[test]
fn a_broken_unit_keeps_its_last_lowering() {
    let libs = std_libs();
    const SMALL: &str = "use mod.elec.*\nlet e = mod.elec\nlet a = e.net({name: \"A\"})\nlet r = e.r(1kohm, a, a)\n";
    let mut src = sources(LEDCTL);
    let first = evaluate(&src, &libs).expect("the fixture evaluates");
    assert_eq!(first.units.iter().map(|u| (u.id, u.state.clone())).collect::<Vec<_>>(), [(BoardId(0), UnitState::Built)], "one built unit");

    // A new document sorting first takes the next free id: no unit is renumbered (change 6).
    src.add("aaa.ckt.splash", SMALL.to_string()).expect("a circuit document name");
    let second = evaluate_from(Some(&first), &src, &libs, &NO_CANCEL).expect("both evaluate");
    let ids: Vec<_> = second.units.iter().map(|u| (u.name.as_str(), u.id)).collect();
    assert_eq!(ids, [("ledctl", BoardId(0)), ("aaa", BoardId(1))], "ids follow the order documents arrived");
    assert_eq!(second.design.units.len(), 2, "two units built");

    // A mistake in one document keeps that unit's last lowering, and the other unit builds.
    let doc = src.find("ledctl.ckt.splash").expect("the fixture is a document");
    let text = LEDCTL.replace("let btn   = button({out: key gnd: gnd})", "let btn   = button({out: key})");
    let mut broken = Sources::new();
    broken.add("ledctl.ckt.splash", text).expect("name");
    broken.add("aaa.ckt.splash", SMALL.replace("1kohm", "2kohm")).expect("name");
    let third = evaluate_from(Some(&second), &broken, &libs, &NO_CANCEL).expect("a kept unit is not a failure");
    let state = &third.units[0].state;
    assert!(matches!(state, UnitState::Kept(d) if d.iter().any(|d| d.message.contains("btn: port gnd"))), "ledctl is kept with its located error: {state:?}");
    let unit = |ev: &fab_elec_lang::Evaluated, id: BoardId| ev.design.units.iter().find(|u| u.id == id).cloned();
    assert_eq!(unit(&third, BoardId(0)), unit(&second, BoardId(0)), "the kept unit is its last good lowering");
    assert_eq!(third.map.of_doc(doc), second.map.of_doc(doc), "and its map entries stay");
    assert_eq!(third.units[1].state, UnitState::Built, "the other unit builds");
    let r = &unit(&third, BoardId(1)).expect("aaa is built").circuit.comps[0];
    assert_eq!(r.value.as_ref().map(|v| v.text.as_str()), Some("2k"), "with its new value");

    // Nothing ever built: every mistake, located.
    let mut only = Sources::new();
    only.add("ledctl.ckt.splash", LEDCTL.replace("let btn   = button({out: key gnd: gnd})", "let btn   = button({out: key})")).expect("name");
    let errors = evaluate(&only, &libs).expect_err("nothing was built");
    assert!(errors.iter().any(|d| d.message.contains("btn: port gnd")), "the error is reported");
    let failed = evaluate_from(Some(&second), &only, &libs, &NO_CANCEL).expect("kept from before");
    assert!(matches!(failed.units[0].state, UnitState::Kept(_)), "a unit built before is kept, not failed");
}
