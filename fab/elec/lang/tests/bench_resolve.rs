//! Reference edits are returned only with a matching evaluation and their collateral changes.

use fab_elec_core::{
    Diag, Sel,
    drive::{Drive, Pull},
};
use fab_elec_design::{BenchOp, DesignOp, ProbeOf};
use fab_elec_lang::{
    EvalError, NO_CANCEL, BenchKey, BenchOut, Evaluated, Libs, Sources, UnitState, Verified, evaluate,
    resolve::{Leaf, Term, resolve},
    splice, splice_verified, std_libs,
    units::circuit_units,
};
use makepad_script::syntax;

const CIRCUIT: &str = include_str!("../../fixtures/ledctl.ckt.splash");
const FIXTURE: &str = include_str!("../../fixtures/ledctl.bench.splash");

struct Setup {
    sources: Sources,
    libs: Libs,
    evaluated: Evaluated,
}

impl Setup {
    fn new(extra: &str) -> Self {
        let mut sources = Sources::new();
        sources
            .add("ledctl.ckt.splash", CIRCUIT)
            .expect("circuit source");
        sources
            .add("bringup.bench.splash", FIXTURE)
            .expect("fixture source");
        if !extra.is_empty() {
            sources
                .add("references.bench.splash", extra)
                .expect("reference source");
        }
        let libs = std_libs();
        let evaluated = evaluate(&sources, &libs).expect("setup evaluates");
        assert!(
            evaluated
                .benches
                .iter()
                .all(|b| b.state == UnitState::Built),
            "all initial benches build: {:?}",
            evaluated.benches
        );
        Self {
            sources,
            libs,
            evaluated,
        }
    }

    fn run(&self, bench: &str, op: BenchOp) -> Result<Verified, Diag> {
        splice_verified(
            &self.sources,
            &self.libs,
            &self.evaluated,
            &[DesignOp::Bench {
                bench: bench.into(),
                op,
            }],
            &NO_CANCEL,
        )
        .map_err(EvalError::into_diag)
    }

    fn leaf(&self, bench: &str, pin: &str, arg: usize) -> (Leaf, f64) {
        let origin = self
            .evaluated
            .map
            .bench_origin(bench, &stub(pin))
            .expect("drive origin");
        let doc = self.sources.doc(origin.call.doc).expect("origin document");
        let tree = syntax::parse_with(&doc.text, &circuit_units()).expect("bench syntax");
        let mut call = None;
        tree.walk(&mut |_, node| {
            if node.span == (origin.call.span.start as usize..origin.call.span.end as usize) {
                call = Some(node);
            }
        });
        let expr = &call.expect("origin call").as_call().expect("constructor").1[arg];
        let term = resolve(&tree, expr, origin);
        let (leaf, scale) = term.leaf().expect("a reference has one numeric leaf");
        (leaf.clone(), scale)
    }

    fn accept(&mut self, result: Verified) {
        self.sources = result.sources;
        self.evaluated = result.evaluated;
    }
}

fn stub(pin: &str) -> BenchKey {
    BenchKey::Stub("mcu.u".into(), pin.into())
}

fn drive(pin: &str, freq: f64, duty: f64) -> BenchOp {
    BenchOp::SetDrive {
        stub: "mcu.u".into(),
        pin: pin.into(),
        drive: Drive::Pwm { freq, duty },
    }
}

fn bench<'a>(evaluated: &'a Evaluated, name: &str) -> &'a BenchOut {
    evaluated
        .benches
        .iter()
        .find(|b| b.name == name)
        .expect("named bench")
}

fn pin<'a>(evaluated: &'a Evaluated, name: &str, pin: &str) -> &'a Drive {
    &bench(evaluated, name)
        .spec
        .stubs
        .iter()
        .find(|s| s.path == "mcu.u")
        .expect("stub")
        .pins
        .iter()
        .find(|(name, _)| name == pin)
        .expect("pin")
        .1
}

fn text<'a>(sources: &'a Sources, name: &str) -> &'a str {
    &sources
        .doc(sources.find(name).expect("document name"))
        .expect("document")
        .text
}

fn selected<'a>(sources: &'a Sources, diag: &Diag) -> Vec<&'a str> {
    diag.sel
        .iter()
        .filter_map(|sel| match sel {
            Sel::Source { doc, start, end } => Some(
                &sources.doc(*doc).expect("selected document").text[*start as usize..*end as usize],
            ),
            _ => None,
        })
        .collect()
}

fn replacement<'a>(sources: &'a Sources, result: &'a Verified) -> (&'a str, &'a str) {
    assert_eq!(
        result.edit.edits.len(),
        1,
        "a reference changes exactly one token"
    );
    let edit = &result.edit.edits[0];
    (
        &sources.doc(edit.doc).expect("edited document").text
            [edit.span.start as usize..edit.span.end as usize],
        &edit.text,
    )
}

