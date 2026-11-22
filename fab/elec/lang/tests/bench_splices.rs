//! Literal bench operations agree with applying their requested values after evaluation.

use fab_elec_bench::expect::Expect;
use fab_elec_core::{
    Diag, Sel,
    drive::{Drive, Pull},
};
use fab_elec_design::{BenchOp, DesignOp, ProbeOf, TextEdit};
use fab_elec_lang::{
    NO_CANCEL, BenchKey, BenchOut, BenchScope, Evaluated, Libs, Sources, UnitState, evaluate, splice,
    splice_verified, std_libs,
};
use std::sync::Arc;

const CIRCUIT: &str = include_str!("../../fixtures/ledctl.ckt.splash");
const FIXTURE: &str = include_str!("../../fixtures/ledctl.bench.splash");
const BENCH: &str = r#"bench("literal", {}, fn(b, c) {
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, 30%) PA1: b.high() PB7: b.input(@up)})
    b.press(c.btn.sw) // held button
    b.expect("rail", b.now(b.v(c.net("3V3"))), {min: 3V max: 4V})
    b.expect("open", b.now(b.i(c.lamps[0].d)), {min: 4.5mA})
    b.expect("plain", b.now(b.v(c.net("GND"))), {min: -1 max: 1})
    for i in 0..2 {
        b.expect("loop" + i, b.now(b.i(c.lamps[i].d)), {min: 1mA max: 6mA})
    }
    let limit = 4V
    let bounds = {min: 3V max: 4V}
    b.expect("mixed", b.now(b.v(c.net("3V3"))), {min: 3V max: limit})
    b.expect("indirect", b.now(b.v(c.net("3V3"))), bounds)
    let check = fn(name) { b.expect(name, b.now(b.v(c.net("3V3"))), {min: 3V max: 4V}) }
    check("helper")
})
bench("looppress", {}, fn(b, c) {
    for i in 0..2 { b.press(c.switches[i].sw) }
})
bench("renamed", {}, fn(tb, ck) {
    tb.stub(ck.mcu.u, {PA0: tb.high()})
})
bench("computed", {}, fn(b, c) {
    let rate = 1kHz
    let amount = 30%
    let pull = @up
    b.stub(c.mcu.u, {PA0: b.pwm(rate, 30%) PA1: b.pwm(1kHz, amount) PB7: b.input(pull)})
})
bench("shared", {}, fn(b, c) {
    for i in 0..2 {
        b.stub(c.switches[i].sw, {A: b.high()}, {rail: c.net("3V3")})
    }
})
"#;

struct Setup {
    sources: Sources,
    libs: Libs,
    evaluated: Evaluated,
}

impl Setup {
    fn new() -> Self {
        let mut sources = Sources::new();
        // Two extra instances exercise indexed access paths and shared literal sites.
        let circuit = format!(
            "{CIRCUIT}\nlet switches = for i in 0..2 {{ button({{out: key gnd: gnd}}) }}\n"
        );
        sources
            .add("ledctl.ckt.splash", circuit)
            .expect("the circuit document is accepted");
        sources
            .add("bringup.bench.splash", FIXTURE)
            .expect("the fixture bench is accepted");
        sources
            .add("literal.bench.splash", BENCH)
            .expect("the literal bench is accepted");
        let libs = std_libs();
        let evaluated = evaluate(&sources, &libs).expect("the setup circuit evaluates");
        assert!(
            evaluated
                .benches
                .iter()
                .all(|b| b.state == UnitState::Built),
            "all setup benches build: {:?}",
            evaluated.benches
        );
        Self {
            sources,
            libs,
            evaluated,
        }
    }

    fn text(&self, name: &str) -> &str {
        &self
            .sources
            .doc(self.sources.find(name).expect("the document is named"))
            .expect("the document exists")
            .text
    }

    fn bench(&self, name: &str) -> &BenchOut {
        self.evaluated
            .benches
            .iter()
            .find(|b| b.name == name)
            .expect("the bench exists")
    }

