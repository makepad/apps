//! A part's `value:` is a quantity in its kind's unit (ohms, farads, henries, hertz), as e.r and e.c
//! take theirs, or a label for a kind without one; anything else is refused at its call.

use fab_elec_circuit::{QtyUnit, Quantity, Value};
use fab_elec_core::{Sel, Severity};
use fab_elec_lang::{evaluate, std_libs, Sources, UnitState};

const HEAD: &str = "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\n";

#[test]
fn part_values_take_their_kinds_quantity_or_a_label() {
    let mut sources = Sources::new();
    sources.add("values.ckt.splash", format!(r#"{HEAD}let r = e.part(chip("R", "0402"), {{value: 4.7kohm}})
let c = e.part(chip("C", "0402"), {{value: 100nF}})
let l = e.part(chip("L", "0402"), {{value: 10uH}})
let y = e.part(crystal_part("3225"), {{value: 16MHz}})
let jumper = e.part(chip("R", "0402"), {{value: 0ohm}})
let sw = e.part(tact_switch(), {{value: "reset"}})
"#)).expect("the valid values document is accepted");
    let refused = [
        ("r_label", r#"e.part(chip("R", "0402"), {value: "10k"})"#,
            r#"value: a resistor's value is a quantity in ohms (not negative), got "10k""#),
        ("r_negative", r#"e.part(chip("R", "0402"), {value: -1ohm})"#,
            "value: a resistor's value is a quantity in ohms (not negative), got -1"),
        ("y_label", r#"e.part(crystal_part("3225"), {value: "16MHz"})"#,
            r#"value: a crystal's value is a quantity in hertz (not negative), got "16MHz""#),
        ("sw_number", r#"e.part(tact_switch(), {value: 5})"#,
            "value: a switch's value is a label, got 5 (resistors, capacitors, inductors and crystals take quantities)"),
    ];
    let text = format!("{HEAD}{}", refused.iter().map(|(name, call, _)| format!("let {name} = {call}\n")).collect::<String>());
    let bad = sources.add("refused.ckt.splash", text.clone()).expect("the refused values document is accepted");
    let ev = evaluate(&sources, &std_libs()).expect("evaluation reports refused values per circuit");

    let circuit = &ev.design.units.iter().find(|u| u.circuit.name == "values").expect("the valid circuit lowers").circuit;
    let value = |path: &str| circuit.comps.iter().find(|c| c.path == path)
        .unwrap_or_else(|| panic!("{path}: the component exists")).value.clone();
    let quantity = |si: f64, unit: QtyUnit, text: &str| Some(Value { text: text.into(), qty: Some(Quantity { si, unit }), tol: None });
    assert_eq!(value("r"), quantity(4700.0, QtyUnit::Ohm, "4.7k"), "a resistor part's value is its resistance");
    assert_eq!(value("c"), quantity(100e-9, QtyUnit::Farad, "100nF"), "a capacitor part's value is its capacitance");
    assert_eq!(value("l"), quantity(10e-6, QtyUnit::Henry, "10uH"), "an inductor part's value is its inductance");
    assert_eq!(value("y"), quantity(16e6, QtyUnit::Hertz, "16MHz"), "a crystal part's value is its frequency");
    assert_eq!(value("jumper"), quantity(0.0, QtyUnit::Ohm, "0"), "a zero-ohm jumper is a resistor value");
    assert_eq!(value("sw"), Some(Value { text: "reset".into(), qty: None, tol: None }), "a switch's value is a label");

    let failed = ev.units.iter().find(|u| u.docs.contains(&bad)).expect("the refused circuit has a unit");
    assert!(matches!(failed.state, UnitState::Failed(_)), "refused values fail their circuit");
    let diags = failed.state.diags();
    assert_eq!(diags.len(), refused.len(), "each refused value is one refusal: {diags:?}");
    for (name, call, message) in refused {
        let expected = format!("{name}: {message}");
        let diag = diags.iter().find(|d| d.message == expected)
            .unwrap_or_else(|| panic!("{name}: expected {expected:?}, got {diags:?}"));
        assert_eq!(diag.severity, Severity::Error, "{name}: a refused value is an error");
        let start = text.find(call).expect("the call is in the document") as u32;
        assert_eq!(diag.sel.first(), Some(&Sel::Source { doc: bad, start, end: start + call.len() as u32 }),
            "{name}: the refusal selects its e.part call");
    }
}
