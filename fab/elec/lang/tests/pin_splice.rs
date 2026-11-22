//! Pin splices preserve the surrounding source and evaluate to the requested connections.

use fab_elec_circuit::{Circuit, CircuitOp, Component, End, NetKind, RefKind, SheetHint};
use fab_elec_core::{BoardId, CompId, CompRef, NetId, PinIx, SchPt, Sel};
use fab_elec_design::DesignOp;
use fab_elec_lang::{evaluate, splice, splice_verified, std_libs, Evaluated, Sources, NO_CANCEL};

const LEDCTL: &str = include_str!("../../fixtures/ledctl.ckt.splash");

fn component<'a>(c: &'a Circuit, path: &str) -> &'a Component {
    c.comps.iter().find(|c| c.path == path).unwrap_or_else(|| panic!("component {path} exists"))
}

fn pin(c: &Circuit, path: &str, name: &str) -> (CompId, PinIx) {
    let c = component(c, path);
    (c.id, c.part.pin(name).unwrap_or_else(|| panic!("{} has pin {name}", c.path)))
}

fn net(c: &Circuit, name: &str) -> NetId {
    c.nets.iter().find(|n| n.name == name).unwrap_or_else(|| panic!("net {name} exists")).id
}

fn connection<'a>(c: &'a Circuit, path: &str, pin: PinIx) -> Option<&'a str> {
    component(c, path).conns.iter().find(|(p, _)| *p == pin).map(|(_, n)| c.nets[n.ix()].name.as_str())
}

fn op(operation: CircuitOp) -> DesignOp {
    DesignOp::Circuit { unit: BoardId(0), op: operation }
}

fn setup(text: &str) -> (Sources, Evaluated) {
    let mut sources = Sources::new();
    sources.add("pins.ckt.splash", text.to_string()).expect("the circuit document is added");
    let ev = evaluate(&sources, &std_libs()).expect("the original source evaluates");
    (sources, ev)
}

#[test]
fn part_template_copies_only_the_statement_and_its_written_ends() {
    let source = concat!(
        "let e = mod.elec\nlet a = e.net()\nlet b = e.net()\n",
        "let r = e.r(10kohm 1%, a, e.net(), {package: \"0603\", ref: \"R5\", fields: {ref: \"catalog\"}}) // outside\n",
        "let numbered = e.r(4kohm, opts: {ref: \"R5\"})\n",
        "let j = e.part(mod.std.header({pins: 4 pitch: 2.54mm}), {pins: {\"3\": b \"1\": a} ref: \"J1\"})\n",
        "let loose = e.c(2uF)\n",
        "let lamp = e.led({color: @green})\ne.connect(lamp.K, b)\n",
        "let sw = e.part(mod.std.tact_switch(), {pins: {A: nil}})\ne.connect(sw.A, b)\n",
        "let rev = e.r(1kohm, b: b, a: a)\n",
        "e.near(e.r(3kohm, a, b), r, 1mm)\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    let template = |part: &Component| fab_elec_lang::part_template(&ev.map, c.unit, part,
        |id| src.doc(id).map(|doc| doc.text.as_ref()));
    for (path, expected, pins) in [
        ("r", "let r = e.r(10kohm 1%, _n0, _n1, {package: \"0603\", fields: {ref: \"catalog\"}})", vec![PinIx(0), PinIx(1)]),
        ("numbered", "let numbered = e.r(4kohm, opts: {})", vec![]),
        ("j", "let j = e.part(mod.std.header({pins: 4 pitch: 2.54mm}), {pins: {\"3\": _n1 \"1\": _n0}})", vec![PinIx(0), PinIx(2)]),
        ("loose", "let loose = e.c(2uF)", vec![]),
        ("lamp", "let lamp = e.led({color: @green})", vec![]),
        ("sw", "let sw = e.part(mod.std.tact_switch(), {pins: {A: nil}})", vec![]),
        ("rev", "let rev = e.r(1kohm, b: _n1, a: _n0)", vec![PinIx(0), PinIx(1)]),
    ] {
        let (stmt, order) = template(component(c, path)).expect("a standalone part can be copied");
        assert_eq!(stmt, expected, "{path}: the copy slots inline connections, removes the stored ref and preserves all other fields");
        assert_eq!(order, pins, "{path}: omitted pins and separate connections have no slot");
        let tree = makepad_script::syntax::parse_with(&stmt, &fab_elec_lang::units::circuit_units())
            .expect("the copied text parses with circuit units");
        assert_eq!(tree.statements().len(), 1, "{path}: the paste is one Splash statement");
    }
    let header = component(c, "j");
    let (stmt, _) = template(header).expect("the header has a template");
    let hint = SheetHint { at: Some(SchPt::new(3, -2)), ..Default::default() };
    let (text, pasted) = after(&src, &ev, CircuitOp::AddPart {
        stmt, after: Some(header.id), ends: vec![End::Open, End::Net(net(c, "b"))],
        hint: Some(hint.clone()),
    });
    let c2 = &pasted.design.units[0].circuit;
    assert_eq!(c2.comps.len(), c.comps.len() + 1, "the spliced template evaluates to one new part");
    let new = component(c2, "j_b");
    assert_eq!(component(c2, "j").refdes, "J1", "the source part keeps its annotated reference");
    assert_eq!((new.ref_kind, new.refdes.as_str()), (RefKind::Auto, "J2"), "the pasted part gets the next free automatic reference");
    assert_eq!(new.conns, [(PinIx(2), net(c2, "b"))], "the open field disappears and the retained field joins its net");
    assert_eq!(c2.hints.parts, [(new.path.clone(), hint)], "the source stores the pasted part's own grid position");
    assert!(text.contains("let j_b = e.part("), "the copied statement gets a fresh binding in the source");
    let nested = c.comps.last().expect("the nested resistor exists");
    let err = template(nested).expect_err("a nested call cannot copy its enclosing statement");
    let at = ev.map.locate(&Sel::Comp(CompRef { board: c.unit, comp: nested.id })).expect("the nested call is mapped");
    assert!(err.message.contains("copy it by hand"), "the refusal explains how to copy the nested part");
    assert_eq!(err.sel, [Sel::Source { doc: at.doc, start: at.span.start, end: at.span.end }],
        "the refusal selects the part's own call");
}

