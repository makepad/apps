//! Design benches use the evaluated circuit's ids and retain their last good data and sites.

use fab_elec_bench::{
    expect::{Bound, Expect, Measure, Probe, When},
    fixture::ledctl_bench,
};
use fab_elec_core::{BoardId, CompRef, Diag, NetRef, PinIx, PinRef, Sel};
use fab_elec_lang::{
    NO_CANCEL, BenchKey, BenchPanel, BenchScope, Evaluated, PanelEntry, PanelGroup, Sources, UnitState, evaluate,
    evaluate_from, std_libs,
};

const CIRCUIT: &str = include_str!("../../fixtures/ledctl.ckt.splash");
const BENCH: &str = include_str!("../../fixtures/ledctl.bench.splash");
const DUTY: [f64; 8] = [0.0, 1.0, 0.1, 0.25, 0.3, 0.5, 0.75, 0.9];

fn sources(bench: &str) -> Sources {
    let mut s = Sources::new();
    s.add("ledctl.ckt.splash", CIRCUIT).expect("circuit name");
    s.add("ledctl.bench.splash", bench).expect("bench name");
    s
}

fn at<'a>(ev: &Evaluated, source: &'a str, name: &str, key: BenchKey) -> &'a str {
    let site = ev
        .map
        .bench_site(name, &key)
        .unwrap_or_else(|| panic!("{name} {key:?}: a source site"));
    &source[site.span.start as usize..site.span.end as usize]
}

fn diag_text<'a>(d: &Diag, s: &'a Sources) -> &'a str {
    match d.sel.first() {
        Some(Sel::Source { doc, start, end }) => {
            let text = &s.doc(*doc).expect("diagnostic document").text;
            &text[*start as usize..*end as usize]
        }
        _ => panic!("a diagnostic must be located: {d:?}"),
    }
}

#[test]
fn fixture_matches_stimulus_and_checks_and_keeps_them_after_an_error() {
    let libs = std_libs();
    let src = sources(BENCH);
    let ev = evaluate(&src, &libs).expect("fixture circuit evaluates");
    assert_eq!(ev.benches.len(), 1, "one bench call produces one bench");
    let bench = &ev.benches[0];
    assert_eq!(bench.state, UnitState::Built, "the fixture bench builds");
    assert!(bench.probes.is_empty(), "scope probes are opt-in");
    assert!(bench.spec.probes.is_empty(), "simulator scope channels are opt-in");
    assert_eq!(bench.name, "bringup", "bench name");
    assert_eq!(
        bench.doc,
        src.find("ledctl.bench.splash").unwrap(),
        "bench document"
    );
    assert_eq!(bench.scope, BenchScope::Design(BoardId(0)), "bench unit");
    assert_eq!(
        bench.spec,
        ledctl_bench(1000.0, DUTY),
        "ordered drives equal the typed fixture, including static endpoints and pulls"
    );
    assert_eq!(bench.stop, Some(0.02), "run stop is in seconds");
    let circuit = &ev.design.units[0].circuit;
    let mut expects = Vec::new();
    for (i, duty) in DUTY.iter().enumerate() {
        let comp = circuit
            .comps
            .iter()
            .find(|c| c.path == format!("lamps[{i}].d"))
            .expect("LED path");
        expects.push(Expect {
            name: format!("LED{i}"),
            probe: Probe::I(PinRef {
                board: bench.scope_id(),
                comp: comp.id,
                pin: PinIx(0),
            }),
            measure: Measure::Mean,
            when: When::Trailing {
                window: 0.001,
                after: 0.005,
            },
            bound: Bound::Range {
                min: (1.0 - duty) * 0.0045,
                max: (1.0 - duty) * 0.0055,
            },
        });
    }
    let rail = circuit
        .nets
        .iter()
        .find(|n| n.name == "3V3")
        .expect("3V3 net")
        .id;
    expects.push(Expect {
        name: "3V3 up".into(),
        probe: Probe::V(NetRef {
            board: bench.scope_id(),
            net: rail,
        }),
        measure: Measure::Mean,
        when: When::Trailing {
            window: 0.001,
            after: 0.005,
        },
        bound: Bound::Range { min: 3.2, max: 3.4 },
    });
    assert_eq!(
        bench.expects, expects,
        "checks resolve paths and net names to existing circuit ids in loop order"
    );
    assert!(
        at(&ev, BENCH, "bringup", BenchKey::Expect("LED3".into())).starts_with("b.expect("),
        "a generated expectation maps to its written call"
    );
    assert!(
        at(
            &ev,
            BENCH,
            "bringup",
            BenchKey::Stub("mcu.u".into(), "PB7".into())
        )
        .starts_with("PB7:"),
        "a copied stub pin maps to its written key and drive"
    );

    let broken = BENCH.replace("PB7: b.input", "ABSENT: b.input");
    let src = sources(&broken);
    let kept =
        evaluate_from(Some(&ev), &src, &libs, &NO_CANCEL).expect("a bench failure does not fail a built unit");
    let now = &kept.benches[0];
    assert!(
        matches!(now.state, UnitState::Kept(_)),
        "a broken bench keeps its last good evaluation"
    );
    assert_eq!(
        (&now.spec, &now.expects, &now.probes, now.stop),
        (&bench.spec, &bench.expects, &bench.probes, bench.stop),
        "all previous bench data is retained"
    );
    for key in [
        BenchKey::Body,
        BenchKey::Expect("LED3".into()),
        BenchKey::Stub("mcu.u".into(), "PB7".into()),
    ] {
        assert_eq!(
            kept.map.bench_site("bringup", &key),
            ev.map.bench_site("bringup", &key),
            "kept bench retains site {key:?}"
        );
    }
    assert_eq!(
        kept.units[0].state,
        UnitState::Built,
        "bench errors do not change the unit's state"
    );

    let syntax_error = sources("bench(\"bringup\", {");
    let kept = evaluate_from(Some(&ev), &syntax_error, &libs, &NO_CANCEL)
        .expect("a syntax error keeps the previous bench");
    assert!(
        matches!(kept.benches[0].state, UnitState::Kept(_)),
        "unreached calls survive a parse failure"
    );
    assert_eq!(
        kept.benches[0].spec, bench.spec,
        "parse failure retains the last stimulus"
    );
    assert!(
        kept.benches[0]
            .state
            .diags()
            .iter()
            .all(|d| d.message.starts_with("bringup:")),
        "parse errors carry the kept bench name"
    );
}

