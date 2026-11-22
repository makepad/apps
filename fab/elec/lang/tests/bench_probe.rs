//! Scope edits preserve surrounding source and produce the requested probes after evaluation.

use fab_elec_bench::{bench::ScopeProbe, expect::Probe};
use fab_elec_core::{CompRef, Diag, NetRef, PinIx, PinRef, Sel};
use fab_elec_design::{BenchOp, DesignOp, ProbeOf, Tip};
use fab_elec_lang::{
    NO_CANCEL, BenchKey, BenchOut, BenchProbe, Evaluated, Libs, Sources, UnitState, evaluate, evaluate_from,
    splice, std_libs,
};

const CIRCUIT: &str = include_str!("../../fixtures/ledctl.ckt.splash");
const LITERAL: &str = "    b.probe(c.net(\"LED0\"), {label: \"First\"}) // scope channel\n";
const RAIL: &str = "    b.probe(c.net(\"3V3\"), {label: \"Rail\"})\n";
const BENCH: &str = r#"bench("scope", {}, fn(b, c) {
    b.stub(c.mcu.u, {PA0: b.high()})
    b.probe(c.net("LED0"), {label: "First"}) // scope channel
    b.expect("rail", b.now(b.v(c.net("3V3"))), {min: 3V max: 4V})
})
"#;

fn sources(text: &str) -> Sources {
    let mut sources = Sources::new();
    sources
        .add("ledctl.ckt.splash", CIRCUIT)
        .expect("circuit document");
    sources
        .add("scope.bench.splash", text)
        .expect("bench document");
    sources
}

fn setup(text: &str) -> (Sources, Libs, Evaluated) {
    let sources = sources(text);
    let libs = std_libs();
    let evaluated = evaluate(&sources, &libs).expect("the circuit evaluates");
    assert!(
        evaluated
            .benches
            .iter()
            .all(|b| b.state == UnitState::Built),
        "all initial benches build: {:?}",
        evaluated.benches
    );
    (sources, libs, evaluated)
}

fn text(sources: &Sources) -> &str {
    &sources
        .doc(sources.find("scope.bench.splash").expect("bench name"))
        .expect("bench document")
        .text
}

fn bench<'a>(evaluated: &'a Evaluated, name: &str) -> &'a BenchOut {
    evaluated
        .benches
        .iter()
        .find(|b| b.name == name)
        .expect("named bench")
}

fn probe(net: &str, label: &str) -> BenchProbe {
    BenchProbe {
        of: ProbeOf::v(net),
        label: label.into(),
        tip: Tip::default(),
        at: None,
    }
}

fn op(bench: &str, op: BenchOp) -> DesignOp {
    DesignOp::Bench {
        bench: bench.into(),
        op,
    }
}

fn add(net: &str, label: &str) -> BenchOp {
    BenchOp::AddProbe {
        of: ProbeOf::v(net),
        label: label.into(),
    }
}

fn remove(net: &str) -> BenchOp {
    BenchOp::RemoveProbe { of: ProbeOf::v(net) }
}

fn after(
    sources: &Sources,
    libs: &Libs,
    evaluated: &Evaluated,
    op: DesignOp,
    edits: usize,
) -> (Sources, Evaluated) {
    let change = splice(sources, &evaluated.map, &evaluated.design, &[op])
        .expect("the probe operation splices");
    assert_eq!(
        change.edits.len(),
        edits,
        "the operation writes only its probe, or is a no-op"
    );
    assert!(change.reevaluate, "probe operations request evaluation");
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("probe edits apply");
    let got = evaluate(&edited, libs).expect("the edited sources evaluate");
    assert!(
        got.benches.iter().all(|b| b.state == UnitState::Built),
        "the edited benches build: {:?}",
        got.benches
    );
    assert_eq!(
        edited.docs()[0].text,
        sources.docs()[0].text,
        "probe edits preserve the circuit document"
    );
    // Existing ranges, including the enclosing function, follow the edits before re-evaluation.
    let mut followed = evaluated.map.clone();
    followed.follow(&change);
    for before in &evaluated.benches {
        assert_eq!(
            followed.bench_site(&before.name, &BenchKey::Body),
            got.map.bench_site(&before.name, &BenchKey::Body),
            "the bench body site follows probe edits"
        );
        for p in &before.probes {
            let key = BenchKey::Probe(p.of.clone());
            if let Some(now) = got.map.bench_site(&before.name, &key) {
                assert_eq!(
                    followed.bench_site(&before.name, &key),
                    Some(now),
                    "surviving probe sites follow edits"
                );
            }
        }
    }
    (edited, got)
}

fn selected<'a>(diag: &Diag, sources: &'a Sources) -> &'a str {
    let Some(Sel::Source { doc, start, end }) = diag.sel.first() else {
        panic!("the diagnostic must select source: {diag:?}")
    };
    &sources.doc(*doc).expect("diagnostic document").text[*start as usize..*end as usize]
}

