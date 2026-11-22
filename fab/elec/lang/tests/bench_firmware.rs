//! Firmware bindings keep the MCU's clock and non-GPIO pins, and refuse invalid bench calls.

use fab_elec_bench::{Firmware, ModelLibs, lower};
use fab_elec_core::{Sel, Severity};
use fab_elec_lang::{NO_CANCEL, Sources, UnitState, evaluate, evaluate_from, std_libs};

const CIRCUIT: &str = r#"
use mod.elec.*
use mod.std.*
let e = mod.elec
e.define("AVR_T", {
    kind: @mcu
    fields: {model: "ATMEGA328P"}
    symbol: ic_symbol("AVR_T", {left: [
        ["VCC" @power_in] ["AVCC" @power_in] ["GND" @power_in]
        ["PB6" @bidi] ["PB7" @bidi] ["PD2" @bidi] ["PD6" @bidi] ["PD0" @bidi]
    ]})
})
let gnd = e.net({name: "GND" kind: @ground})
let vcc = e.net({name: "VCC" kind: @power volts: 5V})
let x1 = e.net({name: "X1"})
let x2 = e.net({name: "X2"})
let u = e.part("AVR_T", {pins: {VCC: vcc AVCC: vcc GND: gnd PB6: x1 PB7: x2} nc: [@PD0]})
let crystal = e.part(crystal_part("3225"), {value: 16MHz pins: [x1, x2, gnd]})
let c1 = e.c(22pF, x1, gnd)
let c2 = e.c(22pF, x2, gnd)
let d = e.led({color: @red package: "0603"})
let led_r = e.r(330ohm, u.PD6, d.A)
e.connect(d.K, gnd)
let pull = e.r(10kohm, vcc, u.PD2)
"#;