    fn expect(&self, bench: &str, name: &str) -> &Expect {
        self.bench(bench)
            .expects
            .iter()
            .find(|e| e.name == name)
            .expect("the expectation exists")
    }

    fn apply(&mut self, name: &str, operation: BenchOp, count: usize) -> Vec<TextEdit> {
        let change = splice(
            &self.sources,
            &self.evaluated.map,
            &self.evaluated.design,
            &[op(name, operation.clone())],
        )
        .expect("a literal bench operation splices");
        assert_eq!(
            change.edits.len(),
            count,
            "the operation edits only changed targets"
        );
        assert!(
            change.reevaluate,
            "every bench splice, including a no-op, requests evaluation"
        );
        let circuit = self.text("ledctl.ckt.splash").to_string();
        self.sources
            .apply(&change.edits)
            .expect("the bench edits are disjoint and apply");
        self.evaluated = evaluate(&self.sources, &self.libs).expect("the edited sources evaluate");
        assert!(
            self.evaluated
                .benches
                .iter()
                .all(|b| b.state == UnitState::Built),
            "all benches build after the splice: {:?}",
            self.evaluated.benches
        );
        assert_eq!(
            self.text("ledctl.ckt.splash"),
            circuit,
            "bench edits preserve the circuit byte for byte"
        );
        let bench = self.bench(name);
        match operation {
            BenchOp::SetPressed { comp, pressed } => assert_eq!(
                bench.spec.pressed.contains(&comp),
                pressed,
                "splice then evaluate gives the requested pressed state for {comp}"
            ),
            BenchOp::SetRange { expect, min, max } => {
                let got = self.expect(name, &expect);
                assert_eq!(
                    got.range(),
                    Some((min, max)),
                    "splice then evaluate gives both requested bounds for {expect}"
                );
            }
            BenchOp::SetDrive { stub, pin, drive } => {
                let got = bench
                    .spec
                    .stubs
                    .iter()
                    .find(|s| s.path == stub)
                    .and_then(|s| s.pins.iter().find(|(name, _)| name == &pin))
                    .map(|(_, drive)| drive);
                assert_eq!(
                    got,
                    Some(&drive),
                    "splice then evaluate gives the requested drive for {stub}.{pin}"
                );
            }
            _ => unreachable!("these properties cover pressed states, ranges and drives"),
        }
        change.edits
    }

    fn refuse(&self, name: &str, operation: BenchOp, message: &str, selected: &str) {
        let error = splice(
            &self.sources,
            &self.evaluated.map,
            &self.evaluated.design,
            &[op(name, operation)],
        )
        .expect_err("a computed target refuses its literal splice");
        assert_eq!(
            error.message, message,
            "the refusal explains the specific target"
        );
        assert_eq!(
            selection(&error, &self.sources),
            selected,
            "the refusal selects the expression that needs editing"
        );
    }
}

fn op(bench: &str, op: BenchOp) -> DesignOp {
    DesignOp::Bench {
        bench: bench.into(),
        op,
    }
}

fn pressed(comp: &str, pressed: bool) -> BenchOp {
    BenchOp::SetPressed {
        comp: comp.into(),
        pressed,
    }
}

fn range(expect: &str, min: f64, max: f64) -> BenchOp {
    BenchOp::SetRange {
        expect: expect.into(),
        min,
        max,
    }
}

fn drive(pin: &str, drive: Drive) -> BenchOp {
    BenchOp::SetDrive {
        stub: "mcu.u".into(),
        pin: pin.into(),
        drive,
    }
}

fn pwm(freq: f64, duty: f64) -> Drive {
    Drive::Pwm { freq, duty }
}

fn computed(what: &str) -> String {
    format!("{what}: this value is computed here; edit the values it is made from")
}

fn selection<'a>(diag: &Diag, sources: &'a Sources) -> &'a str {
    let Some(Sel::Source { doc, start, end }) = diag.sel.first() else {
        panic!("a computed refusal selects source: {diag:?}")
    };
    &sources
        .doc(*doc)
        .expect("the diagnostic document exists")
        .text[*start as usize..*end as usize]
}