#[test]
fn evaluation_records_names_labels_sites_and_located_errors() {
    let extra = r#"bench("handles", {}, fn(b, c) {
    b.probe(c.mcu.u.VDD, {label: "Supply"})
    b.probe(c.net("GND"))
})
"#;
    let (src, libs, ev) = setup(&format!("{BENCH}{extra}"));
    assert_eq!(
        bench(&ev, "scope").probes,
        [probe("LED0", "First")],
        "a literal probe records its label"
    );
    assert_eq!(
        bench(&ev, "handles").probes,
        [probe("3V3", "Supply"), probe("GND", "GND")],
        "pin handles use their net's lowered name and omitted options default the label"
    );
    let body = ev
        .map
        .bench_site("scope", &BenchKey::Body)
        .expect("body site");
    assert_eq!(
        &text(&src)[body.span.start as usize..body.span.end as usize],
        BENCH
            .strip_prefix("bench(\"scope\", {}, ")
            .unwrap()
            .strip_suffix(")\n")
            .unwrap(),
        "Body selects the entire build function literal"
    );
    let call = ev
        .map
        .bench_site("scope", &BenchKey::Probe(ProbeOf::v("LED0")))
        .expect("probe site");
    assert_eq!(
        &text(&src)[call.span.start as usize..call.span.end as usize],
        "b.probe(c.net(\"LED0\"), {label: \"First\"})",
        "Probe selects the whole call without its comment"
    );

    let duplicate = "b.probe(c.mcu.u.PA0)";
    let broken = BENCH.replace("    b.expect", &format!("    {duplicate}\n    b.probe(c.net(\"GND\"), {{colour: \"red\"}})\n    b.probe(c.net(\"3V3\"), {{label: 1}})\n    b.expect"));
    let bad_sources = sources(&broken);
    let bad = evaluate(&bad_sources, &libs).expect("bench errors preserve the circuit");
    assert!(
        matches!(bench(&bad, "scope").state, UnitState::Failed(_)),
        "invalid probes fail their bench"
    );
    let diags = bench(&bad, "scope").state.diags();
    for (message, selection) in [
        ("LED0: net is already probed", duplicate),
        (
            "unknown option colour (the options are label, load, bw, rise, at)",
            "colour: \"red\"",
        ),
        ("label must be a string", "1"),
    ] {
        let diag = diags
            .iter()
            .find(|d| d.message == format!("scope: {message}"))
            .unwrap_or_else(|| {
                panic!("{message}: a specific probe diagnostic is reported: {diags:?}")
            });
        assert_eq!(
            selected(diag, &bad_sources),
            selection,
            "{message}: the diagnostic selects the offending call or field"
        );
        if selection == duplicate {
            let start = broken.find(duplicate).unwrap() as u32;
            assert!(
                matches!(diag.sel[0], Sel::Source { start: at, .. } if at == start),
                "a duplicate probe selects the second call, including a pin alias of the net"
            );
        }
    }
    let kept =
        evaluate_from(Some(&ev), &bad_sources, &libs, &NO_CANCEL).expect("bad probes keep the previous bench");
    assert!(
        matches!(bench(&kept, "scope").state, UnitState::Kept(_)),
        "probe errors keep a previously built bench"
    );
    assert_eq!(
        bench(&kept, "scope").probes,
        bench(&ev, "scope").probes,
        "kept benches retain scope probes"
    );
    assert_eq!(
        kept.map
            .bench_site("scope", &BenchKey::Probe(ProbeOf::v("LED0"))),
        Some(call),
        "kept benches retain probe sites"
    );
}

#[test]
fn probe_splices_round_trip_and_no_ops_preserve_labels() {
    let (src, libs, ev) = setup(BENCH);
    let (rail_src, rail) = after(&src, &libs, &ev, op("scope", add("3V3", "Rail")), 1);
    assert_eq!(
        text(&rail_src),
        BENCH.replace(LITERAL, &format!("{LITERAL}{RAIL}")),
        "AddProbe inserts exactly one line immediately after the last direct probe"
    );
    assert_eq!(
        bench(&rail, "scope").probes,
        [probe("LED0", "First"), probe("3V3", "Rail")],
        "splice then evaluate adds the scope channel in call order"
    );
    let mut expected_spec = bench(&ev, "scope").spec.clone();
    expected_spec.probes.push(ScopeProbe {
        probe: Probe::V(NetRef {
            board: bench(&ev, "scope").scope_id(),
            net: bench(&ev, "scope").circuit(&ev.design).unwrap().net("3V3").unwrap().id,
        }),
        label: "Rail".into(),
        tip: Tip::default(),
    });
    assert_eq!(
        (&bench(&rail, "scope").spec, &bench(&rail, "scope").expects),
        (&expected_spec, &bench(&ev, "scope").expects),
        "probe edits add the scope channel and preserve stimulus and expectations"
    );

    let (same_src, same) = after(
        &rail_src,
        &libs,
        &rail,
        op("scope", add("3V3", "Ignored")),
        0,
    );
    assert_eq!(
        same_src, rail_src,
        "probing an already probed net preserves every source byte"
    );
    assert_eq!(
        bench(&same, "scope").probes,
        bench(&rail, "scope").probes,
        "an AddProbe no-op preserves the existing label"
    );

    let (restored, restored_ev) = after(&rail_src, &libs, &rail, op("scope", remove("3V3")), 1);
    assert_eq!(
        restored, src,
        "removing the added probe restores the original documents byte for byte"
    );
    assert_eq!(
        bench(&restored_ev, "scope").probes,
        bench(&ev, "scope").probes,
        "removing an added probe restores the original channels"
    );

    let (ground_src, ground) = after(
        &rail_src,
        &libs,
        &rail,
        op("scope", add("GND", "Ground")),
        1,
    );
    assert_eq!(
        text(&ground_src),
        text(&rail_src).replace(
            RAIL,
            &format!("{RAIL}    b.probe(c.net(\"GND\"), {{label: \"Ground\"}})\n")
        ),
        "another probe is appended after the last probe line"
    );
    assert_eq!(
        bench(&ground, "scope").probes,
        [
            probe("LED0", "First"),
            probe("3V3", "Rail"),
            probe("GND", "Ground")
        ],
        "GND is recorded by stable net name"
    );

    let (removed, removed_ev) = after(&src, &libs, &ev, op("scope", remove("LED0")), 1);
    assert_eq!(
        text(&removed),
        BENCH.replace(LITERAL, ""),
        "literal removal owns its indentation, trailing comment and newline only"
    );
    assert!(
        bench(&removed_ev, "scope").probes.is_empty(),
        "literal removal removes the scope channel"
    );
    let (absent, absent_ev) = after(&removed, &libs, &removed_ev, op("scope", remove("GND")), 0);
    assert_eq!(
        absent, removed,
        "removing an unprobed net preserves all source bytes"
    );
    assert!(
        bench(&absent_ev, "scope").probes.is_empty(),
        "an unprobed removal stays empty after evaluation"
    );
}