#[test]
fn panel_option_arranges_named_controls_and_locates_bad_entries() {
    // One small box with stored refs, evaluated once: a document of good panels and one of bad
    // ones (any fault fails every bench of its document, so each bad bench carries one fault).
    let circuit = r#"let e = mod.elec
let key = e.part(mod.std.tact_switch(), {ref: "SW1"})
let knob = e.part(mod.std.pot_9mm(), {value: 10kohm ref: "RV1"})
let load = e.r(1kohm, nil, nil, {ref: "R1"})
"#;
    let good = r#"bench("plain", {}, fn(b, c) {})
bench("arranged", {panel: [{group: "Play" cols: 1 controls: [{part: "RV1" label: "Pitch"} "SW1"]}]}, fn(b, c) {})
"#;
    let bad = r#"bench("resistor", {panel: [{group: "G" controls: ["R1"]}]}, fn(b, c) {})
bench("twice", {panel: [{group: "A" controls: ["SW1"]} {group: "B" controls: ["SW1"]}]}, fn(b, c) {})
bench("unknown", {panel: [{group: "G" controls: ["SW9"]}]}, fn(b, c) {})
"#;
    let mut src = Sources::new();
    src.add("box.ckt.splash", circuit).expect("circuit name");
    src.add("good.bench.splash", good).expect("good bench name");
    src.add("bad.bench.splash", bad).expect("bad bench name");
    let ev = evaluate(&src, &std_libs()).expect("the box evaluates");
    let comps = &ev.design.units[0].circuit.comps;
    let id = |refdes: &str| comps.iter().find(|c| c.refdes == refdes).unwrap_or_else(|| panic!("{refdes} exists")).id;
    let bench = |name: &str| ev.benches.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("bench {name} has a result"));

    let plain = bench("plain");
    assert!(plain.state == UnitState::Built && plain.panel.is_none(), "without the option a bench builds and keeps the generated panel");
    let arranged = bench("arranged");
    assert_eq!(arranged.state, UnitState::Built, "a good panel builds: {:?}", arranged.state);
    assert_eq!(
        arranged.panel,
        Some(BenchPanel { groups: vec![PanelGroup {
            name: "Play".into(),
            cols: Some(1),
            controls: vec![
                PanelEntry { comp: id("RV1"), label: Some("Pitch".into()) },
                PanelEntry { comp: id("SW1"), label: None },
            ],
        }] }),
        "the panel keeps its groups, order, columns and labels, by component id"
    );

    for (name, message) in [
        ("resistor", "resistor: panel: group \"G\": R1 is not a switch or a potentiometer"),
        ("twice", "twice: panel: group \"B\": SW1 is already in the panel"),
        ("unknown", "unknown: panel: group \"G\": no part SW9"),
    ] {
        let b = bench(name);
        assert!(matches!(b.state, UnitState::Failed(_)), "{name}: a bad panel fails the bench");
        let diag = b.state.diags().iter().find(|d| d.message == message)
            .unwrap_or_else(|| panic!("{name}: reports {message:?}: {:?}", b.state.diags()));
        assert!(diag_text(diag, &src).starts_with("[{group:"), "{name}: the diagnostic selects the panel option");
    }
}

