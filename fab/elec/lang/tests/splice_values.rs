//! L2: a value edit splices where the value is written (architect change 3), and evaluating after
//! the splice agrees with the op (change 4); a commit's edits come as one splice (change 5).

use fab_elec_circuit::{Circuit, CircuitOp, QtyUnit, Quantity, Value};
use fab_elec_core::{BoardId, CompId, CompRef, Sel};
use fab_elec_design::DesignOp;
use fab_elec_lang::eseries::{nearest, Series};
use fab_elec_lang::led::forward_volts_at;
use fab_elec_lang::{evaluate, source::line_col, splice, std_libs, Evaluated, Sources, ValueSite};
use fab_elec_parts::Tol;

const LEDCTL: &str = include_str!("../../fixtures/ledctl.ckt.splash");

fn qty(si: f64, unit: QtyUnit, text: &str) -> Value {
    Value { text: text.into(), qty: Some(Quantity { si, unit }), tol: None }
}

fn comp(c: &Circuit, path: &str) -> CompId {
    c.comps.iter().find(|x| x.path == path).unwrap_or_else(|| panic!("{path} exists")).id
}

fn set(c: &Circuit, path: &str, value: Value) -> DesignOp {
    DesignOp::Circuit { unit: BoardId(0), op: CircuitOp::SetValue { comp: comp(c, path), value } }
}

/// Splice `ops`, apply the edits and evaluate again: the text after and the new evaluation.
fn after(src: &Sources, ev: &Evaluated, ops: &[DesignOp]) -> (String, Evaluated) {
    let e = splice(src, &ev.map, &ev.design, ops).unwrap_or_else(|d| panic!("the splice is written: {}", d.message));
    assert!(e.reevaluate, "a circuit edit asks for evaluation");
    let mut src = src.clone();
    src.apply(&e.edits).expect("the edits apply");
    let text = src.docs()[0].text.to_string();
    let ev = evaluate(&src, &std_libs()).unwrap_or_else(|d| panic!("the spliced text evaluates: {:?}\n{text}", d.iter().map(|d| &d.message).collect::<Vec<_>>()));
    (text, ev)
}

fn ohms_of(ev: &Evaluated, path: &str) -> f64 {
    let c = &ev.design.units[0].circuit;
    c.comps[comp(c, path).ix()].value.as_ref().and_then(|v| v.qty).unwrap_or_else(|| panic!("{path} has a quantity")).si
}

fn value_of(ev: &Evaluated, path: &str) -> String {
    let c = &ev.design.units[0].circuit;
    c.comps[comp(c, path).ix()].value.as_ref().map(|v| v.text.clone()).unwrap_or_default()
}