fn rename(net: &str, label: &str) -> BenchOp {
    BenchOp::SetProbeLabel { of: ProbeOf::v(net), label: label.into() }
}

/// A probe's name is its label in the bench source: a rename rewrites only that literal (quoted, so
/// any text survives), adds the field or the options when the probe has none, is a no-op when the
/// label already reads so, and refuses a label made by code. Each result evaluates from the edited
/// text alone, as a reload does.
#[test]
fn a_probe_rename_writes_only_its_label_and_survives_evaluation() {
    let (src, libs, ev) = setup(BENCH);
    let (renamed_src, renamed) = after(&src, &libs, &ev, op("scope", rename("LED0", "LED \"row\" 0")), 1);
    assert_eq!(
        text(&renamed_src),
        BENCH.replace("{label: \"First\"}", "{label: \"LED \\\"row\\\" 0\"}"),
        "a rename rewrites only the label literal, quoted"
    );
    assert_eq!(bench(&renamed, "scope").probes, [probe("LED0", "LED \"row\" 0")], "the renamed label evaluates back exactly");

    let (same_src, _) = after(&renamed_src, &libs, &renamed, op("scope", rename("LED0", "LED \"row\" 0")), 0);
    assert_eq!(same_src, renamed_src, "renaming to the label it has preserves every byte");

    let bare = BENCH.replace(LITERAL, "    b.probe(c.net(\"LED0\"))\n");
    let (bare_src, bare_libs, bare_ev) = setup(&bare);
    assert_eq!(bench(&bare_ev, "scope").probes[0].label, "LED0", "a probe without a label is named after what it reads");
    let (bare_renamed, bare_renamed_ev) = after(&bare_src, &bare_libs, &bare_ev, op("scope", rename("LED0", "Row")), 1);
    assert_eq!(
        text(&bare_renamed),
        BENCH.replace(LITERAL, "    b.probe(c.net(\"LED0\"), {label: \"Row\"})\n"),
        "a probe without options gains them with its label"
    );
    assert_eq!(bench(&bare_renamed_ev, "scope").probes, [probe("LED0", "Row")], "the added label evaluates");

    let tipped = BENCH.replace(LITERAL, "    b.probe(c.net(\"LED0\"), {bw: 20MHz})\n");
    let (tipped_src, tipped_libs, tipped_ev) = setup(&tipped);
    let (tipped_renamed, tipped_renamed_ev) = after(&tipped_src, &tipped_libs, &tipped_ev, op("scope", rename("LED0", "Row")), 1);
    assert!(text(&tipped_renamed).contains("label: \"Row\""), "options without a label gain the field: {}", text(&tipped_renamed));
    let got = &bench(&tipped_renamed_ev, "scope").probes[0];
    assert_eq!((got.label.as_str(), got.tip.bw), ("Row", Some(20e6)), "the added label keeps the probe's other options");

    let coded = BENCH.replace(LITERAL, "    let name = \"First\"\n    b.probe(c.net(\"LED0\"), {label: name})\n");
    let (coded_src, _, coded_ev) = setup(&coded);
    let refused = splice(&coded_src, &coded_ev.map, &coded_ev.design, &[op("scope", rename("LED0", "Row"))])
        .expect_err("a label made by code is not rewritten");
    assert!(refused.message.contains("made by code"), "the refusal says why: {refused:?}");
    let missing = splice(&src, &ev.map, &ev.design, &[op("scope", rename("3V3", "Rail"))]).expect_err("an unprobed net has no label");
    assert!(missing.message.contains("no probe"), "renaming an absent probe says so: {missing:?}");
}

