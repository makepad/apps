//! Sheet edits preserve the rest of the source and evaluate to the requested hints.

use fab_elec_circuit::{CircuitOp, Flow, NetStyle, PortSide, SheetHint, SheetHints};
use fab_elec_core::{BoardId, Diag, SchPt, Sel, Severity};
use fab_elec_design::DesignOp;
use fab_elec_lang::{Evaluated, Libs, NO_CANCEL, Sources, evaluate, splice, splice_verified, std_libs};

const FIXTURE: &str = include_str!("../../fixtures/ledctl.ckt.splash");

fn sources(text: &str) -> Sources {
    let mut sources = Sources::new();
    sources.add("hints.ckt.splash", text.to_string()).expect("the hint circuit is added");
    sources
}

fn setup(text: &str, libs: &Libs) -> (Sources, Evaluated) {
    let sources = sources(text);
    let evaluated = evaluate(&sources, libs).expect("the original hint circuit evaluates");
    (sources, evaluated)
}

fn operation(path: &str, hint: Option<SheetHint>) -> DesignOp {
    DesignOp::Circuit { unit: BoardId(0), op: CircuitOp::Hint { path: path.into(), hint } }
}

fn after(
    sources: &Sources,
    evaluated: &Evaluated,
    libs: &Libs,
    path: &str,
    hint: Option<SheetHint>,
) -> (Sources, Evaluated) {
    let mut expected = evaluated.design.units[0].circuit.as_ref().clone();
    match &hint {
        Some(hint) => match expected.hints.parts.iter_mut().find(|(key, _)| key == path) {
            Some((_, before)) => *before = hint.clone(),
            None => expected.hints.parts.push((path.into(), hint.clone())),
        },
        None => expected.hints.parts.retain(|(key, _)| key != path),
    }
    let change = splice(sources, &evaluated.map, &evaluated.design, &[operation(path, hint)])
        .expect("the hint operation produces edits");
    assert!(change.reevaluate, "hint operations request evaluation");
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("hint edits are disjoint and apply together");
    let got = evaluate(&edited, libs).unwrap_or_else(|errors| {
        panic!("the edited hints evaluate: {errors:?}\n{}", edited.docs()[0].text)
    });
    assert_eq!(got.design.units[0].circuit.hints, expected.hints,
        "splice then evaluate equals applying the hint operation for {path}");
    assert_eq!(got.design.units[0].circuit.as_ref(), &expected,
        "a hint operation preserves every other circuit property");
    (edited, got)
}

fn selected<'a>(sources: &'a Sources, diag: &Diag) -> &'a str {
    let Some(Sel::Source { doc, start, end }) = diag.sel.first() else {
        panic!("a hint diagnostic selects its source: {diag:?}")
    };
    assert!(end > start, "a hint diagnostic selects a nonempty offending range");
    &sources.doc(*doc).expect("the selected circuit exists").text[*start as usize..*end as usize]
}

#[test]
fn sheet_evaluates_paths_pin_sides_and_net_styles_in_order() {
    let libs = std_libs();
    let tail = r#"
sheet({
    "mcu": {at: [24 10] flow: @down turn: 90deg mirror: true free: true}
    "mcu.pins": {PA0: @right PA1: @left}
    "lamps[3].r.pins": {A: @top B: @bottom}
    nets: {"SWCLK": @label LED0: @wire}
    "renamed_part": {mirror: false}
})
"#;
    let (sources, got) = setup(&format!("{FIXTURE}{tail}"), &libs);
    assert_eq!(got.design.units[0].circuit.hints, SheetHints {
        parts: vec![
            ("mcu".into(), SheetHint {
                at: Some(SchPt::new(24, 10)), flow: Some(Flow::Down), turn: 1, mirror: true,
                pins: vec![("PA0".into(), PortSide::Right), ("PA1".into(), PortSide::Left)], free: true,
            }),
            ("lamps[3].r".into(), SheetHint {
                pins: vec![("A".into(), PortSide::Top), ("B".into(), PortSide::Bottom)],
                ..SheetHint::default()
            }),
            ("renamed_part".into(), SheetHint::default()),
        ],
        nets: vec![("SWCLK".into(), NetStyle::Label), ("LED0".into(), NetStyle::Wire)],
    }, "entries retain table order and combine path and pin hints, including pin-only paths");
    assert_eq!(got.notes.len(), 1, "only the missing path warns; components and block instances are valid");
    assert_eq!(got.notes[0].severity, Severity::Warning, "missing paths produce warnings, not errors");
    assert!(got.notes[0].message.contains("renamed_part"), "the missing-path warning names its path");
    assert_eq!(selected(&sources, &got.notes[0]), "\"renamed_part\"", "the warning selects the missing path's key");
}

