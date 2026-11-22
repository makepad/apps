//! Series insertion changes only the requested connections and preserves the surrounding text.

use fab_elec_circuit::{Circuit, CircuitOp, Component};
use fab_elec_core::{BoardId, CompId, Diag, NetId, NetRef, PinIx, Sel};
use fab_elec_design::DesignOp;
use fab_elec_lang::{evaluate, splice, std_libs, Evaluated, Sources};

const FIXTURE: &str = include_str!("../../fixtures/ledctl.ckt.splash");
const RESISTOR: &str = "e.r(0ohm, _a, _b, {package: \"0402\"})";

fn component<'a>(circuit: &'a Circuit, path: &str) -> &'a Component {
    circuit.comp(path).unwrap_or_else(|| panic!("the component {path} exists"))
}

fn pin(circuit: &Circuit, path: &str, name: &str) -> (CompId, PinIx) {
    let component = component(circuit, path);
    (component.id, component.part.pin(name).unwrap_or_else(|| panic!("{path} has pin {name}")))
}

fn net(circuit: &Circuit, name: &str) -> NetId {
    circuit.net(name).unwrap_or_else(|| panic!("the net {name} exists")).id
}

fn setup(text: &str) -> (Sources, Evaluated) {
    let mut sources = Sources::new();
    sources.add("series.ckt.splash", text.to_string()).expect("the circuit document is added");
    let evaluated = evaluate(&sources, &std_libs()).expect("the original circuit evaluates");
    (sources, evaluated)
}

fn operation(net: NetId, stmt: &str, moved: Vec<(CompId, PinIx)>) -> DesignOp {
    DesignOp::Circuit {
        unit: BoardId(0),
        op: CircuitOp::InsertSeries { net, stmt: stmt.into(), moved },
    }
}

fn after(sources: &Sources, evaluated: &Evaluated, op: DesignOp) -> (Sources, Evaluated) {
    let change = splice(sources, &evaluated.map, &evaluated.design, &[op])
        .expect("a valid series insertion splices");
    assert!(change.reevaluate, "series insertion requests evaluation");
    assert!(!change.edits.is_empty(), "series insertion produces text edits");
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("all series edits are disjoint and apply together");
    for doc in sources.docs() {
        let text = &edited.doc(doc.id).expect("the edited document exists").text;
        let mut edits: Vec<_> = change.edits.iter().filter(|edit| edit.doc == doc.id).collect();
        edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
        let (mut old, mut new) = (0, 0);
        for edit in edits {
            let untouched = edit.span.start as usize - old;
            assert_eq!(&doc.text[old..old + untouched], &text[new..new + untouched],
                "every byte outside the series edits is preserved");
            new += untouched;
            assert_eq!(&text[new..new + edit.text.len()], edit.text,
                "each series replacement appears exactly as spliced");
            new += edit.text.len();
            old = edit.span.end as usize;
        }
        assert_eq!(&doc.text[old..], &text[new..], "the suffix after the last series edit is preserved");
    }
    let got = evaluate(&edited, &std_libs()).unwrap_or_else(|errors| {
        panic!("every edited series document evaluates: {errors:?}\n{}", edited.docs()[0].text)
    });
    (edited, got)
}

fn refused(sources: &Sources, evaluated: &Evaluated, op: DesignOp, phrase: &str) -> Diag {
    let original = sources.clone();
    let error = splice(sources, &evaluated.map, &evaluated.design, &[op])
        .expect_err("an invalid series insertion returns no edits");
    assert!(error.message.contains(phrase), "the refusal explains {phrase}: {}", error.message);
    assert!(matches!(error.sel.first(), Some(Sel::Source { .. })), "a series refusal selects its source");
    assert_eq!(*sources, original, "a refused series insertion leaves every source byte unchanged");
    error
}

