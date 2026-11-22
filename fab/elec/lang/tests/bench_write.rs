//! Literal writes are explicit source edits; subsequent knob turns edit the written constructor.

use fab_elec_core::{Diag, Sel, drive::{Drive, Pull}};
use fab_elec_design::{BenchOp, DesignOp, TextEdit};
use fab_elec_lang::{
    EvalError, NO_CANCEL, BenchEdit, BenchKey, BenchOut, Evaluated, Libs, Sources, SpanEdit, UnitState,
    bench_views, evaluate, splice, splice_verified, std_libs,
};

const CIRCUIT: &str = include_str!("../../fixtures/ledctl.ckt.splash");
const FIXTURE: &str = include_str!("../../fixtures/ledctl.bench.splash");

struct Setup {
    sources: Sources,
    libs: Libs,
    evaluated: Evaluated,
}

impl Setup {
    fn new(source: &str) -> Self {
        let mut sources = Sources::new();
        sources.add("ledctl.ckt.splash", CIRCUIT).expect("circuit source is accepted");
        sources.add("write.bench.splash", source).expect("bench source is accepted");
        let libs = std_libs();
        let evaluated = evaluate(&sources, &libs).expect("setup evaluates");
        built(&evaluated);
        Self { sources, libs, evaluated }
    }

    fn plain(&self, bench: &str, op: BenchOp) -> Result<SpanEdit, Diag> {
        splice(&self.sources, &self.evaluated.map, &self.evaluated.design, &[operation(bench, op)])
    }

    fn apply(&mut self, edit: &SpanEdit) {
        assert!(edit.reevaluate, "a literal drive write requests evaluation");
        self.sources.apply(&edit.edits).expect("drive edits are disjoint and apply");
        self.evaluated = evaluate(&self.sources, &self.libs).expect("edited sources evaluate");
        built(&self.evaluated);
        assert_eq!(text(&self.sources, "ledctl.ckt.splash"), CIRCUIT, "a drive write preserves the circuit bytes");
    }
}

fn built(evaluated: &Evaluated) {
    assert!(
        evaluated.benches.iter().all(|b| b.state == UnitState::Built),
        "every bench builds: {:?}", evaluated.benches
    );
}

fn operation(bench: &str, op: BenchOp) -> DesignOp {
    DesignOp::Bench { bench: bench.into(), op }
}

fn write(pin: &str, drive: Drive) -> BenchOp {
    BenchOp::WriteDrive { stub: "mcu.u".into(), pin: pin.into(), drive }
}

fn set(pin: &str, drive: Drive) -> BenchOp {
    BenchOp::SetDrive { stub: "mcu.u".into(), pin: pin.into(), drive }
}

fn text<'a>(sources: &'a Sources, name: &str) -> &'a str {
    &sources.doc(sources.find(name).expect("named document")).expect("document exists").text
}

fn bench<'a>(evaluated: &'a Evaluated, name: &str) -> &'a BenchOut {
    evaluated.benches.iter().find(|b| b.name == name).expect("named bench exists")
}

fn pin<'a>(evaluated: &'a Evaluated, name: &str, pin: &str) -> &'a Drive {
    &bench(evaluated, name).spec.stubs.iter().find(|s| s.path == "mcu.u")
        .expect("MCU stub exists").pins.iter().find(|(name, _)| name == pin).expect("pin exists").1
}

fn replacement<'a>(sources: &'a Sources, edit: &'a SpanEdit) -> (&'a str, &'a str) {
    assert_eq!(edit.edits.len(), 1, "a drive write has exactly one source edit");
    let edit = &edit.edits[0];
    (&sources.doc(edit.doc).expect("edited document").text[edit.span.start as usize..edit.span.end as usize], &edit.text)
}

fn selected<'a>(sources: &'a Sources, diag: &Diag) -> &'a str {
    let [Sel::Source { doc, start, end }] = diag.sel.as_slice() else {
        panic!("a refused write selects exactly the stub call: {diag:?}")
    };
    &sources.doc(*doc).expect("selected document").text[*start as usize..*end as usize]
}

fn literal(evaluated: &Evaluated, sources: &Sources, name: &str, pin: &str) {
    let views = bench_views(evaluated, sources);
    let view = views.iter().find(|view| view.out.name == name).expect("bench view exists");
    let key = BenchKey::Stub("mcu.u".into(), pin.into());
    assert_eq!(
        view.edits.iter().find(|(k, _)| k == &key).map(|(_, edit)| edit),
        Some(&BenchEdit::Literal), "{name}.{pin}: the written constructor is a Literal drive site"
    );
}