#[test]
fn net_styles_round_trip_with_connections_and_part_hints() {
    let libs = std_libs();
    let (src, ev) = setup(concat!(
        "let e = mod.elec\r\n",
        "let aux = e.net({name: \"AUX\"})\r\nlet clk = e.net({name: \"CLK\"})\r\n",
        "let lamp = e.led({color: @green}); // last part\r\n",
    ), &libs);
    let circuit_op = |op| DesignOp::Circuit { unit: BoardId(0), op };
    let style = |name: &str, value| circuit_op(CircuitOp::NetStyle { name: name.into(), style: value });
    let apply = |src: &Sources, ev: &Evaluated, ops: &[DesignOp]| {
        splice_verified(src, &libs, ev, ops, &NO_CANCEL).expect("the sheet edits splice and re-evaluate in one commit")
    };

    let lamp = ev.design.units[0].circuit.comp("lamp").expect("the circuit contains the LED");
    let pin = (lamp.id, lamp.part.pin("A").expect("the LED has an anode"));
    let name = "CLK/OUT";
    let global = apply(&src, &ev, &[
        circuit_op(CircuitOp::ConnectNamed { pin, name: name.into() }), style(name, Some(NetStyle::Label)),
    ]);
    let c = &global.evaluated.design.units[0].circuit;
    let net = c.net(name).expect("the Global label creates its named net");
    assert_eq!(c.net_of(pin.0, pin.1), Some(net.id), "the lone pin joins the named net");
    assert_eq!(c.hints.nets, vec![(name.into(), NetStyle::Label)], "the new sheet styles that net as a label");
    assert!(global.sources.docs()[0].text.ends_with("e.connect(lamp.A, net_clk_out)\r\n\r\nmod.elec.sheet({\r\n    nets: {\"CLK/OUT\": @label}\r\n})\r\n"),
        "the connection precedes the first sheet, with CRLF and a sheet call in scope");
    let mut undone = global.sources;
    undone.apply(&global.edit.inverse(&src, &ev.map).edits).expect("the two-op inverse applies");
    assert_eq!(undone, src, "one undo restores the original circuit text");

    let hint = SheetHint { turn: 1, ..SheetHint::default() };
    let (mut src, mut ev) = after(&src, &ev, &libs, "lamp", Some(hint.clone()));
    for (ops, expected) in [
        (vec![style("CLK", Some(NetStyle::Label))], vec![("CLK", NetStyle::Label)]),
        (vec![style("CLK", Some(NetStyle::Wire)), style("AUX", Some(NetStyle::Label))],
            vec![("CLK", NetStyle::Wire), ("AUX", NetStyle::Label)]),
        (vec![style("CLK", None)], vec![("AUX", NetStyle::Label)]),
        (vec![style("AUX", None)], vec![]),
    ] {
        let next = apply(&src, &ev, &ops);
        let hints = &next.evaluated.design.units[0].circuit.hints;
        let nets: Vec<_> = hints.nets.iter().map(|(name, style)| (name.as_str(), *style)).collect();
        assert_eq!(nets, expected, "creating, replacing or removing a style preserves the other net entries: {ops:?}");
        assert_eq!(hints.parts, vec![("lamp".into(), hint.clone())], "each net edit preserves the existing part hint");
        assert!(next.sources.docs()[0].text.contains("    \"lamp\": {turn: 90deg}\r\n"),
            "each net edit preserves the part hint's original line");
        src = next.sources;
        ev = next.evaluated;
    }
}