fn place(net: &str, at: Option<&str>) -> BenchOp {
    BenchOp::SetProbeAt { of: ProbeOf::v(net), at: at.map(str::to_owned) }
}

/// Where a probe's named connection sits is its `at` option in the bench source: placing it adds
/// the field after the others, moving it rewrites only that literal, None removes the field, and a
/// non-string `at` is refused at evaluation. Each result evaluates from the edited text alone, as a
/// reload does.
#[test]
fn a_probe_place_writes_its_at_field_and_survives_evaluation() {
    let (src, libs, ev) = setup(BENCH);
    assert_eq!(bench(&ev, "scope").probes[0].at, None, "a probe without at lets the drawings pick");
    let (placed_src, placed) = after(&src, &libs, &ev, op("scope", place("LED0", Some("R1.2"))), 1);
    assert_eq!(
        text(&placed_src),
        BENCH.replace("{label: \"First\"}", "{label: \"First\" at: \"R1.2\"}"),
        "placing adds the at field after the label"
    );
    assert_eq!(bench(&placed, "scope").probes[0].at.as_deref(), Some("R1.2"), "the place evaluates back");
    let (moved_src, moved) = after(&placed_src, &libs, &placed, op("scope", place("LED0", Some("D1.1"))), 1);
    assert!(text(&moved_src).contains("at: \"D1.1\"") && !text(&moved_src).contains("R1.2"), "a move rewrites the literal");
    assert_eq!(bench(&moved, "scope").probes[0].label, "First", "a move keeps the label");
    let (same_src, _) = after(&moved_src, &libs, &moved, op("scope", place("LED0", Some("D1.1"))), 0);
    assert_eq!(same_src, moved_src, "placing where it is preserves every byte");
    let (cleared_src, cleared) = after(&moved_src, &libs, &moved, op("scope", place("LED0", None)), 1);
    assert_eq!(text(&cleared_src), BENCH, "None removes the field");
    assert_eq!(bench(&cleared, "scope").probes, [probe("LED0", "First")], "and the probe is as it was");

    let bad = BENCH.replace("{label: \"First\"}", "{label: \"First\" at: 2}");
    let refused = evaluate(&sources(&bad), &libs).expect("the circuit evaluates");
    assert_ne!(bench(&refused, "scope").state, UnitState::Built, "a number is no pin: the bench says so");
}

#[test]
fn insertion_uses_function_parameters_first_statement_and_empty_body() {
    let no_probe = BENCH.replace(LITERAL, "");
    let renamed = "bench(\"renamed\", {}, fn(bench, ckt) {\n\tbench.press(ckt.btn.sw)\n})\n";
    let empty = "  bench(\"empty\", {},\n      fn(b, c) {\n  })\n";
    let inline = "bench(\"inline\", {}, fn(b, c) {})\n";
    let short = "bench(\"short\", {}, fn(b) {})\n";
    let original = format!("{no_probe}{renamed}{empty}{inline}{short}");
    let (src, libs, ev) = setup(&original);
    let (first_src, first) = after(&src, &libs, &ev, op("scope", add("3V3", "Rail")), 1);
    assert_eq!(
        text(&first_src),
        original.replacen("    b.stub", &format!("{RAIL}    b.stub"), 1),
        "a bench without probes gets its probe before the first statement with the same indentation"
    );
    assert_eq!(
        bench(&first, "scope").probes,
        [probe("3V3", "Rail")],
        "first-statement insertion evaluates to the new channel"
    );

    let label = "Rail \"sense\" \\ A\nB";
    let (renamed_src, renamed_ev) = after(
        &first_src,
        &libs,
        &first,
        op("renamed", add("3V3", label)),
        1,
    );
    assert_eq!(
        text(&renamed_src),
        text(&first_src).replace(
            "\tbench.press",
            concat!(
                "\tbench.probe(ckt.net(\"3V3\"), {label: \"Rail \\\"sense\\\" \\\\ A\\nB\"})\n",
                "\tbench.press"
            )
        ),
        "insertion uses declared parameters, existing tabs and escaped string literals"
    );
    assert_eq!(
        bench(&renamed_ev, "renamed").probes,
        [probe("3V3", label)],
        "quoted labels survive evaluation unchanged"
    );

    let (empty_src, empty_ev) = after(
        &renamed_src,
        &libs,
        &renamed_ev,
        op("empty", add("3V3", "Rail")),
        1,
    );
    assert_eq!(text(&empty_src), text(&renamed_src).replace(empty,
        "  bench(\"empty\", {},\n      fn(b, c) {\n      b.probe(c.net(\"3V3\"), {label: \"Rail\"})\n  })\n"),
        "empty-body indentation comes from the bench call line plus four spaces");
    assert_eq!(
        bench(&empty_ev, "empty").probes,
        [probe("3V3", "Rail")],
        "an empty body accepts its first probe"
    );
    let (inline_src, inline_ev) = after(
        &empty_src,
        &libs,
        &empty_ev,
        op("inline", add("GND", "Ground")),
        1,
    );
    assert_eq!(text(&inline_src), text(&empty_src).replace(inline,
        "bench(\"inline\", {}, fn(b, c) {\n    b.probe(c.net(\"GND\"), {label: \"Ground\"})\n})\n"),
        "an inline empty body puts the new statement on its own line before the closing brace");
    assert_eq!(
        bench(&inline_ev, "inline").probes,
        [probe("GND", "Ground")],
        "an inline empty body evaluates after insertion"
    );

    let error = splice(
        &src,
        &ev.map,
        &ev.design,
        &[op("short", add("3V3", "Rail"))],
    )
    .expect_err("insertion needs two function parameters");
    assert_eq!(
        selected(&error, &src),
        "fn(b) {}",
        "a missing circuit parameter selects the function literal"
    );
    for operation in [add("3V3", "Rail"), remove("3V3")] {
        let error = splice(&src, &ev.map, &ev.design, &[op("absent", operation)])
            .expect_err("a missing bench refuses both probe operations");
        assert_eq!(
            error.message, "no bench named absent",
            "the missing bench is named in the refusal"
        );
    }
}

