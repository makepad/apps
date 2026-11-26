use fab_elec_board::{resolve_rules, LengthMatch, NetPair, Spot};
use fab_elec_circuit::{BlockDefId, Intent, RefKind, Target};
use fab_elec_core::{mm, um, Angle, BlockId, Canon, Sel, Severity, P};
use fab_elec_lang::{evaluate, std_libs, Sources, UnitState};

fn sources(name: &str, text: &str) -> Sources {
    let mut sources = Sources::new();
    sources.add(name, text.to_string()).expect("a circuit document name");
    sources
}

#[test]
fn pair_intents_keep_options_resolve_merges_and_locate_refusals() {
    let libs = std_libs();
    let text = r#"
use mod.elec.*
let e = mod.elec
let a = e.net({name: "a"})
let b = e.net({name: "b"})
e.pair(a, b, {gap: 0.15mm z0: 90 skew: 0.1mm})
e.pair(a, b)

let p = e.net({})
let n = e.net({})
e.pair(p, n)
e.match_length([n, p], {tol: 0.25mm})
e.connect(a, p)
e.connect(b, n)
"#;
    let src = sources("pairs.ckt.splash", text);
    let ev = evaluate(&src, &libs).unwrap_or_else(|errors| panic!("pair declarations evaluate:\n{errors:#?}"));
    assert_eq!(ev.design.units.len(), 1, "pairs: one circuit unit");
    let circuit = &ev.design.units[0].circuit;
    let a = circuit.net("a").expect("pairs: net a exists").id;
    let b = circuit.net("b").expect("pairs: net b exists").id;
    assert_ne!(a, b, "pairs: the two sides remain distinct nets");
    assert_eq!(circuit.nets.len(), 2, "pairs: both later net records merge into a and b");
    assert_eq!(circuit.intents.len(), 5);
    assert_eq!(
        circuit.intents[0],
        Intent::Pair { p: a, n: b, gap: Some(um(150.0)), z0: Some(90.0) },
        "pairs: explicit options retain polarity, the gap in nanometres and impedance in ohms"
    );
    assert_eq!(
        circuit.intents[2],
        Intent::Pair { p: a, n: b, gap: None, z0: None },
        "pairs: omitted options stay unset for downstream rule resolution"
    );
    assert_eq!(
        circuit.intents[3],
        Intent::Pair { p: a, n: b, gap: None, z0: None },
        "pairs: both net record indices resolve through connections made after the declaration"
    );
    assert_eq!(circuit.intents[1], Intent::LengthMatch { nets: vec![a, b], tol: um(100.0) });
    assert_eq!(circuit.intents[4], Intent::LengthMatch { nets: vec![b, a], tol: um(250.0) });
    let board = ev.design.units[0].board.as_deref().expect("pairs: the circuit unit has a board");
    assert_eq!(board.length_matches, [
        LengthMatch { nets: vec![a, b], tol: um(100.0) },
        LengthMatch { nets: vec![b, a], tol: um(250.0) },
    ]);
    let mut without_matches = board.clone();
    without_matches.length_matches.clear();
    assert_ne!(without_matches.content_hash(), board.content_hash());
    assert_eq!(
        board.pairs,
        vec![
            NetPair { p: a, n: b, gap: Some(um(150.0)) },
            NetPair { p: a, n: b, gap: None },
            NetPair { p: a, n: b, gap: None },
        ],
        "pairs: the board mirrors every pair intent in order, without z0"
    );
    let rules = resolve_rules(board, None);
    assert_eq!(rules.partner(a), Some((b, um(150.0))), "pairs: the first pair's own gap wins for a");
    assert_eq!(rules.partner(b), Some((a, um(150.0))), "pairs: and for b");

    let prelude = "use mod.elec.*\nlet e = mod.elec\nlet a = e.net({})\nlet b = e.net({})\n";
    for (call, message) in [
        ("e.pair(1, b)", "p: expected a net, got a number"),
        ("e.pair(a, 1)", "n: expected a net, got a number"),
        ("e.pair(a, a)", "p and n are the same net"),
        ("e.pair(a, b, {unknown: 1})", "unknown option"),
        ("e.pair(a, b, {gap: 0mm})", "gap: must be > 0"),
        ("e.pair(a, b, {z0: 0})", "z0: must be a finite number > 0"),
        ("e.pair(a, b, {skew: -1mm})", "skew: must be > 0"),
        ("e.pair(a, b, {skew: @wide})", "skew: expected a length"),
        ("e.match_length(a, {tol: 1mm})", "expected a list"),
        ("e.match_length([], {tol: 1mm})", "at least two distinct nets"),
        ("e.match_length([a], {tol: 1mm})", "at least two distinct nets"),
        ("e.match_length([a, a], {tol: 1mm})", "at least two distinct nets"),
        ("e.match_length([a, 1], {tol: 1mm})", "nets[1]: expected a net"),
        ("e.match_length([a, b])", "tol: missing"),
        ("e.match_length([a, b], {})", "tol: missing"),
        ("e.match_length([a, b], 1)", "expected an options object"),
        ("e.match_length([a, b], {skew: 1mm})", "unknown option"),
        ("e.match_length([a, b], {tol: @wide})", "tol: expected a length"),
        ("e.match_length([a, b], {tol: 0mm})", "tol: must be > 0"),
        ("e.match_length([a, b], {tol: -1mm})", "tol: must be > 0"),
        ("e.match_length([a, b], {tol: 3000mm})", "tol: length is outside"),
    ] {
        let mut src = Sources::new();
        let doc = src.add("invalid_pair.ckt.splash", format!("{prelude}{call}\n")).expect("invalid pair source is added");
        let errors = evaluate(&src, &libs).expect_err("an invalid intent must be refused");
        assert_eq!(errors.len(), 1, "{call}: exactly one diagnostic, got {errors:?}");
        let diag = &errors[0];
        let callee = call.split_once('(').unwrap().0;
        assert!(diag.message.starts_with(&format!("{callee}:")), "{call}: the diagnostic names its call: {diag:?}");
        assert!(diag.message.contains(message), "{call}: the diagnostic explains {message:?}: {diag:?}");
        assert!(
            diag.sel.contains(&Sel::Source { doc, start: prelude.len() as u32, end: (prelude.len() + call.len()) as u32 }),
            "{call}: the diagnostic selects the offending call: {diag:?}"
        );
    }

    let call = "e.match_length([a, b], {tol: 0.25mm})";
    let mut src = Sources::new();
    let doc = src.add("merged_match.ckt.splash", format!("{prelude}{call}\ne.connect(a, b)\n")).unwrap();
    let errors = evaluate(&src, &libs).expect_err("a subsequent join leaves only one distinct net");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.starts_with("e.match_length:"));
    assert!(errors[0].sel.contains(&Sel::Source { doc, start: prelude.len() as u32, end: (prelude.len() + call.len()) as u32 }));
}