#[test]
fn part_templates_detach_instance_values_and_report_block_locals() {
    let (src, ev) = setup(concat!(
        "let e = mod.elec\nlet gnd = e.net()\nlet resistance = fn(net) { 1kohm }\n",
        "let mixer_out = e.block({name: \"Mixer\" ports: {gnd: e.port(@ground)} ",
        "params: {r_bleed: 10kohm package: \"0603\"} build: fn(ports, settings) {\n",
        "    let mix = 2kohm\n",
        "    let rb = e.r(settings.r_bleed, ports.gnd, e.net(), {package: settings.package ref: \"R115\"})\n",
        "    let local = e.r(mix, ports.gnd, e.net())\n",
        "    let from_port = e.r(resistance(ports.gnd))\n",
        "    let plain = e.r(1kohm, ports.gnd, e.net())\n",
        "}})\nlet channel = mixer_out({gnd: gnd r_bleed: 100kohm package: \"0402\"})\n",
        "let anchor = e.r(1kohm)\n",
    ));
    let c = &ev.design.units[0].circuit;
    let copy = |part| fab_elec_lang::part_templates(&ev.map, c, part, |id| src.doc(id).map(|doc| doc.text.as_ref()))
        .expect("each part has a standalone statement");
    let bleed = component(c, "channel.rb");
    let copied = copy(bleed);
    assert_eq!(copied.stmt, "let rb = e.r(settings.r_bleed, _n0, _n1, {package: settings.package})",
        "same-block copies keep both parameter reads and remove the stored reference");
    let detached = copied.detached.expect("all remaining block reads have literals");
    assert_eq!(detached, "let rb = e.r(100kohm, _n0, _n1, {package: \"0402\"})",
        "the detached value and package come from this instance's overrides");
    assert_eq!(copied.pins, [PinIx(0), PinIx(1)], "both statements share the same slot order");
    let (_, pasted) = after(&src, &ev, CircuitOp::AddPart {
        stmt: detached, after: Some(component(c, "anchor").id), ends: vec![End::Open, End::Open], hint: None,
    });
    assert_eq!(component(&pasted.design.units[0].circuit, "rb").value, bleed.value,
        "pasting at the document root evaluates to the source instance's resistance");
    let local = component(c, "channel.local");
    let copied = copy(local);
    assert_eq!(copied.stmt, "let local = e.r(mix, _n0, _n1)", "the local remains usable in its own block");
    assert_eq!(copied.detached, Err(format!("{} uses mix, which exists only inside mixer_out: paste it there", local.refdes)),
        "the refusal names the part, captured local and enclosing block definition");
    let from_port = component(c, "channel.from_port");
    assert_eq!(copy(from_port).detached, Err(format!("{} uses ports, which exists only inside mixer_out: paste it there", from_port.refdes)),
        "a port read outside a pin slot refuses detachment");
    let plain = copy(component(c, "channel.plain"));
    assert_eq!(plain.detached, Ok(plain.stmt), "a statement with local reads only in pin slots needs no changes");
}

fn after(src: &Sources, ev: &Evaluated, operation: CircuitOp) -> (String, Evaluated) {
    after_all(src, ev, vec![operation])
}

/// One commit of `operations`, checked as `after` checks one.
fn after_all(src: &Sources, ev: &Evaluated, operations: Vec<CircuitOp>) -> (String, Evaluated) {
    let ops: Vec<DesignOp> = operations.into_iter().map(op).collect();
    let change = splice(src, &ev.map, &ev.design, &ops).expect("the pin operation splices");
    assert!(change.reevaluate, "pin operations request evaluation");
    assert!(!change.edits.is_empty(), "a successful pin operation changes text");
    let mut edited = src.clone();
    edited.apply(&change.edits).expect("pin edits are disjoint and apply together");
    for doc in src.docs() {
        let text = &edited.doc(doc.id).expect("the edited document exists").text;
        let mut edits: Vec<_> = change.edits.iter().filter(|e| e.doc == doc.id).collect();
        edits.sort_by_key(|e| (e.span.start, e.span.end));
        let (mut old, mut new) = (0, 0);
        for edit in edits {
            let untouched = edit.span.start as usize - old;
            assert_eq!(&doc.text[old..old + untouched], &text[new..new + untouched], "every byte before a pin edit is preserved");
            new += untouched;
            assert_eq!(&text[new..new + edit.text.len()], edit.text, "the replacement appears exactly as spliced");
            new += edit.text.len();
            old = edit.span.end as usize;
        }
        assert_eq!(&doc.text[old..], &text[new..], "every byte after the final pin edit is preserved");
    }
    let text = edited.docs()[0].text.to_string();
    let ev = evaluate(&edited, &std_libs()).unwrap_or_else(|errors| panic!("every edited source must evaluate: {errors:?}\n{text}"));
    (text, ev)
}