#[test]
fn tolerance_edits_round_trip_with_values_and_preserve_computed_tolerances() {
    let source = concat!(
        "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\n",
        "let a = e.net({})\nlet b = e.net({})\n",
        "let add = e.r(10kohm, a, b)\n",
        "let replace = e.r(10kohm 1%, a, b)\n",
        "let remove = e.r(10kohm 1%, a, b)\n",
        "let absolute = e.c(10pF, a, b)\n",
        "let asymmetric = e.c(10uF 10%, a, b, {package: \"0603\"})\n",
        "let new_options = e.c(10uF 10%, a, b)\n",
        "let option = e.r(10kohm, a, b, {tol: 1%, package: \"0603\"})\n",
        "let remove_option = e.r(10kohm, a, b, {package: \"0603\", tol: 1%})\n",
        "let ppm = e.r(1kohm 20ppm, a, b)\n",
        "let new_ppm = e.r(1kohm, a, b)\n",
        "let cap_prefix = e.c(100nF, a, b)\n",
        "let t = 5%\nlet computed = e.r(10kohm t, a, b)\n",
        "let pair = e.c(10pF, a, b, {tol: [-0.5pF, 1pF]})\n",
        "let unchanged_option = e.r(10kohm, a, b, {tol: 5%})\n",
        "let precision = e.r(10kohm 1%, a, b)\n",
    );
    let mut src = Sources::new();
    src.add("values.ckt.splash", source.to_string()).expect("the circuit is added");
    let ev = evaluate(&src, &std_libs()).expect("the original tolerances evaluate");
    let c = &ev.design.units[0].circuit;
    let asymmetric = Some(Tol::Pct { minus: 20.0, plus: 80.0 });
    let cases = [
        ("add", 5100.0, QtyUnit::Ohm, "5.1k", Some(Tol::pct(5.0)), "e.r(5.1kohm 5%, a, b)"),
        ("replace", 10_000.0, QtyUnit::Ohm, "10k", Some(Tol::pct(0.5)), "e.r(10kohm 0.5%, a, b)"),
        ("remove", 10_000.0, QtyUnit::Ohm, "10k", None, "e.r(10kohm, a, b)"),
        ("absolute", 1e-11, QtyUnit::Farad, "10pF", Some(Tol::abs(5e-13)), "e.c(10pF 0.5pF, a, b)"),
        ("asymmetric", 1e-5, QtyUnit::Farad, "10uF", asymmetric, "e.c(10uF, a, b, {package: \"0603\" tol: [-20%, 80%]})"),
        ("new_options", 1e-5, QtyUnit::Farad, "10uF", asymmetric, "e.c(10uF, a, b, {tol: [-20%, 80%]})"),
        ("option", 10_000.0, QtyUnit::Ohm, "10k", Some(Tol::pct(0.5)), "e.r(10kohm 0.5%, a, b, { package: \"0603\"})"),
        ("remove_option", 10_000.0, QtyUnit::Ohm, "10k", None, "e.r(10kohm, a, b, {package: \"0603\"})"),
        ("ppm", 1000.0, QtyUnit::Ohm, "1k", Some(Tol::pct(0.003)), "e.r(1kohm 30ppm, a, b)"),
        ("new_ppm", 1000.0, QtyUnit::Ohm, "1k", Some(Tol::pct(0.002)), "e.r(1kohm 20ppm, a, b)"),
        ("cap_prefix", 1e-7, QtyUnit::Farad, "100nF", Some(Tol::abs(5e-10)), "e.c(100nF 500pF, a, b)"),
        ("computed", 5100.0, QtyUnit::Ohm, "5.1k", Some(Tol::pct(5.0)), "e.r(5.1kohm t, a, b)"),
        ("pair", 1e-11, QtyUnit::Farad, "10pF", Some(Tol::Abs { minus: 1e-12, plus: 2e-12 }), "e.c(10pF, a, b, {tol: [-1pF, 2pF]})"),
        ("unchanged_option", 5100.0, QtyUnit::Ohm, "5.1k", Some(Tol::pct(5.0)), "e.r(5.1kohm, a, b, {tol: 5%})"),
        ("precision", 10_000.0, QtyUnit::Ohm, "10k", Some(Tol::pct(0.123456789)), "e.r(10kohm 0.123456789%, a, b)"),
    ];
    let ops: Vec<_> = cases.iter().map(|&(path, si, unit, text, tol, _)| {
        set(c, path, Value { tol, ..qty(si, unit, text) })
    }).collect();
    let (text, got) = after(&src, &ev, &ops);
    let got = &got.design.units[0].circuit;
    for ((path, _, _, _, _, written), operation) in cases.iter().zip(&ops) {
        assert!(text.contains(&format!("let {path} = {written}\n")), "{path}: only the requested value and tolerance change their spelling:\n{text}");
        let DesignOp::Circuit { op: CircuitOp::SetValue { value, .. }, .. } = operation else { unreachable!() };
        assert_eq!(got.comps[comp(got, path).ix()].value.as_ref(), Some(value), "{path}: splice then evaluate equals apply for text, quantity and tolerance");
        assert_eq!(got.comps[comp(got, path).ix()].conns, c.comps[comp(c, path).ix()].conns, "{path}: tolerance changes preserve both connections");
    }
    for tol in [None, Some(Tol::pct(1.0))] {
        let value = Value { tol, ..qty(10_000.0, QtyUnit::Ohm, "10k") };
        let error = splice(&src, &ev.map, &ev.design, &[set(c, "computed", value)]).expect_err("changing or removing a computed tolerance is refused");
        assert!(error.message.contains("tolerance is computed"), "a computed tolerance refusal explains the expression");
        let Some(Sel::Source { start, end, .. }) = error.sel.first() else { panic!("a computed tolerance refusal has a source location") };
        assert_eq!(&source[*start as usize..*end as usize], "t", "the refusal selects the tolerance expression");
    }
}