#[test]
fn series_insertion_resolves_block_names_and_refuses_shared_or_mixed_bodies() {
    let (sources, evaluated) = setup(FIXTURE);
    let circuit = &evaluated.design.units[0].circuit;
    // The resistor's first pin is on the supply; the cathode is the LED bus pin.
    let wrong_net = refused(&sources, &evaluated,
        operation(net(circuit, "LED3"), RESISTOR, vec![(component(circuit, "lamps[3].r").id, PinIx(0))]),
        "not on net LED3");
    assert!(wrong_net.message.contains("lamps[3].r pin"), "the membership refusal names the offending pin");
    for (name, moved, call) in [
        ("LED3", pin(circuit, "lamps[3].d", "K"), "e.connect(d.K, p.ctl)"),
        ("3V3", (component(circuit, "lamps[3].r").id, PinIx(0)), "e.r(e.series("),
    ] {
        let error = refused(&sources, &evaluated, operation(net(circuit, name), RESISTOR, vec![moved]), "8 components share");
        let Some(Sel::Source { doc, start, end }) = error.sel.first() else {
            panic!("the shared-site refusal has a source location")
        };
        assert!(sources.doc(*doc).unwrap().text[*start as usize..*end as usize].starts_with(call),
            "the shared-site refusal selects the connection call");
    }
    refused(&sources, &evaluated,
        operation(net(circuit, "GND"), RESISTOR, vec![
            (component(circuit, "reg.c_in").id, PinIx(1)),
            (component(circuit, "mcu.boot_r").id, PinIx(1)),
        ]),
        "the pins to move are written in different blocks: move them one block at a time");

    let (local_text, local) = after(&sources, &evaluated,
        operation(net(circuit, "mcu.NRST"), RESISTOR, vec![(component(circuit, "mcu.rst_c").id, PinIx(0))]));
    assert!(local_text.docs()[0].text.contains("        let mcu_nrst_s = e.net({name: \"mcu.NRST_S\"})\n"),
        "a dotted net name becomes a lower-case binding and keeps the build body's indentation");
    assert!(local_text.docs()[0].text.contains("e.r(0ohm, swd.NRST, mcu_nrst_s, {package: \"0402\"})"),
        "the original net uses its local bus-member expression");
    let local = &local.design.units[0].circuit;
    let split = net(local, "mcu.NRST_S");
    assert_eq!(local.net_of(component(local, "mcu.rst_c").id, PinIx(0)), Some(split),
        "a single build body's moved pin reaches the new net");
    assert!(local.comps.iter().any(|part| part.block == component(local, "mcu.rst_c").block
        && part.conns == vec![(PinIx(0), net(local, "mcu.NRST")), (PinIx(1), split)]),
        "the inserted part uses the original bus member inside the same build body");
}