#[test]
fn fixture_write_inserts_one_line_and_subsequent_turns_edit_its_literal() {
    let mut s = Setup::new(FIXTURE);
    let op = write("PA3", Drive::Input(Pull::Up));
    let plain = s.plain("bringup", op.clone()).expect("a computed pin has a predictable literal write");
    let pos = FIXTURE.find("    b.stub(c.mcu.u, pins)").expect("fixture stub statement") as u32;
    assert_eq!(
        plain.edits,
        [TextEdit {
            doc: s.sources.find("write.bench.splash").expect("bench document"),
            span: pos..pos,
            text: "    pins.PA3 = b.input(@up)\n".into(),
        }],
        "WriteDrive inserts exactly one line immediately before the stub at its indentation"
    );
    let verified = splice_verified(&s.sources, &s.libs, &s.evaluated, &[operation("bringup", op)], &NO_CANCEL)
        .expect("the explicit PA3 write verifies");
    assert_eq!(verified.edit, plain, "plain and verified writes produce exactly the same edit");
    assert_eq!(
        verified.also, Vec::<(String, BenchKey)>::new(),
        "also is empty: LED3 limits read duty[3], which the literal write does not change"
    );
    assert_eq!(
        bench(&verified.evaluated, "bringup").expects, bench(&s.evaluated, "bringup").expects,
        "every expectation, including LED3, retains its original bounds"
    );
    s.sources = verified.sources;
    s.evaluated = verified.evaluated;
    built(&s.evaluated);
    let written = FIXTURE.replace("    b.stub(c.mcu.u, pins)", "    pins.PA3 = b.input(@up)\n    b.stub(c.mcu.u, pins)");
    assert_eq!(text(&s.sources, "write.bench.splash"), written, "every byte except the inserted assignment is preserved");
    assert_eq!(pin(&s.evaluated, "bringup", "PA3"), &Drive::Input(Pull::Up), "PA3 evaluates to input(@up)");
    let origin = s.evaluated.map.bench_origin("bringup", &BenchKey::Stub("mcu.u".into(), "PA3".into())).expect("written origin");
    assert_eq!(
        &text(&s.sources, "write.bench.splash")[origin.call.span.start as usize..origin.call.span.end as usize],
        "b.input(@up)", "the pin origin is the new assignment's constructor"
    );
    assert!(origin.loops.is_empty(), "the written drive does not retain the old loop iteration");
    literal(&s.evaluated, &s.sources, "bringup", "PA3");

    for (op, old, new, expected) in [
        (set("PA3", Drive::Level(true)), "b.input(@up)", "b.high()", Drive::Level(true)),
        (write("PA3", Drive::Pwm { freq: 1000.0, duty: 0.4 }), "b.high()", "b.pwm(1kHz, 0.4)", Drive::Pwm { freq: 1000.0, duty: 0.4 }),
        (write("PA3", Drive::Pwm { freq: 1000.0, duty: 0.6 }), "0.4", "0.6", Drive::Pwm { freq: 1000.0, duty: 0.6 }),
    ] {
        let before = text(&s.sources, "write.bench.splash").to_string();
        let assignment = before.lines().find(|line| line.starts_with("    pins.PA3 = ")).expect("written PA3 assignment");
        let expected_source = before.replace(assignment, &assignment.replacen(old, new, 1));
        let edit = s.plain("bringup", op).expect("a subsequent plain splice edits the written literal");
        assert_eq!(replacement(&s.sources, &edit), (old, new), "the next turn edits only the constructor or its changed argument");
        s.apply(&edit);
        let after = text(&s.sources, "write.bench.splash");
        assert_eq!(after, expected_source, "a subsequent turn preserves all surrounding bytes");
        assert_eq!(after.matches("pins.PA3 = ").count(), 1, "repeated WriteDrive never inserts a second assignment");
        assert_eq!(pin(&s.evaluated, "bringup", "PA3"), &expected, "the next turn evaluates to the requested drive");
        literal(&s.evaluated, &s.sources, "bringup", "PA3");
    }
    let noop = s.plain("bringup", write("PA3", Drive::Pwm { freq: 1000.0, duty: 0.6 })).expect("unchanged literal write");
    assert!(noop.edits.is_empty(), "writing the same literal again is a byte-preserving no-op");
}

