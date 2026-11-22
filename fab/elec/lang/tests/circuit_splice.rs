//! Circuit operations edit source, then evaluation supplies the resulting circuit.

use fab_elec_circuit::{Circuit, CircuitOp, End, NetStyle, QtyUnit, Quantity, SheetHint, Value};
use fab_elec_core::{BoardId, Canon, CompId, CompRef, NetRef, PinIx, SchPt, Sel};
use fab_elec_design::{BenchOp, DesignOp};
use fab_elec_lang::{evaluate, splice, std_libs, Evaluated, Sources};

const LEDCTL: &str = include_str!("../../fixtures/ledctl.ckt.splash");

fn comp(c: &Circuit, path: &str) -> CompId {
    c.comps.iter().find(|c| c.path == path).unwrap_or_else(|| panic!("{path} exists")).id
}

fn net(c: &Circuit, name: &str) -> Sel {
    let net = c.nets.iter().find(|n| n.name == name).unwrap_or_else(|| panic!("net {name} exists")).id;
    Sel::Net(NetRef { board: BoardId(0), net })
}

fn op(op: CircuitOp) -> DesignOp {
    DesignOp::Circuit { unit: BoardId(0), op }
}

fn sources(text: &str) -> Sources {
    let mut sources = Sources::new();
    sources.add("ledctl.ckt.splash", text.to_string()).expect("a circuit document");
    sources
}

fn setup(text: &str) -> (Sources, Evaluated) {
    let sources = sources(text);
    let ev = evaluate(&sources, &std_libs()).expect("the source evaluates");
    (sources, ev)
}

fn edited(src: &Sources, ev: &Evaluated, operation: CircuitOp) -> Sources {
    let splice = splice(src, &ev.map, &ev.design, &[op(operation)]).expect("the operation splices");
    assert!(splice.reevaluate, "a circuit operation asks for evaluation");
    let mut src = src.clone();
    src.apply(&splice.edits).expect("the edits apply");
    src
}

fn after(src: &Sources, ev: &Evaluated, operation: CircuitOp) -> (String, Evaluated) {
    let src = edited(src, ev, operation);
    let text = src.docs()[0].text.to_string();
    let ev = evaluate(&src, &std_libs()).unwrap_or_else(|errors| panic!("the edited source evaluates: {errors:?}\n{text}"));
    (text, ev)
}

#[test]
fn stored_ref_round_trips_through_call_options() {
    let source = "let e = mod.elec\nlet r = e.r(1kohm, 1%, nil, nil)\nlet d = e.led({color: @green})\nlet rb = e.r(2kohm)\nlet rc = e.r(3kohm)\n";
    let (mut src, mut ev) = setup(source);
    let libs = std_libs();
    // The automatic parts keep their numbers whatever `r` stores, except the one whose number
    // `r` takes: it alone moves, to the lowest number nobody holds (the one `r` counted as).
    for (path, to, before, written, expected, others) in [
        ("r", Some("R12"), "1%, nil, nil)", "1%, nil, nil, {ref: \"R12\"})", "R12", ["R2", "R3"]),
        ("r", Some("R21"), "ref: \"R12\"", "ref: \"R21\"", "R21", ["R2", "R3"]),
        ("r", Some("R3"), "ref: \"R21\"", "ref: \"R3\"", "R3", ["R2", "R1"]),
        ("r", None, ", {ref: \"R3\"}", "", "R1", ["R2", "R3"]),
        ("d", Some("LED.7"), "{color: @green}", "{color: @green ref: \"LED.7\"}", "LED.7", ["R2", "R3"]),
        ("d", None, " ref: \"LED.7\"", "", "D1", ["R2", "R3"]),
    ] {
        let id = comp(&ev.design.units[0].circuit, path);
        let text = src.docs()[0].text.replacen(before, written, 1);
        src = edited(&src, &ev, CircuitOp::SetRef { comp: id, to: to.map(str::to_string) });
        assert_eq!(src.docs()[0].text.as_ref(), text, "{path} {to:?}: only the reference and its required separators change");
        ev = evaluate(&src, &libs).expect("the edited source evaluates");
        let c = &ev.design.units[0].circuit;
        assert_eq!(c.comps[id.ix()].refdes, expected, "{path} {to:?}: evaluation reads the edited reference");
        let got = ["rb", "rc"].map(|p| c.comps[comp(c, p).ix()].refdes.clone());
        assert_eq!(got, others, "{path} {to:?}: automatic references move only where the stored one takes their number");
    }
    assert_eq!(src.docs()[0].text.as_ref(), source, "removing both stored refs restores the original calls byte for byte");
}

#[test]
fn add_part_drops_unfilled_pins_fields_but_keeps_positional_nets() {
    let (src, ev) = setup("let e = mod.elec\nlet vcc = e.net({name: \"VCC\"})\n");
    let c = &ev.design.units[0].circuit;
    let Sel::Net(vcc) = net(c, "VCC") else { unreachable!() };
    let template = "let j = e.part(mod.std.header({pins: 3 pitch: 2.54mm}), {pins: {\"1\": _n0 \"2\": _n1 \"3\": _n2}})";
    let (text, got) = after(&src, &ev, CircuitOp::AddPart {
        stmt: template.into(), after: None, ends: vec![End::Net(vcc.net), End::Open], hint: None,
    });
    assert!(text.ends_with("let j_vcc = e.part(mod.std.header({pins: 3 pitch: 2.54mm}), {pins: {\"1\": vcc} added: 1})\n"),
        "both an explicit open end and a missing end remove their whole fields and whitespace");
    let c = &got.design.units[0].circuit;
    let j = comp(c, "j_vcc");
    assert_eq!(c.comps[j.ix()].conns, [(PinIx(0), vcc.net)], "only the supplied pin is connected");

    let (text, got) = after(&src, &ev, CircuitOp::AddPart {
        stmt: template.into(), after: None, ends: vec![], hint: None,
    });
    assert!(text.ends_with("let j = e.part(mod.std.header({pins: 3 pitch: 2.54mm}), {pins: {} added: 1})\n"),
        "removing all fields leaves an empty pins object");
    assert!(got.design.units[0].circuit.comps[0].conns.is_empty(), "an empty pins object makes no connections");

    let (text, got) = after(&src, &ev, CircuitOp::AddPart {
        stmt: "let r = e.r(10kohm, _n0, _n1)".into(), after: None, ends: vec![], hint: None,
    });
    assert!(text.ends_with("let r = e.r(10kohm, e.net(), e.net(), {added: 1})\n"), "open positional ends still get fresh nets");
    let c = &got.design.units[0].circuit;
    let r = comp(c, "r");
    let a = c.net_of(r, PinIx(0)).expect("the first positional end has a net");
    let b = c.net_of(r, PinIx(1)).expect("the second positional end has a net");
    assert_ne!(a, b, "open positional ends keep separate nets");
}