#[test]
fn firmware_binds_an_mcu_its_clock_rails_and_gpio_rows() {
    let mut sources = Sources::new();
    sources.add("fw.ckt.splash", CIRCUIT).expect("the small MCU circuit document is accepted");
    sources.add("fw.fw.splash", "firmware text is not evaluated by the circuit language")
        .expect("the firmware document is accepted without parsing it");
    sources.add("good.bench.splash", r#"
bench("chase", {circuit: "fw" run: {stop: 20ms}}, fn(b, c) {
    b.supply(c.u.VCC, {v: 5V})
    b.firmware(c.u, "fw.fw.splash", {chase_ms: 20})
})
bench("omitted", {circuit: "fw"}, fn(b, c) { b.firmware(c.u, "fw.fw.splash") })
bench("ordered_si", {circuit: "fw"}, fn(b, c) {
    b.firmware(c.u, "fw.fw.splash", {z_delay: 20ms a_count: 3})
})
"#).expect("the valid bench document is accepted");

    // Each refusal shares this circuit and evaluation, and checks its exact source occurrence.
    let cases = [
        (
            "not_mcu",
            r#"b.firmware(c.pull, "fw.fw.splash", {})"#,
            "pull: firmware runs on an MCU part",
            r#"b.firmware(c.pull, "fw.fw.splash", {})"#,
        ),
        (
            "not_component",
            r#"b.firmware(nil, "fw.fw.splash", {})"#,
            "expected a component, got nil",
            r#"b.firmware(nil, "fw.fw.splash", {})"#,
        ),
        (
            "missing_doc",
            r#"b.firmware(c.u, "missing.fw.splash", {})"#,
            "firmware: no firmware document missing.fw.splash in the project",
            r#""missing.fw.splash""#,
        ),
        (
            "wrong_doc_kind",
            r#"b.firmware(c.u, "fw.ckt.splash", {})"#,
            "firmware: no firmware document fw.ckt.splash in the project",
            r#""fw.ckt.splash""#,
        ),
        (
            "not_doc_name",
            "b.firmware(c.u, false, {})",
            "firmware: expected a document name, got false",
            "false",
        ),
        (
            "string_param",
            r#"b.firmware(c.u, "fw.fw.splash", {chase_ms: "fast"})"#,
            r#"firmware: chase_ms: expected a finite number, got "fast""#,
            r#""fast""#,
        ),
        (
            "infinite_param",
            r#"b.firmware(c.u, "fw.fw.splash", {chase_ms: 1e309})"#,
            "firmware: chase_ms: expected a finite number, got inf",
            "1e309",
        ),
        (
            "not_params_object",
            r#"b.firmware(c.u, "fw.fw.splash", false)"#,
            "firmware: expected an options object",
            "false",
        ),
        (
            "twice",
            "b.firmware(c.u, \"fw.fw.splash\", {})\nb.firmware(c.u, \"fw.fw.splash\", {})",
            "u: component already runs firmware",
            r#"b.firmware(c.u, "fw.fw.splash", {})"#,
        ),
        (
            "stub_first",
            "b.stub(c.u, {})\nb.firmware(c.u, \"fw.fw.splash\", {})",
            "u: component is stubbed; firmware drives its pins itself",
            r#"b.firmware(c.u, "fw.fw.splash", {})"#,
        ),
        (
            "firmware_first",
            "b.firmware(c.u, \"fw.fw.splash\", {})\nb.stub(c.u, {})",
            "u: component runs firmware; it cannot be stubbed",
            "b.stub(c.u, {})",
        ),
    ];
    let mut errors = String::new();
    let mut starts = Vec::new();
    for (name, body, _, selection) in &cases {
        errors.push_str(&format!("bench(\"{name}\", {{circuit: \"fw\"}}, fn(b, c) {{\n"));
        starts.push(errors.len() + body.rfind(*selection).expect("the refusal selection is in its body"));
        errors.push_str(body);
        errors.push_str("\n})\n");
    }
    let errors_doc = sources.add("errors.bench.splash", errors).expect("the refusal document is accepted");
    let ev = evaluate(&sources, &std_libs()).expect("the circuit evaluates despite refused benches");
    assert_eq!(ev.units[0].state, UnitState::Built, "firmware text and bench refusals do not fail the circuit");
    assert_eq!(ev.benches.len(), 3 + cases.len(), "each bench call records one result despite earlier refusals");
    let bench = |name: &str| ev.benches.iter().find(|b| b.name == name).expect("the named bench has a result");
    for name in ["chase", "omitted", "ordered_si"] {
        assert_eq!(bench(name).state, UnitState::Built, "{name}: valid firmware builds");
    }
    let good = bench("chase");
    assert_eq!(good.spec.firmware, [Firmware {
        path: "u".into(), doc: "fw.fw.splash".into(), params: vec![("chase_ms".into(), 20.0)],
    }], "the firmware spec retains the component path, project document and compile-time constants");
    assert_eq!(bench("omitted").spec.firmware.len(), 1, "omitted params still record firmware");
    assert!(bench("omitted").spec.firmware[0].params.is_empty(), "omitted params mean no constants");
    assert_eq!(bench("ordered_si").spec.firmware[0].params,
        [("z_delay".into(), 0.02), ("a_count".into(), 3.0)],
        "constants retain written order and unit suffixes apply SI multipliers");

    let lowered = lower(&ev.design, good.scope_id(), &good.spec, &ModelLibs::generic());
    assert_eq!(lowered.firmware.len(), 1, "the MCU has exactly one co-simulation binding");
    let fw = &lowered.firmware[0];
    assert_eq!(fw.part, "u", "the binding retains the component path");
    assert_eq!(fw.doc, "fw.fw.splash", "the binding retains the firmware document");
    assert_eq!(fw.params, good.spec.firmware[0].params, "the binding retains compile-time constants");
    assert_eq!(fw.f_clk, Some(16e6), "the MCU's clock comes from the attached crystal's frequency");
    assert_eq!(fw.rails, [
        ("VCC".into(), "VCC".into()), ("AVCC".into(), "VCC".into()),
        ("GND".into(), "0".into()), ("PB6".into(), "X1".into()), ("PB7".into(), "X2".into()),
    ], "supply and crystal rails retain package pin order and lowered node names");
    assert_eq!(lowered.cosim_pins(&fw.part).iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["PD2", "PD6"], "only connected signal pins become GPIO rows; PD0 is unconnected");
    for pin in ["VCC", "AVCC", "GND", "PB6", "PB7", "PD0"] {
        assert!(!lowered.circuit.devices.gpio.iter().any(|g| g.name == format!("u#{pin}.io")),
            "{pin}: supply, crystal and unconnected pins have no GPIO device");
        assert!(!lowered.circuit.devices.c.iter().any(|c| c.name == format!("u#{pin}.cin")),
            "{pin}: supply, crystal and unconnected pins have no input capacitance");
    }
    // The clock is ideal and no oscillator is solved; the amplifier's bias through its feedback
    // resistor gives the crystal's nets their DC path. Nothing is diagnosed: no missing model, no
    // clock, no floating crystal net.
    assert!(lowered.diags.is_empty(), "the MCU and ideal crystal lower without diagnostics: {:?}", lowered.diags);

    for ((name, _, message, selection), expected_start) in cases.iter().zip(starts) {
        let failed = bench(name);
        assert!(matches!(failed.state, UnitState::Failed(_)), "{name}: the refusal fails its bench");
        let expected = format!("{name}: {message}");
        let diag = failed.state.diags().iter().find(|d| d.message == expected)
            .unwrap_or_else(|| panic!("{name}: expected {expected:?}, got {:?}", failed.state.diags()));
        assert_eq!(diag.severity, Severity::Error, "{name}: the refusal is an error");
        let Some(Sel::Source { doc, start, end }) = diag.sel.first() else {
            panic!("{name}: the refusal must start with a source selection: {diag:?}");
        };
        assert_eq!(*doc, errors_doc, "{name}: the refusal selects the bench document");
        assert_eq!(*start as usize, expected_start, "{name}: the refusal selects the offending occurrence");
        let text = &sources.doc(*doc).expect("the diagnostic's document exists").text;
        assert_eq!(&text[*start as usize..*end as usize], *selection,
            "{name}: the refusal selects the offending argument or call exactly");
    }
}

/// A bench kept at its last good evaluation still runs its stubs and firmware. A name among them
/// that the project no longer has is an error at its call, never a silent no-op.
#[test]
fn kept_benches_locate_the_names_their_project_no_longer_has() {
    const BENCH: &str = r#"bench("kept", {circuit: "fw"}, fn(b, c) {
    b.stub(c.pull, {}, {rail: c.net("VCC")})
    b.firmware(c.u, "fw.fw.splash")
})
"#;
    const STUB: &str = r#"b.stub(c.pull, {}, {rail: c.net("VCC")})"#;
    const FIRMWARE: &str = r#"b.firmware(c.u, "fw.fw.splash")"#;
    let project = |circuit: &str, bench: &str, firmware: bool| {
        let mut sources = Sources::new();
        sources.add("fw.ckt.splash", circuit.to_owned()).expect("the circuit document is accepted");
        let fw = sources.add("fw.fw.splash", "firmware text").expect("the firmware document is accepted");
        sources.add("kept.bench.splash", bench.to_owned()).expect("the bench document is accepted");
        sources.set_present(fw, firmware).expect("the firmware document can be absent");
        sources
    };
    let libs = std_libs();
    let ev = evaluate(&project(CIRCUIT, BENCH, true), &libs).expect("the project evaluates");
    assert_eq!(ev.benches[0].state, UnitState::Built, "the stub and the firmware build");
    let broken = format!("{BENCH}bench(");
    let renamed = CIRCUIT.replace("let u = ", "let mcu = ").replace("u.PD", "mcu.PD").replace("let pull = ", "let pullup = ");
    // Every error inside one call, by message, for a kept bench.
    let errors_in = |sources: &Sources, call: &str| -> Vec<String> {
        let ev = evaluate_from(Some(&ev), sources, &libs, &NO_CANCEL).expect("a bench failure does not fail the unit");
        let bench = &ev.benches[0];
        assert!(matches!(bench.state, UnitState::Kept(_)), "the broken bench keeps its last good evaluation");
        let doc = sources.find("kept.bench.splash").expect("the bench document");
        let text = &sources.doc(doc).expect("the bench document").text;
        let from = text.find(call).expect("the call is written") as u32;
        let to = from + call.len() as u32;
        bench.state.diags().iter().filter(|d| d.sel.iter().any(|sel| matches!(sel,
            Sel::Source { doc: at, start, end } if *at == doc && from <= *start && *end <= to)))
            .map(|d| {
                assert_eq!(d.severity, Severity::Error, "{}: the stale name is an error", d.message);
                d.message.clone()
            }).collect()
    };

    let gone = project(&renamed, &broken, true);
    assert_eq!(errors_in(&gone, STUB), ["kept: pull: no such component in the circuit now; its stub drives nothing"],
        "a stubbed component the circuit renamed is an error at its stub call");
    assert_eq!(errors_in(&gone, FIRMWARE), ["kept: u: no such component in the circuit now; its firmware runs nowhere"],
        "a firmware component the circuit renamed is an error at its firmware call");

    let absent = project(CIRCUIT, &broken, false);
    assert_eq!(errors_in(&absent, FIRMWARE),
        ["kept: firmware: no firmware document fw.fw.splash in the project; u runs no firmware"],
        "a firmware document gone from the project is an error at the firmware call");
    assert!(errors_in(&absent, STUB).is_empty(), "a stub whose component remains has no error");

    let not_mcu = CIRCUIT.replace("kind: @mcu", "kind: @ic");
    assert_eq!(errors_in(&project(&not_mcu, &broken, true), FIRMWARE), ["kept: u: firmware runs on an MCU part"],
        "a firmware component that is no longer an MCU is an error at its firmware call");

    // An intact bench whose names are gone reports only the missing property; the native and
    // the kept check add none of their own inside a call that already has an error.
    let refused = project(&renamed, BENCH, true);
    for (call, property) in [(STUB, "pull"), (FIRMWARE, "u")] {
        let errors = errors_in(&refused, call);
        assert!(!errors.is_empty(), "{call}: the refusal is at the call");
        assert_eq!(errors.len(), 1, "{call}: one mistake is one diagnostic: {errors:?}");
        assert!(errors.iter().any(|e| e.contains("not found")), "{call}: the missing name is reported: {errors:?}");
        assert!(errors[0].contains(&format!("property {property} not found in prototype chain")),
            "{call}: the only error is the VM's missing property: {errors:?}");
        assert!(!errors.iter().any(|e| e.contains("in the circuit now") || e.contains("runs no firmware")),
            "{call}: no kept-check error inside a refused call: {errors:?}");
    }
}