#[test]
fn generated_probes_refuse_removal_and_unknown_nets_are_reported_by_evaluation() {
    let generated = r#"bench("looped", {}, fn(b, c) {
    for name in ["3V3" "GND"] {
        b.probe(c.net(name))
    }
})
bench("helper", {}, fn(b, c) {
    let record = fn(tb, ck) { tb.probe(ck.net("GND")) }
    record(b, c)
})
"#;
    let (src, libs, ev) = setup(&format!("{BENCH}{generated}"));
    assert_eq!(
        bench(&ev, "looped").probes,
        [probe("3V3", "3V3"), probe("GND", "GND")],
        "loop probes retain iteration order"
    );
    for (name, net, call) in [
        ("looped", "3V3", "b.probe(c.net(name))"),
        ("looped", "GND", "b.probe(c.net(name))"),
        ("helper", "GND", "tb.probe(ck.net(\"GND\"))"),
    ] {
        let error = splice(&src, &ev.map, &ev.design, &[op(name, remove(net))])
            .expect_err("generated probes cannot be removed by a literal splice");
        assert_eq!(
            error.message,
            format!("{net}: this probe is made by code (a loop or a function); edit it there"),
            "a generated-probe refusal names the net and explains where to edit"
        );
        assert_eq!(
            selected(&error, &src),
            call,
            "a generated-probe refusal selects its actual call"
        );
    }
    let (same_src, same) = after(&src, &libs, &ev, op("looped", add("3V3", "Ignored")), 0);
    assert_eq!(
        same_src, src,
        "adding an existing generated probe is a no-op"
    );
    assert_eq!(
        bench(&same, "looped").probes,
        bench(&ev, "looped").probes,
        "generated probe labels survive an AddProbe no-op"
    );

    let change = splice(
        &src,
        &ev.map,
        &ev.design,
        &[op("scope", add("absent", "Missing"))],
    )
    .expect("the splice defers net lookup to evaluation");
    let mut edited = src.clone();
    edited
        .apply(&change.edits)
        .expect("an unknown net still produces a valid text edit");
    let bad = evaluate(&edited, &libs).expect("an unknown probe net does not fail the circuit");
    let error = bench(&bad, "scope")
        .state
        .diags()
        .iter()
        .find(|d| d.message.contains("no net named \"absent\""))
        .expect("evaluation reports the unknown net");
    assert_eq!(
        selected(error, &edited),
        "c.net(\"absent\")",
        "unknown net lookup selects the newly inserted call"
    );
}