#[test]
fn shared_add_parts_write_one_net_before_both_uses_and_undo_together() {
    let source = "let e = mod.elec\r\nlet wire7 = e.net()\r\nlet r = e.r(1kohm, wire7, nil) // keep\r\n";
    let (mut src, ev) = setup(source);
    let r = comp(&ev.design.units[0].circuit, "r");
    let ops = [3, 8].map(|x| op(CircuitOp::AddPart {
        stmt: "let r = e.r(1kohm, _n0, _n1)".into(), after: Some(r), ends: vec![End::Shared(7), End::Open],
        hint: Some(SheetHint { at: Some(SchPt::new(x, 4)), ..SheetHint::default() }),
    }));
    let change = splice(&src, &ev.map, &ev.design, &ops).expect("both additions splice in one commit");
    let inverse = change.inverse(&src, &ev.map);
    src.apply(&change.edits).expect("the grouped insertion applies");
    let text = &src.docs()[0].text;
    let tail = text.strip_prefix(source).expect("the anchor and its comment stay byte-identical");
    let decl = tail.lines().next().expect("a declaration precedes the first part");
    let binding = decl.strip_prefix("let ").and_then(|s| s.strip_suffix(" = e.net()")).expect("the new net has a let binding");
    assert_ne!(binding, "wire7", "the shared binding cannot shadow an existing name");
    assert_eq!(tail.lines().filter(|line| *line == decl).count(), 1, "the shared binding is declared exactly once");
    let got = evaluate(&src, &std_libs()).expect("both parts and their sheet entries evaluate");
    let c = &got.design.units[0].circuit;
    assert_eq!(c.comps.len(), 3, "both copies are added to the original");
    let (a, b) = (&c.comps[1], &c.comps[2]);
    let joined = c.net_of(a.id, PinIx(0)).expect("the first shared slot is connected");
    assert_eq!(c.net_of(b.id, PinIx(0)), Some(joined), "both parts use the same new net");
    assert_eq!(c.pins_of(joined), [(a.id, PinIx(0)), (b.id, PinIx(0))], "the original and the open ends stay off the new net");
    assert_eq!(change.written.len(), 2, "both pasted statements have selection ranges");
    for ((part, x, order), range) in [(a, 3, 1), (b, 8, 2)].into_iter().zip(&change.written) {
        assert_eq!(&text[range.span.start as usize..range.span.end as usize],
            format!("let {} = e.r(1kohm, {binding}, e.net(), {{added: {order}}})", part.path), "each range selects exactly its copied statement");
        assert!(c.hints.parts.iter().any(|(path, hint)| path == &part.path && hint.at == Some(SchPt::new(x, 4))),
            "each generated path gets its own hint");
    }
    src.apply(&inverse.edits).expect("one inverse removes the whole paste");
    assert_eq!(src.docs()[0].text.as_ref(), source, "undo restores every byte including the original line endings");
}

#[test]
fn circuit_and_bench_inverses_restore_text_and_selections() {
    let mut sources = sources(LEDCTL);
    sources.add("ledctl.bench.splash", include_str!("../../fixtures/ledctl.bench.splash")).expect("the bench document is added");
    let ev = evaluate(&sources, &std_libs()).expect("the circuit and bench evaluate");
    let circuit = &ev.design.units[0].circuit;
    let Sel::Net(rail) = net(circuit, "3V3") else { unreachable!() };
    let boot = comp(circuit, "mcu.boot_r");
    for (class, operation) in [
        ("SetValue", op(CircuitOp::SetValue {
            comp: boot, value: Value { text: "4.7k".into(), qty: Some(Quantity { si: 4700.0, unit: QtyUnit::Ohm }), tol: None },
        })),
        ("Connect", op(CircuitOp::Connect { pin: (boot, PinIx(0)), net: rail.net })),
        ("SetRange", DesignOp::Bench { bench: "bringup".into(), op: BenchOp::SetRange { expect: "3V3 up".into(), min: 3.15, max: 3.45 } }),
    ] {
        let splice = splice(&sources, &ev.map, &ev.design, &[operation]).unwrap_or_else(|e| panic!("{class}: operation failed to splice: {e:?}"));
        assert!(!splice.edits.is_empty(), "{class}: the operation changes text");
        let mut edited = sources.clone();
        edited.apply(&splice.edits).unwrap_or_else(|e| panic!("{class}: forward edits failed: {e:?}"));
        let mut followed = ev.map.clone();
        followed.follow(&splice);
        let inverse = splice.inverse(&sources, &ev.map);
        assert_eq!(inverse.reevaluate, splice.reevaluate, "{class}: undo preserves the reevaluation requirement");
        let mut restored_sources = edited.clone();
        restored_sources.apply(&inverse.edits).unwrap_or_else(|e| panic!("{class}: inverse edits failed: {e:?}"));
        let mut restored = followed.clone();
        restored.follow(&inverse);
        for doc in sources.docs() {
            assert_eq!(&restored_sources.doc(doc.id).expect("the restored document exists").text, &doc.text,
                "{class}: undo restores every byte of {}", doc.name);
            let mut before_entries = ev.map.of_doc(doc.id);
            before_entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut restored_entries = restored.of_doc(doc.id);
            restored_entries.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(restored_entries, before_entries, "{class}: undo restores every selection and range of {} with no extras", doc.name);
        }
        assert_eq!(inverse.inverse(&edited, &followed).edits, splice.edits, "{class}: inverting the inverse restores the forward edits");
    }
}

#[test]
fn add_part_keeps_statement_indentation_and_surrounding_text() {
    // A direct button component makes `after: btn` a document statement. The full fixture's
    // button is a block instance; its component maps inside that block's build function.
    let source = concat!(
        "use mod.elec.*\r\nuse mod.std.*\r\nlet e = mod.elec\r\n",
        "let gnd = e.net({name: \"GND\" kind: @ground})\r\n",
        "let v33 = e.net({name: \"3V3\" kind: @power volts: 3.3V})\r\n",
        "\tlet btn = e.part(tact_switch(), {pins: {A: gnd B: v33}}); // keep this comment\r\n",
        "let build = fn() {\r\n\tlet local = e.r(2kohm, gnd, v33, {package: \"0402\"})\r\n}\r\nbuild()\r\n",
        "// keep the final line\r\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    let stmt = "let r9 = e.r(1kohm, gnd, v33, {package: \"0402\"})";
    let written = "let r9 = e.r(1kohm, gnd, v33, {package: \"0402\" added: 1})";
    let (text, got) = after(&src, &ev, CircuitOp::AddPart { stmt: stmt.into(), after: Some(comp(c, "btn")), ends: vec![], hint: None });
    let expected = source.replace("// keep this comment\r\n", &format!("// keep this comment\r\n\t{written}\r\n"));
    assert_eq!(text, expected, "only the new statement is inserted; tabs, comment and CRLF are preserved");
    let got = &got.design.units[0].circuit;
    assert_eq!(got.comps.len(), c.comps.len() + 1, "one component is added");
    assert_eq!(got.comps[comp(got, "r9").ix()].value.as_ref().and_then(|v| v.qty).map(|q| q.si), Some(1000.0), "r9 has the requested resistance");

    let (text, got) = after(&src, &ev, CircuitOp::AddPart { stmt: stmt.into(), after: None, ends: vec![], hint: None });
    assert_eq!(text, format!("{source}{written}\r\n"), "without an anchor the statement follows the entire document");
    assert!(got.design.units[0].circuit.comps.iter().any(|c| c.path == "r9"), "the appended part evaluates");
    let local = c.comps.iter().find(|c| c.value.as_ref().and_then(|v| v.qty).is_some_and(|q| q.si == 2000.0)).expect("the component inside the function").id;
    let (text, got) = after(&src, &ev, CircuitOp::AddPart { stmt: stmt.into(), after: Some(local), ends: vec![], hint: None });
    assert_eq!(text, source.replace("{package: \"0402\"})\r\n}", &format!("{{package: \"0402\"}})\r\n\t{written}\r\n}}")), "an inner statement, rather than the enclosing function declaration, anchors the insertion");
    assert_eq!(got.design.units[0].circuit.comps.len(), c.comps.len() + 1, "the nested insertion evaluates");
    for invalid in ["", "// no statement", "let a = 1; let b = 2", "let r9 = e.r("] {
        let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::AddPart { stmt: invalid.into(), after: None, ends: vec![], hint: None })]).expect_err("only one parsed statement is accepted");
        assert!(error.message.contains("new statement at"), "the diagnostic locates the new text: {}", error.message);
        assert!(matches!(error.sel.first(), Some(Sel::Source { .. })), "the insertion target is located");
    }
}