fn replaced<'a>(sources: &'a Sources, edit: &TextEdit) -> &'a str {
    &sources
        .doc(edit.doc)
        .expect("the edit document exists")
        .text[edit.span.start as usize..edit.span.end as usize]
}

#[test]
fn supply_and_source_options_splice_their_literals() {
    let text = r#"bench("stimulus", {}, fn(b, c) {
    b.supply(c.net("VBUS"), {v: 5000mV}) // board rail
    let period = 500ms
    b.pulse(c.net("3V3"), c.mcu.u.VSS, {v: 3.3V width: 2ms every: period})
    b.vsrc(c.net("LED0"), c.net("GND"), {dc: 2V})
    b.isrc(c.net("LED0"), c.mcu.u.VSS, {dc: 10mA})
})"#;
    let mut sources = Sources::new();
    sources.add("ledctl.ckt.splash", CIRCUIT).expect("the circuit fixture is accepted");
    sources.add("stimulus.bench.splash", text).expect("the stimulus bench is accepted");
    let libs = std_libs();
    let evaluated = evaluate(&sources, &libs).expect("the stimulus project evaluates");
    let s = Setup { sources, libs, evaluated };
    assert_eq!(s.bench("stimulus").state, UnitState::Built, "the stimulus fixture must build");
    let supply = |key: &str, value| BenchOp::SetSupply { net: "VBUS".into(), key: key.into(), value };
    let source = |a: &str, key: &str, value| BenchOp::SetSource {
        a: a.into(), b: "GND".into(), key: key.into(), value,
    };

    let changes = [
        supply("v", 3.3), supply("limit", 0.5), source("3V3", "width", 0.004),
        source("3V3", "every", 0.25),
    ].map(|operation| op("stimulus", operation));
    let verified = splice_verified(&s.sources, &s.libs, &s.evaluated, &changes, &NO_CANCEL)
        .expect("the literal and resolved edits must produce the requested evaluated options");
    let edits: Vec<_> = verified.edit.edits.iter()
        .map(|e| (replaced(&s.sources, e), e.text.as_str())).collect();
    assert!(edits.contains(&("5000mV", "3300mV")), "changing supply voltage must keep the written mV suffix");
    assert!(edits.contains(&("", " limit: 500mA")), "an absent current limit must be inserted in amperes");
    assert!(edits.contains(&("2ms", "4ms")), "changing pulse width must replace only the millisecond literal");
    assert!(edits.contains(&("500ms", "250ms")), "a verified edit must change the input to the computed period");
    assert_eq!(edits.len(), 4, "the splice must preserve all surrounding bench text");

    let noop = splice(&s.sources, &s.evaluated.map, &s.evaluated.design,
        &[op("stimulus", source("3V3", "every", 0.5))]).expect("an unchanged computed period is editable");
    assert!(noop.edits.is_empty(), "an unchanged value must produce no edit");
    s.refuse("stimulus", source("3V3", "every", 0.25), &computed("source (3V3, GND) every"), "period");
    s.refuse("stimulus", source("3V3", "dc", 1.0), "source (3V3, GND): pulse has no numeric option dc",
        "{v: 3.3V width: 2ms every: period}");
    s.refuse("stimulus", source("LED0", "dc", 2.0), "source (LED0, GND): ambiguous source pair",
        r#"b.isrc(c.net("LED0"), c.mcu.u.VSS, {dc: 10mA})"#);
    let after = Setup { sources: verified.sources, evaluated: verified.evaluated, libs: s.libs };
    after.refuse("stimulus", supply("limit", 0.0), "supply VBUS: limit must be finite and greater than 0", "500mA");
}