#[test]
fn sheet_errors_select_the_field_or_second_call_and_libraries_are_refused() {
    let libs = Libs::new();
    for (tail, offending, message) in [
        (r#"sheet({"mcu": {turn: 45deg}})"#, "turn: 45deg", "multiple of 90deg"),
        (r#"sheet({"mcu": {mirror: 1}})"#, "mirror: 1", "mirror: expected true or false"),
        (r#"sheet({"mcu": {flow: @sideways}})"#, "flow: @sideways", "flow must be"),
        (r#"sheet({"mcu": {at: [1.5 2]}})"#, "at: [1.5 2]", "two integers"),
        (r#"sheet({"mcu": {other: 1}})"#, "other: 1", "unknown option other"),
        ("sheet({})\nsheet({})", "sheet({})", "only one sheet call"),
        (r#"sheet({"mcu": {flow: "down"}})"#, "flow: \"down\"", "flow must be"),
        (r#"sheet({"mcu.pins": {PA0: @down}})"#, "PA0: @down", "sheet pin PA0"),
        ("sheet({nets: {N: @other}})", "N: @other", "sheet net N"),
        (r#"sheet({"mcu": 3})"#, "3", "must be objects"),
    ] {
        let sources = sources(&format!("use mod.elec.*\n{tail}\n"));
        let errors = evaluate(&sources, &libs).expect_err("invalid sheet input fails evaluation");
        assert_eq!(errors.len(), 1, "{tail} gives one precise error");
        assert!(errors[0].message.contains(message), "{tail} explains {message}: {:?}", errors[0]);
        assert_eq!(selected(&sources, &errors[0]), offending, "{tail} selects the offending field or call");
        if message == "only one sheet call" {
            let Some(Sel::Source { start, .. }) = errors[0].sel.first() else { unreachable!() };
            assert_eq!(*start as usize, sources.docs()[0].text.rfind("sheet(").unwrap(),
                "the duplicate error selects the second sheet call");
        }
    }
    for name in ["helpers.part.splash", "helpers.blocks.splash"] {
        let mut libs = Libs::new();
        let doc = libs.add("helpers", name, "use mod.elec.*\nsheet({})\n").expect("the library is added");
        let errors = evaluate(&sources("use mod.elec.*\n"), &libs).expect_err("library sheet calls are refused");
        assert_eq!(errors.len(), 1, "a library sheet call gives one error");
        assert_eq!(errors[0].message, "sheet hints belong in the circuit document", "the library refusal explains ownership");
        assert!(matches!(errors[0].sel.first(), Some(Sel::Source { doc: at, start, end }) if *at == doc && end > start),
            "a library refusal selects the call in the library document");
    }
}

#[test]
fn hint_splices_append_replace_and_remove_the_fixture_table() {
    let libs = std_libs();
    let (original, evaluated) = setup(FIXTURE, &libs);
    assert_eq!(evaluated.design.units[0].circuit.hints, SheetHints::default(), "the unchanged fixture has no hints");
    let at = SheetHint { at: Some(SchPt::new(24, 10)), ..SheetHint::default() };
    let (first, first_eval) = after(&original, &evaluated, &libs, "mcu", Some(at));
    let first_table = "sheet({\n    \"mcu\": {at: [24 10]}\n})\n";
    assert_eq!(first.docs()[0].text.as_ref(), format!("{FIXTURE}\n{first_table}"),
        "the first hint appends one tail statement after a blank line");
    assert!(first.docs()[0].text.ends_with(first_table), "the appended sheet is the final statement");

    let second_hint = SheetHint {
        flow: Some(Flow::Down), pins: vec![("A".into(), PortSide::Right), ("B".into(), PortSide::Left)],
        ..SheetHint::default()
    };
    let second_lines = "    \"lamps[3].r\": {flow: @down}\n    \"lamps[3].r.pins\": {A: @right B: @left}\n";
    let (second, second_eval) = after(&first, &first_eval, &libs, "lamps[3].r", Some(second_hint));
    assert_eq!(second.docs()[0].text.as_ref(), format!("{FIXTURE}\nsheet({{\n    \"mcu\": {{at: [24 10]}}\n{second_lines}}})\n"),
        "a second path adds its two entry lines to the existing sheet");

    let turned = SheetHint { turn: 1, ..SheetHint::default() };
    let (third, third_eval) = after(&second, &second_eval, &libs, "mcu", Some(turned));
    assert_eq!(third.docs()[0].text.as_ref(), second.docs()[0].text.replace("\"mcu\": {at: [24 10]}", "\"mcu\": {turn: 90deg}"),
        "replacing a path edits it in place and keeps all other bytes unchanged");
    assert!(third.docs()[0].text.contains(second_lines), "the other path and pins remain byte for byte unchanged");

    let (fourth, fourth_eval) = after(&third, &third_eval, &libs, "lamps[3].r", None);
    assert_eq!(fourth.docs()[0].text.as_ref(), third.docs()[0].text.replace(second_lines, ""),
        "removing a hint removes both of its complete entry lines");
    let (last, last_eval) = after(&fourth, &fourth_eval, &libs, "mcu", None);
    assert_eq!(last.docs()[0].text.trim_end(), FIXTURE.trim_end(), "removing the last path removes the whole sheet statement");
    assert_eq!(last_eval.design.units[0].circuit.hints, SheetHints::default(), "removing the final hint restores empty hints");
}

#[test]
fn hint_splices_collapse_duplicates_preserve_nets_and_write_empty_hints() {
    let libs = Libs::new();
    let source = concat!(
        "use mod.elec.*\r\nlet e = mod.elec\r\n",
        "    e.sheet({\r\n",
        "        \"long_component_path\": {turn: 180deg},\r\n",
        "        nets: {\"CLOCK\": @label}, // keep this net style\r\n",
        "        \"long_component_path.pins\": {P: @left},\r\n",
        "        \"long_component_path\": {flow: @up mirror: false},\r\n",
        "        \"long_component_path.pins\": {P: @right},\r\n",
        "    });\r\n",
    );
    let (sources, got) = setup(source, &libs);
    assert_eq!(got.design.units[0].circuit.hints.parts, vec![("long_component_path".into(), SheetHint {
        flow: Some(Flow::Up), pins: vec![("P".into(), PortSide::Right)], ..SheetHint::default()
    })], "duplicate path and pin entries take their last values, including heap string keys");
    let replacement = SheetHint { at: Some(SchPt::new(-24, -10)), turn: 3, mirror: true, ..SheetHint::default() };
    let (replaced, replaced_eval) = after(&sources, &got, &libs, "long_component_path", Some(replacement.clone()));
    let expected = concat!(
        "use mod.elec.*\r\nlet e = mod.elec\r\n",
        "    e.sheet({\r\n",
        "        nets: {\"CLOCK\": @label}, // keep this net style\r\n",
        "        \"long_component_path\": {at: [-24, -10] turn: 270deg mirror: true},\r\n",
        "    });\r\n",
    );
    assert_eq!(replaced.docs()[0].text.as_ref(), expected,
        "duplicate cleanup replaces the last path, removes every old pin entry, and preserves CRLF and nets");
    let (cleared, cleared_eval) = after(&replaced, &replaced_eval, &libs, "long_component_path",
        Some(SheetHint { mirror: false, ..replacement }));
    assert_eq!(cleared.docs()[0].text.as_ref(), expected.replace(" mirror: true", ""),
        "clearing the mirror removes only that field and preserves the position, turn and net style");
    let (empty, empty_eval) = after(&cleared, &cleared_eval, &libs, "long_component_path", Some(SheetHint::default()));
    assert_eq!(empty.docs()[0].text.as_ref(), expected.replace("{at: [-24, -10] turn: 270deg mirror: true}", "{}"),
        "an unconstrained hint writes an explicit empty object");
    let (removed, removed_eval) = after(&empty, &empty_eval, &libs, "long_component_path", None);
    assert!(removed.docs()[0].text.contains("e.sheet({"), "a net-only sheet survives removal of its last path");
    assert_eq!(removed_eval.design.units[0].circuit.hints, SheetHints {
        parts: vec![], nets: vec![("CLOCK".into(), NetStyle::Label)],
    }, "removing a path preserves net styles");
    let (added, _) = after(&removed, &removed_eval, &libs, "new_path", Some(SheetHint {
        flow: Some(Flow::Left), turn: 2, mirror: true, pins: vec![("pin with spaces".into(), PortSide::Bottom)], free: true,
        ..SheetHint::default()
    }));
    assert!(added.docs()[0].text.contains(concat!(
        "        \"new_path\": {flow: @left turn: 180deg mirror: true free: true}\r\n",
        "        \"new_path.pins\": {\"pin with spaces\": @bottom}\r\n",
    )), "added entries use statement indentation plus four spaces, quote pin keys and retain CRLF");
}

#[test]
fn computed_sheet_entries_are_evaluated_but_refused_by_the_editor() {
    let libs = Libs::new();
    for (source, offending) in [
        ("let table = {\"mcu\": {at: [1 2]}}\nsheet(table)", "table"),
        ("let entry = {flow: @down}\nsheet({\"mcu\": entry})", "entry"),
        ("let point = [1 2]\nsheet({\"mcu\": {at: point}})", "point"),
        ("let flip = true\nsheet({\"mcu\": {mirror: flip}})", "flip"),
    ] {
        let (sources, got) = setup(&format!("use mod.elec.*\n{source}\n"), &libs);
        for hint in [Some(SheetHint::default()), None] {
            let error = splice(&sources, &got.map, &got.design, &[operation("mcu", hint)])
                .expect_err("the editor refuses computed sheet tables and values");
            assert_eq!(error.message, "the sheet table is written by the editor: write it as literal entries",
                "computed-entry refusals explain how to make the table editable");
            assert_eq!(selected(&sources, &error), offending, "the editor refusal selects the computed expression");
        }
    }
}

#[test]
fn pin_entries_before_paths_and_signed_turns_keep_their_meaning() {
    let libs = Libs::new();
    let (sources, got) = setup(concat!(
        "use mod.elec.*\n",
        "sheet({\"mcu.pins\": {P: @top} \"mcu\": {turn: -90deg} \"reg\": {turn: 450deg}})\n",
    ), &libs);
    assert_eq!(got.design.units[0].circuit.hints.parts, vec![
        ("mcu".into(), SheetHint { turn: 3, pins: vec![("P".into(), PortSide::Top)], ..SheetHint::default() }),
        ("reg".into(), SheetHint { turn: 1, ..SheetHint::default() }),
    ], "pin entries survive a later path entry and angles normalize modulo four");
    let (removed, removed_eval) = after(&sources, &got, &libs, "mcu", None);
    assert!(removed.docs()[0].text.contains("\"reg\": {turn: 450deg}"), "inline removals preserve unrelated entries");
    let (empty, _) = after(&removed, &removed_eval, &libs, "reg", None);
    assert_eq!(empty.docs()[0].text.as_ref(), "use mod.elec.*\n", "the final inline table is removed with its line");
}

#[test]
fn one_commit_inserts_and_replaces_hints_in_the_fixture_table() {
    let libs = std_libs();
    let text = format!("{FIXTURE}\nsheet({{\n    \"mcu\": {{at: [1 2]}}\n    \"reg\": {{flow: @left}}\n    \"reg.pins\": {{vin: @left}}\n    nets: {{LED0: @label}}\n}})\n");
    let (sources, evaluated) = setup(&text, &libs);
    let first = SheetHint { at: Some(SchPt::new(24, -10)), ..SheetHint::default() };
    let second = SheetHint { flow: Some(Flow::Down), turn: 1, ..SheetHint::default() };
    let replaced = SheetHint { at: Some(SchPt::new(12, 8)), ..SheetHint::default() };
    let change = splice(&sources, &evaluated.map, &evaluated.design, &[
        operation("lamps[3].r", Some(first.clone())),
        operation("mcu", Some(replaced.clone())),
        operation("btn", Some(second.clone())),
    ]).expect("one commit can insert two hints and replace an existing hint");
    assert!(change.reevaluate, "a batched hint commit requests evaluation");
    assert_eq!(change.edits.len(), 2, "two new hints share one insertion beside the replacement");
    let replacement = &change.edits[0];
    assert_eq!(&text[replacement.span.start as usize..replacement.span.end as usize], "\"mcu\": {at: [1 2]}",
        "the replacement selects only the existing path entry");
    assert_eq!(replacement.text, "\"mcu\": {at: [12 8]}", "the replacement writes the requested hint");
    let insertion = &change.edits[1];
    assert_eq!(insertion.span.start, insertion.span.end, "the new hints use a single insertion");
    assert_eq!(insertion.span.start as usize, text.rfind("})").unwrap(), "the insertion is at the table's close");
    assert_eq!(insertion.text, "    \"lamps[3].r\": {at: [24, -10]}\n    \"btn\": {flow: @down turn: 90deg}\n",
        "the insertion keeps new hints in commit order");
    assert!(replacement.span.end <= insertion.span.start, "replacement and insertion are sorted and disjoint");
    let inverse = change.inverse(&sources, &evaluated.map);
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("the batched hint edits apply together");
    let got = evaluate(&edited, &libs).expect("the batched fixture hints evaluate");
    let mut expected = evaluated.design.units[0].circuit.as_ref().clone();
    expected.hints.parts[0].1 = replaced;
    expected.hints.parts.push(("lamps[3].r".into(), first));
    expected.hints.parts.push(("btn".into(), second));
    assert_eq!(got.design.units[0].circuit.as_ref(), &expected,
        "all three hints change and untouched hints, pin sides, nets and circuit properties survive");
    edited.apply(&inverse.edits).expect("the batched hint inverse applies");
    assert_eq!(edited.docs()[0].text.as_ref(), text, "the batched hint inverse restores every original byte");
}

#[test]
fn one_commit_creates_one_sheet_for_all_new_hints() {
    let libs = std_libs();
    let (sources, evaluated) = setup(FIXTURE, &libs);
    let first = SheetHint { at: Some(SchPt::new(24, -10)), ..SheetHint::default() };
    let second = SheetHint { flow: Some(Flow::Down), turn: 1, ..SheetHint::default() };
    let change = splice(&sources, &evaluated.map, &evaluated.design, &[
        operation("lamps[3].r", Some(first.clone())),
        operation("btn", Some(second.clone())),
    ]).expect("one commit can create a sheet with two hints");
    assert_eq!(change.edits.len(), 1, "a missing sheet produces one insertion for the whole commit");
    assert_eq!(change.edits[0].span, FIXTURE.len() as u32..FIXTURE.len() as u32,
        "the new sheet is appended at the end of the document");
    let inverse = change.inverse(&sources, &evaluated.map);
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("the single sheet insertion applies");
    assert_eq!(edited.docs()[0].text.as_ref(), format!(
        "{FIXTURE}\nsheet({{\n    \"lamps[3].r\": {{at: [24, -10]}}\n    \"btn\": {{flow: @down turn: 90deg}}\n}})\n"
    ), "both hints are written in order inside exactly one new sheet statement");
    let got = evaluate(&edited, &libs).expect("the new sheet with two hints evaluates");
    assert_eq!(got.design.units[0].circuit.hints, SheetHints {
        parts: vec![("lamps[3].r".into(), first), ("btn".into(), second)], nets: vec![],
    }, "the new sheet evaluates to both requested hints in order");
    edited.apply(&inverse.edits).expect("the new sheet inverse applies");
    assert_eq!(edited.docs()[0].text.as_ref(), FIXTURE, "undoing sheet creation restores every fixture byte");
    let removed = splice(&sources, &evaluated.map, &evaluated.design, &[
        operation("lamps[3].r", None), operation("btn", None),
    ]).expect("removing hints without a sheet is valid");
    assert!(removed.edits.is_empty(), "an all-removal commit does not create an empty sheet");
}

#[test]
fn batched_adjacent_and_complete_removals_are_disjoint_and_reversible() {
    let libs = Libs::new();
    for (newline, table, remaining) in [
        ("\n", concat!(
            "sheet({\n",
            "    \"first\": {at: [1 2]}\n",
            "    \"second\": {turn: 180deg}\n",
            "    \"kept\": {flow: @up}\n",
            "})\n",
        ), "sheet({\n    \"kept\": {flow: @up}\n})\n"),
        ("\n", "sheet({\"first\": {} \"second\": {} \"kept\": {flow: @up}});\n",
            "sheet({  \"kept\": {flow: @up}});\n"),
        ("\n", "sheet({\"first\": {}, \"second\": {}, \"kept\": {flow: @up}});\n",
            "sheet({  \"kept\": {flow: @up}});\n"),
        ("\r\n", concat!(
            "    e.sheet({\r\n",
            "        \"first\": {}, // remove this line\r\n",
            "        \"first.pins\": {P: @left},\r\n",
            "        \"second\": {},\r\n",
            "        \"second.pins\": {P: @right},\r\n",
            "        \"kept\": {flow: @up}, // keep this line\r\n",
            "    });\r\n",
        ), "    e.sheet({\r\n        \"kept\": {flow: @up}, // keep this line\r\n    });\r\n"),
    ] {
        let prefix = format!("use mod.elec.*{newline}let e = mod.elec{newline}");
        let suffix = format!("let unchanged = 1{newline}");
        let text = format!("{prefix}{table}{suffix}");
        let (sources, evaluated) = setup(&text, &libs);
        for remove_all in [false, true] {
            // Reverse source order to exercise sorting independently of commit order.
            let mut ops = vec![operation("second", None), operation("first", None)];
            if remove_all { ops.push(operation("kept", None)); }
            let change = splice(&sources, &evaluated.map, &evaluated.design, &ops)
                .expect("adjacent and complete hint removals splice as one commit");
            assert!(change.edits.iter().all(|edit| edit.text.is_empty()), "removing hints emits only deletions");
            assert!(change.edits.windows(2).all(|pair| pair[0].span.end <= pair[1].span.start),
                "adjacent removals produce sorted disjoint edits");
            let inverse = change.inverse(&sources, &evaluated.map);
            let mut edited = sources.clone();
            edited.apply(&change.edits).expect("adjacent removal spans apply together");
            let got = evaluate(&edited, &libs).expect("the remaining table and surrounding statements evaluate");
            if remove_all {
                assert_eq!(change.edits.len(), 1, "removing every hint deletes the statement in one edit");
                assert_eq!(edited.docs()[0].text.as_ref(), format!("{prefix}{suffix}"),
                    "removing every entry deletes the statement line, including indentation and semicolon");
                assert_eq!(got.design.units[0].circuit.hints, SheetHints::default(),
                    "removing every entry leaves no sheet hints");
            } else {
                assert_eq!(edited.docs()[0].text.as_ref(), format!("{prefix}{remaining}{suffix}"),
                    "adjacent removals preserve the remaining entry, separators, comments and newlines");
                assert_eq!(got.design.units[0].circuit.hints, SheetHints {
                    parts: vec![("kept".into(), SheetHint { flow: Some(Flow::Up), ..SheetHint::default() })], nets: vec![],
                }, "adjacent removals erase both paths and their pins, leaving only the untouched hint");
            }
            edited.apply(&inverse.edits).expect("the removal inverse applies");
            assert_eq!(edited.docs()[0].text.as_ref(), text, "undoing removals restores all original bytes");
        }
    }
}

#[test]
fn repeated_hint_paths_use_the_last_op_in_commit_order() {
    let libs = Libs::new();
    for table in ["", "sheet({\"part\": {turn: 180deg} \"part.pins\": {P: @left}})\n"] {
        let text = format!("use mod.elec.*\n{table}");
        let (sources, evaluated) = setup(&text, &libs);
        let first = SheetHint { turn: 1, pins: vec![("P".into(), PortSide::Right)], ..SheetHint::default() };
        let last = SheetHint { at: Some(SchPt::new(-4, 6)), flow: Some(Flow::Left), ..SheetHint::default() };
        for (before, after) in [
            (Some(first.clone()), Some(last.clone())),
            (Some(first), None),
            (None, Some(last.clone())),
        ] {
            let change = splice(&sources, &evaluated.map, &evaluated.design, &[
                operation("part", before),
                operation("middle", Some(SheetHint::default())),
                operation("part", after.clone()),
            ]).expect("a repeated path splices once using its last hint or removal");
            let inverse = change.inverse(&sources, &evaluated.map);
            let mut edited = sources.clone();
            edited.apply(&change.edits).expect("repeated hints produce disjoint edits");
            let got = evaluate(&edited, &libs).expect("the final hint for a repeated path evaluates");
            let mut expected = vec![("middle".into(), SheetHint::default())];
            if let Some(hint) = after {
                if table.is_empty() { expected.push(("part".into(), hint)); }
                else { expected.insert(0, ("part".into(), hint)); }
            }
            assert_eq!(got.design.units[0].circuit.hints.parts, expected,
                "the last op wins, clears earlier pins and orders additions by their final commit positions");
            edited.apply(&inverse.edits).expect("the repeated-path inverse applies");
            assert_eq!(edited.docs()[0].text.as_ref(), text, "undoing repeated hints restores every original byte");
        }
    }
}