#[test]
fn mistakes_are_located_and_name_the_bench() {
    // One circuit/VM setup for all independent native mistakes; the whole bench document fails.
    let text = r#"
bench("bringup", {}, fn(b, c) {
    b.stub(c.mcu.u, {ABSENT: b.high()})
    b.stub(c.mcu.u, {PA0: b.high()})
    b.stub(c.mcu.u, {PA1: b.low()})
    b.press(c.mcu.u)
    b.turn(c.mcu.u, 1)
    b.expect("twice", b.now(b.i(c.lamps[0].d)), {})
    b.expect("twice", b.now(b.i(c.lamps[0].d)), {})
    b.expect("units", b.now(b.i(c.lamps[0].d)), {min: 1V})
    b.expect("span", b.mean(b.i(c.lamps[0].d), {from: 2ms to: 1ms}), {})
    b.stub(c.lamps[9], {})
    b.expect("mixed", b.mean(b.v(c.net("3V3")), {window: 1ms to: 2ms}), {})
    b.expect("field", b.mean(b.v(c.net("3V3")), {wind: 1ms}), {})
    b.expect("range", b.now(b.v(c.net("3V3"))), {min: 4V max: 3V})
    b.v(c.mcu.u.PA8)
    b.pwm(1kHz, 1.1)
    b.v(c.net("3V3"), nil)
    b.expect("partial", b.mean(b.v(c.net("3V3")), {to: 2ms}), {})
    c.net("absent")
})
bench("missing", {circuit: "absent"}, fn(b, c) {})
"#;
    let src = sources(text);
    let ev = evaluate(&src, &std_libs()).expect("the circuit still builds");
    assert_eq!(ev.benches.len(), 2, "both bench calls have results");
    assert!(
        ev.benches
            .iter()
            .all(|b| matches!(b.state, UnitState::Failed(_))),
        "every bench of the bad document fails without a previous result"
    );
    let diags = ev.benches[0].state.diags();
    assert!(
        diags.iter().all(|d| d.message.starts_with("bringup:")),
        "all errors start with the owning bench's name: {diags:?}"
    );
    let find = |message: &str| {
        diags
            .iter()
            .find(|d| d.message.contains(message))
            .unwrap_or_else(|| panic!("error {message:?} is reported: {diags:?}"))
    };
    for (message, starts) in [
        ("no pin named ABSENT", "ABSENT:"),
        ("component is already stubbed", "b.stub(c.mcu.u, {PA1:"),
        ("press expects a switch", "b.press("),
        ("turn expects a pot", "b.turn("),
        ("duplicate expectation name", "b.expect(\"twice\""),
        ("expected A bounds", "1V"),
        ("cannot mix span", "{window:"),
        ("unknown option wind", "wind:"),
        ("min is greater than max", "b.expect(\"range\""),
        ("is unconnected", "b.v("),
        ("duty must be between", "b.pwm("),
        ("no net named", "c.net("),
        ("expected a net or pin", "b.v(c.net(\"3V3\"), nil)"),
        ("a span requires both", "{to:"),
    ] {
        assert!(
            diag_text(find(message), &src).starts_with(starts),
            "{message}: the offending call or field is selected"
        );
    }
    let index = text.find("c.lamps[9]").unwrap() as u32;
    assert!(
        matches!(find("array index 9 out of bounds").sel[0], Sel::Source { start, end, .. } if index <= start && end <= index + 10),
        "the out-of-range block is located inside c.lamps[9]"
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("expected a component, got nil")),
        "an out-of-bounds component is one diagnostic: the stub reports no second error for it: {diags:?}"
    );
    let duplicate = find("duplicate expectation name");
    let second = text.match_indices("b.expect(\"twice\"").nth(1).unwrap().0 as u32;
    assert!(
        matches!(duplicate.sel[0], Sel::Source { start, .. } if start == second),
        "duplicate expectation points at the second call"
    );
    let span = find("span from");
    assert_eq!(
        span.message, "bringup: span from 0.002 to 0.001 is empty or not finite",
        "span diagnostic uses seconds and the bench name"
    );
    assert_eq!(
        diag_text(span, &src),
        "{from: 2ms to: 1ms}",
        "span diagnostic selects the window object"
    );
    let missing = ev.benches[1]
        .state
        .diags()
        .iter()
        .find(|d| d.message.contains("no unit named"))
        .expect("unknown circuit diagnostic");
    assert!(
        missing.message.starts_with("missing:"),
        "unknown circuit names the bench"
    );
    assert_eq!(
        diag_text(missing, &src),
        "\"absent\"",
        "unknown circuit selects the circuit value"
    );
}

