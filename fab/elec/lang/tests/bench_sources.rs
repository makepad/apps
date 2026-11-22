//! Bench supplies, sources, loads and stub rails resolve against one evaluated circuit.

use fab_elec_bench::bench::{BenchSource, SourceKind, Supply};
use fab_elec_core::{Diag, NetRef, Sel};
use fab_elec_lang::{Sources, UnitState, evaluate, std_libs};

const CIRCUIT: &str = include_str!("../../fixtures/ledctl.ckt.splash");

fn diag_text<'a>(diag: &Diag, sources: &'a Sources) -> &'a str {
    match diag.sel.first() {
        Some(Sel::Source { doc, start, end }) => {
            let text = &sources.doc(*doc).expect("diagnostic document").text;
            &text[*start as usize..*end as usize]
        }
        _ => panic!("a diagnostic must be located: {diag:?}"),
    }
}

#[test]
fn stimulus_records_and_located_errors_share_one_circuit() {
    let valid = r#"
bench("stimulus", {}, fn(b, c) {
    b.supply(c.net("VBUS"), {v: 5V limit: 500mA ramp: 1ms})
    b.vsrc(c.mcu.u.VDD, c.mcu.u.VSS, {dc: 3.3V})
    b.isrc(c.net("LED0"), c.net("GND"), {dc: 5mA})
    b.load(c.net("VBUS"), c.net("GND"), {r: 100ohm})
    b.load(c.net("3V3"), c.net("GND"), {i: 10mA})
    b.load(c.net("LED1"), c.mcu.u.VSS, {step: [10mA 300mA] at: 1ms rise: 1us})
    b.stub(c.mcu.u, {PA0: b.high()})
})
bench("rail", {}, fn(b, c) {
    b.stub(c.lamps[0].r, {}, {rail: c.net("3V3")})
    b.stub(c.lamps[1].r, {}, {rail: c.mcu.u.VDD})
})
bench("defaults", {}, fn(b, c) {
    b.supply(c.mcu.u.VDD, {v: 2.5V * 2})
    b.supply(c.net("VBUS"), {v: -5 ramp: 0})
    b.vsrc(c.net("LED0"), c.net("GND"), {dc: -2})
    b.isrc(c.net("LED1"), c.net("GND"), {dc: -0.005})
    b.load(c.net("VBUS"), c.net("GND"), {r: 100})
    b.load(c.net("3V3"), c.net("GND"), {i: -0.01})
    b.load(c.net("LED2"), c.net("GND"), {step: [0 0.3] rise: 0.000001})
    b.load(c.net("LED3"), c.net("GND"), {step: [-0.01, 0.3] at: 0 rise: 1ms})
    let computed = 5mA
    b.vsrc(c.net("LED4"), c.net("GND"), {dc: computed})
    b.vsrc(c.net("LED5"), c.net("GND"), {dc: 2mA * 2})
})
"#;

    // Each tuple names a property, its bench body, its message and its exact selection.
    let mut cases: Vec<(String, String, String, String)> = Vec::new();
    for (name, body, message, selection) in [
        (
            "supply_missing_v",
            r#"b.supply(c.net("VBUS"), {limit: 500mA})"#,
            "missing required option v",
            "{limit: 500mA}",
        ),
        (
            "vsrc_missing_dc",
            r#"b.vsrc(c.net("3V3"), c.net("GND"), {})"#,
            "missing required option dc",
            "{}",
        ),
        (
            "load_step_missing_rise",
            r#"b.load(c.net("3V3"), c.net("GND"), {step: [10mA 300mA] at: 1ms})"#,
            "missing required option rise",
            "{step: [10mA 300mA] at: 1ms}",
        ),
        (
            "isrc_missing_dc",
            r#"b.isrc(c.net("3V3"), c.net("GND"), {})"#,
            "missing required option dc",
            "{}",
        ),
        (
            "load_missing_kind",
            r#"b.load(c.net("3V3"), c.net("GND"), {})"#,
            "load requires exactly one of r, i or step",
            "{}",
        ),
        (
            "load_r_and_i",
            r#"b.load(c.net("3V3"), c.net("GND"), {r: 100ohm i: 10mA})"#,
            "load requires exactly one of r, i or step",
            "{r: 100ohm i: 10mA}",
        ),
        (
            "load_r_and_step",
            r#"b.load(c.net("3V3"), c.net("GND"), {r: 100ohm step: [10mA 300mA]})"#,
            "load requires exactly one of r, i or step",
            "{r: 100ohm step: [10mA 300mA]}",
        ),
        (
            "load_i_and_step",
            r#"b.load(c.net("3V3"), c.net("GND"), {i: 10mA step: [10mA 300mA]})"#,
            "load requires exactly one of r, i or step",
            "{i: 10mA step: [10mA 300mA]}",
        ),
        (
            "load_at_without_step",
            r#"b.load(c.net("3V3"), c.net("GND"), {r: 100ohm at: 1ms})"#,
            "at is only allowed with step",
            "1ms",
        ),
        (
            "load_rise_without_step",
            r#"b.load(c.net("3V3"), c.net("GND"), {i: 10mA rise: 1us})"#,
            "rise is only allowed with step",
            "1us",
        ),
        (
            "ground_supply",
            r#"b.supply(c.net("GND"), {v: 5V})"#,
            "cannot supply the ground net",
            r#"b.supply(c.net("GND"), {v: 5V})"#,
        ),
        (
            "ground_pin_supply",
            "b.supply(c.mcu.u.VSS, {v: 5V})",
            "cannot supply the ground net",
            "b.supply(c.mcu.u.VSS, {v: 5V})",
        ),
        (
            "duplicate_supply",
            "b.supply(c.net(\"VBUS\"), {v: 5V})\nb.supply(c.net(\"VBUS\"), {v: 5V})",
            "a second supply on the same net is not allowed",
            r#"b.supply(c.net("VBUS"), {v: 5V})"#,
        ),
        (
            "duplicate_supply_via_pin",
            "b.supply(c.net(\"3V3\"), {v: 3.3V})\nb.supply(c.mcu.u.VDD, {v: 5V})",
            "a second supply on the same net is not allowed",
            "b.supply(c.mcu.u.VDD, {v: 5V})",
        ),
        (
            "stub_missing_rail",
            "b.stub(c.lamps[0].r, {})",
            "lamps[0].r: the part has no supply pin (VDD, VCC or VIO); give it a rail: {rail: c.net(..)}",
            "b.stub(c.lamps[0].r, {})",
        ),
        (
            "stub_unneeded_rail",
            r#"b.stub(c.mcu.u, {}, {rail: c.net("3V3")})"#,
            "mcu.u: rail is only for a part without a supply pin (it has VDD)",
            r#"c.net("3V3")"#,
        ),
        (
            "stub_bad_rail",
            "b.stub(c.lamps[0].r, {}, {rail: 1})",
            "expected a net or pin",
            "1",
        ),
        (
            "stub_unconnected_rail",
            "b.stub(c.lamps[0].r, {}, {rail: c.mcu.u.PA8})",
            "mcu.u.PA8 is unconnected",
            "c.mcu.u.PA8",
        ),
    ] {
        cases.push((name.into(), body.into(), message.into(), selection.into()));
    }

    for (name, call, opts) in [
        ("supply", r#"b.supply(c.net("VBUS"), OPTS)"#, "v: 5V"),
        (
            "vsrc",
            r#"b.vsrc(c.net("3V3"), c.net("GND"), OPTS)"#,
            "dc: 3.3V",
        ),
        (
            "isrc",
            r#"b.isrc(c.net("3V3"), c.net("GND"), OPTS)"#,
            "dc: 5mA",
        ),
        (
            "load",
            r#"b.load(c.net("3V3"), c.net("GND"), OPTS)"#,
            "r: 100ohm",
        ),
        (
            "stub",
            "b.stub(c.lamps[0].r, {}, OPTS)",
            r#"rail: c.net("3V3")"#,
        ),
    ] {
        cases.push((
            format!("{name}_unknown_option"),
            call.replace("OPTS", &format!("{{{opts} extra: 1}}")),
            "unknown option extra".into(),
            "extra: 1".into(),
        ));
        cases.push((
            format!("{name}_bad_options"),
            call.replace("OPTS", "false"),
            "expected an options object".into(),
            call.replace("OPTS", "false"),
        ));
    }

    for (name, call) in [
        ("vsrc", "b.vsrc(A, B, {dc: 3.3V})"),
        ("isrc", "b.isrc(A, B, {dc: 5mA})"),
        ("load", "b.load(A, B, {r: 100ohm})"),
    ] {
        let body = call.replace("A, B", r#"c.mcu.u.VDD, c.net("3V3")"#);
        cases.push((
            format!("{name}_same_net"),
            body.clone(),
            "source or load terminals must be different nets".into(),
            body,
        ));
        for (endpoint, values) in [("a", r#"nil, c.net("GND")"#), ("b", r#"c.net("3V3"), nil"#)] {
            let body = call.replace("A, B", values);
            cases.push((
                format!("{name}_invalid_{endpoint}"),
                body.clone(),
                "expected a net or pin".into(),
                body,
            ));
        }
    }
    for (name, body, message) in [
        (
            "supply_invalid_net",
            "b.supply(nil, {v: 5V})",
            "expected a net or pin",
        ),
        (
            "supply_unconnected_net",
            "b.supply(c.mcu.u.PA8, {v: 5V})",
            "mcu.u.PA8 is unconnected",
        ),
    ] {
        cases.push((name.into(), body.into(), message.into(), body.into()));
    }
    for (name, step) in [
        ("scalar", "10mA"),
        ("empty", "[]"),
        ("short", "[10mA]"),
        ("long", "[10mA 300mA 20mA]"),
    ] {
        cases.push((
            format!("step_{name}"),
            format!(r#"b.load(c.net("3V3"), c.net("GND"), {{step: {step}}})"#),
            "step: expected two currents [from to]".into(),
            step.into(),
        ));
    }

    for (name, call, key, unit, wrong) in [
        (
            "supply_v",
            r#"b.supply(c.net("VBUS"), {v: VALUE})"#,
            "v",
            "V",
            "5mA",
        ),
        (
            "supply_limit",
            r#"b.supply(c.net("VBUS"), {v: 5V limit: VALUE})"#,
            "limit",
            "A",
            "5V",
        ),
        (
            "supply_ramp",
            r#"b.supply(c.net("VBUS"), {v: 5V ramp: VALUE})"#,
            "ramp",
            "s",
            "5V",
        ),
        (
            "vsrc_dc",
            r#"b.vsrc(c.net("3V3"), c.net("GND"), {dc: VALUE})"#,
            "dc",
            "V",
            "5mA",
        ),
        (
            "isrc_dc",
            r#"b.isrc(c.net("3V3"), c.net("GND"), {dc: VALUE})"#,
            "dc",
            "A",
            "5V",
        ),
        (
            "load_r",
            r#"b.load(c.net("3V3"), c.net("GND"), {r: VALUE})"#,
            "r",
            "ohm",
            "5V",
        ),
        (
            "load_i",
            r#"b.load(c.net("3V3"), c.net("GND"), {i: VALUE})"#,
            "i",
            "A",
            "5V",
        ),
        (
            "step_from",
            r#"b.load(c.net("3V3"), c.net("GND"), {step: [VALUE, 300mA] rise: 1us})"#,
            "step",
            "A",
            "-5V",
        ),
        (
            "step_to",
            r#"b.load(c.net("3V3"), c.net("GND"), {step: [10mA, VALUE] rise: 1us})"#,
            "step",
            "A",
            "5V",
        ),
        (
            "step_at",
            r#"b.load(c.net("3V3"), c.net("GND"), {step: [10mA 300mA] at: VALUE rise: 1us})"#,
            "at",
            "s",
            "5V",
        ),
        (
            "step_rise",
            r#"b.load(c.net("3V3"), c.net("GND"), {step: [10mA 300mA] rise: VALUE})"#,
            "rise",
            "s",
            "5V",
        ),
    ] {
        cases.push((
            format!("{name}_unit"),
            call.replace("VALUE", wrong),
            format!("{key}: expected {unit} for this option"),
            wrong.into(),
        ));
        for (suffix, value) in [("infinite", "1e309"), ("nan", "(0.0 / 0.0)")] {
            cases.push((
                format!("{name}_{suffix}"),
                call.replace("VALUE", value),
                format!("{key} must be finite"),
                value.into(),
            ));
        }
        cases.push((
            format!("{name}_not_numeric"),
            call.replace("VALUE", "false"),
            format!("{key}: expected a number"),
            "false".into(),
        ));
        let range = match key {
            "limit" | "r" | "rise" => Some(("must be finite and greater than 0", &["0", "-1"][..])),
            "ramp" | "at" => Some(("must be finite and nonnegative", &["-1"][..])),
            _ => None,
        };
        if let Some((message, values)) = range {
            for (index, value) in values.iter().enumerate() {
                cases.push((
                    format!("{name}_range_{index}"),
                    call.replace("VALUE", value),
                    format!("{key} {message}"),
                    (*value).into(),
                ));
            }
        }
    }

    let mut errors = String::new();
    let mut selections = Vec::new();
    for (name, body, _, selection) in &cases {
        errors.push_str(&format!("bench(\"{name}\", {{}}, fn(b, c) {{\n"));
        // rfind selects the second occurrence for the duplicate-supply property.
        selections.push(errors.len() + body.rfind(selection).expect("selection in bench body"));
        errors.push_str(body);
        errors.push_str("\n})\n");
    }
    errors
        .push_str("bench(\"after_errors\", {}, fn(b, c) { b.supply(c.net(\"VBUS\"), {v: 5V}) })\n");

    let mut sources = Sources::new();
    sources
        .add("ledctl.ckt.splash", CIRCUIT)
        .expect("circuit document");
    sources
        .add("sources.bench.splash", valid)
        .expect("valid bench document");
    let errors_doc = sources
        .add("errors.bench.splash", errors)
        .expect("error bench document");
    let ev = evaluate(&sources, &std_libs()).expect("the circuit evaluates despite bench errors");
    assert_eq!(
        ev.units[0].state,
        UnitState::Built,
        "bench failures preserve the evaluated unit"
    );
    assert_eq!(
        ev.benches.len(),
        3 + cases.len() + 1,
        "a failing bench does not stop later benches"
    );
    for bench in &ev.benches[..3] {
        assert_eq!(
            bench.state,
            UnitState::Built,
            "{}: valid stimulus builds",
            bench.name
        );
    }
    let circuit = &ev.design.units[0].circuit;
    let net = |name| NetRef {
        board: ev.benches[0].scope_id(),
        net: circuit
            .nets
            .iter()
            .find(|n| n.name == name)
            .expect("fixture net")
            .id,
    };
    let source = |a, b, kind| BenchSource {
        a: net(a),
        b: net(b),
        kind,
    };
    let spec = &ev.benches[0].spec;
    assert_eq!(
        spec.supplies,
        [Supply {
            net: net("VBUS"),
            v: 5.0,
            limit: Some(0.5),
            ramp: Some(0.001),
        }],
        "supply values use SI and retain optional limit and ramp"
    );
    assert_eq!(
        spec.sources,
        [
            source("3V3", "GND", SourceKind::Vdc(3.3)),
            source("LED0", "GND", SourceKind::Idc(0.005)),
            source("VBUS", "GND", SourceKind::LoadR(100.0)),
            source("3V3", "GND", SourceKind::LoadI(0.01)),
            source(
                "LED1",
                "GND",
                SourceKind::LoadStep {
                    from: 0.01,
                    to: 0.3,
                    at: 0.001,
                    rise: 0.000001,
                }
            ),
        ],
        "sources resolve net and pin handles in call order and use SI values"
    );
    assert_eq!(spec.stubs.len(), 1, "the MCU stub is recorded");
    assert_eq!(spec.stubs[0].path, "mcu.u", "the stub retains the MCU path");
    assert_eq!(
        spec.stubs[0].rail, None,
        "a connected MCU supply pin needs no rail override"
    );
    assert_eq!(
        ev.benches[1]
            .spec
            .stubs
            .iter()
            .map(|s| (s.path.as_str(), s.rail))
            .collect::<Vec<_>>(),
        [
            ("lamps[0].r", Some(net("3V3"))),
            ("lamps[1].r", Some(net("3V3"))),
        ],
        "stub rails accept both net and pin handles"
    );
    let defaults = &ev.benches[2].spec;
    assert_eq!(
        defaults.supplies,
        [
            Supply {
                net: net("3V3"),
                v: 5.0,
                limit: None,
                ramp: None
            },
            Supply {
                net: net("VBUS"),
                v: -5.0,
                limit: None,
                ramp: Some(0.0)
            },
        ],
        "computed voltage records 5 V, omitted options stay absent, and supplies retain call order"
    );
    assert_eq!(
        defaults.sources,
        [
            source("LED0", "GND", SourceKind::Vdc(-2.0)),
            source("LED1", "GND", SourceKind::Idc(-0.005)),
            source("VBUS", "GND", SourceKind::LoadR(100.0)),
            source("3V3", "GND", SourceKind::LoadI(-0.01)),
            source(
                "LED2",
                "GND",
                SourceKind::LoadStep {
                    from: 0.0,
                    to: 0.3,
                    at: 0.0,
                    rise: 0.000001
                }
            ),
            source(
                "LED3",
                "GND",
                SourceKind::LoadStep {
                    from: -0.01,
                    to: 0.3,
                    at: 0.0,
                    rise: 0.001
                }
            ),
            source("LED4", "GND", SourceKind::Vdc(0.005)),
            source("LED5", "GND", SourceKind::Vdc(0.004)),
        ],
        "bare SI, signed sources, zero step times and computed quantities are accepted"
    );

    for ((name, _, message, selection), expected_start) in cases.iter().zip(selections) {
        let bench = ev
            .benches
            .iter()
            .find(|b| b.name == *name)
            .expect("error bench result");
        assert!(
            matches!(bench.state, UnitState::Failed(_)),
            "{name}: an invalid bench fails"
        );
        let prefix = format!("{name}: {message}");
        let diag = bench
            .state
            .diags()
            .iter()
            .find(|d| d.message.starts_with(&prefix))
            .unwrap_or_else(|| {
                panic!(
                    "{name}: expected {message:?}, got {:?}",
                    bench.state.diags()
                )
            });
        assert_eq!(
            diag_text(diag, &sources),
            selection,
            "{name}: the offending value, field or call is selected"
        );
        assert!(
            matches!(diag.sel.first(), Some(Sel::Source { doc, start, .. })
            if *doc == errors_doc && *start as usize == expected_start),
            "{name}: the diagnostic selects the correct occurrence in the bench document"
        );
    }
    let after = ev.benches.last().expect("last bench");
    assert_eq!(
        after.name, "after_errors",
        "execution reaches the bench after all errors"
    );
    assert_eq!(
        after.spec.supplies,
        [Supply {
            net: net("VBUS"),
            v: 5.0,
            limit: None,
            ramp: None,
        }],
        "recording continues after independent failing benches"
    );
}