#[test]
fn supplies_and_sources_add_and_remove() {
    use fab_elec_bench::bench::SourceKind;

    let text = r#"let e = mod.elec
bench("desk", {circuit: fn() {
    let rail = e.net({name: "VBUS"})
    let out = e.net({name: "SIG"})
    let gnd = e.net({name: "RETURN" kind: @ground})
}}, fn(tb, ck) {
    let reverse = fn() { tb.load(ck.net("RETURN"), ck.net("SIG"), {r: 10kohm}) }
    reverse()
})"#;
    let mut sources = Sources::new();
    sources.add("desk.bench.splash", text).expect("the bench document is accepted");
    let libs = Libs::default();
    let evaluated = evaluate(&sources, &libs).expect("the desk circuit evaluates");
    let mut s = Setup { sources, libs, evaluated };
    assert_eq!(s.bench("desk").state, UnitState::Built, "the desk bench and its generated load build");
    let circuit = s.bench("desk").circuit(&s.evaluated.design).expect("desk has its own circuit");
    let rail = circuit.net("VBUS").expect("the rail exists").id;
    let signal = circuit.net("SIG").expect("the signal exists").id;
    let ground = circuit.net("RETURN").expect("the named ground exists").id;
    let verify = |s: &Setup, operation| {
        splice_verified(&s.sources, &s.libs, &s.evaluated, &[op("desk", operation)], &NO_CANCEL)
    };
    let apply = |s: &mut Setup, operation| {
        let change = verify(s, operation).expect("the edited bench meets the request");
        (s.sources, s.evaluated) = (change.sources, change.evaluated);
        change.edit.edits
    };
    let refuse = |s: &Setup, operation, message, why| {
        let err = splice(&s.sources, &s.evaluated.map, &s.evaluated.design, &[op("desk", operation)])
            .expect_err(why);
        assert_eq!(err.message, message, "{why}");
    };
    let supply = |net: &str, v| BenchOp::AddSupply { net: net.into(), v };
    let remove_supply = BenchOp::RemoveSupply { net: "VBUS".into() };
    let source = |kind: &str, opts: &[(&str, f64)]| BenchOp::AddSource {
        kind: kind.into(), a: "SIG".into(), b: "RETURN".into(),
        opts: opts.iter().map(|&(key, value)| (key.into(), value)).collect(),
    };
    let remove_source = BenchOp::RemoveSource { a: "SIG".into(), b: "RETURN".into() };

    let edits = apply(&mut s, supply("VBUS", 5.0));
    assert_eq!(edits[0].text, "    tb.supply(ck.net(\"VBUS\"), {v: 5V})\n", "supply insertion uses the bench parameters and indentation");
    assert!(matches!(&s.bench("desk").spec.supplies[..], [v] if v.net.net == rail && v.v == 5.0), "the new supply drives the requested net at five volts");
    refuse(&s, supply("VBUS", 3.3), "VBUS already has a supply", "a second supply on the rail is refused");
    refuse(&s, supply("RETURN", 5.0), "cannot supply the ground net", "the circuit's named ground refuses a supply");
    refuse(&s, supply("SIG", f64::NAN), "v must be finite", "a nonfinite supply voltage is refused before formatting");
    apply(&mut s, remove_supply.clone());
    assert!(s.bench("desk").spec.supplies.is_empty(), "the removed supply is gone");
    assert!(apply(&mut s, remove_supply).is_empty(), "removing an absent supply writes no edit");

    let edits = apply(&mut s, source("pulse", &[("width", 0.002), ("v", 5.0)]));
    assert_eq!(edits[0].text, "    tb.pulse(ck.net(\"SIG\"), ck.net(\"RETURN\"), {width: 2ms v: 5V})\n", "the pulse preserves terminal and option order with SI literals");
    assert!(s.bench("desk").spec.sources.iter().any(|src| src.a.net == signal && src.b.net == ground &&
        matches!(src.kind, SourceKind::Pulse { high: 5.0, width: 0.002, .. })), "the pulse evaluates with its requested voltage and width");
    refuse(&s, source("pulse", &[("v", 5.0)]), "missing required option width", "a pulse needs its width");
    refuse(&s, source("load", &[("r", 0.0)]), "r must be finite and greater than 0", "a zero resistance is refused before writing");
    refuse(&s, source("isrc", &[("dc", 0.5), ("v", 5.0)]), "isrc has no numeric option v", "a source refuses another call's option");
    refuse(&s, source("supply", &[("v", 5.0)]), "unknown source kind \"supply\" (expected vsrc, isrc, pulse or load)",
        "only two-terminal stimulus calls can be added as sources");
    refuse(&s, source("load", &[("i", 0.5), ("r", 100.0)]), "load requires exactly one of r, i or step", "a load needs one mode");
    refuse(&s, source("load", &[("step", 0.5)]), "step: expected two currents [from to]", "a scalar cannot specify a load step");

    let second = verify(&s, source("isrc", &[("dc", 0.5)])).expect("a second source on the pair verifies");
    let repeated = Setup { sources: second.sources, evaluated: second.evaluated, libs: s.libs.clone() };
    assert!(repeated.text("desk.bench.splash").contains("{dc: 500mA}"), "a current source writes an ampere literal");
    refuse(&repeated, remove_source.clone(), "source (SIG, RETURN): ambiguous source pair", "removing an ambiguous pair is refused");
    let reverse = BenchOp::RemoveSource { a: "RETURN".into(), b: "SIG".into() };
    refuse(&s, reverse, "source (RETURN, SIG): this source or load is made by code (a loop or a function); edit it there",
        "a helper's load is protected even when its terminals identify a unique pair");
    apply(&mut s, remove_source.clone());
    assert!(matches!(&s.bench("desk").spec.sources[..], [src] if src.a.net == ground && src.b.net == signal),
        "removing the pulse leaves only the reversed load");
    assert!(apply(&mut s, remove_source).is_empty(), "removing an absent source writes no edit");
    assert_eq!(s.text("desk.bench.splash"), text, "adding then removing the two statements restores the original bench text");
}