#[test]
fn add_part_fills_placeholders_names_the_part_and_writes_its_hint() {
    let source = format!("{LEDCTL}\ne.led({{color: @green package: \"0603\"}})\nlet root = e.part(tact_switch(), {{pins: {{B: v33}}}})\n");
    let (src, ev) = setup(&source);
    let c = &ev.design.units[0].circuit;
    let root = comp(c, "root");
    let Sel::Net(button) = net(c, "BTN") else { unreachable!() };
    let Sel::Net(rail) = net(c, "3V3") else { unreachable!() };
    let template = "let r = e.r(10kohm, _n0, _n1, {package: \"0402\"})";
    let line = "let r_btn = e.r(10kohm, key, v33, {package: \"0402\" added: 1})";
    let hint = SheetHint { at: Some(SchPt::new(12, 8)), ..SheetHint::default() };
    let add = CircuitOp::AddPart {
        stmt: template.into(), after: Some(root), ends: vec![End::Net(button.net), End::Net(rail.net)], hint: Some(hint.clone()),
    };
    let change = splice(&src, &ev.map, &ev.design, &[op(add.clone())]).expect("the catalog template splices");
    assert_eq!(change.edits.len(), 1, "the part and a new sheet at the same offset share one edit");
    let mut first = src.clone();
    first.apply(&change.edits).expect("the combined part and hint edit applies");
    assert_eq!(first.docs()[0].text.as_ref(), format!("{source}{line}\n\nsheet({{\n    \"r_btn\": {{at: [12 8]}}\n}})\n"),
        "the exact insertion resolves net handles, names the part and puts the hint after it");
    assert_eq!(change.written.len(), 1, "AddPart reports one written statement");
    let written = &change.written[0];
    assert_eq!(&first.doc(written.doc).unwrap().text[written.span.start as usize..written.span.end as usize], line,
        "written excludes indentation, newlines and the sheet statement");
    let got = evaluate(&first, &std_libs()).expect("the filled template and hint evaluate");
    let circuit = &got.design.units[0].circuit;
    let added = comp(circuit, "r_btn");
    assert_eq!(circuit.comps[added.ix()].conns, vec![(PinIx(0), button.net), (PinIx(1), rail.net)],
        "the inserted resistor connects to BTN and 3V3 in template order");
    assert_eq!(circuit.hints.parts, vec![("r_btn".into(), hint.clone())],
        "evaluation retains the new path's sheet hint");
    let call = got.map.locate(&Sel::Comp(CompRef { board: BoardId(0), comp: added })).expect("the new component is mapped");
    assert!(written.doc == call.doc && written.span.start <= call.span.start && call.span.end <= written.span.end,
        "the new map locates the new component inside the written statement");
    let (twice, second) = after(&first, &got, add);
    assert!(twice.contains("let r_btn2 = e.r(10kohm, key, v33, {package: \"0402\" added: 2})\n"),
        "a repeated catalog insertion chooses the next free binding");
    assert!(second.design.units[0].circuit.comp("r_btn2").is_some(), "the second binding becomes the component path");

    let (open_text, open) = after(&src, &ev, CircuitOp::AddPart {
        stmt: template.into(), after: Some(root), ends: vec![End::Net(button.net)], hint: None,
    });
    assert!(open_text.contains("let r_btn = e.r(10kohm, key, e.net(), {package: \"0402\" added: 1})\n"),
        "an unsupplied net placeholder becomes an unnamed net expression");
    let open = &open.design.units[0].circuit;
    let resistor = comp(open, "r_btn");
    let fresh = open.net_of(resistor, PinIx(1)).expect("the open second end has a net");
    assert_eq!(open.nets.len(), c.nets.len() + 1, "one open end creates exactly one fresh net");
    assert_eq!(open.pins_of(fresh), vec![(resistor, PinIx(1))], "the second pin alone owns the fresh net");
    assert_eq!(open.net_of(resistor, PinIx(0)), Some(button.net), "the supplied end stays on BTN");

    let root_pin = c.comps[root.ix()].part.pin("A").expect("the root switch has pin A");
    let (pin_text, connected) = after(&src, &ev, CircuitOp::AddPart {
        stmt: "e.connect(_n0, _n1)".into(), after: Some(root), ends: vec![End::Pin(root, root_pin), End::Net(button.net)], hint: None,
    });
    assert!(pin_text.ends_with("e.connect(root.A, key)\n"), "a pin placeholder uses the reachable binding and field name");
    assert_eq!(connected.design.units[0].circuit.net_of(root, root_pin), Some(button.net), "the filled pin expression evaluates on BTN");

    let boot = comp(c, "mcu.boot_r");
    let (scoped_text, scoped) = after(&src, &ev, CircuitOp::AddPart {
        stmt: template.into(), after: Some(boot), ends: vec![End::Net(button.net), End::Net(rail.net)], hint: Some(hint.clone()),
    });
    assert!(scoped_text.contains("        let r_btn = e.r(10kohm, p.user, p.vdd, {package: \"0402\" added: 1})\n"),
        "a block insertion resolves the instance's own port expressions");
    let scoped = &scoped.design.units[0].circuit;
    assert_eq!(scoped.comps[comp(scoped, "mcu.r_btn").ix()].conns, vec![(PinIx(0), button.net), (PinIx(1), rail.net)],
        "the block's new part reaches the requested external nets through its ports");
    assert_eq!(scoped.hints.parts, vec![("mcu.r_btn".into(), hint.clone())], "a nested part's hint uses its full instance path");
    let (rail_text, _) = after(&src, &ev, CircuitOp::AddPart {
        stmt: template.into(), after: None, ends: vec![End::Net(rail.net), End::Net(button.net)], hint: None,
    });
    assert!(rail_text.contains("let r_3v3 = e.r(10kohm, v33, key,"), "a numeric net suffix trims binding_name's leading underscore");
    let u = comp(c, "mcu.u");
    let boot0 = c.comps[u.ix()].part.pin("BOOT0").expect("the controller has a BOOT0 field");
    let pa8 = c.comps[u.ix()].part.pin("PA8").expect("the controller has a free PA8 field");
    let local_pin = edited(&src, &ev, CircuitOp::AddPart {
        stmt: "e.connect(_n0, _n1)".into(), after: Some(boot), ends: vec![End::Pin(u, boot0), End::Net(rail.net)], hint: None,
    });
    assert!(local_pin.docs()[0].text.contains("        e.connect(u.BOOT0, p.vdd)\n"),
        "a wired pin whose net has no binding inside a block resolves its local component binding");
    let Sel::Net(reset) = net(c, "mcu.NRST") else { unreachable!() };
    let local_net = edited(&src, &ev, CircuitOp::AddPart {
        stmt: template.into(), after: Some(boot), ends: vec![End::Net(reset.net)], hint: None,
    });
    assert!(local_net.docs()[0].text.contains("let r_nrst = e.r(10kohm, swd.NRST, e.net(),"),
        "a dotted net name uses its last segment for naming and its local bus expression for wiring");
    let commented = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::AddPart {
        stmt: format!(" \t// _n0 _n1 stay here\n{template}; // _n0 stays here too\n"),
        after: None, ends: vec![End::Net(button.net), End::Net(rail.net)], hint: None,
    })]).expect("comments are not counted as placeholders");
    let mut commented_src = src.clone();
    commented_src.apply(&commented.edits).expect("the commented statement applies");
    assert_eq!(commented.written.len(), 1, "comments do not create extra written statements");
    let at = &commented.written[0];
    assert_eq!(&commented_src.doc(at.doc).unwrap().text[at.span.start as usize..at.span.end as usize], line,
        "written skips leading comments and excludes trailing separators and comments");

    // The Connect op comes second but changes an earlier source range by a different length.
    let shifted = splice(&src, &ev.map, &ev.design, &[
        op(CircuitOp::AddPart { stmt: template.into(), after: Some(root), ends: vec![End::Net(button.net), End::Net(rail.net)], hint: None }),
        op(CircuitOp::Connect { pin: (comp(c, "reg.c_in"), PinIx(0)), net: rail.net }),
    ]).expect("a part insertion and an earlier connection edit splice together");
    let mut shifted_src = src.clone();
    shifted_src.apply(&shifted.edits).expect("the independent connection and insertion apply together");
    assert_eq!(shifted.written.len(), 1, "the commit reports only the inserted statement");
    let at = &shifted.written[0];
    assert_eq!(&shifted_src.doc(at.doc).unwrap().text[at.span.start as usize..at.span.end as usize], line,
        "written includes the length change of another op earlier in the document");
    assert!(shifted_src.docs()[0].text.contains("e.c(o.c_in,  p.vout,  p.gnd,"), "the earlier Connect actually changes the document length");

    // The existing sheet precedes an append; its own AddPart hint and another Hint both shift it.
    let appended = splice(&first, &got.map, &got.design, &[
        op(CircuitOp::AddPart { stmt: template.into(), after: None, ends: vec![End::Net(button.net), End::Net(rail.net)], hint: Some(hint.clone()) }),
        op(CircuitOp::Hint { path: "r_btn".into(), hint: Some(SheetHint { turn: 1, ..SheetHint::default() }) }),
    ]).expect("an append, its new hint and an existing hint replacement are disjoint");
    let mut appended_src = first.clone();
    appended_src.apply(&appended.edits).expect("the append and both sheet edits apply");
    assert_eq!(appended.written.len(), 1, "the hinted append reports one statement");
    let at = &appended.written[0];
    assert_eq!(&appended_src.doc(at.doc).unwrap().text[at.span.start as usize..at.span.end as usize], "let r_btn2 = e.r(10kohm, key, v33, {package: \"0402\" added: 2})",
        "written accounts for its own earlier hint and another op's earlier hint exactly once");

    let unbound = c.comps.iter().find(|part| part.block == c.comps[root.ix()].block && part.id != root)
        .expect("the bare root LED exists");
    let bare_pin = unbound.part.pin("A").expect("the bare LED has a named pin");
    for (stmt, ends, hint, phrase, target) in [
        ("e.connect(root.A, key)", vec![End::Net(button.net)], None, "the new statement has no _n0", root),
        ("e.connect(_n1, key)", vec![End::Net(button.net)], None, "the new statement has no _n0", root),
        ("e.connect(_n0, key)", vec![End::Pin(root, root_pin), End::Open], None, "the new statement has no _n1", root),
        ("e.connect(root.A, _n0)", vec![End::Net(button.net), End::Pin(root, root_pin)], None, "the new statement has no _n1", root),
        ("e.connect(_n0, _n1)", vec![End::Pin(boot, PinIx(0)), End::Net(button.net)], None, "mcu.u.BOOT0 is not reachable inside the document: give the block a port", root),
        ("e.connect(_n0, _n1)", vec![End::Pin(unbound.id, bare_pin), End::Net(button.net)], None, "bind the component to a name first", unbound.id),
        ("e.connect(_n0, _n1)", vec![End::Pin(u, pa8), End::Net(rail.net)], None, "no scope names both pins", u),
        ("e.connect(_n0, _n1)", vec![End::Pin(root, root_pin), End::Net(button.net)], Some(hint), "a sheet hint needs the new part to have a name (write let <stem> = ..)", root),
        ("e.connect(_n0, _n0)", vec![End::Net(button.net)], None, "_n0 exactly once as a plain identifier", root),
        ("e.connect(root._n0, key)", vec![End::Net(button.net)], None, "_n0 exactly once as a plain identifier", root),
        ("e.connect(root.A, {_n0: key})", vec![End::Net(button.net)], None, "_n0 exactly once as a plain identifier", root),
    ] {
        let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::AddPart {
            stmt: stmt.into(), after: Some(root), ends, hint,
        })]).expect_err("invalid placeholders, unreachable pins and nameless hints refuse the whole insertion");
        assert!(error.message.contains(phrase), "the refusal explains {phrase}: {}", error.message);
        let at = ev.map.locate(&Sel::Comp(CompRef { board: BoardId(0), comp: target })).expect("the refusal target is mapped");
        assert_eq!(error.sel, vec![Sel::Source { doc: at.doc, start: at.span.start, end: at.span.end }],
            "the refusal for {phrase} selects the insertion anchor or the offending pin's component");
    }
    let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::AddPart {
        stmt: "e.connect(_n1, key)".into(), after: None, ends: vec![End::Open], hint: None,
    })]).expect_err("an unanchored missing slot refuses");
    assert_eq!(error.sel, vec![Sel::Source { doc: src.docs()[0].id, start: 0, end: 0 }],
        "an unanchored refusal selects the unit document's start");
}