#[test]
fn probe_forms_record_quantities_tips_and_scope_channels() {
    let source = r#"bench("scope", {}, fn(b, c) {
    b.probe(c.net("3V3"))
    b.probe(b.v(c.net("3V3"), c.net("GND")), {label: "Supply difference"})
    b.probe(b.i(c.reg.u.VOUT))
    b.probe(b.i(c.lamps[0].d))
    b.probe(b.p(c.reg.u))
    b.probe(c.net("LED0"), {load: {r: 10Mohm c: 12pF} bw: 200MHz})
    b.probe(b.i(c.mcu.u.PA0), {rise: 1ns})
})
"#;
    let (src, _, ev) = setup(source);
    let out = bench(&ev, "scope");
    let circuit = out.circuit(&ev.design).expect("the scope has its circuit");
    let net = |name| NetRef {
        board: out.scope_id(),
        net: circuit.net(name).expect("fixture net").id,
    };
    let comp = |path| circuit.comp(path).expect("fixture component");
    let pin = |path, name| PinRef {
        board: out.scope_id(),
        comp: comp(path).id,
        pin: comp(path).part.pin(name).expect("fixture pin"),
    };
    let ideal = Tip::default();
    let loaded = Tip {
        r: Some(10e6),
        c: Some(12e-12),
        bw: Some(200e6),
    };
    let rise = Tip {
        bw: Some(Tip::rise_bw(1e-9)),
        ..ideal
    };
    let expected = [
        (ProbeOf::v("3V3"), "3V3", ideal, Probe::V(net("3V3"))),
        (
            ProbeOf::vdiff("3V3", "GND"),
            "Supply difference",
            ideal,
            Probe::VDiff(net("3V3"), net("GND")),
        ),
        (
            ProbeOf::i("reg.u.VOUT"),
            "i(reg.u.VOUT)",
            ideal,
            Probe::I(pin("reg.u", "VOUT")),
        ),
        (
            ProbeOf::i("lamps[0].d.A"),
            "i(lamps[0].d.A)",
            ideal,
            Probe::I(PinRef {
                board: out.scope_id(),
                comp: comp("lamps[0].d").id,
                pin: PinIx(0),
            }),
        ),
        (
            ProbeOf::p("reg.u"),
            "p(reg.u)",
            ideal,
            Probe::P(CompRef {
                board: out.scope_id(),
                comp: comp("reg.u").id,
            }),
        ),
        (ProbeOf::v("LED0"), "LED0", loaded, Probe::V(net("LED0"))),
        (
            ProbeOf::i("mcu.u.PA0"),
            "i(mcu.u.PA0)",
            rise,
            Probe::I(pin("mcu.u", "PA0")),
        ),
    ];
    assert_eq!(
        out.probes.len(),
        expected.len(),
        "every call records one source channel"
    );
    assert_eq!(
        out.spec.probes.len(),
        expected.len(),
        "every call records one simulator channel"
    );
    for (index, (of, label, tip, probe)) in expected.iter().enumerate() {
        assert_eq!(
            &out.probes[index].of, of,
            "channel {index}: stable quantity and endpoints"
        );
        assert_eq!(
            out.probes[index].label.as_str(),
            *label,
            "channel {index}: explicit or Display label"
        );
        assert_eq!(
            &out.probes[index].tip, tip,
            "channel {index}: load and bandwidth in SI units"
        );
        assert_eq!(
            &out.spec.probes[index].probe, probe,
            "channel {index}: ordered circuit references"
        );
        assert_eq!(
            out.spec.probes[index].label.as_str(),
            *label,
            "channel {index}: simulator label"
        );
        assert_eq!(
            &out.spec.probes[index].tip, tip,
            "channel {index}: simulator tip"
        );
        let site = ev
            .map
            .bench_site("scope", &BenchKey::Probe(of.clone()))
            .unwrap_or_else(|| panic!("channel {index}: the stable identity has a source site"));
        let call = &text(&src)[site.span.start as usize..site.span.end as usize];
        assert_eq!(
            call,
            source.lines().nth(index + 1).unwrap().trim(),
            "channel {index}: the site selects its own probe call"
        );
    }
    let bw = out.probes[6].tip.bw.expect("rise records a bandwidth");
    assert!(
        (bw - 349.699e6).abs() <= 1e3,
        "1 ns rise gives ln(9) / (2 pi 1 ns), about 349.7 MHz"
    );
    assert_eq!(
        ProbeOf::vdiff("3V3", "GND").to_string(),
        "v(3V3, GND)",
        "differential default labels name the ordered nets"
    );
}

#[test]
fn probe_option_and_target_errors_are_located() {
    let cases = [
        (
            "b.probe(b.i(c.reg.u.VOUT), {load: {r: 10Mohm}})",
            "a current or power probe has no tip load (load is for voltage probes)",
            "load: {r: 10Mohm}",
        ),
        (
            "b.probe(b.p(c.reg.u), {load: {c: 12pF}})",
            "a current or power probe has no tip load (load is for voltage probes)",
            "load: {c: 12pF}",
        ),
        (
            "b.probe(c.net(\"3V3\"), {bw: 200MHz rise: 1ns})",
            "give bw or rise, not both",
            "rise: 1ns",
        ),
        ("b.probe(c.net(\"3V3\"), {bw: 0Hz})", "bw must be finite and positive", "0Hz"),
        (
            "b.probe(c.net(\"3V3\"), {load: {r: -1kohm}})",
            "load r must be finite and positive",
            "-1kohm",
        ),
        ("b.probe(c.net(\"3V3\"), {load: {}})", "a tip load needs r, c or both", "{}"),
        (
            "b.probe(b.now(b.v(c.net(\"3V3\"))))",
            "b.probe takes a net or b.v(..), b.i(..), b.p(..), not a measure (a measure goes in b.expect)",
            "b.now(b.v(c.net(\"3V3\")))",
        ),
        (
            "b.probe(b.high())",
            "b.probe takes a net or b.v(..), b.i(..), b.p(..), not a drive",
            "b.high()",
        ),
        (
            "b.probe(b.v(c.net(\"3V3\"), c.mcu.u.VDD))",
            "a differential probe needs two different nets",
            "b.probe(b.v(c.net(\"3V3\"), c.mcu.u.VDD))",
        ),
        (
            "b.probe(b.v(c.net(\"3V3\"), c.net(\"GND\")))\n    b.probe(b.v(c.net(\"3V3\"), c.net(\"GND\")), {label: \"duplicate\"})",
            "v(3V3, GND): already probed",
            "b.probe(b.v(c.net(\"3V3\"), c.net(\"GND\")), {label: \"duplicate\"})",
        ),
        (
            "b.probe(c.net(\"3V3\"), {ref: \"GND\"})",
            "unknown option ref (the options are label, load, bw, rise, at)",
            "ref: \"GND\"",
        ),
    ];
    let source = cases
        .iter()
        .enumerate()
        .map(|(index, (body, _, _))| {
            format!("bench(\"bad{index}\", {{}}, fn(b, c) {{\n    {body}\n}})\n")
        })
        .collect::<String>();
    let src = sources(&source);
    let ev = evaluate(&src, &std_libs()).expect("invalid probes preserve the circuit");
    for (index, (_, message, selection)) in cases.iter().enumerate() {
        let name = format!("bad{index}");
        let out = bench(&ev, &name);
        assert!(
            matches!(out.state, UnitState::Failed(_)),
            "{message}: the invalid bench fails"
        );
        let diags = out.state.diags();
        let diag = diags
            .iter()
            .find(|d| d.message == format!("{name}: {message}"))
            .unwrap_or_else(|| panic!("{message}: the specific diagnostic is reported: {diags:?}"));
        assert_eq!(
            selected(diag, &src),
            *selection,
            "{message}: the diagnostic selects the offending syntax"
        );
    }
}

