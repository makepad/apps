//! Part GPIO cards keep SI values and refuse malformed fields at their source expressions.

use fab_elec_core::{Canon, Diag, DocId, Sel, Severity, Writer};
use fab_elec_lang::{evaluate, std_libs, Sources, UnitState};
use fab_elec_parts::GpioCard;

const CARD: &str = "r_high: 26ohm r_low: 23.5ohm pull_up: 36kohm vih: 0.524 vil: 0.418 leak: 1uA";

fn add_circuit(sources: &mut Sources, name: &str, text: &str) -> DocId {
    sources.add(format!("{name}.ckt.splash"), format!(r#"let e = mod.elec
let symbol = e.f.ic_symbol("MCU", {{left: [["IO", @bidi]]}})
{text}
"#)).expect("each GPIO case has a distinct circuit document")
}

fn assert_at(diag: &Diag, doc: DocId, text: &str, spec: &str, selected: &str) {
    let start = (text.find(spec).expect("the malformed GPIO card is in the source")
        + spec.find(selected).expect("the GPIO refusal's selected expression is in its card")) as u32;
    assert_eq!(diag.sel.first(), Some(&Sel::Source {
        doc, start, end: start + selected.len() as u32,
    }), "{}: the refusal must select its offending field value or the object for a missing field", diag.message);
}

#[test]
fn gpio_cards_read_and_refuse_located() {
    let mut sources = Sources::new();
    add_circuit(&mut sources, "cards", &format!(r#"
let typical = e.part(e.define("MCU_X", {{kind: @mcu symbol: symbol gpio: {{{CARD}}}}}), {{}})
let edge = e.part(e.define("MCU_EDGE", {{kind: @mcu symbol: symbol gpio: {{{CARD} edge: 5ns}}}}), {{}})
let generic = e.part(e.define("MCU_GENERIC", {{kind: @mcu symbol: symbol}}), {{}})
let nil_edge = e.part(e.define("MCU_NIL_EDGE", {{kind: @mcu symbol: symbol gpio: {{{CARD} edge: nil}}}}), {{}})
let cap = e.part(e.define("MCU_CAP", {{kind: @mcu symbol: symbol gpio: {{{CARD} c_pin: 5pF}}}}), {{}})
let zero_cap = e.part(e.define("MCU_ZERO_CAP", {{kind: @mcu symbol: symbol gpio: {{{CARD} c_pin: 0}}}}), {{}})
let nil_cap = e.part(e.define("MCU_NIL_CAP", {{kind: @mcu symbol: symbol gpio: {{{CARD} c_pin: nil}}}}), {{}})
"#));
    let cases = [
        (
            "missing_low",
            "{r_high: 26ohm pull_up: 36kohm vih: 0.524 vil: 0.418 leak: 1uA}",
            "gpio: r_low is required",
            None,
        ),
        (
            "thresholds",
            "{r_high: 26ohm r_low: 23.5ohm pull_up: 36kohm vil: 0.6 vih: 0.5 leak: 1uA}",
            "gpio: vil and vih are fractions of the supply with vil < vih < 1, got vil 0.6 vih 0.5",
            Some("0.5"),
        ),
        (
            "unknown_key",
            "{r_high: 26ohm r_low: 23.5ohm pull_up: 36kohm vih: 0.524 vil: 0.418 leak: 1uA r_hi: 27ohm}",
            "gpio: unknown option r_hi (the options are r_high, r_low, pull_up, vih, vil, leak, edge, c_pin)",
            Some("27ohm"),
        ),
        (
            "string_leak",
            r#"{r_high: 26ohm r_low: 23.5ohm pull_up: 36kohm vih: 0.524 vil: 0.418 leak: "1uA"}"#,
            r#"gpio: leak: expected a positive number, got "1uA""#,
            Some(r#""1uA""#),
        ),
        (
            "negative_pull_up",
            "{r_high: 26ohm r_low: 23.5ohm pull_up: -36kohm vih: 0.524 vil: 0.418 leak: 1uA}",
            "gpio: pull_up: expected a positive number, got -36000",
            Some("-36kohm"),
        ),
        (
            "negative_cap",
            "{r_high: 26ohm r_low: 23.5ohm pull_up: 36kohm vih: 0.524 vil: 0.418 leak: 1uA c_pin: -1}",
            "gpio: c_pin: expected a finite non-negative number, got -1",
            Some("-1"),
        ),
        (
            "infinite_cap",
            "{r_high: 26ohm r_low: 23.5ohm pull_up: 36kohm vih: 0.524 vil: 0.418 leak: 1uA c_pin: 1e309}",
            "gpio: c_pin: expected a finite non-negative number, got inf",
            Some("1e309"),
        ),
        (
            "nan_cap",
            "{r_high: 26ohm r_low: 23.5ohm pull_up: 36kohm vih: 0.524 vil: 0.418 leak: 1uA c_pin: (0.0 / 0.0)}",
            "gpio: c_pin: expected a finite non-negative number, got NaN",
            Some("(0.0 / 0.0)"),
        ),
        (
            "string_cap",
            r#"{r_high: 26ohm r_low: 23.5ohm pull_up: 36kohm vih: 0.524 vil: 0.418 leak: 1uA c_pin: "5pF"}"#,
            r#"gpio: c_pin: expected a finite non-negative number, got "5pF""#,
            Some(r#""5pF""#),
        ),
    ];
    let invalid_text = cases.iter().map(|(name, spec, _, _)| {
        format!("e.define(\"{name}\", {{kind: @mcu symbol: symbol gpio: {spec}}})")
    }).collect::<Vec<_>>().join("\n");
    let invalid_doc = add_circuit(&mut sources, "invalid_cards", &invalid_text);
    let evaluated = evaluate(&sources, &std_libs()).expect("valid GPIO cards must build beside malformed cards");
    assert_eq!(evaluated.units.len(), 2, "both the valid and invalid GPIO circuits must be recorded");
    assert_eq!(evaluated.units[0].state, UnitState::Built, "the circuit with valid and absent GPIO cards must build");
    assert!(evaluated.notes.is_empty(), "valid GPIO cards must not produce warnings");
    let comps = &evaluated.design.units.iter().find(|u| u.circuit.name == "cards")
        .expect("the valid GPIO circuit must lower").circuit.comps;
    assert_eq!(comps.len(), 7, "each accepted GPIO definition must place one component");
    let expected = GpioCard {
        r_high: 26.0, r_low: 23.5, pull_up: 36000.0, vih: 0.524, vil: 0.418, leak: 1e-6, edge: None, c_pin: None,
    };
    for (name, edge) in [("MCU_X", None), ("MCU_EDGE", Some(5e-9)), ("MCU_NIL_EDGE", None)] {
        let part = &comps.iter().find(|c| c.part.name == name)
            .unwrap_or_else(|| panic!("{name}: the component must keep its defined part")).part;
        let card = part.gpio.as_ref().unwrap_or_else(|| panic!("{name}: the part must carry its GPIO card"));
        assert_eq!(card.c_pin, None, "{name}: an absent capacitance stays unknown");
        for (key, got, want) in [
            ("r_high", card.r_high, expected.r_high),
            ("r_low", card.r_low, expected.r_low),
            ("pull_up", card.pull_up, expected.pull_up),
            ("vih", card.vih, expected.vih),
            ("vil", card.vil, expected.vil),
            ("leak", card.leak, expected.leak),
        ] {
            assert!((got - want).abs() <= want.abs() * 1e-12,
                "{name}: gpio.{key} must preserve its SI value within relative tolerance 1e-12, got {got}, expected {want}");
        }
        match edge {
            Some(want) => {
                let got = card.edge.expect("MCU_EDGE: a supplied edge time must be Some");
                assert!((got - want).abs() <= want.abs() * 1e-12,
                    "{name}: gpio.edge must convert nanoseconds to seconds, got {got}, expected {want}");
            }
            None => assert_eq!(card.edge, expected.edge, "{name}: an absent or nil edge must use the simulator's default"),
        }
    }
    let generic = &comps.iter().find(|c| c.part.name == "MCU_GENERIC")
        .expect("the part without GPIO must still place a component").part;
    assert_eq!(generic.gpio, None, "an absent GPIO card must keep the simulator's generic profile");
    // An absent card writes nothing, so every part keeps the hash it had before cards; a card adds.
    let bytes = |def: &dyn Canon| { let mut w = Writer::bytes(); def.canon(&mut w); w.into_bytes() };
    let mut before = Writer::bytes();
    let fields: [&dyn Canon; 8] = [&generic.name, &generic.kind, &generic.symbol, &generic.footprint, &generic.body,
        &generic.pins, &generic.fields, &generic.emitter];
    for field in fields {
        field.canon(&mut before);
    }
    assert_eq!(bytes(&**generic), before.into_bytes(), "a part without a GPIO card keeps its canonical form");
    let mut carded = (**generic).clone();
    carded.gpio = Some(expected.clone());
    assert_ne!(bytes(&carded), bytes(&**generic), "a GPIO card is part of the part's canonical form");
    let mut old_card = Writer::bytes();
    let fields: [&dyn Canon; 7] = [&expected.r_high, &expected.r_low, &expected.pull_up, &expected.vih,
        &expected.vil, &expected.leak, &expected.edge];
    for field in fields { field.canon(&mut old_card); }
    assert_eq!(bytes(&expected), old_card.into_bytes(), "a card without c_pin keeps its exact pre-capacitance canonical bytes and hash");
    for (name, want) in [("MCU_CAP", Some(5e-12)), ("MCU_ZERO_CAP", Some(0.0)), ("MCU_NIL_CAP", None)] {
        let card = comps.iter().find(|c| c.part.name == name).and_then(|c| c.part.gpio.as_ref())
            .unwrap_or_else(|| panic!("{name}: the capacitance card must be retained"));
        match (card.c_pin, want) {
            (Some(got), Some(want)) => assert!((got - want).abs() <= want.abs() * 1e-12,
                "{name}: c_pin preserves its SI capacitance, including zero"),
            (None, None) => (),
            _ => panic!("{name}: an absent capacitance is distinct from an explicit zero"),
        }
        assert_eq!(bytes(card) == bytes(&expected), want.is_none(), "{name}: only a supplied c_pin changes the canonical form");
    }

    let invalid = &evaluated.units[1];
    assert!(matches!(invalid.state, UnitState::Failed(_)), "malformed GPIO cards must fail their circuit");
    let diags = invalid.state.diags();
    assert_eq!(diags.len(), cases.len(), "every malformed GPIO card must produce exactly one independent refusal");
    let text = &sources.doc(invalid_doc).expect("the malformed GPIO source must remain available").text;
    for (name, spec, message, selected) in cases {
        let message = format!("{name}: {message}");
        let diag = diags.iter().find(|d| d.message == message)
            .unwrap_or_else(|| panic!("{name}: the GPIO refusal must explain the broken property: {message}; got {diags:?}"));
        assert_eq!(diag.severity, Severity::Error, "{name}: the GPIO refusal must be an error");
        assert_at(diag, invalid_doc, text, spec, selected.unwrap_or(spec));
    }
}