#[test]
fn add_part_ends_take_open_slots_and_free_pins() {
    let source = concat!(
        "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\n",
        "let v33 = e.net({name: \"3V3\" kind: @power volts: 3.3V})\n",
        "let sw = e.part(tact_switch(), {})\n",
        "let r_free_1 = 0\nlet r_free = e.r(10kohm, nil, nil)\n",
        "let isolated = e.block({name: \"Isolated\" ports: {} build: fn(p, o) {\n",
        "    let sw = e.part(tact_switch(), {})\n",
        "}})\nlet left = isolated({})\nlet right = isolated({})\n",
        "sheet({\"sw\": {at: [0 0]}})\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    let sw = comp(c, "sw");
    let a = c.comps[sw.ix()].part.pin("A").expect("the switch has pin A");
    let Sel::Net(rail) = net(c, "3V3") else { unreachable!() };
    let template = "let r_pu = e.r(10kohm, _n0, _n1)";
    assert_eq!(c.net_of(sw, a), None, "the switch pin starts free");
    let (text, got) = after(&src, &ev, CircuitOp::AddPart {
        stmt: template.into(), after: Some(sw), ends: vec![End::Pin(sw, a), End::Net(rail.net)], hint: None,
    });
    assert!(text.contains("let r_pu_a = e.r(10kohm, sw.A, v33, {added: 1})"),
        "a free identifier pin fills slot zero with its field expression and names the part from its pin path");
    let got = &got.design.units[0].circuit;
    let resistor = comp(got, "r_pu_a");
    let joined = got.net_of(comp(got, "sw"), a).expect("the free switch pin acquires a net");
    assert_eq!(got.net_of(resistor, PinIx(0)), Some(joined), "the pull-up's pin 1 joins the free switch pin");
    assert_eq!(got.net_of(resistor, PinIx(1)), Some(rail.net), "the pull-up's pin 2 stays on 3V3");
    assert_eq!(got.nets[joined.ix()].name, "sw.A", "the field route names the new net after the free pin");

    let (text, got) = after(&src, &ev, CircuitOp::AddPart {
        stmt: "let hdr = e.part(header({pins: 3 pitch: 2.54mm}), {pins: [_n0 _n1 _n2]})".into(),
        after: None, ends: vec![End::Open, End::Open, End::Net(rail.net)], hint: None,
    });
    assert!(text.ends_with("let hdr_3v3 = e.part(header({pins: 3 pitch: 2.54mm}), {pins: [e.net() e.net() v33] added: 1})\n"),
        "two open slots precede the supplied net, which supplies the new part's name");
    let got = &got.design.units[0].circuit;
    let header = comp(got, "hdr_3v3");
    let open0 = got.net_of(header, PinIx(0)).expect("open slot zero has its own net");
    let open1 = got.net_of(header, PinIx(1)).expect("open slot one has its own net");
    assert_ne!(open0, open1, "separate open slots do not share a net");
    assert_eq!(got.pins_of(open0), vec![(header, PinIx(0))], "slot zero stays open after evaluation");
    assert_eq!(got.pins_of(open1), vec![(header, PinIx(1))], "slot one stays open after evaluation");
    assert_eq!(got.net_of(header, PinIx(2)), Some(rail.net), "the third slot evaluates on 3V3");

    let free = comp(c, "r_free");
    let one = c.comps[free.ix()].part.pin("1").expect("the resistor has pin 1");
    assert_eq!(c.net_of(free, one), None, "the numbered resistor pin starts free");
    let change = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::AddPart {
        stmt: template.into(), after: None, ends: vec![End::Pin(free, one), End::Pin(free, one)],
        hint: Some(SheetHint { at: Some(SchPt::new(12, 8)), ..SheetHint::default() }),
    })]).expect("a repeated free numeric pin takes the binding route");
    let mut connected = src.clone();
    connected.apply(&change.edits).expect("the binding, connection, part and hint edits apply together");
    let text = connected.docs()[0].text.as_ref();
    assert_eq!(text.matches("let r_free_1_2 = e.net()\n").count(), 1,
        "two slots naming one free pin make one binding, with a digit separator when its name is taken");
    assert!(text.contains("let r_free_1_2 = e.net()\nlet r_free = e.r(10kohm, r_free_1_2, nil)"),
        "the binding precedes the existing resistor, whose pin 1 connects to it");
    let line = "let r_pu_r_free_1_2 = e.r(10kohm, r_free_1_2, r_free_1_2, {added: 1})";
    assert!(text.ends_with(&format!("{line}\n")), "both repeated slots use the one binding");
    assert_eq!(change.written.len(), 1, "only the new part is reported as a written statement");
    let written = &change.written[0];
    assert_eq!(&connected.doc(written.doc).unwrap().text[written.span.start as usize..written.span.end as usize], line,
        "written shifts by the earlier binding, connection, nc and sheet edits and covers exactly the new statement");
    let got = evaluate(&connected, &std_libs()).expect("the numbered free pin binding and its hint evaluate");
    let got = &got.design.units[0].circuit;
    let added = comp(got, "r_pu_r_free_1_2");
    let joined = got.net_of(comp(got, "r_free"), one).expect("the existing resistor acquires the binding's net");
    assert_eq!(got.net_of(added, PinIx(0)), Some(joined), "the added pin 1 shares the existing resistor's new net");
    assert_eq!(got.net_of(added, PinIx(1)), Some(joined), "the repeated free pin slot shares that same net");
    assert_eq!(got.nets.len(), c.nets.len() + 1, "the binding route creates exactly one net for a repeated free pin");

    let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::AddPart {
        stmt: "let r_pu = e.r(10kohm, _n0, nil)".into(),
        after: Some(sw), ends: vec![End::Pin(sw, a), End::Open], hint: None,
    })]).expect_err("every explicit end, even Open, needs its placeholder");
    assert_eq!(error.message, "the new statement has no _n1", "a missing explicit open slot is refused by index");

    let other = comp(c, "left.sw");
    let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::AddPart {
        stmt: template.into(), after: Some(comp(c, "right.sw")),
        ends: vec![End::Pin(other, a)], hint: None,
    })]).expect_err("a free pin in another block instance has no shared scope");
    assert!(error.message.contains("no scope names both pins") && error.message.contains("give the block a port"),
        "the different-instance refusal explains the scope problem and its remedy: {}", error.message);
    let at = ev.map.locate(&Sel::Comp(CompRef { board: BoardId(0), comp: other })).expect("the refused pin's component is mapped");
    assert_eq!(error.sel, vec![Sel::Source { doc: at.doc, start: at.span.start, end: at.span.end }],
        "the scope refusal selects the free pin's exact mapped call");
    assert_eq!(src.docs()[0].text.as_ref(), source, "refused ends leave every byte of the source unchanged");
    assert_eq!(c.net_of(other, a), None, "the refused cross-instance pin remains free");

    let (text, got) = after(&src, &ev, CircuitOp::AddPart {
        stmt: "let _p0 = e.r(10kohm, _n0, _n1)".into(),
        after: Some(sw), ends: vec![End::Pin(sw, a), End::Pin(sw, a)], hint: None,
    });
    assert!(text.contains("let _p0_a = e.r(10kohm, sw.A, sw.A, {added: 1})"),
        "_p0 is an ordinary binding and repeated field expressions name the same free pin");
    assert_eq!(got.design.units[0].circuit.nets.len(), c.nets.len() + 1,
        "repeating a free field-expression pin also creates exactly one net");
}