#[test]
fn fixture_references_report_iterations_and_collateral_values() {
    let s = Setup::new(
        r#"bench("arms", {}, fn(b, c) {
    let x = 0.75
    let negative = -0.2
    let choice = 0.25
    let blocked = 0.3
    b.stub(c.mcu.u, {
        PA0: if x > 0.5 { b.pwm(1kHz, x) } else { b.high() }
        PA1: if 1.0 == -(negative) { b.high() } else { b.pwm(1kHz, -(negative)) }
        PA2: if blocked > 0.1 { b.low() }
             else if blocked == 1.0 { b.high() }
             else if blocked == -1.0 { b.high() }
             else { b.input(@none) }
        PB7: if choice == 0.5 { b.input(@up) }
             else if choice == 0.0 { b.input(@down) }
             else { b.pwm(1kHz, choice) }
    })
})"#,
    );
    let changed = s
        .run("bringup", drive("PA3", 1000.0, 0.4))
        .expect("indexed duty verifies");
    assert_eq!(
        replacement(&s.sources, &changed),
        ("0.25", "0.4"),
        "PA3 writes its own list item"
    );
    assert_eq!(
        text(&changed.sources, "bringup.bench.splash"),
        FIXTURE.replacen("0.25", "0.4", 1),
        "the indexed duty edit preserves every other byte"
    );
    assert_eq!(
        pin(&changed.evaluated, "bringup", "PA3"),
        &Drive::Pwm {
            freq: 1000.0,
            duty: 0.4
        },
        "the returned evaluation has the requested PA3 drive"
    );
    assert_eq!(
        changed.also,
        [("bringup".into(), BenchKey::Expect("LED3".into()))],
        "only the LED3 expectation changes alongside PA3"
    );
    assert_eq!(
        text(&changed.sources, "ledctl.ckt.splash"),
        CIRCUIT,
        "bench edits leave circuit bytes intact"
    );
    for (pin, index, written) in [("PA2", 2, "0.1"), ("PA5", 5, "0.5")] {
        let origin = s
            .evaluated
            .map
            .bench_origin("bringup", &stub(pin))
            .expect("pin origin");
        assert_eq!(
            origin.loops,
            [index],
            "{pin} records its own loop iteration"
        );
        let (leaf, scale) = s.leaf("bringup", pin, 1);
        assert_eq!(
            (&FIXTURE[leaf.at], scale),
            (written, 1.0),
            "{pin} resolves to its own duty item"
        );
    }
    let mut followed = s.evaluated.map.clone();
    followed.follow(&changed.edit);
    for key in [stub("PA3"), BenchKey::Expect("LED3".into())] {
        assert_eq!(
            followed.bench_origin("bringup", &key),
            changed.evaluated.map.bench_origin("bringup", &key),
            "origin call ranges follow edits with their loop positions intact"
        );
        assert_eq!(
            followed.bench_site("bringup", &key),
            changed.evaluated.map.bench_site("bringup", &key),
            "bench source ranges still follow edits"
        );
    }
    let frequency = s
        .run("bringup", drive("PA3", 2000.0, 0.25))
        .expect("shared frequency verifies");
    assert_eq!(
        replacement(&s.sources, &frequency),
        ("1kHz", "2kHz"),
        "frequency retains its written suffix"
    );
    assert_eq!(
        frequency.also,
        ["PA2", "PA4", "PA5", "PA6", "PA7"].map(|p| ("bringup".into(), stub(p))),
        "the shared frequency names every other PWM pin and no unchanged expectation"
    );
    let changed = s
        .run("bringup", drive("PA0", 1000.0, 0.4))
        .expect("the PWM alternative resolves from the low arm");
    assert_eq!(
        replacement(&s.sources, &changed),
        ("0.0", "0.4"),
        "PA0 writes the first duty item"
    );
    assert_eq!(
        text(&changed.sources, "bringup.bench.splash"),
        FIXTURE.replacen("0.0", "0.4", 1),
        "the alternative changes only the first list item, leaving its guard intact"
    );
    assert_eq!(
        pin(&changed.evaluated, "bringup", "PA0"),
        &Drive::Pwm { freq: 1000.0, duty: 0.4 },
        "PA0 evaluates to the requested PWM alternative"
    );
    assert_eq!(
        changed.also,
        [("bringup".into(), BenchKey::Expect("LED0".into()))],
        "LED0 reads the edited duty item in its bounds"
    );
    for (pin_name, high, old, new, expectation) in [
        ("PA3", true, "0.25", "1.0", "LED3"),
        ("PA1", false, "1.0", "0.0", "LED1"),
    ] {
        let op = BenchOp::SetDrive {
            stub: "mcu.u".into(), pin: pin_name.into(), drive: Drive::Level(high),
        };
        let changed = s.run("bringup", op.clone()).expect("the level arm's own guard solves");
        assert_eq!(replacement(&s.sources, &changed), (old, new), "{pin_name} edits its duty item");
        assert_eq!(
            pin(&changed.evaluated, "bringup", pin_name), &Drive::Level(high),
            "{pin_name} evaluates to the requested level"
        );
        assert_eq!(
            changed.also, [("bringup".into(), BenchKey::Expect(expectation.into()))],
            "the changed duty item also changes its LED expectation"
        );
        assert!(
            splice(&s.sources, &s.evaluated.map, &s.evaluated.design,
                &[DesignOp::Bench { bench: "bringup".into(), op }]).is_err(),
            "plain splice refuses alternative forms"
        );
    }
    assert!(
        splice(&s.sources, &s.evaluated.map, &s.evaluated.design,
            &[DesignOp::Bench { bench: "bringup".into(), op: drive("PA0", 1000.0, 0.4) }]).is_err(),
        "plain splice refuses the PWM alternative"
    );
    let error = s.run("bringup", BenchOp::SetDrive {
        stub: "mcu.u".into(), pin: "PA3".into(), drive: Drive::Input(Pull::Up),
    }).expect_err("no arm constructs an input");
    let sites = selected(&s.sources, &error);
    assert!(
        error.message.contains("PA3's drive is made by this `if`; no arm makes an input")
            && error.message.contains("`0.25` = 0.25"),
        "the refusal explains the missing form and names its input: {}",
        error.message
    );
    assert!(
        sites[0].starts_with("if duty[i]") && sites[0].ends_with("{ b.pwm(1kHz, duty[i]) }"),
        "the refusal selects the whole if chain"
    );
    assert_eq!(
        &sites[1..], ["0.25", "0.0", "1.0", "1kHz"],
        "the refusal selects the indexed duty item and the literals its arms read"
    );

    let high = BenchOp::SetDrive {
        stub: "mcu.u".into(), pin: "PA0".into(), drive: Drive::Level(true),
    };
    let error = s.run("arms", high).expect_err("the else arm has no guard to solve");
    assert!(
        error.message.contains("no matching arm has a solvable edit")
            && selected(&s.sources, &error)[0].starts_with("if x > 0.5"),
        "an unreachable else refuses at the if"
    );
    let changed = s.run("arms", drive("PA0", 1000.0, 0.6)).expect("the PWM arm still resolves");
    assert_eq!(replacement(&s.sources, &changed), ("0.75", "0.6"), "PWM edits x's literal");
    assert_eq!(
        pin(&changed.evaluated, "arms", "PA0"), &Drive::Pwm { freq: 1000.0, duty: 0.6 },
        "the changed PWM stays on its arm"
    );
    let changed = s.run("arms", BenchOp::SetDrive {
        stub: "mcu.u".into(), pin: "PA1".into(), drive: Drive::Level(true),
    }).expect("a reversed equality applies the term's scale");
    assert_eq!(replacement(&s.sources, &changed), ("-0.2", "-1.0"), "the guard inverts unary minus");
    assert_eq!(pin(&changed.evaluated, "arms", "PA1"), &Drive::Level(true), "the reversed guard verifies");
    for (pull, new) in [(Pull::Up, "0.5"), (Pull::Down, "0.0")] {
        let changed = s.run("arms", BenchOp::SetDrive {
            stub: "mcu.u".into(), pin: "PB7".into(), drive: Drive::Input(pull),
        }).expect("an input arm solves its own guard");
        assert_eq!(replacement(&s.sources, &changed), ("0.25", new), "the input pull stays written");
        assert_eq!(pin(&changed.evaluated, "arms", "PB7"), &Drive::Input(pull), "the input arm verifies");
    }
    let error = s.run("arms", BenchOp::SetDrive {
        stub: "mcu.u".into(), pin: "PB7".into(), drive: Drive::Input(Pull::None),
    }).expect_err("an input arm must have the requested written pull");
    assert!(error.message.contains("no arm makes an input"), "pull literals are not edited");
    let error = s.run("arms", BenchOp::SetDrive {
        stub: "mcu.u".into(), pin: "PA2".into(), drive: Drive::Level(true),
    }).expect_err("the first high arm is blocked by an earlier guard");
    assert!(
        error.message.contains("verification failed"),
        "failed verification never falls through to the reachable second high arm"
    );
    assert_eq!(
        selected(&s.sources, &error), ["b.low()", "0.3"],
        "verification reports the origin and the proposed edit, leaving the original source intact"
    );
}

