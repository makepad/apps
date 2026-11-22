//! Cards see the same editable sites as the plain and verified splice entry points.

use fab_elec_core::drive::{Drive, Pull};
use fab_elec_design::{BenchOp, DesignOp, ProbeOf, TextEdit};
use fab_elec_lang::{
    NO_CANCEL, BenchEdit, BenchKey, BenchOut, Evaluated, Libs, Sources, UnitState,
    bench_views, evaluate, evaluate_from, splice, splice_verified, std_libs,
};

const CIRCUIT: &str = include_str!("../../fixtures/ledctl.ckt.splash");
const FIXTURE: &str = include_str!("../../fixtures/ledctl.bench.splash");
const EXTRA: &str = r#"bench("literal", {}, fn(b, c) {
    let lo = 1V
    b.stub(c.mcu.u, {PA0: b.pwm(1kHz, 0.3) PA1: b.high() PB7: b.input(@up)})
    b.expect("rail", b.now(b.v(c.net("3V3"))), {min: 3V max: 4V})
    b.expect("computed", b.now(b.v(c.net("3V3"))), {min: lo * 2 max: 4V})
    b.press(c.btn.sw)
    b.probe(c.net("3V3"), {label: "Rail"})
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
        sources.add("ledctl.ckt.splash", CIRCUIT).expect("circuit source");
        sources.add("bringup.bench.splash", FIXTURE).expect("fixture source");
        sources.add("literal.bench.splash", EXTRA).expect("extra bench source");
        let libs = std_libs();
        let evaluated = evaluate(&sources, &libs).expect("setup evaluates");
        assert!(
            evaluated.benches.iter().all(|out| out.state == UnitState::Built),
            "every initial bench builds: {:?}", evaluated.benches
        );
        Self { sources, libs, evaluated }
    }
}

fn operations(out: &BenchOut) -> Vec<(BenchKey, BenchOp)> {
    let mut ops = Vec::new();
    for stub in &out.spec.stubs {
        for (pin, drive) in &stub.pins {
            let drive = match drive {
                Drive::Pwm { freq, duty } => Drive::Pwm {
                    freq: *freq,
                    duty: if *duty == 0.4 { 0.6 } else { 0.4 },
                },
                Drive::Level(high) => Drive::Level(!high),
                Drive::Input(pull) => Drive::Input(if *pull == Pull::Up { Pull::Down } else { Pull::Up }),
            };
            ops.push((
                BenchKey::Stub(stub.path.clone(), pin.clone()),
                BenchOp::SetDrive { stub: stub.path.clone(), pin: pin.clone(), drive },
            ));
        }
    }
    for expect in &out.expects {
        // Halved and doubled bounds keep every derived LED solution a short, plausible token.
        let (min, max) = expect.range().expect("bench expectations have ranges");
        ops.push((
            BenchKey::Expect(expect.name.clone()),
            BenchOp::SetRange {
                expect: expect.name.clone(),
                min: if min == 0.0 { -0.001 } else { min / 2.0 },
                max: if max == 0.0 { 0.001 } else { max * 2.0 },
            },
        ));
    }
    for comp in &out.spec.pressed {
        ops.push((BenchKey::Press(comp.clone()), BenchOp::SetPressed { comp: comp.clone(), pressed: false }));
    }
    for probe in &out.probes {
        ops.push((BenchKey::Probe(probe.of.clone()), BenchOp::RemoveProbe { of: probe.of.clone() }));
    }
    ops
}