#[test]
fn led_trailing_ends_connect_and_rewire_only_the_third_argument() {
    let source = concat!(
        "let e = mod.elec\n",
        "let vcc = e.net({name: \"VCC\"})\nlet x = e.net({name: \"X\"})\nlet y = e.net({name: \"Y\"})\n",
        "let d = e.led({color: @green package: \"0603\"}, vcc, x)\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    assert_eq!(connection(c, "d", PinIx(0)), Some("VCC"), "A connects to the second argument");
    assert_eq!(connection(c, "d", PinIx(1)), Some("X"), "K connects to the third argument");
    let (text, got) = after(&src, &ev, CircuitOp::Connect { pin: pin(c, "d", "K"), net: net(c, "Y") });
    assert_eq!(text, source.replace(", vcc, x)", ", vcc, y)"), "rewiring K changes exactly the third argument");
    let c = &got.design.units[0].circuit;
    assert_eq!(connection(c, "d", PinIx(0)), Some("VCC"), "rewiring K preserves A");
    assert_eq!(connection(c, "d", PinIx(1)), Some("Y"), "K evaluates on the new net");
}

#[test]
fn a_loose_pin_disconnected_and_connected_in_one_commit_is_rewritten_once() {
    // A placed part's open ends are fresh nets; wiring one to a rail first disconnects it (the
    // plan's rule for a loose destination), then connects it, in one commit.
    let source = concat!(
        "let e = mod.elec\n",
        "let gnd = e.net({name: \"GND\"})\nlet a = e.net({name: \"A\"})\n",
        "let load = e.r(1kohm, a, gnd)\n",
        "let r = e.r(10kohm, e.net(), e.net())\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    let end = pin(c, "r", "1");
    let (text, got) = after_all(&src, &ev, vec![CircuitOp::Disconnect { pin: end }, CircuitOp::Connect { pin: end, net: net(c, "GND") }]);
    assert_eq!(text, source.replace("e.r(10kohm, e.net(), e.net())", "e.r(10kohm, gnd, e.net())"), "the loose end's one argument becomes the rail");
    assert_eq!(connection(&got.design.units[0].circuit, "r", PinIx(0)), Some("GND"), "the end evaluates on the rail");
}

#[test]
fn two_open_ends_written_in_place_wire_through_one_new_net() {
    // Two placed parts' open ends are nets written in place, so no name reaches either: wiring
    // one to the other (the plan disconnects a loose destination first) binds one new net.
    let source = concat!(
        "let e = mod.elec\n",
        "let r = e.r(10kohm, e.net(), e.net())\n",
        "let c = e.c(100nF, e.net(), e.net())\n",
    );
    let (src, ev) = setup(source);
    let circuit = &ev.design.units[0].circuit;
    let (a, b) = (pin(circuit, "r", "2"), pin(circuit, "c", "1"));
    let na = circuit.net_of(a.0, a.1).expect("r pin 2 has its own net");
    let (text, got) = after_all(&src, &ev, vec![CircuitOp::Disconnect { pin: b }, CircuitOp::Connect { pin: b, net: na }]);
    assert_eq!(text.matches("e.net()").count(), 3, "one new binding; the two other open ends stay in place:\n{text}");
    let got = &got.design.units[0].circuit;
    let (ra, cb) = (connection(got, "r", a.1), connection(got, "c", b.1));
    assert!(ra.is_some() && ra == cb, "both ends evaluate on one net: {ra:?} and {cb:?}\n{text}");
    assert_ne!(connection(got, "r", PinIx(0)), ra, "r's other end stays open");
}

#[test]
fn positional_tolerances_do_not_shift_pin_edits_onto_the_tolerance() {
    let source = concat!(
        "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\n",
        "let a = e.net({name: \"A\"})\nlet b = e.net({name: \"B\"})\n",
        "let r = e.r(10kohm 1%, a, b)\n",
        "let t = 5%\nlet computed = e.r(10kohm t, a, b)\n",
        "let options = {package: \"0603\"}\nlet plain = e.r(10kohm, a, b, options)\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    for (path, before, after_text) in [
        ("r", "e.r(10kohm 1%, a, b)", "e.r(10kohm 1%, b, b)"),
        ("computed", "e.r(10kohm t, a, b)", "e.r(10kohm t, b, b)"),
        ("plain", "e.r(10kohm, a, b, options)", "e.r(10kohm, b, b, options)"),
    ] {
        let pin = (component(c, path).id, PinIx(0));
        let (text, got) = after(&src, &ev, CircuitOp::Connect { pin, net: net(c, "B") });
        assert_eq!(text, source.replace(before, after_text), "{path}: Connect rewrites only the first net argument");
        let got = &got.design.units[0].circuit;
        assert_eq!(connection(got, path, PinIx(0)), Some("B"), "{path}: the rewritten pin connects to the target net");
        assert_eq!(component(got, path).value, component(c, path).value, "{path}: rewiring preserves the entire value, including tolerance");
    }
}

#[test]
fn fixture_pin_edits_resolve_scope_and_refuse_shared_connections() {
    let (src, ev) = setup(LEDCTL);
    let c = &ev.design.units[0].circuit;
    let boot = (component(c, "mcu.boot_r").id, PinIx(0));
    let boot0 = pin(c, "mcu.u", "BOOT0").1;
    let (text, got) = after(&src, &ev, CircuitOp::Connect { pin: boot, net: net(c, "3V3") });
    assert_eq!(text, LEDCTL.replacen("e.r(10kohm, u.BOOT0, p.gnd", "e.r(10kohm, p.vdd, p.gnd", 1), "connecting boot_r replaces only its first pin expression with the local port");
    let got = &got.design.units[0].circuit;
    assert_eq!(got.comps.len(), c.comps.len(), "rewiring preserves every component");
    for before in &c.comps {
        for (i, definition) in before.part.pins.iter().enumerate() {
            let p = PinIx(i as u16);
            let expected = if before.path == "mcu.boot_r" && p == PinIx(0) {
                Some("3V3")
            } else if before.path == "mcu.u" && p == boot0 {
                None
            } else {
                connection(c, &before.path, p)
            };
            assert_eq!(connection(got, &before.path, p), expected, "{} pin {} retains its connection except boot_r pin 0 and the released BOOT0", before.path, definition.name);
        }
    }

    let pa8 = pin(c, "mcu.u", "PA8");
    let (text, got) = after(&src, &ev, CircuitOp::Connect { pin: pa8, net: net(c, "mcu.SWDIO") });
    let expected = LEDCTL.replace("PA7: p.gpio[7]\n", "PA7: p.gpio[7]\n            PA8: swd.SWDIO\n").replace("@PA8", "");
    assert_eq!(text, expected, "adding PA8 preserves the pins object's indentation and removes only its nc item");
    let got = &got.design.units[0].circuit;
    assert_eq!(connection(got, "mcu.u", pa8.1), Some("mcu.SWDIO"), "the added field resolves the instance's named bus member");
    assert!(!component(got, "mcu.u").nc.contains(&pa8.1), "the connected pin is no longer marked nc");

    let c_in = (component(c, "reg.c_in").id, PinIx(1));
    let (text, got) = after(&src, &ev, CircuitOp::Disconnect { pin: c_in });
    assert_eq!(text, LEDCTL.replace("e.c(o.c_in,  p.vin,  p.gnd,", "e.c(o.c_in,  p.vin,  nil,"), "disconnecting a positional pin preserves its place and all surrounding spaces");
    assert_eq!(connection(&got.design.units[0].circuit, "reg.c_in", c_in.1), None, "nil leaves the capacitor's second pin unconnected");

    let vdd = pin(c, "mcu.u", "VDD");
    let (text, got) = after(&src, &ev, CircuitOp::Disconnect { pin: vdd });
    assert_eq!(text, LEDCTL.replace("\n            VDD: p.vdd", ""), "disconnecting VDD removes its field and preceding whitespace only");
    assert_eq!(connection(&got.design.units[0].circuit, "mcu.u", vdd.1), None, "removing the field disconnects VDD");

    for (operation, phrase, call_text) in [
        (CircuitOp::Connect { pin: (component(c, "lamps[3].r").id, PinIx(0)), net: net(c, "3V3") }, "8 components share", "e.r(e.series("),
        (CircuitOp::Connect { pin: pin(c, "btn.sw", "A"), net: net(c, "3V3") }, "3V3 is not reachable inside btn: give the block a port", "e.part(tact_switch()"),
        (CircuitOp::Disconnect { pin: pin(c, "lamps[0].d", "K") }, "8 components share", "e.connect(d.K, p.ctl)"),
    ] {
        let error = splice(&src, &ev.map, &ev.design, &[op(operation)]).expect_err("unsafe or unreachable pin edits are refused");
        assert!(error.message.contains(phrase), "the refusal explains {phrase}: {}", error.message);
        let Some(Sel::Source { doc, start, end }) = error.sel.first() else { panic!("every refusal selects its source call: {error:?}") };
        assert!(src.doc(*doc).unwrap().text[*start as usize..*end as usize].starts_with(call_text), "the refusal selects the responsible call, including a separate shared e.connect");
    }
}

#[test]
fn top_level_pin_edits_preserve_lines_separators_and_other_sites() {
    let source = concat!(
        "use mod.elec.*\r\nuse mod.std.*\r\nlet e = mod.elec\r\n",
        "let gnd = e.net({name: \"GND\" kind: @ground})\r\n",
        "let v33 = e.net({name: \"3V3\" kind: @power volts: 3.3V})\r\n",
        "let signals = e.bus(\"SIGNALS\", 2)\r\n",
        "let named = e.bus(\"NAMED\", [@FIRST @SECOND])\r\n",
        "let x = e.part(tact_switch(), {})\r\n",
        "\te.connect(x.A, gnd); // this connection's line\r\n",
        "e.connect(gnd, x.B)\r\n",
        "\tlet d = e.led({color: @green package: \"0603\"}); // keep this comment\r\n",
        "let marked = e.part(tact_switch(), {nc: [@A, @B]})\r\n",
        "let listed = e.part(tact_switch(), {pins: [gnd]})\r\n",
        "let twice = e.part(tact_switch(), {pins: {A: gnd, B: v33}})\r\n",
        "e.connect(twice.A, gnd)\r\n",
        "let probe_block = e.block({name: \"Probe\" ports: {supply: e.port(@power_in) lanes: e.port(@bidi, {width: 2})} build: fn(ports, settings) {\r\n",
        "    let hidden = e.net({})\r\n",
        "    let probe = e.led({color: @green package: \"0603\"})\r\n",
        "}})\r\n",
        "let scoped = probe_block({supply: v33 lanes: signals})\r\n",
        "// keep the final line\r\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    let xa = pin(c, "x", "A");
    let (text, got) = after(&src, &ev, CircuitOp::Disconnect { pin: xa });
    assert_eq!(text, source.replace("\te.connect(x.A, gnd); // this connection's line\r\n", ""), "disconnecting a bare e.connect removes its whole line, including its separator and comment");
    assert_eq!(connection(&got.design.units[0].circuit, "x", xa.1), None, "the pin connected only by the deleted statement is unconnected");
    assert_eq!(connection(&got.design.units[0].circuit, "x", pin(c, "x", "B").1), Some("GND"), "the other pin's separate connection survives");

    let da = pin(c, "d", "A");
    for (net_name, expression) in [("3V3", "v33"), ("SIGNALS1", "signals[1]"), ("SECOND", "named.SECOND")] {
        let (text, got) = after(&src, &ev, CircuitOp::Connect { pin: da, net: net(c, net_name) });
        let expected = source.replace("// keep this comment\r\n", &format!("// keep this comment\r\n\te.connect(d.A, {expression})\r\n"));
        assert_eq!(text, expected, "a missing site inserts a line after the binding, retaining tabs, comments and CRLF for {expression}");
        assert_eq!(connection(&got.design.units[0].circuit, "d", da.1), Some(net_name), "the inserted expression {expression} resolves to the requested net");
    }
    for (p, old, new) in [
        (xa, "e.connect(x.A, gnd)", "e.connect(x.A, v33)"),
        (pin(c, "x", "B"), "e.connect(gnd, x.B)", "e.connect(v33, x.B)"),
    ] {
        let (text, got) = after(&src, &ev, CircuitOp::Connect { pin: p, net: net(c, "3V3") });
        assert_eq!(text, source.replace(old, new), "reconnecting either e.connect end replaces its opposite argument");
        assert_eq!(connection(&got.design.units[0].circuit, "x", p.1), Some("3V3"), "either end evaluates on the requested net");
    }
    let probe = pin(c, "scoped.probe", "A");
    for (net_name, expression) in [("3V3", "ports.supply"), ("SIGNALS1", "ports.lanes[1]"), ("scoped.hidden", "hidden")] {
        let (text, got) = after(&src, &ev, CircuitOp::Connect { pin: probe, net: net(c, net_name) });
        let anchor = "    let probe = e.led({color: @green package: \"0603\"})\r\n";
        assert_eq!(text, source.replace(anchor, &format!("{anchor}    e.connect(probe.A, {expression})\r\n")), "instance lookup uses the actual port parameter, bus index or local net binding for {expression}");
        assert_eq!(connection(&got.design.units[0].circuit, "scoped.probe", probe.1), Some(net_name), "the scoped expression {expression} connects the intended net");
    }

    let marked = pin(c, "marked", "A");
    let (text, got) = after(&src, &ev, CircuitOp::Connect { pin: marked, net: net(c, "3V3") });
    assert_eq!(text, source.replace("{nc: [@A, @B]}", "{nc: [ @B] pins: {A: v33}}"), "a missing pins object is added and the first nc item's comma is removed");
    let got = &got.design.units[0].circuit;
    assert_eq!(connection(got, "marked", marked.1), Some("3V3"), "the newly added pins object connects the pin");
    assert_eq!(component(got, "marked").nc, vec![pin(c, "marked", "B").1], "the other nc entry survives");

    let listed = pin(c, "listed", "A");
    let (text, got) = after(&src, &ev, CircuitOp::Disconnect { pin: listed });
    assert_eq!(text, source.replace("pins: [gnd]", "pins: [nil]"), "disconnecting a list item retains its index");
    assert_eq!(connection(&got.design.units[0].circuit, "listed", listed.1), None, "a nil list item is unconnected");
    let (text, _) = after(&src, &ev, CircuitOp::Connect { pin: listed, net: net(c, "3V3") });
    assert_eq!(text, source.replace("pins: [gnd]", "pins: [v33]"), "connecting a list item replaces only that item");

    let twice = pin(c, "twice", "A");
    let (text, got) = after(&src, &ev, CircuitOp::Disconnect { pin: twice });
    assert_eq!(text, source.replace("pins: {A: gnd, B: v33}", "pins: { B: v33}").replace("e.connect(twice.A, gnd)\r\n", ""), "disconnect removes both the field with its comma and the separate connection statement");
    assert_eq!(connection(&got.design.units[0].circuit, "twice", twice.1), None, "removing every site fully disconnects the pin");
    let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::Connect { pin: twice, net: net(c, "3V3") })]).expect_err("a multiply connected pin cannot be reconnected in one place");
    assert!(error.message.contains("2 places") && error.message.contains("pins.A") && error.message.contains("e.connect end 0"), "a multiple-site refusal names both sites: {}", error.message);
    assert!(matches!(error.sel.first(), Some(Sel::Source { .. })), "the multiple-site refusal is located");
    for (operation, phrase) in [
        (CircuitOp::Connect { pin: pin(c, "listed", "B"), net: net(c, "3V3") }, "pins is not an object"),
        (CircuitOp::Disconnect { pin: da }, "d pin A is not connected in the text"),
    ] {
        let error = splice(&src, &ev.map, &ev.design, &[op(operation)]).expect_err("missing sites in lists and absent connections are refused");
        assert!(error.message.contains(phrase), "the refusal explains {phrase}: {}", error.message);
        assert!(matches!(error.sel.first(), Some(Sel::Source { .. })), "each refusal is located");
    }
}