#[test]
fn block_bench_splices_verify_and_reuse_the_circuit() {
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
    let mut sources = Sources::new();
    sources
        .add("divider.bench.splash", text)
        .expect("the block bench document is accepted");
    let libs = std_libs();
    let evaluated = evaluate(&sources, &libs).expect("the block bench setup evaluates");
    let s = Setup { sources, libs, evaluated };
    let before = s.bench("divider.half");
    assert_eq!(before.state, UnitState::Built, "the setup block bench must build");
    let BenchScope::Own { circuit, generation } = &before.scope else {
        panic!("the setup block bench must own its circuit");
    };

    for (why, operation) in [
        ("SetRange", range("half", 2.3, 2.7)),
        ("AddProbe", BenchOp::AddProbe { of: ProbeOf::v("OUT"), label: "output".into() }),
    ] {
        let verified = splice_verified(
            &s.sources,
            &s.libs,
            &s.evaluated,
            &[op("divider.half", operation.clone())],
            &NO_CANCEL,
        )
        .unwrap_or_else(|diag| panic!("{why}: the block bench splice must verify: {diag:?}"));
        let after = Setup {
            sources: verified.sources,
            libs: s.libs.clone(),
            evaluated: verified.evaluated,
        };
        let bench = after.bench("divider.half");
        assert_eq!(bench.state, UnitState::Built, "{why}: the edited block bench must build");
        match operation {
            BenchOp::SetRange { min, max, .. } => assert_eq!(
                after.expect("divider.half", "half").range(),
                Some((min, max)),
                "SetRange: the evaluated expectation must have both requested bounds"
            ),
            BenchOp::AddProbe { of, label } => {
                assert!(
                    bench.probes.iter().any(|probe| probe.of == of && probe.label == label),
                    "AddProbe: the evaluated bench must contain the requested net and label"
                );
                assert_eq!(
                    after.text("divider.bench.splash"),
                    text.replace(
                        "}, fn(b, c) {\n",
                        "}, fn(b, c) {\n    b.probe(c.net(\"OUT\"), {label: \"output\"})\n"
                    ),
                    "AddProbe: the probe line must be inserted inside the bench function"
                );
            }
            _ => unreachable!("this test covers bounds and probes"),
        }
        let BenchScope::Own { circuit: next, generation: next_generation } = &bench.scope else {
            panic!("{why}: the block bench must retain its Own scope");
        };
        assert!(Arc::ptr_eq(circuit, next), "{why}: the circuit Arc must be reused");
        assert_eq!(generation, next_generation, "{why}: the circuit generation must stay unchanged");
        assert_eq!(before.scope_id(), bench.scope_id(), "{why}: the bench scope id must stay unchanged");
    }
}