const REFERENCES: &str = r#"bench("scalar", {}, fn(b, c) {
    let d = 0.3
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, d)})
})
bench("assigned", {}, fn(b, c) {
    let d = 0.3
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, d)})
    d = 0.5
})
bench("indexed", {}, fn(b, c) {
    let k = [0.2 0.4]
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, k[1])})
})
bench("nested", {}, fn(b, c) {
    let grid = [[0.1 0.2] [0.3 0.4]]
    let pins = {}
    for r in 0..2 {
        for c in 0..2 { pins["PA" + (r * 2 + c)] = b.pwm(1kHz, grid[r][c]) }
    }
    b.stub(c.mcu.u, pins)
})
bench("members", {}, fn(b, c) {
    let rows = [{duty: 0.2} {duty: 0.4}]
    let o = {duty: 0.3}
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, o.duty) PA1: b.pwm(1kHz, rows[1].duty)})
})
bench("negative", {}, fn(b, c) {
    let d = -30%
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, -(d))})
})
bench("items", {}, fn(b, c) {
    let pins = {}
    for v in [0.2 0.4] { pins.PA0 = b.pwm(1kHz, v) }
    b.stub(c.mcu.u, pins)
})
bench("offset", {}, fn(b, c) {
    let d = [0.2 0.4 0.6]
    let pins = {}
    for i in 1..3 { pins["PA" + i] = b.pwm(1kHz, d[i]) }
    b.stub(c.mcu.u, pins)
})
bench("frames", {}, fn(b, c) {
    let d = [0.2 0.4]
    let pins = {}
    let make = fn() { for j in 0..2 { pins["PA" + j] = b.pwm(1kHz, d[j]) } }
    for i in 0..3 { make() }
    b.stub(c.mcu.u, pins)
})
bench("mutated", {}, fn(b, c) {
    let d = [0.2 0.4]
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, d[1])})
    d[1] = 0.5
})
bench("arithmetic", {}, fn(b, c) {
    let d = 0.3
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, d * 1.0)})
})
"#;