#[test]
fn connect_pins_makes_and_names_a_net() {
    let source = concat!(
        "use mod.elec.*\r\nuse mod.std.*\r\nlet e = mod.elec\r\n",
        "let gnd = e.net({name: \"GND\"})\r\n",
        "\tlet r_x = e.r(1kohm, gnd, nil); // keep the resistor comment\r\n",
        "\tlet d = e.led({color: @green}); // keep the LED comment\r\n",
        "let r_taken_2 = 0\r\nlet r_taken = e.r(2kohm, nil, nil)\r\n",
        "let d_taken = e.led({color: @red})\r\n",
        "let held = e.net({name: \"r_net_2\"})\r\nlet r_net = e.r(5kohm, nil, nil)\r\n",
        "let r_2 = 0\r\n",
        "let part = tact_switch()\r\n",
        "let bare = e.part(part)\r\nlet empty = e.part(part, {})\r\n",
        "let marked = e.part(part, {pins: {} nc: [@A, @B, @A]})\r\n",
        "let local_block = e.block({name: \"Local\" ports: {} build: fn(p, o) {\r\n",
        "    let r = e.r(3kohm, nil, nil)\r\n",
        "    let led = e.led({color: @green})\r\n",
        "}})\r\nlet local = local_block({})\r\n",
        "let nested_block = e.block({name: \"Nested\" ports: {} build: fn(p, o) {\r\n",
        "    if true {\r\n        let r_inner = e.r(4kohm, nil, nil)\r\n    }\r\n",
        "    let outer = e.led({color: @green})\r\n",
        "}})\r\nlet nested = nested_block({})\r\n",
        "let shared_block = e.block({name: \"Shared\" ports: {} build: fn(p, o) {\r\n",
        "    let u = e.part(part, {})\r\n",
        "}})\r\nlet left = shared_block({})\r\nlet right = shared_block({})\r\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    let cases = [
        ("r_x", "2", "d", "A", "r_x_2", source
            .replace("\tlet r_x =", "\tlet r_x_2 = e.net()\r\n\tlet r_x =")
            .replace("e.r(1kohm, gnd, nil)", "e.r(1kohm, gnd, r_x_2)")
            .replace("// keep the LED comment\r\n", "// keep the LED comment\r\n\te.connect(d.A, r_x_2)\r\n")),
        ("r_taken", "2", "d_taken", "A", "r_taken_2_2", source
            .replace("let r_taken =", "let r_taken_2_2 = e.net()\r\nlet r_taken =")
            .replace("e.r(2kohm, nil, nil)", "e.r(2kohm, nil, r_taken_2_2)")
            .replace("let d_taken = e.led({color: @red})\r\n", "let d_taken = e.led({color: @red})\r\ne.connect(d_taken.A, r_taken_2_2)\r\n")),
        ("r_net", "2", "bare", "B", "r_net_2_2", source
            .replace("let r_net =", "let r_net_2_2 = e.net()\r\nlet r_net =")
            .replace("e.r(5kohm, nil, nil)", "e.r(5kohm, nil, r_net_2_2)")
            .replace("let bare = e.part(part)", "let bare = e.part(part, {pins: {B: r_net_2_2}})")),
        ("local.r", "2", "local.led", "A", "local.r_2_2", source
            .replace("    let r =", "    let r_2_2 = e.net()\r\n    let r =")
            .replace("e.r(3kohm, nil, nil)", "e.r(3kohm, nil, r_2_2)")
            .replace("    let led = e.led({color: @green})\r\n", "    let led = e.led({color: @green})\r\n    e.connect(led.A, r_2_2)\r\n")),
        ("nested.r_inner", "2", "nested.outer", "A", "nested.r_inner_2", source
            .replace("    if true {", "    let r_inner_2 = e.net()\r\n    if true {")
            .replace("e.r(4kohm, nil, nil)", "e.r(4kohm, nil, r_inner_2)")
            .replace("    let outer = e.led({color: @green})\r\n", "    let outer = e.led({color: @green})\r\n    e.connect(outer.A, r_inner_2)\r\n")),
        ("bare", "A", "bare", "B", "bare_a", source.replace(
            "let bare = e.part(part)", "let bare_a = e.net()\r\nlet bare = e.part(part, {pins: {A: bare_a B: bare_a}})")),
        ("bare", "A", "d", "A", "bare_a", source
            .replace("\tlet d =", "\tlet bare_a = e.net()\r\n\tlet d =")
            .replace("// keep the LED comment\r\n", "// keep the LED comment\r\n\te.connect(d.A, bare_a)\r\n")
            .replace("let bare = e.part(part)", "let bare = e.part(part, {pins: {A: bare_a}})")),
        ("d", "A", "d", "K", "d_a", source
            .replace("\tlet d =", "\tlet d_a = e.net()\r\n\tlet d =")
            .replace("// keep the LED comment\r\n", "// keep the LED comment\r\n\te.connect(d.A, d_a)\r\n\te.connect(d.K, d_a)\r\n")),
        ("empty", "A", "empty", "B", "empty_a", source.replace(
            "let empty = e.part(part, {})", "let empty_a = e.net()\r\nlet empty = e.part(part, { pins: {A: empty_a B: empty_a}})")),
        ("marked", "A", "marked", "B", "marked_a", source.replace(
            "let marked = e.part(part, {pins: {} nc: [@A, @B, @A]})",
            "let marked_a = e.net()\r\nlet marked = e.part(part, {pins: { A: marked_a B: marked_a} nc: []})")),
    ];
    for (pa, na, pb, nb, name, expected) in cases {
        let a = pin(c, pa, na);
        let b = pin(c, pb, nb);
        let operation = CircuitOp::ConnectPins { a, b };
        let change = splice(&src, &ev.map, &ev.design, &[op(operation.clone())]).expect("the unconnected pair splices");
        assert!(change.written.is_empty(), "{pa}/{pb}: wiring does not report added component statements");
        let (text, got) = after(&src, &ev, operation);
        assert_eq!(text, expected, "{pa}/{pb}: declaration placement, name, connections and untouched formatting match exactly");
        let got = &got.design.units[0].circuit;
        assert_eq!(got.nets.len(), c.nets.len() + 1, "{pa}/{pb}: wiring creates exactly one net");
        assert_eq!(connection(got, pa, a.1), Some(name), "{pa}/{pb}: the first pin uses the net named by its binding");
        assert_eq!(connection(got, pb, b.1), Some(name), "{pa}/{pb}: the second pin uses that same net");
        assert!(!component(got, pa).nc.contains(&a.1) && !component(got, pb).nc.contains(&b.1),
            "{pa}/{pb}: wired pins lose every no-connect entry");
    }
    let free = pin(c, "r_x", "2");
    let live = pin(c, "r_x", "1");
    for (a, b, phrase, located_comp) in [
        (free, free, "a pin cannot be wired to itself", free.0),
        (live, free, "r_x pin 1 is on net GND: use Connect", live.0),
        (pin(c, "d", "A"), live, "r_x pin 1 is on net GND: use Connect", live.0),
        (pin(c, "left.u", "A"), pin(c, "right.u", "B"), "no scope names both pins: left.u pin A and right.u pin B (give the block a port)", component(c, "left.u").id),
    ] {
        let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::ConnectPins { a, b })]).expect_err("an invalid pair is refused");
        assert_eq!(error.message, phrase, "the pair refusal states the offending pin and remedy");
        let at = ev.map.locate(&Sel::Comp(CompRef { board: BoardId(0), comp: located_comp })).expect("the refused component is mapped");
        assert_eq!(error.sel.first(), Some(&Sel::Source { doc: at.doc, start: at.span.start, end: at.span.end }),
            "{phrase}: the refusal selects the component's exact mapped call");
    }
}