#[test]
fn probes_measures_and_document_order_use_the_existing_circuit() {
    let text = r#"
mod.elec.bench("other", {}, fn(b, c) {
    b.press(c.btn.sw)
    let v = b.v(c.mcu.u.VDD)
    let delta = b.v(c.net("3V3"), c.net("GND"))
    let i = b.i(c.lamps[3].d.A)
    let p = b.p(c.lamps[3].r)
    b.expect("now", b.now(v), {min: 3V})
    b.expect("mean", b.mean(delta, {from: 1ms to: 2ms}), {max: 4V})
    b.expect("rms", b.rms(i, {}), {max: 6mA})
    b.expect("min", b.min(p, {after: 5ms}), {min: 0W})
    b.expect("max", b.max(b.v(c.bus("LED")[3]), {window: 1ms}), {})
    b.expect("pp", b.pp(i, {window: 2ms}), {})
    b.expect("now span", b.now(v, {from: 1ms to: 2ms}), {})
    let computed = 1V
    b.expect("computed", b.now(i), {min: computed max: 2})
})
bench("last", {run: {}}, fn(b, c) {})
"#;
    let mut src = sources(BENCH);
    src.add("other.bench.splash", text)
        .expect("second bench document");
    let ev = evaluate(&src, &std_libs()).expect("circuit evaluates");
    assert_eq!(
        ev.benches
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        ["bringup", "other", "last"],
        "benches follow document and call order"
    );
    assert!(
        ev.benches.iter().all(|b| b.state == UnitState::Built),
        "every probe and measure builds: {:?}",
        ev.benches
    );
    let b = &ev.benches[1];
    assert_eq!(b.spec.pressed, ["btn.sw"], "press records the switch path");
    assert_eq!(
        at(&ev, text, "other", BenchKey::Press("btn.sw".into())),
        "b.press(c.btn.sw)",
        "press maps to its call"
    );
    let circuit = &ev.design.units[0].circuit;
    let net = |name| NetRef {
        board: b.scope_id(),
        net: circuit.nets.iter().find(|n| n.name == name).unwrap().id,
    };
    let comp = |path| circuit.comps.iter().find(|c| c.path == path).unwrap();
    let d = comp("lamps[3].d");
    let pin = PinRef {
        board: b.scope_id(),
        comp: d.id,
        pin: PinIx(d.part.pins.iter().position(|p| p.name == "A").unwrap() as u16),
    };
    assert_eq!(
        b.expects[0].probe,
        Probe::V(net("3V3")),
        "voltage on a pin resolves its net"
    );
    assert_eq!(
        b.expects[1].probe,
        Probe::VDiff(net("3V3"), net("GND")),
        "differential voltage preserves endpoint order"
    );
    assert_eq!(
        b.expects[2].probe,
        Probe::I(pin),
        "explicit pin current preserves its pin index"
    );
    assert_eq!(
        b.expects[3].probe,
        Probe::P(CompRef {
            board: b.scope_id(),
            comp: comp("lamps[3].r").id
        }),
        "power resolves the component"
    );
    assert_eq!(
        b.expects[4].probe,
        Probe::V(net("LED3")),
        "bus lookup returns member net handles"
    );
    assert_eq!(
        b.expects[..6].iter().map(|e| e.measure.clone()).collect::<Vec<_>>(),
        [
            Measure::Now,
            Measure::Mean,
            Measure::Rms,
            Measure::Min,
            Measure::Max,
            Measure::PeakToPeak
        ],
        "all measure variants"
    );
    assert_eq!(
        b.expects[0].when,
        When::Trailing {
            window: 0.0,
            after: 0.0
        },
        "now defaults"
    );
    assert_eq!(
        b.expects[1].when,
        When::Span {
            from: 0.001,
            to: 0.002
        },
        "span times"
    );
    assert_eq!(
        b.expects[6].when, b.expects[1].when,
        "now accepts an absolute span"
    );
    assert_eq!(
        b.expects[0].range().map(|(_, max)| max),
        Some(f64::INFINITY),
        "omitted upper bound"
    );
    assert_eq!(
        b.expects[1].range().map(|(min, _)| min),
        Some(f64::NEG_INFINITY),
        "omitted lower bound"
    );
    assert_eq!(
        b.expects[7].range().map(|(min, _)| min),
        Some(1.0),
        "computed bounds do not acquire literal unit checks"
    );
    assert_eq!(
        ev.benches[2].stop, None,
        "an optional run has no implicit stop"
    );
}