#[test]
fn evaluated_views_agree_with_splices_and_keep_bench_order() {
    let s = Setup::new();
    let views = bench_views(&s.evaluated, &s.sources);
    assert_eq!(views.len(), s.evaluated.benches.len(), "there is one view per evaluated bench");
    for (view, out) in views.iter().zip(&s.evaluated.benches) {
        assert_eq!(&view.out, out, "{} preserves its evaluated output and bench order", out.name);
        assert_eq!(view.insert, BenchEdit::Literal, "{} has a writable built bench body", out.name);
        let operations = operations(out);
        assert_eq!(
            view.edits.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            operations.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            "{} has exactly one edit entry per pin, expectation, press and probe, in that order", out.name
        );
        for ((key, editable), (_, op)) in view.edits.iter().zip(operations) {
            assert_eq!(
                view.edits.iter().filter(|(other, _)| other == key).count(), 1,
                "{} {key:?} has no duplicate edit entry", out.name
            );
            let ops = [DesignOp::Bench { bench: out.name.clone(), op }];
            let plain = splice(&s.sources, &s.evaluated.map, &s.evaluated.design, &ops);
            assert_eq!(
                plain.is_ok(), matches!(editable, BenchEdit::Literal),
                "{} {key:?}: Literal agrees with plain splice: {plain:?}", out.name
            );
            match editable {
                BenchEdit::Literal => {
                    assert!(
                        !plain.expect("literal splice succeeds").edits.is_empty(),
                        "{} {key:?}: a changed literal produces edits", out.name
                    );
                }
                BenchEdit::Derived { leaves } => {
                    let verified = splice_verified(&s.sources, &s.libs, &s.evaluated, &ops, &NO_CANCEL)
                        .unwrap_or_else(|error| panic!("{} {key:?}: derived value must verify: {error:?}", out.name));
                    assert!(!verified.edit.edits.is_empty(), "{} {key:?}: changed derived value produces edits", out.name);
                    assert!(
                        verified.edit.edits.iter().all(|edit| {
                            leaves.iter().any(|leaf| leaf.doc == edit.doc && leaf.span == edit.span)
                        }),
                        "{} {key:?}: every verified edit is one of the advertised leaves", out.name
                    );
                }
                BenchEdit::Refused(diag) => {
                    let error = plain.expect_err("refused view must refuse plain splice");
                    assert_eq!(diag.message, error.message, "{} {key:?}: view and plain refusal agree", out.name);
                    assert!(!diag.sel.is_empty(), "{} {key:?}: refusal has a source location", out.name);
                    assert!(
                        splice_verified(&s.sources, &s.libs, &s.evaluated, &ops, &NO_CANCEL).is_err(),
                        "{} {key:?}: refused view also refuses verified splice", out.name
                    );
                }
            }
        }
        let add = [DesignOp::Bench {
            bench: out.name.clone(),
            op: BenchOp::AddProbe { of: ProbeOf::v("GND"), label: "Ground".into() },
        }];
        let inserted = splice(&s.sources, &s.evaluated.map, &s.evaluated.design, &add)
            .expect("a literal insert accepts a new probe");
        assert!(!inserted.edits.is_empty(), "{}: Literal insert writes a new statement", out.name);
    }

    let bringup = views.iter().find(|view| view.out.name == "bringup").expect("bringup view");
    for index in 0..8 {
        let expect = BenchKey::Expect(format!("LED{index}"));
        let edit = &bringup.edits.iter().find(|(other, _)| other == &expect).expect("LED bounds entry").1;
        if index == 1 {
            let BenchEdit::Refused(diag) = edit else { panic!("LED1 has zero coefficients: {edit:?}") };
            assert!(diag.message.contains("zero coefficient"), "LED1 explains why no bound can move");
        } else {
            let BenchEdit::Derived { leaves } = edit else { panic!("LED{index} resolves its bounds: {edit:?}") };
            let written: Vec<_> = leaves.iter().map(|at| {
                &s.sources.doc(at.doc).expect("bound leaf document").text[at.span.start as usize..at.span.end as usize]
            }).collect();
            assert_eq!(written, ["4.5mA", "5.5mA"], "LED{index}: both bounds advertise only their unit-bearing leaf");
        }
        let key = BenchKey::Stub("mcu.u".into(), format!("PA{index}"));
        let edit = &bringup.edits.iter().find(|(other, _)| other == &key).expect("bringup pin entry").1;
        if index < 2 {
            let BenchEdit::Derived { leaves } = edit else {
                panic!("PA{index}: an if arm's guard is a written list item: {edit:?}");
            };
            let written: Vec<_> = leaves.iter().map(|at| {
                &s.sources.doc(at.doc).expect("leaf document").text[at.span.start as usize..at.span.end as usize]
            }).collect();
            assert_eq!(
                written, [["0.0", "1.0"][index], "1kHz"],
                "PA{index}: an if arm's guard is a written list item"
            );
        } else {
            let BenchEdit::Derived { leaves } = edit else {
                panic!("PA{index}: loop PWM arguments must resolve to written leaves: {edit:?}");
            };
            assert_eq!(leaves.len(), 2, "PA{index}: frequency and indexed duty each have a leaf");
            let written: Vec<_> = leaves.iter().map(|at| {
                &s.sources.doc(at.doc).expect("leaf document").text[at.span.start as usize..at.span.end as usize]
            }).collect();
            assert_eq!(written, ["1kHz", ["0.1", "0.25", "0.3", "0.5", "0.75", "0.9"][index - 2]],
                "PA{index}: leaves select 1kHz and this iteration's duty list item");
        }
    }
    let literal = views.iter().find(|view| view.out.name == "literal").expect("literal view");
    for (key, edit) in &literal.edits {
        assert!(
            if key == &BenchKey::Expect("computed".into()) {
                matches!(edit, BenchEdit::Derived { .. })
            } else {
                matches!(edit, BenchEdit::Literal)
            },
            "literal {key:?}: computed bounds now resolve through their unit-bearing leaf"
        );
    }

    let mut broken = s.sources.clone();
    let doc = broken.find("literal.bench.splash").expect("extra document");
    let start = EXTRA.find("lo * 2").expect("computed bound") as u32;
    broken.apply(&[TextEdit { doc, span: start..start + 6, text: "missing_value".into() }])
        .expect("break one bench in the source copy");
    let failed = evaluate_from(Some(&s.evaluated), &broken, &s.libs, &NO_CANCEL).expect("broken bench has unit diagnostics");
    let failed_views = bench_views(&failed, &broken);
    assert_eq!(failed_views.len(), failed.benches.len(), "failed benches retain a view in evaluation order");
    for (view, out) in failed_views.iter().zip(&failed.benches) {
        assert_eq!(&view.out, out, "{}: even a failed view retains the full evaluated output", out.name);
        if out.name == "literal" {
            assert_ne!(out.state, UnitState::Built, "the changed bench must fail evaluation");
            assert!(view.edits.is_empty(), "failed benches expose no retained edit sites");
            assert_eq!(
                view.insert, BenchEdit::Refused(out.state.diags()[0].clone()),
                "failed insertion carries the first evaluation diagnostic exactly"
            );
        } else {
            assert_eq!(out.state, UnitState::Built, "breaking one document leaves the other bench built");
        }
    }
    assert_eq!(s.sources.doc(doc).expect("original extra document").text.as_ref(), EXTRA,
        "classifying and verifying entries leave the original source snapshot unchanged");
}