#[test]
fn connect_named_creates_one_signal_and_reuses_named_nets() {
    let source = concat!(
        "use mod.elec.*\r\nlet e = mod.elec\r\n",
        "let supply = e.net({name: \"VCC\" kind: @power volts: 5V})\r\n",
        "let net_sense_2 = 0\r\n",
        "\tlet r = e.r(1kohm, supply, nil); // keep the part comment\r\n",
    );
    let label = "SENSE\"2";
    let declared = source.replace("\tlet r =", "\tlet net_sense_2_2 = e.net({name: \"SENSE\\\"2\"})\r\n\tlet r =");
    let (mut sources, mut ev) = setup(source);
    for (end, name, args, first, second) in [
        ("1", label, "net_sense_2_2, nil", label, None),
        ("2", label, "net_sense_2_2, net_sense_2_2", label, Some(label)),
        ("1", "VCC", "supply, net_sense_2_2", "VCC", Some(label)),
    ] {
        let pin = pin(&ev.design.units[0].circuit, "r", end);
        let ops = [op(CircuitOp::ConnectNamed { pin, name: name.into() })];
        let next = splice_verified(&sources, &ev.libs, &ev, &ops, &NO_CANCEL)
            .unwrap_or_else(|err| panic!("connecting r.{end} to {name} splices and evaluates: {err:?}"));
        sources = next.sources;
        ev = next.evaluated;
        assert_eq!(&*sources.docs()[0].text, declared.replace("supply, nil", args),
            "r.{end} to {name}: exactly one escaped declaration precedes the part and only its pin arguments change");
        let c = &ev.design.units[0].circuit;
        assert_eq!(c.nets.len(), 2, "r.{end} to {name}: the circuit has one new net alongside VCC");
        assert_eq!(c.nets[net(c, label).ix()].kind, NetKind::Signal, "the exact label names a signal net");
        assert_eq!(c.nets[net(c, "VCC").ix()].kind, NetKind::Power, "VCC keeps its original power kind");
        assert_eq!((connection(c, "r", PinIx(0)), connection(c, "r", PinIx(1))), (Some(first), second),
            "r.{end} to {name}: each pin retains its own connection across successive edits");
    }
}