#[test]
fn a_spot_fixes_one_part_and_the_board_mirrors_it() {
    let libs = std_libs();
    let fixture = include_str!("../../fixtures/tonebox.ckt.splash");
    let spot = "e.fixed(key, {at: [20mm, 10mm] rot: 90deg})";
    let ev = evaluate(&sources("tonebox.ckt.splash", &format!("{fixture}\n{spot}\n")), &libs)
        .unwrap_or_else(|errors| panic!("a spot on a part evaluates:\n{errors:#?}"));
    let circuit = &ev.design.units[0].circuit;
    let key = circuit.comps.iter().find(|c| c.path == "key").expect("tonebox: the key is a part").id;
    let board = ev.design.units[0].board.as_deref().expect("tonebox: the circuit unit has a board");
    assert_eq!(board.spots, vec![Spot { comp: key, at: P::new(mm(20.0), mm(10.0)), rot: Some(Angle::QUARTER) }],
        "the board mirrors the spot, in nanometres and micro-degrees");

    let call = "e.fixed(usb, {at: [1mm, 1mm]})";
    let mut src = Sources::new();
    let doc = src.add("tonebox.ckt.splash", format!("{fixture}\n{call}\n")).expect("a circuit document name");
    let errors = evaluate(&src, &libs).expect_err("a spot on a block must be refused");
    assert_eq!(errors.len(), 1, "a spot on a block: exactly one diagnostic, got {errors:?}");
    assert!(errors[0].message.starts_with("e.fixed:") && errors[0].message.contains("at: a spot fixes one part"),
        "the diagnostic says a spot is one part's: {:?}", errors[0]);
    let start = (fixture.len() + 1) as u32;
    assert!(errors[0].sel.contains(&Sel::Source { doc, start, end: start + call.len() as u32 }),
        "the diagnostic selects the call: {:?}", errors[0]);
}