#[test]
fn references_follow_bindings_containers_and_function_local_loop_depths() {
    let s = Setup::new(REFERENCES);
    for (name, pin_name, old, new, target) in [
        ("scalar", "PA0", "0.3", "0.45", 0.45),
        ("indexed", "PA0", "0.4", "0.45", 0.45),
        ("nested", "PA3", "0.4", "0.45", 0.45),
        ("members", "PA0", "0.3", "0.45", 0.45),
        ("members", "PA1", "0.4", "0.45", 0.45),
        ("negative", "PA0", "-30%", "-40%", 0.4),
        ("items", "PA0", "0.4", "0.45", 0.45),
        ("offset", "PA2", "0.6", "0.45", 0.45),
        ("frames", "PA1", "0.4", "0.45", 0.45),
    ] {
        let changed = s
            .run(name, drive(pin_name, 1000.0, target))
            .expect("reference verifies");
        assert_eq!(
            replacement(&s.sources, &changed),
            (old, new),
            "{name} edits the resolved written leaf"
        );
        assert_eq!(
            pin(&changed.evaluated, name, pin_name),
            &Drive::Pwm {
                freq: 1000.0,
                duty: target
            },
            "{name} returns the requested evaluated drive"
        );
        assert!(
            changed.also.is_empty(),
            "{name} changes no other pin or expectation"
        );
    }
    let nested = s
        .evaluated
        .map
        .bench_origin("nested", &stub("PA3"))
        .expect("nested origin");
    assert_eq!(nested.loops, [1, 1], "nested loops are outermost first");
    let frames = s
        .evaluated
        .map
        .bench_origin("frames", &stub("PA1"))
        .expect("helper origin");
    assert_eq!(
        frames.loops,
        [1],
        "caller loops are excluded from the helper's origin"
    );
    let term = s.leaf("negative", "PA0", 1);
    assert_eq!(
        (term.0.value, term.1),
        (-0.3, -1.0),
        "a negative literal is one written leaf; the negation around it is its scale"
    );
    for name in ["assigned", "mutated", "arithmetic"] {
        let error = s
            .run(name, drive("PA0", 1000.0, 0.6))
            .expect_err("unsupported value is refused");
        assert!(
            error.message.contains("computed") && !error.sel.is_empty(),
            "{name} refuses with a located computed diagnostic"
        );
    }
    let error = s
        .run("arithmetic", drive("PA0", 1000.0, 0.6))
        .expect_err("equal sibling counts leave arithmetic ambiguous");
    assert!(
        error.message.contains("tied editable leaves")
            && error.message.contains("`0.3` = 0.3") && error.message.contains("`1.0` = 1"),
        "tied arithmetic names both source inputs: {}",
        error.message
    );
    assert_eq!(
        selected(&s.sources, &error),
        ["d * 1.0", "0.3", "1.0"],
        "tied arithmetic selects the expression and each input"
    );
}

const LITERAL: &str = r#"bench("literal", {}, fn(b, c) {
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, 30%) PA1: b.high() PB7: b.input(@up)})
    b.expect("rail", b.now(b.v(c.net("3V3"))), {min: 3V max: 4V})
    b.expect("open", b.now(b.i(c.lamps[0].d)), {min: 4.5mA})
    b.expect("plain", b.now(b.v(c.net("GND"))), {min: -1 max: 1})
    for i in 0..2 { b.expect("loop" + i, b.now(b.i(c.lamps[i].d)), {min: 1mA max: 6mA}) }
})
"#;