#[test]
fn no_connect_marks_and_clears_part_pins() {
    let source = concat!(
        "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\n",
        "let part = tact_switch()\nlet gnd = e.net({name: \"GND\"})\n",
        "let spaced = e.part(part, {nc: [@A]})\n",
        "let commas = e.part(part, {nc: [@A, @A]})\n",
        "let trailing = e.part(part, {nc: [@A,]})\n",
        "let empty = e.part(part, {nc: []})\n",
        "let field = e.part(part, {value: \"switch\"})\n",
        "let bare = e.part(part)\n",
        "let numbered = e.part(header({pins: 2 pitch: 1.27mm}), {nc: [\"1\"]})\n",
        "let live = e.part(part, {pins: {A: gnd}})\n",
        "let r = e.r(1kohm, nil, nil)\n",
        "let d = e.led({color: @green})\n",
        "let names = [@A]\nlet computed = e.part(part, {nc: names})\n",
        "let repeated = e.part(part, {nc: [@A, @B, @A, @A]})\n",
        "let shared_block = e.block({name: \"Shared\" ports: {} build: fn(p, o) { let u = e.part(part, {}) }})\n",
        "let left = shared_block({})\nlet right = shared_block({})\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    for (path, name, before, marked, cleared) in [
        ("spaced", "B", "nc: [@A]", "nc: [@A @B]", "nc: [@A]"),
        ("commas", "B", "nc: [@A, @A]", "nc: [@A, @A, @B]", "nc: [@A, @A]"),
        ("trailing", "B", "nc: [@A,]", "nc: [@A, @B,]", "nc: [@A,]"),
        ("empty", "A", "nc: []", "nc: [@A]", "nc: []"),
        ("field", "B", "{value: \"switch\"}", "{value: \"switch\" nc: [@B]}", "{value: \"switch\" nc: []}"),
        ("bare", "A", "e.part(part)", "e.part(part, {nc: [@A]})", "e.part(part, {nc: []})"),
        ("numbered", "2", "nc: [\"1\"]", "nc: [\"1\" \"2\"]", "nc: [\"1\"]"),
        ("r", "2", "e.r(1kohm, nil, nil)", "e.r(1kohm, nil, nil, {nc: [\"2\"]})", "e.r(1kohm, nil, nil, {nc: []})"),
        ("d", "A", "e.led({color: @green})", "e.led({color: @green nc: [@A]})", "e.led({color: @green nc: []})"),
    ] {
        let pin = pin(c, path, name);
        let operation = CircuitOp::NoConnect { pin, nc: true };
        let change = splice(&src, &ev.map, &ev.design, &[op(operation.clone())]).expect("a free part pin can be marked");
        assert!(change.written.is_empty(), "{path}: no-connect does not report added statements");
        let (text, got) = after(&src, &ev, operation);
        assert_eq!(text, source.replacen(before, marked, 1), "{path}: marking preserves the list's separators and surrounding source");
        assert!(component(&got.design.units[0].circuit, path).nc.contains(&pin.1), "{path}: evaluation records the no-connect pin");
        let mut marked_src = src.clone();
        marked_src.apply(&change.edits).expect("the marking edits apply together");
        let (text, cleared_ev) = after(&marked_src, &got, CircuitOp::NoConnect { pin, nc: false });
        assert_eq!(text, source.replacen(before, cleared, 1), "{path}: clearing round-trips an existing list and retains an added empty list");
        assert!(!component(&cleared_ev.design.units[0].circuit, path).nc.contains(&pin.1), "{path}: clearing removes the evaluated no-connect mark");
        marked_src.apply(&change.inverse(&src, &ev.map).edits).expect("undo restores the marking edit");
        assert_eq!(marked_src, src, "{path}: undo round-trips every source byte, including newly added fields and options");
    }
    let (text, got) = after(&src, &ev, CircuitOp::NoConnect { pin: pin(c, "repeated", "A"), nc: false });
    assert_eq!(text, source.replace("nc: [@A, @B, @A, @A]", "nc: [ @B]"), "clearing removes all repeated entries without overlapping edits");
    assert_eq!(component(&got.design.units[0].circuit, "repeated").nc, vec![pin(c, "repeated", "B").1], "clearing repeated entries preserves the other pin");
    for (path, name, nc, phrase) in [
        ("live", "A", true, "live pin A is connected: disconnect it first"),
        ("spaced", "A", true, "spaced pin A is already no-connect"),
        ("spaced", "B", false, "spaced pin B is not no-connect"),
        ("computed", "B", true, "the part's nc list is not a literal list: edit it by hand"),
        ("computed", "A", false, "removing a pin from nc needs a literal list"),
        ("left.u", "A", true, "written in a body that 2 components share: give the block a port or a parameter"),
    ] {
        let pin = pin(c, path, name);
        let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::NoConnect { pin, nc })]).expect_err("an invalid no-connect operation is refused");
        assert_eq!(error.message, phrase, "{path}: the no-connect refusal names the precise problem");
        let at = ev.map.locate(&Sel::Comp(CompRef { board: BoardId(0), comp: pin.0 })).expect("the refused component is mapped");
        assert_eq!(error.sel.first(), Some(&Sel::Source { doc: at.doc, start: at.span.start, end: at.span.end }),
            "{path}: the refusal selects the component's exact mapped call");
    }
}