#[test]
fn value_edits_splice_where_the_value_is_written() {
    let mut src = Sources::new();
    src.add("ledctl.ckt.splash", LEDCTL.to_string()).expect("a circuit document");
    let ev = evaluate(&src, &std_libs()).expect("the fixture evaluates");
    let c: Circuit = (*ev.design.units[0].circuit).clone();
    let site = |path: &str| ev.map.value_site(&CompRef { board: BoardId(0), comp: comp(&c, path) }).cloned().unwrap_or_else(|| panic!("{path} has a value site"));
    let text = |r: &fab_elec_lang::SourceRange| LEDCTL[r.span.start as usize..r.span.end as usize].to_string();

    // The sites: what the chip says before the drop.
    match site("mcu.rst_c") {
        ValueSite::Literal { at, count } => assert_eq!((text(&at).as_str(), count), ("100nF", 1), "rst_c: its own literal"),
        other => panic!("rst_c: a literal, got {other:?}"),
    }
    match site("reg.c_in") {
        ValueSite::Param { name, item, call, default, count } => {
            assert_eq!((name.as_str(), item, count), ("c_in", None, 1), "c_in: a parameter of one instance");
            assert!(text(&call).starts_with("ldo({"), "c_in: overridden at reg's call, not {:?}", text(&call));
            assert_eq!(default.map(|d| text(&d)).as_deref(), Some("1uF"), "c_in: the default literal");
        }
        other => panic!("c_in: a parameter, got {other:?}"),
    }
    assert!(matches!(site("mcu.dec[2]"), ValueSite::Param { ref name, item: Some(2), .. } if name == "decoupling"), "dec[2]: item 2 of a list parameter");
    assert!(matches!(site("lamps[3].d"), ValueSite::Param { ref name, count: 8, .. } if name == "color"), "lamps[3].d: a parameter of a call eight instances share");
    match site("lamps[3].r") {
        ValueSite::Derived { at, leaf, inputs, siblings } => {
            assert!(
                text(&at).starts_with("e.series("),
                "the resistor's value is computed by its expression"
            );
            assert_eq!((leaf, siblings), (None, 0), "the expression remains a refusal");
            assert_eq!(inputs.len(), 2, "only current and sink are inputs, not port voltage or the LED's forward voltage");
            for (input, expected) in inputs.iter().zip(["current", "sink"]) {
                assert_eq!(text(&input.at), format!("o.{expected}"), "the parameter input selects its access");
                assert_eq!(input.value, None, "a parameter has no static numeric value");
                let ValueSite::Param { name, item, call, count, .. } = &input.site else { panic!("{expected}: an editable parameter site") };
                assert_eq!((name.as_str(), *item, *count), (expected, None, 8), "the parameter belongs to the shared instance call");
                assert_eq!(text(call), "led_driver({ctl: led[i] vdd: v33})", "the input overrides the led_driver instance");
            }
        }
        other => panic!("lamps[3].r: computed, got {other:?}"),
    }

    // Change 4: splice then evaluate equals applying the op where the value feeds nothing else.
    let mut want = c.clone();
    for (path, v) in [("reg.c_in", qty(2.2e-6, QtyUnit::Farad, "2.2uF")), ("mcu.dec[2]", qty(10e-6, QtyUnit::Farad, "10uF")), ("mcu.boot_r", qty(4700.0, QtyUnit::Ohm, "4.7k"))] {
        let (text, got) = after(&src, &ev, &[set(&c, path, v.clone())]);
        want.comps[comp(&c, path).ix()].value = Some(v.clone());
        let got = &got.design.units[0].circuit;
        assert_eq!(got.comps[comp(&c, path).ix()], want.comps[comp(&c, path).ix()], "{path}: the evaluated component has the op's value");
        for (g, w) in got.comps.iter().zip(&c.comps).filter(|(g, _)| g.path != path) {
            assert_eq!(g, w, "{path}: no other component changes ({})", w.path);
        }
        want.comps[comp(&c, path).ix()].value = c.comps[comp(&c, path).ix()].value.clone();
        match path {
            "reg.c_in" => assert!(text.contains("ldo({vin: vbus gnd: gnd vout: v33 c_in: 2.2uF})"), "c_in: an override at reg's call:\n{text}"),
            "mcu.dec[2]" => assert!(text.contains("gpio: led user: key decoupling: [100nF 100nF 10uF]})"), "dec[2]: the list overridden at mcu's call:\n{text}"),
            _ => assert!(text.contains("e.r(4.7kohm, u.BOOT0"), "boot_r: its literal, its suffix kept:\n{text}"),
        }
    }

    // Where a value feeds more items, evaluation is the truth: both pull-downs read `rd`, and all
    // eight LEDs read the shared call's colour (their resistors follow the new forward voltage).
    let (_, got) = after(&src, &ev, &[set(&c, "usb.cc[0]", qty(4700.0, QtyUnit::Ohm, "4.7k"))]);
    assert_eq!((value_of(&got, "usb.cc[0]"), value_of(&got, "usb.cc[1]")), ("4.7k".into(), "4.7k".into()), "rd feeds both pull-downs");
    let (text, got) = after(&src, &ev, &[set(&c, "lamps[3].d", Value { text: "red".into(), qty: None, tol: None })]);
    assert!(text.contains("led_driver({ctl: led[i] vdd: v33 color: @red})"), "the colour is a name:\n{text}");
    assert!((0..8).all(|i| value_of(&got, &format!("lamps[{i}].d")) == "red"), "every lamp is red");
    // The red lamps are sized at the red curve: the resistor takes what the LED at 5 mA and the
    // 25 ohm sinking pin leave of the 3.3 V rail, and the current it then drives lands inside the
    // bringup bench's 4.5..5.5 mA window.
    let r = ohms_of(&got, "lamps[0].r");
    let vf = forward_volts_at("red", 0.005).expect("the red curve");
    assert_eq!(r, nearest((3.3 - vf) / 0.005 - 25.0, Series::E24), "red: (3.3 V - Vf(5 mA)) / 5 mA - 25 ohm, in E24");
    let (mut lo, mut hi) = (0.0, 3.3 / r);
    for _ in 0..64 {
        let i = 0.5 * (lo + hi);
        if forward_volts_at("red", i).expect("the red curve") + i * (r + 25.0) < 3.3 { lo = i } else { hi = i }
    }
    let on = 0.5 * (lo + hi);
    assert!((0.0045..=0.0055).contains(&on), "red: {r} ohm drives {on} A through the red curve and the 25 ohm pin, inside 4.5..5.5 mA");

    // A computed value is a located refusal naming the expression.
    let refused = splice(&src, &ev.map, &ev.design, &[set(&c, "lamps[3].r", qty(1000.0, QtyUnit::Ohm, "1k"))]).expect_err("computed");
    assert!(refused.message.contains("lamps[3].r: the value is computed by `e.series("), "{}", refused.message);
    let Sel::Source { start, .. } = refused.sel[0] else { panic!("located") };
    let line = LEDCTL.lines().position(|l| l.contains("let r = e.r(e.series(")).expect("the line") as u32 + 1;
    assert_eq!(line_col(LEDCTL, start).map(|l| l.0), Some(line), "at the expression");
    assert!(refused.message.ends_with("; inputs: `o.current` (parameter current), `o.sink` (parameter sink)"), "{}", refused.message);
    let selections: Vec<_> = refused.sel.iter().skip(1).map(|sel| {
        let Sel::Source { start, end, .. } = sel else { panic!("each input is a located selection") };
        &LEDCTL[*start as usize..*end as usize]
    }).collect();
    assert_eq!(selections, ["o.current", "o.sink"], "the refusal locates exactly its two parameter inputs");

    // Change 5: one commit, one splice with both edits; two ops on one literal are refused.
    let ops = [set(&c, "mcu.rst_c", qty(220e-9, QtyUnit::Farad, "220nF")), set(&c, "reg.c_out", qty(4.7e-6, QtyUnit::Farad, "4.7uF"))];
    let e = splice(&src, &ev.map, &ev.design, &ops).expect("one commit");
    assert_eq!(e.edits.len(), 2, "two edits in one splice");
    let (_, got) = after(&src, &ev, &ops);
    assert_eq!((value_of(&got, "mcu.rst_c"), value_of(&got, "reg.c_out")), ("220nF".into(), "4.7uF".into()), "both values land");
    let twice = [set(&c, "mcu.rst_c", qty(220e-9, QtyUnit::Farad, "220nF")), set(&c, "mcu.rst_c", qty(330e-9, QtyUnit::Farad, "330nF"))];
    assert!(splice(&src, &ev.map, &ev.design, &twice).expect_err("one text twice").message.contains("two ops of one commit"), "a clash is refused");
}