#[test]
fn pair_halves_merging_locate_the_pair_and_the_join() {
    let libs = std_libs();
    let prelude = "use mod.elec.*\nlet e = mod.elec\nlet a = e.net({name: \"a\"})\nlet b = e.net({name: \"b\"})\nlet c = e.net({name: \"c\"})\n";
    let pair = "e.pair(a, b)";
    let mut src = sources("valid_pair.ckt.splash", &format!("{prelude}{pair}\ne.connect(a, c)\n"));
    let mut expected = Vec::new();
    for (name, setup, join, callee) in [
        ("direct_pair", "", "e.connect(a, b)", "e.connect"),
        (
            "pin_pair",
            concat!(
                "let part = e.define(\"Two pins\", {kind: @resistor symbol: e.f.two_terminal([[\"A\", @passive], [\"B\", @passive]])})\n",
                "let j = e.part(part, {pins: [a]})\n",
                "e.connect(j.B, a)\n",
            ),
            "e.connect(j.B, b)",
            "e.connect",
        ),
        (
            "transitive_pair",
            "let wiring = e\ne.connect(a, c)\ne.connect(c, a)\n",
            "wiring.connect(c, b)",
            "wiring.connect",
        ),
    ] {
        let text = format!("{prelude}{pair}\n{setup}{join}\ne.connect(b, a)\n");
        let join_start = prelude.len() + pair.len() + 1 + setup.len();
        let doc = src.add(&format!("{name}.ckt.splash"), text).expect("collapsed pair source is added");
        expected.push((name, doc, join_start, join, callee));
    }
    // Keep a good unit so evaluation also exposes which failed units emitted no circuit.
    let ev = evaluate(&src, &libs).unwrap_or_else(|errors| panic!("the pair merged only with a third net must build:\n{errors:#?}"));
    assert_eq!(ev.units.len(), expected.len() + 1, "pairs: every circuit document has an evaluation state");
    for (name, doc, join_start, join, callee) in expected {
        let unit = ev.units.iter().find(|u| u.docs.contains(&doc)).expect("collapsed pair has a unit evaluation state");
        assert!(matches!(&unit.state, UnitState::Failed(_)), "{name}: a collapsed pair must fail lowering: {:?}", unit.state);
        let errors = unit.state.diags();
        assert_eq!(errors.len(), 1, "{name}: exactly one error for the collapsed pair, got {errors:?}");
        let diag = &errors[0];
        assert_eq!(diag.severity, Severity::Error, "{name}: a shorted pair is an error");
        assert_eq!(
            diag.message,
            format!("e.pair: p and n are one net: {callee} joins them"),
            "{name}: the diagnostic names the pair and the callee that first joined its halves"
        );
        assert_eq!(
            diag.sel,
            vec![
                Sel::Source { doc, start: prelude.len() as u32, end: (prelude.len() + pair.len()) as u32 },
                Sel::Source { doc, start: join_start as u32, end: (join_start + join.len()) as u32 },
            ],
            "{name}: the pair call is selected first, then the joining call, before later redundant connections"
        );
        assert!(
            !ev.design.units.iter().any(|u| u.id == unit.id),
            "{name}: the failed unit must emit no circuit or collapsed Intent::Pair"
        );
    }
    assert_eq!(ev.design.units.len(), 1, "pairs: only the unit with distinct halves emits a circuit");
    let circuit = &ev.design.units[0].circuit;
    let a = circuit.net("a").expect("valid pair: net a exists").id;
    let b = circuit.net("b").expect("valid pair: net b exists").id;
    assert_ne!(a, b, "valid pair: merging a with a third net leaves the halves distinct");
    assert_eq!(circuit.nets.len(), 2, "valid pair: the third net merges into a");
    assert_eq!(
        circuit.intents,
        vec![Intent::Pair { p: a, n: b, gap: None, z0: None }],
        "valid pair: merging only with a third net preserves the pair intent and polarity"
    );
}