#[test]
fn an_open_end_is_marked_no_connect_and_a_later_connection_clears_the_mark() {
    let source = concat!(
        "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\nlet gnd = e.net({name: \"GND\"})\n",
        "let r = e.r(10kohm, e.net(), e.net(), {package: \"0402\"})\n",
        "let j = e.part(header({pins: 2 pitch: 2.54mm}), {pins: {\"1\": e.net() \"2\": e.net()}})\n",
        "let d = e.led({color: @green}, e.net(), e.net())\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    for (path, name, marked, connected) in [
        ("r", "2", "let r = e.r(10kohm, e.net(), nil, {package: \"0402\" nc: [\"2\"]})",
            "let r = e.r(10kohm, e.net(), gnd, {package: \"0402\" nc: []})"),
        ("j", "2", "let j = e.part(header({pins: 2 pitch: 2.54mm}), {pins: {\"1\": e.net()} nc: [\"2\"]})",
            "let j = e.part(header({pins: 2 pitch: 2.54mm}), {pins: {\"1\": e.net() \"2\": gnd} nc: []})"),
        ("d", "K", "let d = e.led({color: @green nc: [@K]}, e.net(), nil)",
            "let d = e.led({color: @green nc: []}, e.net(), nil)"),
    ] {
        let end = pin(c, path, name);
        let line = |text: &str| text.lines().find(|l| l.starts_with(&format!("let {path} = "))).unwrap_or("").to_string();
        // The plan marks a loose pin (an open end alone on its net) after disconnecting it.
        let (text, got) = after_all(&src, &ev, vec![CircuitOp::Disconnect { pin: end }, CircuitOp::NoConnect { pin: end, nc: true }]);
        assert_eq!(line(&text), marked, "{path}: the open end is dropped and the pin is listed in nc");
        let gc = &got.design.units[0].circuit;
        let marked_pin = pin(gc, path, name);
        assert!(component(gc, path).nc.contains(&marked_pin.1), "{path}: evaluation records the no-connect pin");
        assert_eq!(gc.net_of(marked_pin.0, marked_pin.1), None, "{path}: the marked pin is on no net");
        let alone = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::NoConnect { pin: end, nc: true })]).expect_err("a mark alone on a loose pin is refused");
        assert_eq!(alone.message, format!("{path} pin {name} is connected: disconnect it first"), "{path}: without the disconnect the open end still counts");
        let mut marked_src = src.clone();
        marked_src.apply(&splice(&src, &ev.map, &ev.design, &[op(CircuitOp::Disconnect { pin: end }), op(CircuitOp::NoConnect { pin: end, nc: true })])
            .expect("the mark splices").edits).expect("the mark applies");
        let (text, wired) = after(&marked_src, &got, CircuitOp::Connect { pin: marked_pin, net: net(gc, "GND") });
        let wc = &wired.design.units[0].circuit;
        let wired_pin = pin(wc, path, name);
        assert!(!component(wc, path).nc.contains(&wired_pin.1), "{path}: a connection clears the no-connect mark");
        assert_eq!(connection(wc, path, wired_pin.1), Some("GND"), "{path}: the pin is on the net it was wired to");
        assert_eq!(line(&text), connected, "{path}: the connection is written and the pin's mark cleared");
        // An LED's nil end records no site, so its new connection is an e.connect statement.
        assert_eq!(text.contains("e.connect(d.K, gnd)"), path == "d", "{path}: only the siteless end is wired by e.connect");
    }
}