#[test]
fn add_part_reaches_an_open_end_written_in_place_through_its_pin() {
    // A placed part's open end is a net written in place: no name reaches it, so a part dropped
    // on that pin (End::Net of its net) connects through the pin and joins it.
    let source = "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\nlet r = e.part(mod.std.header({pins: 2 pitch: 2.54mm}), {pins: {\"1\": e.net() \"2\": e.net()}})\n";
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    let r = comp(c, "r");
    let open = c.net_of(r, PinIx(0)).expect("pin 1 is on its own net");
    let (text, got) = after(&src, &ev, CircuitOp::AddPart {
        stmt: "let flg = e.part(mod.std.power_flag(), {pins: {\"1\": _n0}})".into(), after: Some(r), ends: vec![End::Net(open)], hint: None,
    });
    let got = &got.design.units[0].circuit;
    let flag = got.comps.iter().find(|part| part.path.starts_with("flg")).expect("the flag was added").id;
    assert_eq!(got.net_of(flag, PinIx(0)), got.net_of(comp(got, "r"), PinIx(0)), "the flag joins r's pin 1:\n{text}");
    assert_ne!(got.net_of(comp(got, "r"), PinIx(0)), got.net_of(comp(got, "r"), PinIx(1)), "r's pin 2 stays open");
}