#[test]
fn inline_writes_replace_computed_values_and_add_missing_fields() {
    let source = r#"bench("inline", {}, fn(b, c) {
    let x = 0.75
    b.stub(c.mcu.u, {PA0: if x > 0.5 { b.pwm(1kHz, x) } else { b.low() } PA1: b.high()})
})
bench("empty", {}, fn(b, c) { b.stub(c.mcu.u, {}) })
"#;
    let mut s = Setup::new(source);
    let edit = s.plain("inline", write("PA0", Drive::Input(Pull::Up))).expect("an inline computed field is writable");
    let old = "if x > 0.5 { b.pwm(1kHz, x) } else { b.low() }";
    assert_eq!(replacement(&s.sources, &edit), (old, "b.input(@up)"), "WriteDrive replaces only the computed field value");
    s.apply(&edit);
    assert_eq!(text(&s.sources, "write.bench.splash"), source.replace(old, "b.input(@up)"), "inline replacement preserves all other fields and inputs");
    assert_eq!(pin(&s.evaluated, "inline", "PA0"), &Drive::Input(Pull::Up), "the replaced inline value evaluates to input(@up)");
    literal(&s.evaluated, &s.sources, "inline", "PA0");
    let before = text(&s.sources, "write.bench.splash").to_string();
    let verified = splice_verified(&s.sources, &s.libs, &s.evaluated, &[operation("inline", write("PA2", Drive::Level(false)))], &NO_CANCEL)
        .expect("adding a missing inline pin verifies");
    assert_eq!(
        text(&verified.sources, "write.bench.splash"),
        before.replace("PA1: b.high()}", "PA1: b.high() PA2: b.low()}"),
        "the syntax field helper adds only the missing PA2 field"
    );
    assert!(verified.also.is_empty(), "adding PA2 leaves every other pin unchanged");
    assert_eq!(pin(&verified.evaluated, "inline", "PA2"), &Drive::Level(false), "the new PA2 field evaluates low");
    s.sources = verified.sources;
    s.evaluated = verified.evaluated;
    literal(&s.evaluated, &s.sources, "inline", "PA2");
    let literal_write = s.plain("inline", write("PA1", Drive::Level(false))).expect("an existing literal field is writable");
    let knob = s.plain("inline", set("PA1", Drive::Level(false))).expect("the literal field is knob-editable");
    assert_eq!(literal_write, knob, "case a uses the same exact edit as SetDrive");
    assert_eq!(replacement(&s.sources, &literal_write), ("b.high()", "b.low()"), "a literal field replaces only its constructor");
    let empty = s.plain("empty", write("PA0", Drive::Level(true))).expect("a stub without any pins still has a mapped call");
    s.apply(&empty);
    assert_eq!(pin(&s.evaluated, "empty", "PA0"), &Drive::Level(true), "adding the first pin to an empty object evaluates high");
}