#[test]
fn std_blocks_build_what_they_say() {
    let libs = std_libs();
    let mut fixture = include_str!("../../fixtures/ledctl.ckt.splash").to_string();
    for name in ["usb_c_power", "ldo", "led_driver", "button"] {
        let marker = format!("let {name} = e.block(");
        let start = fixture.find(&marker).unwrap_or_else(|| panic!("fixture: inline {name} definition exists"));
        // Nested bodies are indented; the statement's closing `})` starts a line.
        let end = start
            + fixture[start..].find("\n})").unwrap_or_else(|| panic!("fixture: inline {name} has a closing statement"))
            + "\n})".len();
        fixture.replace_range(start..end, "");
    }
    let src = sources("ledctl.ckt.splash", &fixture);
    let ev = evaluate(&src, &libs).unwrap_or_else(|errors| panic!("fixture using std blocks evaluates:\n{errors:#?}"));
    let twin = fab_elec_fixtures::ledctl();
    assert_eq!(ev.design.units.len(), 1, "fixture using std blocks: one circuit unit");
    let (got, want) = (&ev.design.units[0].circuit, &twin.units[0].circuit);

    // Compare the pieces before the hash so a failure names the part of the circuit.
    assert_eq!(got.unit, want.unit, "fixture: unit id");
    assert_eq!(got.name, want.name, "fixture: unit name");
    assert_eq!(got.classes, want.classes, "fixture: net classes");
    assert_eq!(got.nets, want.nets, "fixture: nets, including names, order, kinds, volts and classes");
    assert_eq!(got.buses, want.buses, "fixture: buses");
    assert_eq!(got.block_defs, want.block_defs, "fixture: block definitions");
    assert_eq!(got.tree, want.tree, "fixture: block tree");
    assert_eq!(got.comps.len(), want.comps.len(), "fixture: component count");
    for (g, w) in got.comps.iter().zip(&want.comps) {
        assert_eq!(
            (g.id, &g.path, &g.refdes, &g.value, &g.conns, &g.nc, g.block, g.dnp, &g.fields),
            (w.id, &w.path, &w.refdes, &w.value, &w.conns, &w.nc, w.block, w.dnp, &w.fields),
            "fixture: component {}", w.path
        );
        assert_eq!(g.part.name, w.part.name, "fixture: {}: part name", w.path);
        assert_eq!(g.part.pins, w.part.pins, "fixture: {}: part pins", w.path);
        assert_eq!(g.part.symbol, w.part.symbol, "fixture: {}: part symbol", w.path);
        assert_eq!(g.part.footprint, w.part.footprint, "fixture: {}: footprint", w.path);
        assert_eq!(g.part, w.part, "fixture: {}: part", w.path);
        // A part a std block makes keeps an automatic ref (its call is in a library document,
        // which Annotate cannot write); the twin's inline block could store one.
        let in_block = !got.tree[g.block.0 as usize].path.is_empty();
        assert!(g.ref_kind == w.ref_kind || (in_block && (w.ref_kind, g.ref_kind) == (RefKind::Auto, RefKind::Shared)),
            "fixture: {}: ref kind {:?} against the twin's {:?}", w.path, g.ref_kind, w.ref_kind);
    }
    assert!(got.comps.iter().any(|c| c.ref_kind == RefKind::Shared), "fixture: std blocks' parts keep automatic refs");
    assert_eq!(got.intents, want.intents, "fixture: intents");
    assert_eq!(got.hints, want.hints, "fixture: sheet hints");
    let mut same = (**got).clone();
    for (g, w) in same.comps.iter_mut().zip(&want.comps) { g.ref_kind = w.ref_kind; }
    assert_eq!(same.content_hash(), want.content_hash(), "fixture using std blocks equals its twin by canonical hash, ref kinds aside");
    assert_eq!(ev.design.assembly, twin.assembly, "fixture: one board mounted in the world");

    let text = r#"
use mod.elec.*
use mod.std.*
let e = mod.elec

let vdd = e.net({name: "vdd" kind: @power volts: 3.3V})
let gnd = e.net({name: "gnd" kind: @ground})
let sda = e.net({name: "sda"})
let scl = e.net({name: "scl"})
let xin = e.net({name: "xin"})
let xout = e.net({name: "xout"})
let sense = e.net({name: "sense"})
let nrst = e.net({name: "nrst"})

let dec = decoupling({vdd: vdd gnd: gnd})
let pullups = i2c_pullups({sda: sda scl: scl vdd: vdd})
let xtal = crystal_load({xin: xin xout: xout gnd: gnd})
let div = divider({top: vdd out: sense gnd: gnd ratio: 0.5 r_total: 100kohm})
let reset = reset_rc({nrst: nrst vdd: vdd gnd: gnd})
"#;
    let src = sources("std_blocks.ckt.splash", text);
    let ev = evaluate(&src, &libs).unwrap_or_else(|errors| panic!("the five additional std blocks evaluate:\n{errors:#?}"));
    assert_eq!(ev.design.units.len(), 1, "additional std blocks: one circuit unit");
    let circuit = &ev.design.units[0].circuit;

    let expected = [
        ("dec.caps[0]", Some("100nF"), "C_0402", BlockId(1)),
        ("dec.caps[1]", Some("100nF"), "C_0402", BlockId(1)),
        ("pullups.r_sda", Some("4.7k"), "R_0402", BlockId(2)),
        ("pullups.r_scl", Some("4.7k"), "R_0402", BlockId(2)),
        ("xtal.y", None, "XTAL_3225", BlockId(3)),
        ("xtal.c_in", Some("12pF"), "C_0402", BlockId(3)),
        ("xtal.c_out", Some("12pF"), "C_0402", BlockId(3)),
        // 50 kohm rounds to 49.9 kohm in E96 on each side of the divider.
        ("div.r_top", Some("49.9k"), "R_0402", BlockId(4)),
        ("div.r_bottom", Some("49.9k"), "R_0402", BlockId(4)),
        ("reset.r", Some("10k"), "R_0402", BlockId(5)),
        ("reset.c", Some("100nF"), "C_0402", BlockId(5)),
    ];
    assert_eq!(circuit.comps.len(), expected.len(), "additional std blocks: component count");
    for (comp, (path, value, part, block)) in circuit.comps.iter().zip(expected) {
        assert_eq!(comp.path, path, "{path}: component path in construction order");
        assert_eq!(comp.value.as_ref().map(|v| v.text.as_str()), value, "{path}: component value");
        assert_eq!(comp.part.name, part, "{path}: part and package");
        assert_eq!(comp.block, block, "{path}: owning block");
    }

    let comp = |path: &str| circuit.comp(path).unwrap_or_else(|| panic!("{path}: component exists"));
    let wired = |path: &str, pins: &[(&str, &str)]| {
        let c = comp(path);
        assert_eq!(c.conns.len(), pins.len(), "{path}: connected pin count");
        for &(pin, net) in pins {
            let pin_ix = c.part.pin(pin).unwrap_or_else(|| panic!("{path}: pin {pin} exists"));
            let net_id = circuit.net(net).unwrap_or_else(|| panic!("{path}.{pin}: net {net} exists")).id;
            assert_eq!(circuit.net_of(c.id, pin_ix), Some(net_id), "{path}.{pin}: connected to {net}");
        }
    };
    for (path, a, b) in [
        ("dec.caps[0]", "vdd", "gnd"),
        ("dec.caps[1]", "vdd", "gnd"),
        ("pullups.r_sda", "vdd", "sda"),
        ("pullups.r_scl", "vdd", "scl"),
        ("xtal.c_in", "xin", "gnd"),
        ("xtal.c_out", "xout", "gnd"),
        ("div.r_top", "vdd", "sense"),
        ("div.r_bottom", "sense", "gnd"),
        ("reset.r", "vdd", "nrst"),
        ("reset.c", "nrst", "gnd"),
    ] {
        wired(path, &[("1", a), ("2", b)]);
    }
    wired("xtal.y", &[("1", "xin"), ("2", "xout"), ("GND", "gnd")]);

    assert_eq!(circuit.intents.len(), 2, "crystal load: exactly two proximity intents");
    for (intent, path) in circuit.intents.iter().zip(["xtal.c_in", "xtal.c_out"]) {
        assert_eq!(
            intent,
            &Intent::Near { a: Target::Comp(comp(path).id), b: Target::Comp(comp("xtal.y").id), max: mm(2.0) },
            "{path}: within 2 mm of the crystal"
        );
    }

    let blocks = [
        (BlockId(0), "", None, None),
        (BlockId(1), "dec", Some(BlockDefId(0)), Some(BlockId(0))),
        (BlockId(2), "pullups", Some(BlockDefId(1)), Some(BlockId(0))),
        (BlockId(3), "xtal", Some(BlockDefId(2)), Some(BlockId(0))),
        (BlockId(4), "div", Some(BlockDefId(3)), Some(BlockId(0))),
        (BlockId(5), "reset", Some(BlockDefId(4)), Some(BlockId(0))),
    ];
    assert_eq!(circuit.tree.len(), blocks.len(), "block tree: root and five instances");
    for (block, expected) in circuit.tree.iter().zip(blocks) {
        assert_eq!(
            (block.id, block.path.as_str(), block.def, block.parent),
            expected,
            "block {:?}: path, definition and parent in instantiation order", expected.1
        );
    }
    let definitions = ["Decoupling", "I2C pull-ups", "Crystal load", "Divider", "Reset RC"];
    assert_eq!(circuit.block_defs.len(), definitions.len(), "block definitions: only the five instantiated std blocks");
    for (i, (def, name)) in circuit.block_defs.iter().zip(definitions).enumerate() {
        assert_eq!((def.id, def.name.as_str()), (BlockDefId(i as u32), name), "{name}: block definition in instantiation order");
    }
}