#[test]
fn remove_owns_one_statement_and_refuses_shared_calls() {
    let bare = "e.r(2kohm, gnd, v33, {package: \"0402\"})\n";
    let source = format!("{LEDCTL}\n{bare}e.near(e.r(3kohm, gnd, v33, {{package: \"0402\"}}), btn, 1mm)\n");
    let (src, ev) = setup(&source);
    let c = &ev.design.units[0].circuit;
    let (text, got) = after(&src, &ev, CircuitOp::Remove(comp(c, "mcu.boot_r")));
    assert_eq!(text, source.replace("        let boot_r = e.r(10kohm, u.BOOT0, p.gnd, {package: \"0402\"})\n", ""), "the unused binding and its line are removed, with every other byte kept");
    let got = &got.design.units[0].circuit;
    assert_eq!(got.comps.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), c.comps.iter().filter(|c| c.path != "mcu.boot_r").map(|c| c.path.as_str()).collect::<Vec<_>>(), "only boot_r disappears");
    for remaining in &got.comps {
        let before = &c.comps[comp(c, &remaining.path).ix()];
        assert_eq!((&remaining.part, &remaining.value, &remaining.fields), (&before.part, &before.value, &before.fields), "{} retains its part and value", remaining.path);
    }
    let by_value = |si: f64| c.comps.iter().find(|c| c.value.as_ref().and_then(|v| v.qty).is_some_and(|q| q.si == si)).expect("the added resistance exists").id;
    let (text, got) = after(&src, &ev, CircuitOp::Remove(by_value(2000.0)));
    assert_eq!(text, source.replace(bare, ""), "a bare call is removed with its own line");
    assert_eq!(got.design.units[0].circuit.comps.len(), c.comps.len() - 1, "the bare call made one component");
    let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::Remove(by_value(3000.0)))]).expect_err("an argument cannot be removed as a statement");
    assert!(error.message.contains("inside another expression") && error.message.contains("by hand"), "{}", error.message);
    for (path, count) in [("usb.cc[0]", 2), ("lamps[3].d", 8)] {
        let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::Remove(comp(c, path)))]).expect_err("a shared call cannot remove one component");
        assert!(error.message.contains(&format!("makes {count} components")), "{path}: {}", error.message);
        let at = ev.map.locate(&Sel::Comp(CompRef { board: BoardId(0), comp: comp(c, path) })).expect("mapped");
        assert_eq!(error.sel, vec![Sel::Source { doc: at.doc, start: at.span.start, end: at.span.end }], "the refusal selects the shared call");
    }
    // Removing a used binding leaves its use for evaluation to diagnose, with its location.
    let broken = edited(&src, &ev, CircuitOp::Remove(comp(c, "reg.c_in")));
    assert!(broken.docs()[0].text.contains("e.near(c_in,  u.VIN,  2mm)"), "a later use is not silently removed");
    let errors = evaluate(&broken, &std_libs()).expect_err("the surviving use is now unresolved");
    assert!(errors.iter().any(|d| d.message.contains("c_in") && matches!(d.sel.first(), Some(Sel::Source { .. }))), "the unresolved binding is located: {errors:?}");
}

#[test]
fn rename_respects_scopes_and_distinguishes_net_names_from_handles() {
    // The fixture spells BTN directly. Make its name indirect to exercise a handle rename
    // independently of the explicit-string rule: an unnamed net takes its binding as content.
    let source = LEDCTL.replace("let key  = e.net({name: \"BTN\"})", "let key_name = \"BTN\"\nlet key  = e.net({name: key_name})")
        + "\nlet labels = {key: \"key\" handle: key}\n"
        + "let shadow_param = fn(key) { key }\n"
        + "let shadow_local = fn() { let key = 1; key }\n"
        + "let capture = fn() { key }\n"
        + "let conflicting_capture = fn(target) { key }\n"
        + "let dotted = fn() { key.volts }\n"
        + "let member = fn(o) { o.key }\n"
        + "// key stays in this comment\n";
    let (src, ev) = setup(&source);
    let c = &ev.design.units[0].circuit;
    let key = net(c, "BTN");
    let (text, got) = after(&src, &ev, CircuitOp::Rename { what: key.clone(), to: "button_net".into() });
    let expected = source.replace("let key  =", "let button_net  =")
        .replace("user: key}", "user: button_net}")
        .replace("out: key gnd:", "out: button_net gnd:")
        .replace("handle: key}", "handle: button_net}")
        .replace("fn() { key }", "fn() { button_net }")
        .replace("fn(target) { key }", "fn(target) { button_net }")
        .replace("key.volts", "button_net.volts");
    assert_eq!(text, expected, "only the binding and its identifier uses change, including the first dotted segment");
    assert_eq!(got.design.units[0].circuit.content_hash(), c.content_hash(), "renaming a handle with an independent net name preserves circuit content");

    let (text, got) = after(&src, &ev, CircuitOp::Rename { what: net(c, "GND"), to: "GROUND".into() });
    assert_eq!(text, source.replace("name: \"GND\"", "name: \"GROUND\""), "an explicit net name edits only its string");
    assert!(got.design.units[0].circuit.nets.iter().any(|n| n.name == "GROUND"), "the net receives its new name");
    assert_ne!(got.design.units[0].circuit.content_hash(), c.content_hash(), "a net's explicit name is content");

    let (text, got) = after(&src, &ev, CircuitOp::Rename { what: Sel::Comp(CompRef { board: BoardId(0), comp: comp(c, "reg.u") }), to: "chip".into() });
    assert!(text.contains("let chip     = e.part(o.part"), "the component binding is renamed without changing its spacing");
    assert!(text.contains("e.near(c_in,  chip.VIN,  2mm)"), "dotted pin uses follow the binding");
    assert!(text.contains("let u = e.part(o.part"), "the other block's binding is untouched");
    assert!(got.design.units[0].circuit.comps.iter().any(|c| c.path == "reg.chip"), "the new component path evaluates");

    let reg = c.tree.iter().find(|b| b.path == "reg").expect("the block instance").id;
    let (text, got) = after(&src, &ev, CircuitOp::Rename { what: Sel::Block(BoardId(0), reg), to: "supply".into() });
    assert!(text.contains("let supply   = ldo("), "the instance binding is renamed");
    assert!(text.contains("e.near(supply, usb, 6mm)"), "instance uses follow the binding");
    assert!(got.design.units[0].circuit.tree.iter().any(|b| b.path == "supply"), "the renamed instance evaluates");

    for to in ["gnd", "shadow_param", "target", "for", "a.b", "two names"] {
        let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::Rename { what: key.clone(), to: to.into() })]).expect_err("collisions and invalid identifiers are refused");
        assert!(matches!(error.sel.first(), Some(Sel::Source { .. })), "a rename refusal is located");
    }
    let error = splice(&src, &ev.map, &ev.design, &[op(CircuitOp::Rename { what: net(c, "GND"), to: "3V3".into() })]).expect_err("an explicit name collides with another net's name");
    assert!(error.message.contains("a net named \"3V3\" exists"), "{}", error.message);
}