#[test]
fn duplicate_names_and_failed_units_are_bench_failures() {
    let mut src = sources("bench(\"same\", {}, fn(b, c) {})");
    let duplicate = "bench(\"same\", {}, fn(b, c) {})";
    src.add("duplicate.bench.splash", duplicate).unwrap();
    src.add(
        "broken.ckt.splash",
        "use mod.elec.*\nlet r = r(\"bad\", nil, nil)",
    )
    .unwrap();
    src.add(
        "failed.bench.splash",
        "bench(\"failed\", {circuit: \"broken\"}, fn(b, c) {})",
    )
    .unwrap();
    // The first document must name its unit now that there are two.
    let mut sources_with_unit = Sources::new();
    for doc in src.docs() {
        sources_with_unit
            .add(
                &doc.name,
                doc.text.replace("{}, fn", "{circuit: \"ledctl\"}, fn"),
            )
            .unwrap();
    }
    let ev = evaluate(&sources_with_unit, &std_libs())
        .expect("one unit builds despite another unit and benches failing");
    assert_eq!(
        ev.benches[0].state,
        UnitState::Built,
        "the first name remains valid"
    );
    let duplicate = &ev.benches[1];
    assert!(
        matches!(duplicate.state, UnitState::Failed(_)),
        "a name cannot repeat in another document"
    );
    let d = duplicate
        .state
        .diags()
        .iter()
        .find(|d| d.message.contains("duplicate bench name"))
        .unwrap();
    assert_eq!(
        diag_text(d, &sources_with_unit),
        "bench(\"same\", {circuit: \"ledctl\"}, fn(b, c) {})",
        "the second bench call is selected"
    );
    assert!(
        matches!(ev.benches[2].state, UnitState::Failed(_)),
        "a bench cannot use a failed unit"
    );
    assert!(
        ev.benches[2]
            .state
            .diags()
            .iter()
            .any(|d| d.message.starts_with("failed: unit \"broken\" failed:")),
        "a failed unit explains its bench failure"
    );
}