#[test]
fn pressed_states_round_trip_and_generated_statements_refuse_removal() {
    let mut s = Setup::new();
    let original = s.sources.clone();
    let line = "    b.press(c.btn.sw) // held button\n";
    s.apply("literal", pressed("switches[1].sw", true), 1);
    assert_eq!(
        s.text("literal.bench.splash"),
        BENCH.replace(line, &format!("{line}    b.press(c.switches[1].sw)\n")),
        "a press uses an access path and is inserted after the last direct press with its comment kept"
    );
    assert_eq!(
        s.bench("literal").spec.pressed,
        ["btn.sw", "switches[1].sw"],
        "the existing press and the added indexed press both evaluate"
    );
    let added = s.sources.clone();
    s.apply("literal", pressed("switches[1].sw", true), 0);
    assert_eq!(
        s.sources, added,
        "pressing an already pressed component changes no bytes"
    );
    s.apply("literal", pressed("switches[1].sw", false), 1);
    assert_eq!(
        s.sources, original,
        "release removes the inserted line and restores all original bytes"
    );
    s.apply("literal", pressed("switches[0].sw", false), 0);
    assert_eq!(
        s.sources, original,
        "releasing an unpressed component is a byte-preserving no-op"
    );
    s.refuse(
        "looppress",
        pressed("switches[0].sw", false),
        "switches[0].sw: this press is made by code (a loop or a function); edit it there",
        "b.press(c.switches[i].sw)",
    );
    s.apply("renamed", pressed("btn.sw", true), 1);
    assert!(
        s.text("literal.bench.splash")
            .contains("fn(tb, ck) {\n    tb.press(ck.btn.sw)\n    tb.stub"),
        "a first press uses the declared bench and circuit parameters and becomes the first statement"
    );
    s.apply("renamed", pressed("btn.sw", false), 1);
    assert_eq!(
        s.sources, original,
        "first-statement insertion also releases back to the original bytes"
    );
}

#[test]
fn bounds_preserve_suffixes_add_missing_limits_and_remove_infinite_limits() {
    let mut s = Setup::new();
    let before = s.sources.clone();
    let edits = s.apply("bringup", range("3V3 up", 3.1, 3.4), 1);
    assert_eq!(
        (replaced(&before, &edits[0]), edits[0].text.as_str()),
        ("3.2V", "3.1V"),
        "the fixture rail change replaces only its min literal and keeps volts"
    );
    assert_eq!(
        s.text("bringup.bench.splash"),
        FIXTURE.replace("3.2V", "3.1V"),
        "an unchanged max is not edited"
    );
    let unchanged = s.sources.clone();
    s.apply("bringup", range("3V3 up", 3.1, 3.4), 0);
    assert_eq!(
        s.sources, unchanged,
        "unchanged bounds produce no text edits"
    );
    s.apply("literal", range("open", 0.0045, 0.005), 1);
    assert!(
        s.text("literal.bench.splash")
            .contains("{min: 4.5mA max: 5mA}"),
        "an absent max is added with the probe unit and an SI prefix"
    );
    s.apply("literal", range("open", f64::NEG_INFINITY, 0.005), 1);
    assert!(
        s.text("literal.bench.splash").contains("{max: 5mA}"),
        "an infinite min removes its written field and separator"
    );
    let unchanged = s.sources.clone();
    s.apply("literal", range("open", f64::NEG_INFINITY, 0.005), 0);
    assert_eq!(s.sources, unchanged, "an infinite absent min stays absent");
    let before = s.sources.clone();
    let edits = s.apply("literal", range("plain", -0.25, 2.0), 2);
    assert_eq!(
        edits
            .iter()
            .map(|e| (replaced(&before, e), e.text.as_str()))
            .collect::<Vec<_>>(),
        [("-1", "-0.25"), ("1", "2")],
        "signed bare bounds stay bare and each changed bound has its own edit"
    );
}