#[test]
fn sheet_hints_follow_instance_component_and_net_edits() {
    let tail = concat!(
        "\nsheet({\n",
        "    \"mcu\": {at: [24 10] flow: @down}, // keep this layout\n",
        "    \"mcu.pins\": {vdd: @top gpio: @right}\n",
        "    \"mcu.u\": {turn: 90deg}\n",
        "    \"mcu.boot_r\": {at: [12 8]}, // remove this part\n",
        "    \"mcu.boot_r.pins\": {A: @left B: @right},\n",
        "    \"lamps[0].r\": {at: [4 6]}\n",
        "    \"lamps[7].r\": {turn: 270deg}\n",
        "    \"lamps[7].r.pins\": {A: @top}\n",
        "    nets: {\"BTN\": @label VBUS: @wire}\n",
        "})\n",
    );
    let (src, ev) = setup(&format!("{LEDCTL}{tail}"));
    let c = &ev.design.units[0].circuit;
    assert!(ev.notes.is_empty(), "every original hint names an existing item");
    let renamed_hints = |paths: &[(&str, &str)]| {
        let mut hints = c.hints.clone();
        for (path, _) in &mut hints.parts {
            if let Some((_, to)) = paths.iter().find(|(old, _)| *old == path.as_str()) {
                *path = (*to).into();
            }
        }
        hints
    };

    let mcu = c.tree.iter().find(|b| b.path == "mcu").expect("the pinned instance exists").id;
    let (text, got) = after(&src, &ev, CircuitOp::Rename { what: Sel::Block(BoardId(0), mcu), to: "ctrl".into() });
    let expected_tail = tail.replace("\"mcu\"", "\"ctrl\"").replace("\"mcu.", "\"ctrl.");
    assert_eq!(text, format!("{}{expected_tail}", LEDCTL.replace("let mcu   =", "let ctrl   =")),
        "an instance rename changes only its binding and its path, pins and descendant keys");
    assert_eq!(got.design.units[0].circuit.hints, renamed_hints(&[
        ("mcu", "ctrl"), ("mcu.u", "ctrl.u"), ("mcu.boot_r", "ctrl.boot_r"),
    ]), "instance, pin and descendant hints retain their values at the new paths");
    assert!(got.notes.is_empty(), "renaming an instance leaves no stale-path note");

    let (text, got) = after(&src, &ev, CircuitOp::Rename {
        what: Sel::Comp(CompRef { board: BoardId(0), comp: comp(c, "lamps[0].r") }), to: "res".into(),
    });
    let expected_tail = tail.replace("\"lamps[0].r\"", "\"lamps[0].res\"")
        .replace("\"lamps[7].r\"", "\"lamps[7].res\"")
        .replace("\"lamps[7].r.pins\"", "\"lamps[7].res.pins\"");
    assert_eq!(text, format!("{}{expected_tail}", LEDCTL.replace("let r = e.r(", "let res = e.r(")),
        "one shared binding rename rewrites both loop instances' keys and the pin key only");
    assert_eq!(got.design.units[0].circuit.hints, renamed_hints(&[
        ("lamps[0].r", "lamps[0].res"), ("lamps[7].r", "lamps[7].res"),
    ]), "all hinted instances of the renamed call retain their hints");
    assert!(got.notes.is_empty(), "renaming a shared component leaves no stale-path note");

    let (text, got) = after(&src, &ev, CircuitOp::Rename { what: net(c, "BTN"), to: "BUTTON \"input\"".into() });
    let expected_tail = tail.replace("\"BTN\":", "\"BUTTON \\\"input\\\"\":");
    assert_eq!(text, format!("{}{expected_tail}", LEDCTL.replace("name: \"BTN\"", "name: \"BUTTON \\\"input\\\"\"")),
        "a named net rename quotes its new name in both the call and the sheet key");
    assert_eq!(got.design.units[0].circuit.hints.nets,
        vec![("BUTTON \"input\"".into(), NetStyle::Label), ("VBUS".into(), NetStyle::Wire)],
        "the renamed net retains its label style and other net styles remain unchanged");
    assert_eq!(got.design.units[0].circuit.hints.parts, c.hints.parts,
        "a net rename preserves every part hint");

    let (text, got) = after(&src, &ev, CircuitOp::Remove(comp(c, "mcu.boot_r")));
    let expected_tail = tail.replace(concat!(
        "    \"mcu.boot_r\": {at: [12 8]}, // remove this part\n",
        "    \"mcu.boot_r.pins\": {A: @left B: @right},\n",
    ), "");
    assert_eq!(text, format!("{}{expected_tail}", LEDCTL.replace(
        "        let boot_r = e.r(10kohm, u.BOOT0, p.gnd, {package: \"0402\"})\n", "")),
        "removing a component removes its path and pin entries and leaves every other byte intact");
    let mut expected = c.hints.clone();
    expected.parts.retain(|(path, _)| path != "mcu.boot_r");
    assert_eq!(got.design.units[0].circuit.hints, expected,
        "removing a component preserves all surviving hints");
    assert!(got.notes.is_empty(), "removing a pinned component leaves no stale-path note");

    let (text, got) = after(&src, &ev, CircuitOp::Rename {
        what: Sel::Comp(CompRef { board: BoardId(0), comp: comp(c, "reg.u") }), to: "chip".into(),
    });
    assert!(text.ends_with(tail), "renaming an unhinted component preserves the sheet byte for byte");
    assert_eq!(got.design.units[0].circuit.hints, c.hints,
        "renaming an unhinted component preserves all hint values");
    assert!(got.design.units[0].circuit.comps.iter().any(|c| c.path == "reg.chip"),
        "renaming an unhinted component still changes its evaluated path");
}

#[test]
fn sheet_key_collisions_keep_order_and_final_component_removal_drops_the_table() {
    let source = concat!(
        "use mod.elec.*\nlet e = mod.elec\n",
        "let rail = e.net({name: \"RAIL\"})\n",
        "let r = e.r(1kohm, rail, rail, {package: \"0402\"})\n",
    );
    let tail = concat!(
        "e.sheet(table: {\n",
        "    \"r\": {at: [2 3]},\n",
        "    \"r.pins\": {A: @left},\n",
        "    \"res\": {turn: 180deg}, // the later entry wins\n",
        "    \"res.pins\": {A: @right},\n",
        "});\n",
    );
    let (src, ev) = setup(&format!("{source}{tail}"));
    let c = &ev.design.units[0].circuit;
    let renamed = edited(&src, &ev, CircuitOp::Rename {
        what: Sel::Comp(CompRef { board: BoardId(0), comp: comp(c, "r") }), to: "res".into(),
    });
    let got = evaluate(&renamed, &std_libs()).expect("duplicate sheet keys still evaluate");
    assert_eq!(renamed.docs()[0].text.as_ref(), format!("{}{}",
        source.replace("let r =", "let res ="), tail.replace("\"r\"", "\"res\"").replace("\"r.pins\"", "\"res.pins\"")),
        "renaming into stale keys keeps every entry and its order, including named table arguments");
    assert_eq!(got.design.units[0].circuit.hints.parts, vec![("res".into(), c.hints.parts[1].1.clone())],
        "the later path and pin entries keep winning after a key collision");
    assert!(got.notes.is_empty(), "the formerly stale destination becomes a valid path");
    let (text, removed) = after(&renamed, &got, CircuitOp::Remove(comp(&got.design.units[0].circuit, "res")));
    assert_eq!(text, "use mod.elec.*\nlet e = mod.elec\nlet rail = e.net({name: \"RAIL\"})\n",
        "removing the final component's duplicate path and pin entries removes the whole statement and semicolon");
    assert!(removed.design.units[0].circuit.hints.parts.is_empty(),
        "removing the final hinted component leaves no part hints");
}

#[test]
fn sheet_hints_keep_indices_on_a_renamed_loop_binding() {
    let source = concat!(
        "use mod.elec.*\nlet e = mod.elec\n",
        "let rail = e.net({name: \"RAIL\"})\n",
        "for i in 0..2 { let r = e.r(1kohm, rail, rail, {package: \"0402\"}) }\n",
        "sheet({\"r[0]\": {at: [2 3]} \"r[1].pins\": {A: @left}})\n",
    );
    let (src, ev) = setup(source);
    let c = &ev.design.units[0].circuit;
    let (text, got) = after(&src, &ev, CircuitOp::Rename {
        what: Sel::Comp(CompRef { board: BoardId(0), comp: comp(c, "r[0]") }), to: "res".into(),
    });
    assert_eq!(text, source.replace("let r =", "let res =").replace("\"r[", "\"res["),
        "renaming a binding inside a bare loop preserves each index and the pin suffix");
    let mut expected = c.hints.clone();
    expected.parts[0].0 = "res[0]".into();
    expected.parts[1].0 = "res[1]".into();
    assert_eq!(got.design.units[0].circuit.hints, expected,
        "indexed paths retain their original hints after a shared binding rename");
    assert!(got.notes.is_empty(), "renaming an indexed binding leaves no stale-path note");
}