#[test]
fn literal_operations_keep_their_splices_and_verify_all_bench_op_kinds() {
    let mut s = Setup::new(LITERAL);
    for (op, old, new) in [
        (drive("PA0", 1000.0, 0.4), "30%", "40%"),
        (drive("PA0", 2000.0, 0.4), "1kHz", "2kHz"),
        (drive("PA1", 1000.0, 0.5), "b.high()", "b.pwm(1kHz, 0.5)"),
        (
            BenchOp::SetDrive {
                stub: "mcu.u".into(),
                pin: "PB7".into(),
                drive: Drive::Input(Pull::Down),
            },
            "@up",
            "@down",
        ),
        (
            BenchOp::SetRange {
                expect: "rail".into(),
                min: 3.1,
                max: 4.0,
            },
            "3V",
            "3.1V",
        ),
        (
            BenchOp::SetRange {
                expect: "open".into(),
                min: 0.0004,
                max: f64::INFINITY,
            },
            "4.5mA",
            "0.4mA",
        ),
    ] {
        let changed = s.run("literal", op).expect("literal edit verifies");
        assert_eq!(
            replacement(&s.sources, &changed),
            (old, new),
            "verification preserves the existing literal splice"
        );
        assert!(
            changed.also.is_empty(),
            "independent literals have no collateral changes"
        );
        s.accept(changed);
    }
    for (op, count) in [
        (
            BenchOp::SetRange {
                expect: "plain".into(),
                min: -0.25,
                max: 2.0,
            },
            2,
        ),
        (
            BenchOp::SetRange {
                expect: "rail".into(),
                min: f64::NEG_INFINITY,
                max: f64::INFINITY,
            },
            1,
        ),
        (
            BenchOp::SetRange {
                expect: "rail".into(),
                min: 3.1,
                max: 3.4,
            },
            1,
        ),
        (
            BenchOp::SetPressed {
                comp: "btn.sw".into(),
                pressed: true,
            },
            1,
        ),
        (
            BenchOp::SetPressed {
                comp: "btn.sw".into(),
                pressed: true,
            },
            0,
        ),
        (
            BenchOp::SetPressed {
                comp: "btn.sw".into(),
                pressed: false,
            },
            1,
        ),
        (
            BenchOp::AddProbe {
                of: ProbeOf::v("3V3"),
                label: "Rail".into(),
            },
            1,
        ),
        (BenchOp::RemoveProbe { of: ProbeOf::v("3V3") }, 1),
    ] {
        let changed = s
            .run("literal", op)
            .expect("structural or state edit verifies");
        assert_eq!(
            changed.edit.edits.len(),
            count,
            "verification preserves structural edit counts and no-ops"
        );
        assert!(
            changed
                .evaluated
                .benches
                .iter()
                .all(|b| b.state == UnitState::Built),
            "literal edits return built benches"
        );
        s.accept(changed);
    }
    let looped = s
        .run(
            "literal",
            BenchOp::SetRange {
                expect: "loop0".into(),
                min: 0.002,
                max: 0.006,
            },
        )
        .expect("shared literal range verifies");
    assert_eq!(
        looped.also,
        [("literal".into(), BenchKey::Expect("loop1".into()))],
        "literal range verification names the other expectation reading its bound"
    );
    let both = splice_verified(
        &s.sources,
        &s.libs,
        &s.evaluated,
        &[
            DesignOp::Bench {
                bench: "literal".into(),
                op: BenchOp::SetRange {
                    expect: "loop0".into(),
                    min: 0.002,
                    max: 0.006,
                },
            },
            DesignOp::Bench {
                bench: "literal".into(),
                op: BenchOp::SetRange {
                    expect: "loop1".into(),
                    min: 0.001,
                    max: 0.006,
                },
            },
        ],
        &NO_CANCEL,
    )
    .expect_err("verification checks even a requested no-op changed by another op")
    .into_diag();
    assert!(
        both.message.contains("verification failed"),
        "every requested bench target is verified"
    );
}

#[test]
fn verification_discards_a_resolved_edit_when_a_guard_changes_the_drive_kind() {
    let s = Setup::new(
        r#"bench("guard", {}, fn(b, c) {
    let duty = [0.1 0.2 0.3 0.25]
    let pins = {}
    for i in 0..4 {
        pins["PA" + i] = if duty[i] > 0.5 { b.high() } else { b.pwm(1kHz, duty[i]) }
    }
    b.stub(c.mcu.u, pins)
})"#,
    );
    let original = s.sources.clone();
    let (leaf, _) = s.leaf("guard", "PA3", 1);
    assert_eq!(
        &text(&s.sources, "references.bench.splash")[leaf.at],
        "0.25",
        "the changed argument resolves before verification"
    );
    let error = s
        .run("guard", drive("PA3", 1000.0, 0.7))
        .expect_err("the high arm violates the PWM request");
    assert!(
        error.message.contains("verification failed"),
        "a changed branch produces the verification refusal"
    );
    assert_eq!(
        selected(&s.sources, &error),
        ["b.pwm(1kHz, duty[i])", "0.25"],
        "verification selects the origin expression and proposed leaf edit"
    );
    assert_eq!(
        s.sources, original,
        "failed verification returns no edit and preserves the source snapshot"
    );
    let end = s
        .run("guard", drive("PA3", 1000.0, 1.0))
        .expect("PWM at the high endpoint normalises");
    assert_eq!(
        pin(&end.evaluated, "guard", "PA3"),
        &Drive::Level(true),
        "verification returns the actual high arm after normalising the requested PWM endpoint"
    );
}