#[test]
fn named_object_writes_use_the_bench_parameter_and_replace_the_recorded_origin() {
    let source = concat!(
        "bench(\"named\", {}, fn(tb, ck) {\r\n\tlet x = 0.3\r\n\tlet pins = {PA0: tb.pwm(1kHz, x)}\r\n\ttb.stub(ck.mcu.u, pins) // retained\r\n})\r\n",
        "bench(\"quoted\", {}, fn(b, c) {\r\n    let pins = {}\r\n    b.stub(c.mcu.dbg, pins, {rail: c.net(\"3V3\")})\r\n})\r\n",
    );
    let mut s = Setup::new(source);
    let edit = s.plain("named", write("PA0", Drive::Level(true))).expect("a local object allows an explicit assignment");
    assert_eq!(replacement(&s.sources, &edit), ("", "\tpins.PA0 = tb.high()\r\n"), "the write uses the first parameter, indentation and CRLF");
    s.apply(&edit);
    assert_eq!(
        text(&s.sources, "write.bench.splash"),
        source.replace("\ttb.stub", "\tpins.PA0 = tb.high()\r\n\ttb.stub"),
        "the original computed field and trailing comment are untouched"
    );
    literal(&s.evaluated, &s.sources, "named", "PA0");
    let edit = s.plain("named", set("PA0", Drive::Level(false))).expect("the new assignment is a literal site");
    assert_eq!(replacement(&s.sources, &edit), ("tb.high()", "tb.low()"), "the next knob turn selects the assignment, not the old object field");
    s.apply(&edit);
    assert_eq!(pin(&s.evaluated, "named", "PA0"), &Drive::Level(false), "the renamed parameter's literal evaluates low");
    let quoted = splice_verified(&s.sources, &s.libs, &s.evaluated, &[operation("quoted", BenchOp::WriteDrive {
        stub: "mcu.dbg".into(), pin: "1".into(), drive: Drive::Input(Pull::None),
    })], &NO_CANCEL).expect("a header pin with a numeric name can be written literally");
    assert_eq!(replacement(&s.sources, &quoted.edit), ("", "    pins[\"1\"] = b.input(@none)\r\n"), "non-identifier pin names use a quoted index");
    assert!(quoted.also.is_empty(), "writing the header pin changes no other drive");
    s.sources = quoted.sources;
    s.evaluated = quoted.evaluated;
    let view = bench_views(&s.evaluated, &s.sources).into_iter().find(|view| view.out.name == "quoted").expect("header bench view");
    assert_eq!(view.edits, [(BenchKey::Stub("mcu.dbg".into(), "1".into()), BenchEdit::Literal)], "a quoted assignment is a Literal site after evaluation");
    let edit = s.plain("quoted", BenchOp::SetDrive {
        stub: "mcu.dbg".into(), pin: "1".into(), drive: Drive::Level(true),
    }).expect("a quoted assignment supports a subsequent plain knob turn");
    assert_eq!(replacement(&s.sources, &edit), ("b.input(@none)", "b.high()"), "the quoted assignment's constructor alone changes");
    s.apply(&edit);
    assert_eq!(bench(&s.evaluated, "quoted").spec.stubs[0].pins, [("1".into(), Drive::Level(true))], "the header pin evaluates high after the next turn");
}

#[test]
fn call_results_refuse_at_the_stub_and_invalid_pwm_never_returns_an_edit() {
    let source = r#"bench("call", {}, fn(b, c) {
    let make = fn() { {PA0: b.high()} }
    b.stub(c.mcu.u, make())
})
bench("boundcall", {}, fn(b, c) {
    let make = fn() { {PA0: b.high()} }
    let pins = make()
    b.stub(c.mcu.u, pins)
})
bench("computed", {}, fn(b, c) {
    let x = 0.3
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, x)})
})
"#;
    let s = Setup::new(source);
    for (name, call, reason) in [
        ("call", "b.stub(c.mcu.u, make())", "call results and other expressions are not writable"),
        ("boundcall", "b.stub(c.mcu.u, pins)", "the pins name must be bound by let to an object in this function body"),
    ] {
        let op = write("PA0", Drive::Input(Pull::Up));
        let error = s.plain(name, op.clone()).expect_err("pins from a call result refuse a literal write");
        assert_eq!(selected(&s.sources, &error), call, "{name}: refusal selects the stub call");
        assert!(error.message.contains(reason), "{name}: refusal explains the unsupported pins expression: {}", error.message);
        let verified = splice_verified(&s.sources, &s.libs, &s.evaluated, &[operation(name, op)], &NO_CANCEL).expect_err("verified writes refuse the same call result");
        assert_eq!(verified, EvalError::Diags(vec![error]), "plain and verified call-result refusals agree exactly");
    }
    for (freq, duty) in [(0.0, 0.4), (-1.0, 0.4), (f64::INFINITY, 0.4), (f64::NAN, 0.4), (1000.0, -0.1), (1000.0, 1.1), (1000.0, f64::INFINITY), (1000.0, f64::NAN)] {
        let error = s.plain("computed", write("PA0", Drive::Pwm { freq, duty })).expect_err("invalid PWM cannot be written");
        assert_eq!(error.message, "mcu.u.PA0: PWM needs a finite positive frequency and duty between 0 and 1", "WriteDrive uses SetDrive's PWM validation");
        assert_eq!(selected(&s.sources, &error), "b.stub(c.mcu.u, {PA0: b.pwm(1kHz, x)})", "invalid computed writes select the stub");
    }
    assert_eq!(text(&s.sources, "write.bench.splash"), source, "refused writes preserve the source snapshot");
}