#[test]
fn structural_bounds_and_small_coefficients_round_trip() {
    let mut s = Setup::new();
    s.apply(
        "literal",
        range("rail", f64::NEG_INFINITY, f64::INFINITY),
        1,
    );
    assert!(
        s.text("literal.bench.splash")
            .contains("b.expect(\"rail\", b.now(b.v(c.net(\"3V3\"))), {})"),
        "removing both bounds leaves an empty object with no overlapping deletions"
    );
    s.apply("literal", range("rail", 3.1, 3.4), 1);
    assert!(
        s.text("literal.bench.splash")
            .contains("b.expect(\"rail\", b.now(b.v(c.net(\"3V3\"))), {min: 3.1V max: 3.4V})"),
        "adding both bounds to an empty object produces one disjoint structural edit"
    );
    let before = s.sources.clone();
    let edits = s.apply("literal", range("open", 0.0004, f64::INFINITY), 1);
    assert_eq!(
        (replaced(&before, &edits[0]), edits[0].text.as_str()),
        ("4.5mA", "0.4mA"),
        "a small bound coefficient preserves its written suffix instead of changing prefix"
    );
    let before = s.sources.clone();
    let edits = s.apply("literal", drive("PA0", pwm(1000.0, 0.123456789)), 1);
    assert_eq!(
        (replaced(&before, &edits[0]), edits[0].text.as_str()),
        ("30%", "12.3456789%"),
        "a fractional duty keeps percent and all digits needed to recover the exact value"
    );
}

#[test]
fn bounds_written_by_code_are_editable_but_computed_targets_are_located() {
    let mut s = Setup::new();
    s.apply("literal", range("loop0", 0.002, 0.006), 1);
    assert_eq!(
        s.expect("literal", "loop1").range(),
        Some((0.002, 0.006)),
        "a loop literal changes every expectation produced by the call"
    );
    s.apply("literal", range("helper", 3.1, 4.0), 1);
    assert!(
        s.text("literal.bench.splash")
            .contains("b.expect(name, b.now(b.v(c.net(\"3V3\"))), {min: 3.1V max: 4V})"),
        "a helper-made expectation edits its literal bounds in the helper"
    );
    s.apply("literal", range("mixed", 3.1, 4.0), 1);
    assert!(
        s.text("literal.bench.splash")
            .contains("{min: 3.1V max: limit}"),
        "an unchanged computed max is not a constraint on a literal min"
    );
    s.apply("literal", range("indirect", 3.0, 4.0), 0);
    s.refuse(
        "literal",
        range("indirect", 3.1, 4.0),
        &computed("indirect min"),
        "bounds",
    );
    s.refuse(
        "literal",
        range("mixed", 3.1, 4.1),
        &format!("{}; inputs: `4V` = 4", computed("mixed max")),
        "limit",
    );
    let (_, max) = s
        .expect("bringup", "LED3")
        .range()
        .expect("LED3 has a range");
    s.refuse(
        "bringup",
        range("LED3", 0.003, max),
        &format!("{}; inputs: `0.25` = 0.25, `1.0` = 1, `4.5mA` = 0.0045", computed("LED3 min")),
        "(1.0 - duty[i]) * 4.5mA",
    );
}