#[test]
fn probe_splices_write_every_quantity_and_edit_reference_and_tip() {
    let (original, libs, initial) = setup(BENCH);
    let (mut src, mut ev) = (original.clone(), initial.clone());
    let mut previous = LITERAL.to_string();
    for (of, label, line) in [
        (
            ProbeOf::vdiff("LED0", "3V3"),
            "Difference",
            "    b.probe(b.v(c.net(\"LED0\"), c.net(\"3V3\")), {label: \"Difference\"})\n",
        ),
        (
            ProbeOf::i("mcu.u.PA0"),
            "Current",
            "    b.probe(b.i(c.mcu.u.PA0), {label: \"Current\"})\n",
        ),
        (
            ProbeOf::p("reg.u"),
            "Power",
            "    b.probe(b.p(c.reg.u), {label: \"Power\"})\n",
        ),
    ] {
        let operation = BenchOp::AddProbe {
            of: of.clone(),
            label: label.into(),
        };
        let (next_src, next) = after(&src, &libs, &ev, op("scope", operation), 1);
        assert_eq!(
            text(&next_src),
            text(&src).replace(&previous, &format!("{previous}{line}")),
            "{of}: insertion writes exactly the requested quantity after the last probe"
        );
        let mut expected = bench(&ev, "scope").probes.clone();
        expected.push(BenchProbe {
            of: of.clone(),
            label: label.into(),
            tip: Tip::default(),
            at: None,
        });
        assert_eq!(
            bench(&next, "scope").probes,
            expected,
            "{of}: insertion preserves channels and records its label"
        );
        (src, ev) = (next_src, next);
        previous = line.into();
    }
    let current = ProbeOf::i("mcu.u.PA0");
    let error = splice(
        &src,
        &ev.map,
        &ev.design,
        &[op(
            "scope",
            BenchOp::SetProbeRef {
                of: current.clone(),
                reference: Some("GND".into()),
            },
        )],
    )
    .expect_err("a current probe cannot gain a voltage reference");
    assert_eq!(
        error.message, "i(mcu.u.PA0): only a voltage probe has a reference",
        "a current reference refusal explains the quantity restriction"
    );
    assert!(error.sel.is_empty(), "a quantity refusal is unlocated");
    let differential = ProbeOf::vdiff("LED0", "3V3");
    let (removed_src, removed) = after(
        &src,
        &libs,
        &ev,
        op(
            "scope",
            BenchOp::RemoveProbe {
                of: differential.clone(),
            },
        ),
        1,
    );
    assert_eq!(
        text(&removed_src),
        text(&src).replace(
            "    b.probe(b.v(c.net(\"LED0\"), c.net(\"3V3\")), {label: \"Difference\"})\n",
            ""
        ),
        "differential removal owns only its statement"
    );
    assert_eq!(bench(&removed, "scope").probes,
        bench(&ev, "scope").probes.iter().filter(|p| p.of != differential).cloned().collect::<Vec<_>>(),
        "differential removal preserves the ground probe on the same target and the other quantities");

    let ground = ProbeOf::v("LED0");
    let (referenced_src, referenced) = after(
        &original,
        &libs,
        &initial,
        op(
            "scope",
            BenchOp::SetProbeRef {
                of: ground.clone(),
                reference: Some("3V3".into()),
            },
        ),
        1,
    );
    assert_eq!(
        text(&referenced_src),
        BENCH.replace(
            "b.probe(c.net(\"LED0\")",
            "b.probe(b.v(c.net(\"LED0\"), c.net(\"3V3\"))"
        ),
        "adding a reference wraps only the original argument bytes"
    );
    assert_eq!(
        bench(&referenced, "scope").probes[0].of,
        differential,
        "reference insertion changes the stable identity"
    );
    let (restored, _) = after(
        &referenced_src,
        &libs,
        &referenced,
        op(
            "scope",
            BenchOp::SetProbeRef {
                of: differential.clone(),
                reference: None,
            },
        ),
        1,
    );
    assert_eq!(
        restored, original,
        "None to Some to None restores every original byte"
    );
    let (changed_src, changed) = after(
        &referenced_src,
        &libs,
        &referenced,
        op(
            "scope",
            BenchOp::SetProbeRef {
                of: differential,
                reference: Some("GND".into()),
            },
        ),
        1,
    );
    assert_eq!(
        text(&changed_src),
        text(&referenced_src).replace(
            "c.net(\"3V3\")), {label: \"First\"}",
            "c.net(\"GND\")), {label: \"First\"}"
        ),
        "changing a reference replaces only the second net"
    );
    assert_eq!(
        bench(&changed, "scope").probes[0].of,
        ProbeOf::vdiff("LED0", "GND"),
        "reference replacement records the new differential identity"
    );

    let tip = Tip {
        r: Some(10e6),
        c: Some(12e-12),
        bw: Some(200e6),
    };
    let (loaded_src, loaded) = after(
        &original,
        &libs,
        &initial,
        op(
            "scope",
            BenchOp::SetProbeTip {
                of: ground.clone(),
                tip,
            },
        ),
        1,
    );
    assert_eq!(
        bench(&loaded, "scope").probes,
        [BenchProbe {
            of: ground.clone(),
            label: "First".into(),
            tip,
            at: None,
        }],
        "tip editing preserves identity and label and records R, C and bandwidth"
    );
    assert_eq!(
        bench(&loaded, "scope").spec.probes[0].tip,
        tip,
        "tip editing reaches the simulator channel"
    );
    assert!(
        text(&loaded_src).contains("label: \"First\""),
        "tip editing preserves the label source"
    );
    assert!(
        text(&loaded_src).contains("// scope channel\n"),
        "tip editing preserves the statement comment"
    );
    let (ideal_src, ideal) = after(
        &loaded_src,
        &libs,
        &loaded,
        op(
            "scope",
            BenchOp::SetProbeTip {
                of: ground,
                tip: Tip::default(),
            },
        ),
        1,
    );
    assert_eq!(
        bench(&ideal, "scope").probes,
        [probe("LED0", "First")],
        "clearing a tip restores an ideal probe"
    );
    assert!(
        !text(&ideal_src).contains("load:") && !text(&ideal_src).contains("bw:"),
        "an ideal tip removes both load and bandwidth fields"
    );

    let no_opts = BENCH.replace(", {label: \"First\"}", "");
    let (bare_src, _, bare) = setup(&no_opts);
    let (bare_same, _) = after(
        &bare_src,
        &libs,
        &bare,
        op(
            "scope",
            BenchOp::SetProbeTip {
                of: ProbeOf::v("LED0"),
                tip: Tip::default(),
            },
        ),
        0,
    );
    assert_eq!(
        bare_same, bare_src,
        "an ideal tip on an argument without options writes nothing"
    );
    let (_, bare_loaded) = after(
        &bare_src,
        &libs,
        &bare,
        op(
            "scope",
            BenchOp::SetProbeTip {
                of: ProbeOf::v("LED0"),
                tip,
            },
        ),
        1,
    );
    assert_eq!(
        bench(&bare_loaded, "scope").probes[0].tip,
        tip,
        "a tip inserts an absent options argument"
    );

    // A rise time stays a rise time: read in as a bandwidth, set from the UI, written back.
    let (rise_src, _, rise) = setup(&BENCH.replace("{label: \"First\"}", "{label: \"First\" rise: 1ns}"));
    let read = bench(&rise, "scope").probes[0].tip;
    assert_eq!(read.bw, Some(Tip::rise_bw(1e-9)), "rise: 1ns reads as the bandwidth ln(9) / (2 pi 1 ns)");
    assert_eq!(read.rise(), Some(1e-9), "that bandwidth converts back to exactly 1 ns");
    let from_ui = Tip { r: Some(10e6), ..read };
    let set = |tip| op("scope", BenchOp::SetProbeTip { of: ProbeOf::v("LED0"), tip });
    let (kept_src, kept) = after(&rise_src, &libs, &rise, set(from_ui), 1);
    assert!(
        text(&kept_src).contains("rise: 1ns") && !text(&kept_src).contains("bw:"),
        "a load set from the UI keeps the written rise: 1ns: {}",
        text(&kept_src)
    );
    assert!(text(&kept_src).contains("load: {r: 10Mohm}"), "the load is written: {}", text(&kept_src));
    assert_eq!(bench(&kept, "scope").probes[0].tip, from_ui, "the written tip reads back as set");
    let slower = Tip { bw: Some(Tip::rise_bw(2.5e-9)), ..from_ui };
    let (slower_src, slower_ev) = after(&kept_src, &libs, &kept, set(slower), 1);
    assert_eq!(
        text(&slower_src),
        text(&kept_src).replace("rise: 1ns", "rise: 2.5ns"),
        "a new bandwidth on a rise-written probe is written as a rise time, in place and in its suffix"
    );
    assert_eq!(bench(&slower_ev, "scope").probes[0].tip, slower, "the new rise reads back as the set bandwidth");
}