#[test]
fn static_resolution_respects_scopes_and_keeps_unsupported_terms_opaque() {
    let source = r#"let d = 0.1
let make = fn() {
    let d = 0.3
    target(d)
    for j in 0..1 { let d = 0.4; d += 0.1 }
}

let other = fn(d) { target(d) }
let ambiguous = fn() { let d = 0.2; let d = 0.4; target(d) }
let assigned = fn() { let d = 0.2; target(d); if true { d += 0.1 } }
let conditional = fn() { let d = 0.2; if true { let d = 0.4 }; target(d) }
let opaque = fn() { let d = 0.3; target(d + 1.0) }
let negative = fn() { let d = 0.3; target(-(d)) }
let alias = fn() { let rows = [{duty: 0.3}]; let o = rows[0]; target(rows[0].duty); o.duty = 0.4 }
let indexed_assignment = fn() { let k = [0.2 0.4]; target(k[1]); k[1] += 0.1 }
let algebra = fn() {
    let x = 4
    target(2 + x * 3 / 2 - 1)
    target(x * x * 1mA)
    target(1mA / x)
    target(helper(x))
    target(x - x)
    target(-(x + x))
}
"#;
    let tree = syntax::parse_with(source, &circuit_units()).expect("static reference syntax");
    let calls = tree.find_calls("target");
    let terms: Vec<_> = calls
        .iter()
        .map(|loc| {
            let call = tree.node(loc).expect("target node");
            let origin = fab_elec_lang::Origin {
                call: fab_elec_lang::SourceRange {
                    doc: fab_elec_lang::DocId(0),
                    span: call.span.start as u32..call.span.end as u32,
                },
                loops: vec![],
            };
            resolve(&tree, &call.as_call().expect("target call").1[0], &origin)
        })
        .collect();
    let (leaf, scale) = terms[0].leaf().expect("unmutated shadow binding");
    assert_eq!(
        (&source[leaf.at.clone()], scale),
        ("0.3", 1.0),
        "a loop's assigned shadow does not invalidate the enclosing function's let"
    );
    assert!(
        terms[1..5]
            .iter()
            .all(|term| matches!(term, Term::Opaque { .. })),
        "parameters, repeated lets, assignments and conditional declarations remain opaque"
    );
    let candidates = terms[5].candidates();
    assert_eq!(
        candidates.iter().map(|(leaf, a, b)| (&source[leaf.at.clone()], *a, *b)).collect::<Vec<_>>(),
        [("0.3", 1.0, 1.0), ("1.0", 0.3, 1.0)],
        "arithmetic exposes each leaf's independent affine inverse"
    );
    assert!(
        terms[7..9]
            .iter()
            .all(|term| matches!(term, Term::Opaque { .. })),
        "container assignments through aliases and indexes remain opaque"
    );
    let (leaf, scale) = terms[6].leaf().expect("unary reference");
    assert_eq!(
        (&source[leaf.at.clone()], scale),
        ("0.3", -1.0),
        "parentheses and unary minus retain the input with scale -1"
    );
    let candidates = terms[9].candidates();
    let (_, a, b) = candidates.iter().find(|(leaf, _, _)| &source[leaf.at.clone()] == "4")
        .expect("precedence expression retains x");
    assert_eq!((*a, *b), (1.0, 1.5), "VM grouping makes 2 + x * 3 / 2 - 1 affine with offset 1 and scale 1.5");
    assert_eq!(terms[10].candidates().iter().map(|(leaf, a, b)| (&source[leaf.at.clone()], *a, *b)).collect::<Vec<_>>(),
        [("1mA", 0.0, 16.0)], "x * x is non-affine in x, but still affine in its separate unit-bearing factor");
    assert_eq!(terms[11].candidates().iter().map(|(leaf, a, b)| (&source[leaf.at.clone()], *a, *b)).collect::<Vec<_>>(),
        [("1mA", 0.0, 0.25)], "division is affine in the numerator but never in its variable denominator");
    assert!(matches!(terms[12], Term::Opaque { .. }) && terms[12].candidates().is_empty(),
        "a function call of x stays opaque and cannot supply a numeric edit");
    assert!(terms[13].candidates().is_empty(), "repeated references cancel to a zero coefficient");
    let (leaf, scale) = terms[14].leaf().expect("one repeated affine input");
    assert_eq!((&source[leaf.at.clone()], scale), ("4", -2.0), "leaf keeps its unique invertible-input meaning after arithmetic");
}