#[test]
fn component_fields_round_trip_through_their_calls_options() {
    let source = concat!(
        "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\n",
        "let part = tact_switch()\nlet a = e.net({name: \"A\"})\nlet b = e.net({name: \"B\"})\n",
        "let both = e.r(10kohm, a, b, {fields: {source_value: \"10K\" note: \"keep\"}})\n",
        "let tol = e.r(4.7kohm, 1%, a, b)\n",
        "let opts = e.c(100nF, a, b, {package: \"0603\"})\n",
        "let only = e.r(1kohm, a, b, {fields: {note: \"x\"}})\n",
        "let lamp = e.led({color: @green fields: {source_value: \"GREEN\"}}, a, b)\n",
        "let bare = e.part(part)\n",
        "let sw = e.part(part, {pins: {A: a} fields: {note: \"s\"}})\n",
        "let o = {package: \"0402\"}\nlet computed = e.r(1kohm, a, b, o)\n",
        "let shared_block = e.block({name: \"Shared\" ports: {} build: fn(p, o) { let u = e.part(part, {}) }})\n",
        "let left = shared_block({})\nlet right = shared_block({})\n",
    );
    let fields = |c: &Circuit, path: &str| component(c, path).fields.0.clone();
    let owned = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    for (path, read) in [
        ("both", owned(&[("source_value", "10K"), ("note", "keep")])),
        ("lamp", owned(&[("source_value", "GREEN")])),
        ("sw", owned(&[("note", "s")])),
        ("tol", vec![]),
        ("bare", vec![]),
    ] {
        assert_eq!(fields(c, path), read, "{path}: evaluation reads the call's own fields in order");
    }
    for (path, key, text, before, written, read) in [
        ("both", "note", Some("kept"), "note: \"keep\"", "note: \"kept\"", owned(&[("source_value", "10K"), ("note", "kept")])),
        ("both", "note", None, "{source_value: \"10K\" note: \"keep\"}", "{source_value: \"10K\"}", owned(&[("source_value", "10K")])),
        ("tol", "source_value", Some("4K7"), "e.r(4.7kohm, 1%, a, b)", "e.r(4.7kohm, 1%, a, b, {fields: {source_value: \"4K7\"}})", owned(&[("source_value", "4K7")])),
        ("opts", "source_value", Some("100n"), "{package: \"0603\"}", "{package: \"0603\" fields: {source_value: \"100n\"}}", owned(&[("source_value", "100n")])),
        ("only", "note", None, "e.r(1kohm, a, b, {fields: {note: \"x\"}})", "e.r(1kohm, a, b)", vec![]),
        ("lamp", "source_value", None, "{color: @green fields: {source_value: \"GREEN\"}}", "{color: @green}", vec![]),
        ("bare", "source_value", Some("SW"), "e.part(part)", "e.part(part, {fields: {source_value: \"SW\"}})", owned(&[("source_value", "SW")])),
        ("sw", "lot", Some("A \"7\""), "fields: {note: \"s\"}", "fields: {note: \"s\" lot: \"A \\\"7\\\"\"}", owned(&[("note", "s"), ("lot", "A \"7\"")])),
    ] {
        let comp = component(c, path).id;
        let operation = CircuitOp::SetField { comp, key: key.into(), text: text.map(str::to_string) };
        let change = splice(&src, &ev.map, &ev.design, &[op(operation.clone())]).expect("a literal call's field splices");
        let (written_text, got) = after(&src, &ev, operation);
        assert_eq!(written_text, source.replacen(before, written, 1), "{path} {key}: only the call's options change, separators and other fields kept");
        assert_eq!(fields(&got.design.units[0].circuit, path), read, "{path} {key}: evaluation reads back the written fields");
        assert_eq!(component(&got.design.units[0].circuit, path).conns, component(c, path).conns, "{path} {key}: the pins keep their nets");
        let mut undone = src.clone();
        undone.apply(&change.edits).expect("the field edit applies");
        undone.apply(&change.inverse(&src, &ev.map).edits).expect("undo applies");
        assert_eq!(undone, src, "{path} {key}: undo restores every source byte");
    }
    for (path, key, text, phrase) in [
        ("both", "absent", None, "both field absent is not set"),
        ("computed", "note", Some("x"), "computed field note: the options are not a literal object: edit them by hand"),
        ("left.u", "note", Some("x"), "written in a body that 2 components share: give the block a port or a parameter"),
    ] {
        let comp = component(c, path).id;
        let operation = CircuitOp::SetField { comp, key: key.into(), text: text.map(str::to_string) };
        let error = splice(&src, &ev.map, &ev.design, &[op(operation)]).expect_err("an unwritable field edit is refused");
        assert_eq!(error.message, phrase, "{path}: the refusal names the precise problem");
        let at = ev.map.locate(&Sel::Comp(CompRef { board: BoardId(0), comp })).expect("the refused component is mapped");
        assert_eq!(error.sel.first(), Some(&Sel::Source { doc: at.doc, start: at.span.start, end: at.span.end }), "{path}: the refusal selects the component's call");
    }

    let bad = concat!(
        "use mod.elec.*\nlet e = mod.elec\nlet a = e.net({name: \"A\"})\n",
        "let n = e.r(1kohm, a, a, {fields: {note: 5}})\n",
        "let v = e.c(1uF, a, a, {volts: 16V fields: {volts: \"16V\"}})\n",
        "let k = e.led({color: @green fields: [\"x\"]}, a, a)\n",
    );
    let mut sources = Sources::new();
    sources.add("fields.ckt.splash", bad.to_string()).expect("the circuit document is added");
    let errors = evaluate(&sources, &std_libs()).expect_err("three bad fields are refused");
    assert_eq!(errors.len(), 3, "each bad field is one diagnostic: {errors:?}");
    for (error, (message, selected)) in errors.iter().zip([
        ("n: fields: note: a string, got 5", "5"),
        ("v: fields: volts is already given by the volts: option", "\"16V\""),
        ("k: fields: expected an object, got a list of 1", "[\"x\"]"),
    ]) {
        assert_eq!(error.message, message, "a bad field names its component and its key");
        let Some(Sel::Source { start, end, .. }) = error.sel.first() else { panic!("{message}: the diagnostic is located") };
        assert_eq!(&bad[*start as usize..*end as usize], selected, "{message}: the diagnostic selects the bad value");
    }
}