#[test]
fn literal_drives_edit_arguments_or_replace_the_constructor() {
    let mut s = Setup::new();
    for (pin, value, old, new) in [
        ("PA0", pwm(1000.0, 0.4), "30%", "40%"),
        ("PA0", pwm(2000.0, 0.4), "1kHz", "2kHz"),
        ("PA1", pwm(1000.0, 0.5), "b.high()", "b.pwm(1kHz, 0.5)"),
        ("PA0", Drive::Level(false), "b.pwm(2kHz, 40%)", "b.low()"),
        ("PA0", Drive::Level(true), "b.low()", "b.high()"),
        ("PB7", Drive::Input(Pull::Down), "@up", "@down"),
    ] {
        let before = s.sources.clone();
        let edits = s.apply("literal", drive(pin, value), 1);
        assert_eq!(
            (replaced(&before, &edits[0]), edits[0].text.as_str()),
            (old, new),
            "{pin}: a drive changes only its differing literal argument or its constructor"
        );
    }
    let before = s.sources.clone();
    s.apply("literal", drive("PB7", Drive::Input(Pull::Down)), 0);
    assert_eq!(
        s.sources, before,
        "setting an unchanged literal drive is a no-op"
    );
    s.apply("bringup", drive("PB7", Drive::Input(Pull::Down)), 1);
    assert_eq!(
        s.text("bringup.bench.splash"),
        FIXTURE.replace("PB7: b.input(@up)", "PB7: b.input(@down)"),
        "a pin copied from the inputs object edits that original written field"
    );
}

#[test]
fn computed_and_shared_drives_refuse_at_their_expressions() {
    let mut s = Setup::new();
    let site = s
        .evaluated
        .map
        .bench_site("bringup", &BenchKey::Stub("mcu.u".into(), "PA3".into()))
        .expect("the generated pin has a site");
    let expression = &s
        .sources
        .doc(site.doc)
        .expect("the site document exists")
        .text[site.span.start as usize..site.span.end as usize];
    assert_eq!(
        expression, "b.pwm(1kHz, duty[i])",
        "a generated pin maps to the expression that made its drive"
    );
    s.refuse(
        "bringup",
        drive("PA3", pwm(1000.0, 0.4)),
        &computed("mcu.u.PA3"),
        expression,
    );
    s.refuse(
        "computed",
        drive("PA0", pwm(2000.0, 0.3)),
        &computed("mcu.u.PA0"),
        "rate",
    );
    s.refuse(
        "computed",
        drive("PA1", pwm(1000.0, 0.4)),
        &computed("mcu.u.PA1"),
        "amount",
    );
    s.refuse(
        "computed",
        drive("PB7", Drive::Input(Pull::Down)),
        &computed("mcu.u.PB7"),
        "pull",
    );
    s.refuse(
        "shared",
        BenchOp::SetDrive {
            stub: "switches[0].sw".into(),
            pin: "A".into(),
            drive: Drive::Level(false),
        },
        &computed("switches[0].sw.A"),
        "b.high()",
    );
    let error = splice(
        &s.sources,
        &s.evaluated.map,
        &s.evaluated.design,
        &[op("literal", drive("missing", Drive::Level(false)))],
    )
    .expect_err("an unknown stub pin is refused");
    assert_eq!(
        error.message, "literal: no stub pin mcu.u.missing",
        "an unknown pin is named in the error"
    );
    assert!(
        error.sel.is_empty(),
        "an unknown pin invents no source selection"
    );
    s.apply("computed", drive("PA0", pwm(1000.0, 0.4)), 1);
    assert!(
        s.text("literal.bench.splash")
            .contains("PA0: b.pwm(rate, 40%)"),
        "an unchanged computed frequency does not prevent a literal duty edit"
    );
    s.apply("renamed", drive("PA0", pwm(1000.0, 0.5)), 1);
    assert!(
        s.text("literal.bench.splash")
            .contains("PA0: tb.pwm(1kHz, 0.5)"),
        "a changed drive kind uses the function's bench parameter name"
    );
}