#[test]
fn circuit_edits_leave_computed_and_unmatched_tables_unchanged() {
    let source = concat!(
        "use mod.elec.*\nlet e = mod.elec\n",
        "let rail = e.net({name: \"RAIL\"})\n",
        "let r = e.r(1kohm, rail, rail, {package: \"0402\"})\n",
    );
    for tail in [
        "let table = {\"r\": {at: [1 2]} nets: {RAIL: @label}}\nsheet(table)\n",
        "let entry = {turn: 90deg}\nsheet({\"r\": entry nets: {RAIL: @label}})\n",
        "let point = [1 2]\nsheet({\"r\": {at: point} nets: {RAIL: @label}})\n",
        "sheet({})\n",
        "sheet({\"other\": {turn: 90deg} nets: {OTHER: @label}})\n",
    ] {
        let (src, ev) = setup(&format!("{source}{tail}"));
        let c = &ev.design.units[0].circuit;
        for (operation, expected_source) in [
            (CircuitOp::Rename {
                what: Sel::Comp(CompRef { board: BoardId(0), comp: comp(c, "r") }), to: "res".into(),
            }, source.replace("let r =", "let res =")),
            (CircuitOp::Rename { what: net(c, "RAIL"), to: "POWER".into() },
                source.replace("name: \"RAIL\"", "name: \"POWER\"")),
            (CircuitOp::Remove(comp(c, "r")), source.replace(
                "let r = e.r(1kohm, rail, rail, {package: \"0402\"})\n", "")),
        ] {
            let (text, got) = after(&src, &ev, operation);
            assert_eq!(text, format!("{expected_source}{tail}"),
                "computed or unmatched tables preserve every byte while the circuit edit applies: {tail}");
            assert_eq!(got.design.units[0].circuit.hints, c.hints,
                "computed or unmatched tables retain their evaluated hints: {tail}");
        }
    }
}

#[test]
fn paste_strips_previous_orders_and_marks_makers_in_op_order() {
    let source = r#"let e = mod.elec
let cap = e.c(1uF, opts: {package: "0402" added: 3})
let fixed = e.part(mod.std.tact_switch(), {fields: {added: "keep"} ref: "SW8" added: 7})
let cell = e.block({name: "Cell" ports: {} build: fn(p, o) { let r = e.r(1kohm) }})
"#;
    let mut src = sources(source);
    src.add("panel.ckt.splash", "let lamp = mod.elec.led({color: @green added: 17})\n").expect("another unit holds the highest order");
    let libs = std_libs();
    let ev = evaluate(&src, &libs).expect("both circuit documents evaluate");
    let circuit = &ev.design.units[0].circuit;
    let copy = |name| {
        let part = circuit.comp(name).expect("the source has this part");
        fab_elec_lang::part_template(&ev.map, circuit.unit, part, |id| src.doc(id).map(|d| d.text.as_ref()))
            .expect("a single maker call can be copied").0
    };
    let cap = copy("cap");
    assert_eq!(cap, r#"let cap = e.c(1uF, opts: {package: "0402"})"#, "copy removes added from an automatic part");
    assert_eq!(copy("fixed"), r#"let fixed = e.part(mod.std.tact_switch(), {fields: {added: "keep"}})"#,
        "copy strips the stored ref and addition order together and preserves nested component fields");

    let ops = [
        cap.as_str(),
        "let group = cell({})",
        "let loose = e.c(4pF)",
        "let r = e.r(1kohm, 1%, _n0)",
        "let marked = e.c(2uF, 2%, rest: {added: 4})",
        "let sw = e.part(mod.std.tact_switch())",
        "let indicator = e.led({color: @green})",
    ].map(|stmt| op(CircuitOp::AddPart { stmt: stmt.into(), after: None, ends: vec![], hint: None }));
    let change = splice(&src, &ev.map, &ev.design, &ops).expect("the mixed paste splices");
    src.apply(&change.edits).expect("all statements insert together");
    let expected = [
        r#"let cap2 = e.c(1uF, opts: {package: "0402" added: 18})"#,
        "let group = cell({})",
        "let loose = e.c(4pF)",
        "let r = e.r(1kohm, 1%, e.net(), rest: {added: 19})",
        "let marked = e.c(2uF, 2%, rest: {added: 20})",
        "let sw = e.part(mod.std.tact_switch(), {added: 21})",
        "let indicator = e.led({color: @green added: 22})",
    ];
    assert_eq!(change.written.len(), expected.len(), "every pasted statement has a selection range");
    for (at, stmt) in change.written.iter().zip(expected) {
        let text = &src.doc(at.doc).expect("the selection names an edited document").text;
        assert_eq!(&text[at.span.start as usize..at.span.end as usize], stmt,
            "the selected statement uses the next project order only when its maker has a known options position");
    }
    let got = evaluate(&src, &libs).expect("the edited project evaluates");
    let r = got.design.units[0].circuit.comp("r").expect("the pasted resistor evaluates in the edited unit");
    assert_eq!(r.value.as_ref().and_then(|v| v.tol), Some(fab_elec_circuit::Tol::pct(1.0)),
        "the marker after a tolerance and an omitted pin preserves the positional tolerance");
}

/// A part's body source switch (`SetBody`, EKMODELS): a new override goes at the end of the unit's
/// models document, a written one is rewritten or removed in place, and each result evaluates to
/// the body it names; the map keeps where the override is written and the part's own body.
#[test]
fn set_body_writes_rewrites_and_removes_the_parts_e_body() {
    use fab_elec_parts::BodySource;
    let mut src = sources("let e = mod.elec\nlet r = e.r(1kohm, nil, nil, {package: \"0402\"})\n");
    src.add_asset("parts/lcsc/C25744.glb");
    let name = fab_elec_lang::models_doc_name("ledctl");
    src.add(name.clone(), "let e = mod.elec\n".to_string()).expect("a models document");
    let libs = std_libs();
    let ev = evaluate(&src, &libs).expect("the source evaluates");
    let part = ev.design.units[0].circuit.comps[0].part.name.clone();
    let models = |src: &Sources| src.docs().iter().find(|d| d.name == name).expect("the models document").text.to_string();
    let body = |ev: &Evaluated| ev.design.units[0].circuit.comps[0].part.body.clone().expect("the resistor has a body");
    let set = |src: &Sources, ev: &Evaluated, body: Option<&str>| {
        let splice = splice(src, &ev.map, &ev.design, &[op(CircuitOp::SetBody { part: part.clone(), body: body.map(str::to_string) })])
            .expect("the body splices");
        assert!(splice.reevaluate, "a body edit asks for evaluation");
        let mut src = src.clone();
        src.apply(&splice.edits).expect("the edits apply");
        let ev = evaluate(&src, &libs).unwrap_or_else(|e| panic!("the edited source evaluates: {e:?}\n{}", models(&src)));
        (src, ev)
    };
    let (src, ev) = set(&src, &ev, Some("{mesh: \"parts/lcsc/C25744.glb\"}"));
    assert_eq!(models(&src), format!("let e = mod.elec\ne.body(\"{part}\", {{mesh: \"parts/lcsc/C25744.glb\"}})\n"), "a new override is appended");
    assert_eq!(body(&ev).source, BodySource::Mesh("parts/lcsc/C25744.glb".into()), "the part draws it");
    let site = ev.map.body_site(BoardId(0), &part).expect("the map knows where the override is written");
    assert_eq!(site.own.as_ref().map(|own| &own.source), Some(&BodySource::Package(fab_elec_parts::PackageKind::Chip)), "and the part's own, generated body");
    let (src, ev) = set(&src, &ev, Some("{}"));
    assert_eq!(models(&src), format!("let e = mod.elec\ne.body(\"{part}\", {{}})\n"), "a written override is rewritten in place");
    assert_eq!(body(&ev).source, BodySource::Extrude, "the plain body");
    let (src, ev) = set(&src, &ev, None);
    assert_eq!(models(&src), "let e = mod.elec\n", "removing it removes its line");
    assert!(ev.map.body_site(BoardId(0), &part).is_none(), "and the map forgets it");
}