#[test]
fn value_edits_follow_loop_references() {
    let source = concat!(
        "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\n",
        "let a = e.net({})\nlet b = e.net({})\n",
        "let caps = [100nF 1uF]\n",
        "let indexed = for i in 0..2 { e.c(caps[i], a, b, {package: \"0402\"}) }\n",
        "let base = 10nF\n",
        "let shared = for i in 0..3 { e.c(base, a, b, {package: \"0402\"}) }\n",
        "let matrix = [[2nF 3nF] [4nF 5nF]]\n",
        "let nested = for i in 0..2 { for j in 0..2 { e.c(matrix[i][j], a, b, {package: \"0402\"}) } }\n",
        "let listed = for v in [6nF 7nF] { e.c(v, a, b, {package: \"0402\"}) }\n",
        "let dec = decoupling({vdd: a gnd: b values: [8nF 9nF]})\n",
    );
    let mut src = Sources::new();
    src.add("references.ckt.splash", source).expect("the circuit is added");
    let ev = evaluate(&src, &std_libs()).expect("the loop references evaluate");
    let c = &ev.design.units[0].circuit;
    for (path, written, readers) in [
        ("indexed[0]", "100nF", 1), ("indexed[1]", "1uF", 1),
        ("shared[0]", "10nF", 3), ("shared[1]", "10nF", 3), ("shared[2]", "10nF", 3),
        ("nested[0][0]", "2nF", 1), ("nested[0][1]", "3nF", 1),
        ("nested[1][0]", "4nF", 1), ("nested[1][1]", "5nF", 1),
        ("listed[0]", "6nF", 1), ("listed[1]", "7nF", 1),
    ] {
        let site = ev.map.value_site(&CompRef { board: BoardId(0), comp: comp(c, path) }).expect("the capacitor has a value site");
        let ValueSite::Literal { at, count } = site else { panic!("{path}: a resolved literal, got {site:?}") };
        assert_eq!(&source[at.span.start as usize..at.span.end as usize], written, "{path}: the reference selects its written number");
        assert_eq!(*count, readers, "{path}: count includes every component reading this number");
    }
    for i in 0..2 {
        let site = ev.map.value_site(&CompRef { board: BoardId(0), comp: comp(c, &format!("dec.caps[{i}]")) }).expect("the library capacitor has a value site");
        let ValueSite::Param { name, item, call, count, .. } = site else { panic!("a library list parameter remains editable, got {site:?}") };
        assert_eq!((name.as_str(), *item, *count), ("values", Some(i), 1), "the library loop selects its parameter item");
        assert_eq!(&source[call.span.start as usize..call.span.end as usize], "decoupling({vdd: a gnd: b values: [8nF 9nF]})", "the library parameter is overridden in the project");
    }
    let replacement = qty(2.2e-6, QtyUnit::Farad, "2.2uF");
    let ops = [set(c, "indexed[1]", replacement.clone())];
    let edit = splice(&src, &ev.map, &ev.design, &ops).expect("the list item can be edited");
    assert_eq!(edit.edits.len(), 1, "only one list item is edited");
    assert_eq!(&source[edit.edits[0].span.start as usize..edit.edits[0].span.end as usize], "1uF", "only the second item's literal is replaced");
    let (text, got) = after(&src, &ev, &ops);
    assert_eq!(text, source.replacen("1uF", "2.2uF", 1), "the list's other item and both loops are untouched");
    let got = &got.design.units[0].circuit;
    for (before, after) in c.comps.iter().zip(&got.comps) {
        if before.path == "indexed[1]" {
            let mut expected = before.clone();
            expected.value = Some(replacement.clone());
            assert_eq!(after, &expected, "the selected capacitor evaluates to the requested value");
        } else {
            assert_eq!(after, before, "{}: every other component is unchanged", before.path);
        }
    }
}