#[test]
fn series_insertion_splits_a_named_net_and_preserves_everything_else() {
    let source = concat!(
        "use mod.elec.*\r\nuse mod.std.*\r\nlet e = mod.elec\r\n",
        "let gnd = e.net({name: \"GND\" kind: @ground})\r\n",
        "let vcc = e.net({name: \"VCC\" kind: @power volts: 3.3V})\r\n",
        "let d = e.led({color: @green package: \"0603\"})\r\n",
        "let r = e.r(1kohm, vcc, d.A, {package: \"0402\"})\r\n",
        "\te.connect(d.K, gnd); // keep this connection comment\r\n",
        "// keep the final line\r\n",
    );
    let (sources, evaluated) = setup(source);
    let before = &evaluated.design.units[0].circuit;
    let cathode = pin(before, "d", "K");
    let ground = net(before, "GND");
    let (edited, got) = after(&sources, &evaluated, operation(ground, RESISTOR, vec![cathode]));
    let circuit = &got.design.units[0].circuit;
    let new_net = net(circuit, "GND_S");
    assert_eq!(circuit.nets.len(), before.nets.len() + 1, "series insertion adds exactly one net");
    assert_eq!(circuit.comps.len(), before.comps.len() + 1, "series insertion adds exactly one component");
    assert_eq!(circuit.net_of(component(circuit, "d").id, cathode.1), Some(new_net),
        "the cathode moves to GND_S");
    let added = circuit.comps.iter().find(|part| before.comp(&part.path).is_none())
        .expect("the inserted series component exists");
    assert_eq!(added.conns, vec![(PinIx(0), net(circuit, "GND")), (PinIx(1), new_net)],
        "the series resistor joins GND to GND_S in the requested order");
    assert_eq!(added.part, component(before, "r").part, "the inserted part uses the requested resistor package");
    assert_eq!(added.value.as_ref().and_then(|value| value.qty).map(|quantity| quantity.si), Some(0.0),
        "the inserted resistor has zero resistance");

    // Remove the two additions from a copy to compare the entire original model, with only
    // the moved connection restored. The insertion here follows both original components.
    let mut restored = Circuit::clone(circuit);
    restored.comps.retain(|part| before.comp(&part.path).is_some());
    restored.nets.retain(|wire| before.net(&wire.name).is_some());
    restored.comps[cathode.0.ix()].conns.iter_mut().find(|(pin, _)| *pin == cathode.1)
        .expect("the cathode retains a connection").1 = ground;
    assert_eq!(&restored, &**before, "every original circuit property except the moved pin is unchanged");
    let text = &edited.docs()[0].text;
    assert!(text.contains("\tlet gnd_s = e.net({name: \"GND_S\"})\r\n"),
        "the new binding keeps the anchor's tab and CRLF");
    assert!(text.contains("\te.r(0ohm, gnd, gnd_s, {package: \"0402\"})\r\n"),
        "the new resistor substitutes both plain identifiers and keeps indentation");
    assert!(text.contains("\te.connect(d.K, gnd_s); // keep this connection comment\r\n"),
        "the moved e.connect retains its separator and trailing comment");
    assert_eq!(text.as_ref(), source.replace("\te.connect(d.K, gnd);", concat!(
        "\tlet gnd_s = e.net({name: \"GND_S\"})\r\n",
        "\te.r(0ohm, gnd, gnd_s, {package: \"0402\"})\r\n",
        "\te.connect(d.K, gnd_s);",
    )), "the new binding precedes its first rewritten use without changing surrounding text");

    let (twice, second) = after(&edited, &got,
        operation(net(circuit, "GND"), RESISTOR, vec![(added.id, PinIx(0))]));
    let second = &second.design.units[0].circuit;
    assert!(second.net("GND_S").is_some() && second.net("GND_S2").is_some(),
        "a second insertion on GND chooses GND_S2 without renaming GND_S");
    assert_eq!(second.nets.len(), circuit.nets.len() + 1, "the second insertion adds one net");
    assert_eq!(second.comps.len(), circuit.comps.len() + 1, "the second insertion adds one resistor");
    assert!(twice.docs()[0].text.contains("let gnd_s2 = e.net({name: \"GND_S2\"})"),
        "the second insertion derives its binding from the unused net name");
    assert_eq!(second.net_of(component(second, "d").id, cathode.1), Some(net(second, "GND_S")),
        "the second insertion preserves the cathode's first new net");
    for ends in [["GND", "GND_S2"], ["GND_S2", "GND_S"]] {
        assert!(second.comps.iter().any(|part|
            part.conns == vec![(PinIx(0), net(second, ends[0])), (PinIx(1), net(second, ends[1]))]),
            "the second insertion keeps the series link from {} to {}", ends[0], ends[1]);
    }

    let catalog = "let r = e.r(0ohm, _n0, _n1, {package: \"0402\"})";
    let change = splice(&sources, &evaluated.map, &evaluated.design, &[
        operation(ground, catalog, vec![cathode]),
        DesignOp::Circuit { unit: BoardId(0), op: CircuitOp::Rename {
            what: Sel::Net(NetRef { board: BoardId(0), net: net(before, "VCC") }), to: "SUPPLY".into(),
        } },
    ]).expect("a catalog series insertion can accompany an earlier net-name edit");
    let mut catalog_text = sources.clone();
    catalog_text.apply(&change.edits).expect("the catalog insertion and earlier edit are disjoint");
    let expected = "let r_gnd = e.r(0ohm, gnd, gnd_s, {package: \"0402\"})";
    assert!(catalog_text.docs()[0].text.contains(&format!("\t{expected}\r\n")),
        "the catalog form names the part from the split net and preserves tab and CRLF");
    assert_eq!(change.written.len(), 1, "InsertSeries reports only the new part statement");
    let written = &change.written[0];
    assert_eq!(&catalog_text.doc(written.doc).unwrap().text[written.span.start as usize..written.span.end as usize], expected,
        "the written range excludes the new net declaration and follows the other op's earlier edit");
    let catalog_eval = evaluate(&catalog_text, &std_libs()).expect("the named catalog series part evaluates");
    let catalog_circuit = &catalog_eval.design.units[0].circuit;
    assert_eq!(component(catalog_circuit, "r_gnd").conns,
        vec![(PinIx(0), net(catalog_circuit, "GND")), (PinIx(1), net(catalog_circuit, "GND_S"))],
        "the catalog placeholders join the split net's old and new sides");
    let (repeated_text, repeated) = after(&catalog_text, &catalog_eval,
        operation(net(catalog_circuit, "GND"), catalog, vec![(component(catalog_circuit, "r_gnd").id, PinIx(0))]));
    assert!(repeated_text.docs()[0].text.contains("let r_gnd2 = e.r(0ohm, gnd, gnd_s2,"),
        "a repeated catalog insertion independently advances part and net bindings");
    assert!(repeated.design.units[0].circuit.comp("r_gnd2").is_some(), "the repeated catalog binding names its component");

    for (stmt, moved, phrase) in [
        (RESISTOR, vec![(component(before, "r").id, PinIx(0)), cathode], "not on net GND"),
        (RESISTOR, vec![cathode, cathode], "sorted without repeats"),
        (RESISTOR, vec![pin(before, "d", "A")], "d pin A is not on net GND"),
        (RESISTOR, vec![], "at least one pin"),
        ("e.r(0ohm, _a, gnd, {package: \"0402\"})", vec![cathode], "_b"),
        ("e.r(0ohm, _a, _a)", vec![cathode], "_a"),
        ("e.r(0ohm, _a, value._b)", vec![cathode], "_b"),
        ("e.r(0ohm, _a, \"_b\")", vec![cathode], "_b"),
        ("let r = e.r(0ohm, _a, _n1, {package: \"0402\"})", vec![cathode], "use _a/_b or _n0/_n1, not both"),
        ("e.r(0ohm, _a, _b", vec![cathode], "new statement at byte"),
        ("e.r(0ohm, _a, _b); e.r(1ohm, gnd, vcc)", vec![cathode], "exactly one statement"),
    ] {
        refused(&sources, &evaluated, operation(ground, stmt, moved), phrase);
    }

    // Select two GND pins to check ordering independently of membership, then move both
    // positional sites together while exercising collisions with ordinary local bindings.
    let mut both_ground = sources.clone();
    let at = source.find("vcc, d.A").expect("the resistor's original ends exist") as u32;
    both_ground.apply(&[
        fab_elec_design::TextEdit {
            doc: sources.docs()[0].id, span: at..at + "vcc, d.A".len() as u32, text: "gnd, gnd".into(),
        },
        fab_elec_design::TextEdit {
            doc: sources.docs()[0].id, span: 0..0, text: "let gnd_s = 1\r\nlet gnd_s2 = 2\r\n".into(),
        },
    ]).expect("the independent ground-only variation applies");
    let both = evaluate(&both_ground, &std_libs()).expect("the ground-only variation evaluates");
    let circuit = &both.design.units[0].circuit;
    let resistor = component(circuit, "r").id;
    let unsorted = refused(&both_ground, &both,
        operation(net(circuit, "GND"), RESISTOR, vec![(resistor, PinIx(1)), (resistor, PinIx(0))]),
        "sorted without repeats");
    assert!(unsorted.message.contains(&format!("r pin {}", component(circuit, "r").part.pins[0].name)),
        "the ordering refusal names the first descending pin");
    let moved_cathode = pin(circuit, "d", "K");
    let (collision_text, collision) = after(&both_ground, &both,
        operation(net(circuit, "GND"), &format!("{RESISTOR} // _a and _b stay in this comment"),
            vec![moved_cathode, (resistor, PinIx(0)), (resistor, PinIx(1))]));
    assert!(collision_text.docs()[0].text.contains("let gnd_s3 = e.net({name: \"GND_S\"})"),
        "binding collisions append digits independently of net-name collisions");
    assert!(collision_text.docs()[0].text.contains("// _a and _b stay in this comment"),
        "placeholder replacement preserves comments verbatim");
    let collision = &collision.design.units[0].circuit;
    assert_eq!(component(collision, "r").conns,
        vec![(PinIx(0), net(collision, "GND_S")), (PinIx(1), net(collision, "GND_S"))],
        "two disjoint positional sites in one statement both move to the new net");
    assert_eq!(collision.net_of(component(collision, "d").id, moved_cathode.1), Some(net(collision, "GND_S")),
        "the binding precedes every moved site even when component order differs from source order");
}