#[test]
fn affine_bounds_rank_inputs_and_verify_the_fixture_without_changing_drives() {
    let s = Setup::new(r#"bench("affine", {}, fn(b, c) {
    let left = 1mA
    let right = 2mA
    let square = 3mA
    let shared = 4mA
    let limit = 20mA
    let current = [2mA 3mA]
    let duty = [0.2 0.4]
    let rows = [1mA 2mA]
    let cells = [[3mA 4mA] [5mA 6mA]]
    let aliases = [left left]
    let pins = {}
    for i in 0..2 {
        pins["PA" + i] = b.pwm(1kHz, 1.0 - duty[i])
        b.expect("indexed" + i, b.now(b.i(c.lamps[i].d)), {min: 1mA + current[i] * 2 / 4 max: 9mA})
    }
    for r in 0..2 {
        for j in 0..2 {
            b.expect("nested" + r + j, b.now(b.i(c.lamps[r].d)), {min: rows[r] + cells[r][j] max: 20mA})
        }
    }
    let ordinal = 0
    for v in [7mA 8mA] {
        b.expect("item" + ordinal, b.now(b.i(c.lamps[0].d)), {min: v + 1mA max: 20mA})
        ordinal += 1
    }
    for i in 0..2 {
        b.expect("alias" + i, b.now(b.i(c.lamps[i].d)), {min: aliases[i] + 1mA max: 20mA})
    }
    b.stub(c.mcu.u, pins)
    b.expect("tie", b.now(b.i(c.lamps[0].d)), {min: left + right max: 9mA})
    b.expect("square", b.now(b.i(c.lamps[0].d)), {min: square * square max: 9mA})
    b.expect("reciprocal", b.now(b.i(c.lamps[0].d)), {min: 0.000001 / square max: 9mA})
    b.expect("shared", b.now(b.i(c.lamps[0].d)), {min: shared / 2 max: shared * 2})
    b.expect("open", b.now(b.i(c.lamps[0].d)), {min: shared / 2})
    b.expect("scaled", b.now(b.i(c.lamps[0].d)), {min: 1mA max: limit * 1.2})
})"#);
    let range = |expect: &str, min, max| BenchOp::SetRange { expect: expect.into(), min, max };
    let before = bench(&s.evaluated, "bringup");
    let led3 = before.expects.iter().find(|e| e.name == "LED3").expect("LED3 bound");
    let (_, led3_max) = led3.range().expect("LED3 has a range");
    let changed = s.run("bringup", range("LED3", 0.003, led3_max)).expect("LED3 affine inverse verifies");
    assert_eq!(replacement(&s.sources, &changed), ("4.5mA", "4mA"), "LED3 changes exactly its current-bearing token");
    assert_eq!(text(&changed.sources, "bringup.bench.splash"), FIXTURE.replacen("4.5mA", "4mA", 1),
        "the bound inverse preserves every other fixture byte");
    let after = bench(&changed.evaluated, "bringup");
    let actual = after.expects.iter().find(|e| e.name == "LED3").expect("verified LED3");
    assert_eq!(actual.range(), Some((0.003, led3_max)), "LED3 reaches 3 mA and keeps its maximum");
    assert_eq!(changed.also, [0, 2, 4, 5, 6, 7].map(|i| {
        ("bringup".into(), BenchKey::Expect(format!("LED{i}")))
    }), "also names every other nonzero LED minimum and no stub pin");
    for (old, new) in before.expects.iter().zip(&after.expects) {
        let ((old_min, old_max), (new_min, new_max)) = (old.range().expect("old range"), new.range().expect("new range"));
        assert_eq!(old_max, new_max, "{}: a minimum drag preserves every maximum", old.name);
        assert_eq!(old_min != new_min, old.name.starts_with("LED") && old.name != "LED1",
            "{}: precisely the nonzero LED minima change", old.name);
    }
    for old in &s.evaluated.benches {
        assert_eq!(bench(&changed.evaluated, &old.name).spec.stubs, old.spec.stubs,
            "{}: a bound drag changes no drive", old.name);
    }
    let repeated = s.run("bringup", range("LED3", 0.003, led3_max)).expect("same input resolves again");
    assert_eq!(changed.edit.edits, repeated.edit.edits, "ranking and writing are deterministic");
    let plain = splice(&s.sources, &s.evaluated.map, &s.evaluated.design, &[DesignOp::Bench {
        bench: "bringup".into(), op: range("LED3", 0.003, led3_max),
    }]).expect_err("plain splice still refuses a derived bound");
    assert_eq!(selected(&s.sources, &plain), ["(1.0 - duty[i]) * 4.5mA", "0.25", "1.0", "4.5mA"],
        "plain range refusal locates the expression and its inputs");

    let error = s.run("bringup", range("LED1", 0.003, 0.0)).expect_err("zero current coefficient refuses before range ordering");
    assert!(error.message.contains("LED1 min") && error.message.contains("zero coefficient"),
        "LED1 explains that its current-bearing input cannot move the bound: {}", error.message);
    assert_eq!(selected(&s.sources, &error), ["(1.0 - duty[i]) * 4.5mA", "1.0", "1.0", "4.5mA"],
        "LED1 selects its expression, its own duty item, and both written coefficients");
    let rail = range("3V3 up", 3.1, 3.4);
    let changed = s.run("bringup", rail.clone()).expect("literal voltage still verifies");
    assert_eq!(replacement(&s.sources, &changed), ("3.2V", "3.1V"), "literal voltage keeps the plain token edit");
    let plain = splice(&s.sources, &s.evaluated.map, &s.evaluated.design, &[DesignOp::Bench {
        bench: "bringup".into(), op: rail,
    }]).expect("literal voltage also accepts plain splice");
    assert_eq!(plain.edits, changed.edit.edits, "verification preserves the plain voltage splice");
    assert!(changed.also.is_empty(), "the independent rail literal has no collateral changes");

    for (name, min, max, reason, selections) in [
        ("tie", 0.004, 0.009, "tied editable leaves", vec!["left + right", "1mA", "2mA"]),
        ("square", 0.001, 0.009, "non-affine", vec!["square * square", "3mA"]),
        ("reciprocal", 0.001, 0.009, "non-affine", vec!["0.000001 / square", "3mA", "0.000001"]),
        ("shared", 0.003, 0.010, "different values", vec!["4mA"]),
        ("alias0", 0.003, 0.020, "tied editable leaves", vec!["aliases[i] + 1mA", "1mA", "1mA"]),
    ] {
        let error = s.run("affine", range(name, min, max)).expect_err("ambiguous or unsolvable target refuses");
        assert!(error.message.contains(reason), "{name} states why inversion refuses: {}", error.message);
        assert_eq!(selected(&s.sources, &error), selections, "{name} names the precise expression and inputs, or conflicting leaf");
    }
    let changed = s.run("affine", range("shared", 0.003, 0.012)).expect("equal solutions coalesce");
    assert_eq!(replacement(&s.sources, &changed), ("4mA", "6mA"), "compatible min/max targets write their shared leaf once");
    let changed = s.run("affine", range("indexed0", 0.0025, 0.009)).expect("indexed current wins sibling ranking");
    assert_eq!(replacement(&s.sources, &changed), ("2mA", "3mA"), "one indexed sibling beats the loop-body unit literal");
    assert!(changed.also.is_empty(), "the selected list item affects no other iteration");
    let changed = s.run("affine", range("nested00", 0.005, 0.020)).expect("nested sibling counts resolve");
    assert_eq!(replacement(&s.sources, &changed), ("3mA", "4mA"), "both indexed depths beat a leaf shared by the inner loop");
    assert!(changed.also.is_empty(), "the nested item changes only its own expectation");
    let item = bench(&s.evaluated, "affine").expects.iter().find(|e| e.name.starts_with("item")).expect("list-loop expectation");
    let changed = s.run("affine", range(&item.name, 0.010, 0.020)).expect("list binding supplies its own item address");
    assert_eq!(replacement(&s.sources, &changed), ("7mA", "9mA"), "a list binding beats the literal shared by its loop");
    let changed = s.run("affine", drive("PA0", 1000.0, 0.6)).expect("unitless duty uses sibling ranking");
    assert_eq!(replacement(&s.sources, &changed), ("0.2", "0.4"), "the loop item beats the shared subtraction constant");
    assert_eq!(pin(&changed.evaluated, "affine", "PA0"), &Drive::Pwm { freq: 1000.0, duty: 0.6 },
        "the drive inverse includes its affine offset");
    assert!(changed.also.is_empty(), "the inverse changes only the indexed drive");
    let changed = s.run("affine", range("open", 0.003, 0.009)).expect("derived and structural fields coexist");
    assert_eq!(changed.edit.edits.len(), 2, "adding a maximum preserves the separate derived leaf edit");
    let open = bench(&changed.evaluated, "affine").expects.iter().find(|e| e.name == "open").expect("open range");
    assert_eq!(open.range(), Some((0.003, 0.009)), "the structural edit retains the inverse and new maximum");
    let changed = s.run("affine", range("scaled", 0.001, 0.025)).expect("an inverse needing more than six digits verifies");
    assert_eq!(replacement(&s.sources, &changed), ("20mA", "20.83333333mA"),
        "the shortest decimal reaching the bound within 1e-9 is written, not six digits and a refusal");
    let scaled = bench(&changed.evaluated, "affine").expects.iter().find(|e| e.name == "scaled").expect("scaled range");
    let (min, max) = scaled.range().expect("scaled has a range");
    assert!((max - 0.025).abs() <= 1e-9 * 0.025 && min == 0.001,
        "the written limit makes the maximum 25 mA within 1e-9 and keeps the minimum: {scaled:?}");
    assert_eq!(text(&s.sources, "bringup.bench.splash"), FIXTURE, "all dry runs leave the input snapshot intact");
}