#[test]
fn block_benches_build_scoped_circuits_and_locate_mistakes() {
    use fab_elec_bench::{lower_circuit, ModelLibs};
    use fab_elec_circuit::NetKind;
    use fab_elec_core::BENCH_SCOPE_BASE;

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
    run: {stop: 2ms live: true}
}, fn(b, c) {
    b.vsrc(c.net("TOP"), c.net("GND"), {dc: 5V})
    b.expect("half", b.mean(b.v(c.net("OUT")), {window: 1ms after: 0.5ms}), {min: 2.4V max: 2.6V})
    b.expect("input", b.now(b.v(c.net("TOP"), c.net("GND"))), {min: 5V})
    b.expect("current", b.now(b.i(c.div.r_top)), {min: 0A})
    b.expect("power", b.now(b.p(c.div.r_bottom)), {min: 0W})
})
for name in ["loop.a", "loop.b"] {
    bench(name, {circuit: fn() {
        let gnd = e.net({kind: @ground})
    }}, fn(b, c) {})
}
"#;
    let mut src = Sources::new();
    let doc = src
        .add("blocks.bench.splash", text)
        .expect("block bench document");
    // Each failure gets its own document so it cannot invalidate the successful benches.
    let mistakes = [
        (
            "no_ground",
            "bench(\"no_ground\", {circuit: fn() { e.net({}) }}, fn(b, c) { b.probe(c.net(\"absent\")) })",
            "no_ground: the bench circuit has no ground net (e.net({kind: @ground}))",
            "fn() { e.net({}) }",
        ),
        (
            "two_grounds",
            "bench(\"two_grounds\", {circuit: fn() {\nlet first = e.net({kind: @ground})\nlet second = e.net({kind: @ground})\n}}, fn(b, c) { b.probe(c.net(\"absent\")) })",
            "two_grounds: a second ground net; a bench circuit has exactly one",
            "e.net({kind: @ground})",
        ),
        (
            "stale",
            "let saved = {net: nil}\nbench(\"stale\", {circuit: fn() {\nlet gnd = e.net({kind: @ground})\nlet captured = e.net({name: \"VIN\"})\nsaved.net = captured\n}}, fn(b, c) { b.v(saved.net) })",
            "stale: a handle of another circuit (in a bench function, reach the circuit through c: c.net(\"VIN\"), c.dut.r)",
            "b.v(saved.net)",
        ),
        (
            "top_level",
            "let stray = e.net({name: \"stray\"})\nbench(\"top_level\", {circuit: fn() { e.net({kind: @ground}) }}, fn(b, c) {})",
            "top_level: circuit items in a bench document go in a bench's circuit: fn",
            "e.net({name: \"stray\"})",
        ),
        (
            "live_flag",
            "bench(\"live_flag\", {circuit: fn() { e.net({kind: @ground}) } run: {live: 1}}, fn(b, c) {})",
            "live_flag: live: true or false",
            "1",
        ),
    ];
    let error_docs: Vec<_> = mistakes
        .iter()
        .map(|(name, body, _, _)| {
            let source = format!("let e = mod.elec\n{body}\n");
            src.add(&format!("{name}.bench.splash"), source)
                .expect("independent mistake document")
        })
        .collect();
    let ev = evaluate(&src, &std_libs()).expect("block benches need no design unit");
    assert!(
        ev.units.is_empty() && ev.design.units.is_empty(),
        "block circuits must stay outside the design's units"
    );
    let bench = &ev.benches[0];
    assert_eq!(
        bench.state,
        UnitState::Built,
        "the divider block bench must build: {:?}",
        bench.state.diags()
    );
    let BenchScope::Own {
        circuit,
        generation,
    } = &bench.scope
    else {
        panic!("a circuit function must produce an Own scope");
    };
    let scope = BoardId(BENCH_SCOPE_BASE);
    assert_eq!(
        bench.scope_id(),
        scope,
        "the first block bench must take the first bench scope id"
    );
    assert!(
        scope.is_bench_scope() && !BoardId(BENCH_SCOPE_BASE - 1).is_bench_scope(),
        "the reserved scope range must start exactly at BENCH_SCOPE_BASE"
    );
    assert_eq!(
        circuit
            .comps
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        ["div.r_top", "div.r_bottom"],
        "the circuit function must instantiate both divider parts"
    );
    assert_eq!(
        circuit
            .nets
            .iter()
            .filter(|n| n.kind == NetKind::Ground)
            .count(),
        1,
        "the block circuit must have exactly one ground"
    );
    assert_eq!(
        (bench.stop, bench.live),
        (Some(0.002), true),
        "block bench run options must retain their design-bench meaning"
    );
    assert_eq!(
        bench.spec.sources.len(),
        1,
        "the bench body must record its voltage source"
    );
    assert_eq!(
        bench.expects.len(),
        4,
        "the bench body must record every probe kind"
    );
    let net_is_local = |r: NetRef| r.board == scope && circuit.nets.iter().any(|n| n.id == r.net);
    let comp_is_local =
        |r: CompRef| r.board == scope && circuit.comps.iter().any(|c| c.id == r.comp);
    for source in &bench.spec.sources {
        assert!(
            net_is_local(source.a) && net_is_local(source.b),
            "stimulus endpoints must name nets of the bench circuit"
        );
    }
    for expect in &bench.expects {
        let local = match expect.probe {
            Probe::V(n) => net_is_local(n),
            Probe::VDiff(a, b) => net_is_local(a) && net_is_local(b),
            Probe::I(p) => {
                comp_is_local(p.comp_ref())
                    && circuit.comps[p.comp.ix()]
                        .part
                        .pins
                        .get(p.pin.ix())
                        .is_some()
            }
            Probe::P(c) => comp_is_local(c),
        };
        assert!(
            local,
            "{}: expectation refs must name the bench circuit's nets, components and pins",
            expect.name
        );
    }
    let lowered = lower_circuit(circuit, &bench.spec, *generation, &ModelLibs::generic());
    assert!(
        lowered.diags.is_empty(),
        "the block bench must lower for simulation: {:?}",
        lowered.diags
    );
    let offset = text
        .find("e.net({name: \"TOP\"})")
        .expect("a circuit-function call") as u32
        + 2;
    assert_eq!(
        ev.map.at(doc, offset).and_then(|sel| sel.unit()),
        Some(scope),
        "circuit-function sites must map into the bench scope"
    );
    assert!(
        ev.map.value_sites_of(scope).is_empty() && ev.map.pin_sites_of(scope).is_empty(),
        "bench scopes must not publish value or pin edit sites"
    );
    assert_eq!(
        ev.benches[1..3]
            .iter()
            .map(|b| (&b.state, b.scope_id()))
            .collect::<Vec<_>>(),
        [
            (&UnitState::Built, BoardId(BENCH_SCOPE_BASE + 1)),
            (&UnitState::Built, BoardId(BENCH_SCOPE_BASE + 2))
        ],
        "two calls from one loop must build with distinct consecutive scope ids"
    );
    assert!(
        matches!((&ev.benches[1].scope, &ev.benches[2].scope), (BenchScope::Own { generation: a, .. }, BenchScope::Own { generation: b, .. }) if generation < a && a < b),
        "new circuits must receive distinct increasing generations"
    );
    for (i, ((name, _, message, selection), doc)) in mistakes.iter().zip(error_docs).enumerate() {
        let b = ev
            .benches
            .iter()
            .find(|b| b.name == *name)
            .expect("each mistake has a bench result");
        let source = &src.doc(doc).expect("mistake document").text;
        let start = source
            .rfind(selection)
            .expect("selected mistake call or function") as u32;
        let located = b.state.diags().iter().any(|d| {
            d.message == *message
                && matches!(d.sel.first(), Some(Sel::Source { doc: at_doc, start: at_start, end })
                    if *at_doc == doc && *at_start == start && *end == start + selection.len() as u32)
        });
        let failed_scope = i >= 2 || matches!(&b.scope,
            BenchScope::Own { circuit, generation: 0 }
                if circuit.unit == BoardId(BENCH_SCOPE_BASE + 3 + i as u16)
                    && circuit.name == *name && circuit.nets.is_empty() && circuit.comps.is_empty());
        assert!(
            matches!(b.state, UnitState::Failed(_)) && b.state.diags().len() == 1 && located && failed_scope,
            "{name}: failure must retain its scope and select the offending function or call once: {:?}",
            b.state.diags()
        );
    }
}
