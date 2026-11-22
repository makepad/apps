use fab_elec_core::{mm, BoardId, CompRef, Diag, DocId, LinkId, PinRef, Sel, Xform3};
use fab_elec_design::{sysnets, system_erc, Design, Frame, LinkEnd, LinkKind, PinMap, TextEdit};
use fab_elec_lang::{NO_CANCEL, evaluate, evaluate_from, std_libs, sys_doc::{self, SysLedger}, Evaluated, SourceMap, SourceRange, Sources, SpanEdit, UnitState};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

const PAIR: &str = include_str!("../../fixtures/pair.sys.splash");

fn comp_name(d: &Design, c: CompRef) -> String {
    let unit = d.unit(c.board).expect("the component's unit exists");
    format!("{}.{}", unit.name, unit.circuit.comps[c.comp.ix()].path)
}

fn pin_name(d: &Design, p: PinRef) -> String {
    let part = &d.unit(p.board).expect("the pin's unit exists").circuit.comps[p.comp.ix()].part;
    let name = &part.pin_def(p.pin).expect("the pin exists").name;
    let occurrence = part.pins[..p.pin.ix()].iter().filter(|p| p.name == *name).count();
    format!("{}:{name}:{occurrence}", comp_name(d, CompRef { board: p.board, comp: p.comp }))
}

fn end_name(d: &Design, end: LinkEnd) -> String {
    match end { LinkEnd::Comp(c) => comp_name(d, c), LinkEnd::Pin(p) => pin_name(d, p) }
}

fn selection_name(d: &Design, sel: &Sel) -> String {
    match sel {
        Sel::Net(n) => {
            let u = d.unit(n.board).expect("the net's unit exists");
            format!("{}.{}", u.name, u.circuit.nets[n.net.ix()].name)
        }
        Sel::Link(id) => {
            let l = d.link(*id).expect("the selected link exists");
            format!("{:?}:{}:{}", l.kind, end_name(d, l.a), end_name(d, l.b))
        }
        Sel::Unit(id) => d.unit(*id).expect("the selected unit exists").name.clone(),
        Sel::Comp(c) => comp_name(d, *c),
        Sel::Pin(p) => pin_name(d, *p),
        _ => panic!("a system finding must select a named system item: {sel:?}"),
    }
}

fn named_nets(d: &Design) -> Vec<String> {
    let mut out: Vec<_> = sysnets(d).into_iter().map(|n| {
        let mut members: Vec<_> = n.members.iter().map(|n| selection_name(d, &Sel::Net(*n))).collect();
        let mut sources: Vec<_> = n.sources.iter().map(|p| pin_name(d, *p)).collect();
        members.sort();
        sources.sort();
        format!("{members:?}; {:?}; {sources:?}; {:?}", n.aliases, n.volts)
    }).collect();
    out.sort();
    out
}

fn named_violations(d: &Design) -> Vec<String> {
    let mut out: Vec<_> = system_erc(d).into_iter().map(|v| {
        let mut items: Vec<_> = v.items.iter().map(|s| selection_name(d, s)).collect();
        items.sort();
        let unit = v.unit.map(|id| d.unit(id).expect("the finding's unit exists").name.as_str());
        format!("{unit:?}; {:?}; {:?}; {items:?}; {:?}; {:?}; {}; {}; {}",
            v.kind, v.severity, v.actual, v.required, v.rule, v.message, v.fix_hint)
    }).collect();
    out.sort();
    out
}

fn source_span(diag: &Diag, doc: DocId) -> Range<u32> {
    assert_eq!(diag.sel.len(), 1, "a system diagnostic has exactly one source selection");
    let Sel::Source { doc: got, start, end } = diag.sel[0] else { panic!("a system diagnostic must select source text"); };
    assert_eq!(got, doc, "the diagnostic belongs to the system document");
    start..end
}

fn follow(sources: &mut Sources, prev: &mut Evaluated, edit: TextEdit) {
    let edit = SpanEdit { edits: vec![edit], ..SpanEdit::default() };
    sources.apply(&edit.edits).expect("the test edit is in bounds");
    prev.map.follow(&edit);
}

#[test]
fn pair_documents_match_the_typed_system() {
    let sources = fab_elec_fixtures::docs("pair").expect("the pair has documents");
    let ev = evaluate(&sources, &std_libs()).expect("pair documents evaluate");
    println!("pair system.took: {:?}", ev.system.took);
    assert!(ev.units.iter().all(|u| u.state == UnitState::Built), "both document units build: {:?}", ev.units);
    assert!(ev.benches.iter().all(|b| b.state == UnitState::Built), "the controller's benches still build");
    assert_eq!(ev.system.doc, sources.find("pair.sys.splash"), "the evaluated system names its document");
    assert!(ev.system.diags.is_empty(), "pair system has no diagnostics: {:?}", ev.system.diags);
    assert_eq!(ev.design.links.len(), 1, "the pair has one link");
    let l = &ev.design.links[0];
    assert_eq!(l.id, LinkId(0), "the first link takes id zero");
    assert_eq!(l.kind, LinkKind::Mate, "the connectors mate");
    assert_eq!(end_name(&ev.design, l.a), "ledctl.mcu.dbg", "end a names the controller debug header");
    assert_eq!(end_name(&ev.design, l.b), "probe.j", "end b names the probe header");
    assert_eq!(l.pinmap, PinMap::Derived, "the pair derives its pin map");
    let ledctl = ev.design.unit_by_name("ledctl").expect("controller exists");
    let probe = ev.design.unit_by_name("probe").expect("probe exists");
    let mount = ev.design.mount(probe.id).expect("probe has a mount");
    assert_eq!(mount.parent, Frame::Unit(ledctl.id), "the probe is mounted on the controller");
    assert_eq!(mount.xform, Xform3::translation([mm(10.0), 0, mm(-8.0)]), "the probe has the authored offset");
    let rust = fab_elec_fixtures::ledctl_pair();
    assert_eq!(named_nets(&ev.design), named_nets(&rust), "document and Rust pairs have identical named system nets, sources and voltages");
    assert_eq!(named_violations(&ev.design), named_violations(&rust), "document and Rust pairs have identical named system findings");
    assert!(ev.design.check_consistent().is_empty(), "the document pair satisfies the model invariants");
}

#[test]
fn every_literal_form_round_trips() {
    let mut design = fab_elec_fixtures::ledctl_pair();
    let pair = sys_doc::lower(DocId(0), PAIR, &design.units, None);
    assert!(pair.diags.is_empty(), "the canonical pair lowers: {:?}", pair.diags);
    assert_eq!(pair.links, design.links, "the pair reads its links back exactly");
    assert_eq!(pair.mounts, design.assembly.mounts, "the pair reads its mounts back exactly");
    design.links = pair.links;
    design.assembly.mounts = pair.mounts;
    assert_eq!(sys_doc::write(&design), PAIR, "the pair document is a fixed point");

    for (id, name) in [(2, "led-ctl"), (3, "dbg")] {
        let mut unit = design.units[1].clone();
        unit.id = BoardId(id);
        unit.name = name.into();
        let circuit = Arc::make_mut(&mut unit.circuit);
        circuit.unit = unit.id;
        circuit.name = name.into();
        Arc::make_mut(unit.board.as_mut().expect("probe is a board")).unit = unit.id;
        design.units.push(unit);
    }
    // Exercise repeated names independently of which stock parts happen to repeat a name.
    let probe = Arc::make_mut(&mut design.units[1].circuit);
    let j = probe.comp("j").expect("probe header exists").id;
    let part = Arc::make_mut(&mut probe.comps[j.ix()].part);
    part.pins[1].name = "1".into();
    part.pins[2].name = "1".into();
    let text = concat!(
        "let dbg_2 = mate(ledctl.mcu.dbg, probe.j, {pins: [[\"1\", [\"1\", 0]], [\"2\", [\"1\", 1]], [\"3\", [\"1\", 2]], [\"4\", \"4\"], [\"5\", \"5\"]]})\n",
        "let dbg_3 = wire(ledctl.mcu.dbg.pin(\"1\"), probe.j.pin(\"1\", 1))\n",
        "wire(ledctl.lamps[0].d.pin(\"K\"), unit(\"led-ctl\").j.pin(\"5\"))\n",
        "mount(ledctl, {on: body(\"arm\", \"link2\") rot: [10deg, 20deg, 30deg]})\n",
        "mount(probe, {on: dbg_2 rot: 90deg})\n",
        "mount(unit(\"led-ctl\"), {at: [0.000001mm, -0.012345mm, 2mm]})\n",
        "mount(dbg, {on: dbg_3})\n",
    );
    let got = sys_doc::lower(DocId(1), text, &design.units, None);
    assert!(got.diags.is_empty(), "every literal form lowers: {:?}", got.diags);
    assert_eq!(got.links.len(), 3, "all link forms survive");
    let LinkEnd::Pin(pin) = got.links[1].b else { panic!("wire end is a pin"); };
    assert_eq!(pin.pin.ix(), 1, "an occurrence counts only pins with the same name");
    let PinMap::Given(pairs) = &got.links[0].pinmap else { panic!("explicit pairs remain explicit"); };
    assert_eq!(pairs.iter().map(|(_, b)| b.ix()).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4], "pin pairs resolve names and occurrences in part order");
    design.links = got.links;
    design.assembly.mounts = got.mounts;
    assert_eq!(sys_doc::write(&design), text, "every canonical literal form is a fixed point, including collision-free bindings");
    for (i, link) in design.links.iter_mut().enumerate() { link.id = LinkId(40 + i as u32 * 3); }
    for mount in &mut design.assembly.mounts {
        if let Frame::Mate(id) = &mut mount.parent { *id = LinkId(40 + id.0 * 3); }
    }
    let lowered = sys_doc::lower(DocId(2), &sys_doc::write(&design), &design.units, None);
    assert!(lowered.diags.is_empty(), "a model with sparse link ids writes resolvable text");
    for (i, link) in design.links.iter_mut().enumerate() { link.id = LinkId(i as u32); }
    for mount in &mut design.assembly.mounts {
        if let Frame::Mate(id) = &mut mount.parent { *id = LinkId((id.0 - 40) / 3); }
    }
    assert_eq!(lowered.links, design.links, "the model's links survive with statement-order ids");
    assert_eq!(lowered.mounts, design.assembly.mounts, "the model's snapped mounts survive with rebound mate ids");

    let shadowed = "let ledctl = mate(unit(\"ledctl\").mcu.dbg, probe.j)\nmount(probe, {on: ledctl})\n";
    let got = sys_doc::lower(DocId(3), shadowed, &design.units, None);
    assert!(got.diags.is_empty(), "unit(string) bypasses a document binding that shadows a unit");
    assert_eq!(got.mounts[1].parent, Frame::Mate(LinkId(0)), "on resolves the shadowing link binding");
    let bad = shadowed.replace("unit(\"ledctl\").mcu", "ledctl.mcu");
    let got = sys_doc::lower(DocId(3), &bad, &design.units, None);
    assert_eq!(got.diags.len(), 2, "a shadowed bare unit dangles its link and dependent mount");
    let units = [design.units[3].clone(), design.units[1].clone(), design.units[0].clone()];
    let got = sys_doc::lower(DocId(4), "", &units, None);
    assert_eq!(got.mounts.iter().map(|m| m.unit).collect::<Vec<_>>(), vec![BoardId(0), BoardId(1), BoardId(3)], "default mounts are sorted even when units are not");
}

#[test]
fn diagnostics_are_local_and_link_identity_survives_edits() {
    let mut sources = fab_elec_fixtures::docs("pair").expect("pair documents exist");
    let libs = std_libs();
    let mut prev = evaluate(&sources, &libs).expect("the original pair evaluates");
    let doc = sources.find("pair.sys.splash").expect("system document exists");
    let id = prev.design.links[0].id;
    let start = PAIR.find("ledctl.mcu.dbg").expect("end text exists") as u32;
    follow(&mut sources, &mut prev, TextEdit { doc, span: start..start + 14, text: "ledctl.mcu.missing".into() });
    let mut broken = evaluate_from(Some(&prev), &sources, &libs, &NO_CANCEL).expect("a missing end does not fail evaluation");
    assert_eq!(broken.system.diags.len(), 1, "a missing component gives one diagnostic: {:?}", broken.system.diags);
    let span = source_span(&broken.system.diags[0], doc);
    assert_eq!(&sources.doc(doc).expect("document exists").text[span.start as usize..span.end as usize], "ledctl.mcu.missing", "the diagnostic selects the entire missing end");
    assert!(broken.design.links.is_empty(), "the unresolved link is not kept with stale component ids");
    assert_eq!(broken.design.assembly.mounts, prev.design.assembly.mounts, "the independent mount still stands");
    assert!(broken.map.locate(&Sel::Link(id)).is_some(), "an unresolved link keeps its ledger span");
    follow(&mut sources, &mut broken, TextEdit { doc, span, text: "ledctl.mcu.dbg".into() });
    let mut restored = evaluate_from(Some(&broken), &sources, &libs, &NO_CANCEL).expect("the restored end evaluates");
    assert!(restored.system.diags.is_empty(), "repair clears the system diagnostic");
    assert_eq!(restored.design.links[0].id, id, "repair restores the original link id");

    let circuit_doc = sources.find("ledctl.ckt.splash").expect("controller circuit exists");
    let text = &sources.doc(circuit_doc).expect("circuit document exists").text;
    let at = text.find("let usb   =").expect("the first block follows the nets") as u32;
    let before = restored.design.unit_by_name("ledctl").expect("controller exists").circuit.comp("mcu.dbg").expect("header exists").id;
    follow(&mut sources, &mut restored, TextEdit { doc: circuit_doc, span: at..at, text: "let extra = e.r(1kohm, v33, gnd, {package: \"0402\"})\n".into() });
    let added = evaluate_from(Some(&restored), &sources, &libs, &NO_CANCEL).expect("adding an earlier component evaluates");
    assert!(added.units.iter().all(|u| u.state == UnitState::Built), "the changed circuit builds, rather than keeping the old circuit");
    assert!(added.system.diags.is_empty(), "component renumbering does not dangle the system");
    let after = added.design.unit_by_name("ledctl").expect("controller exists").circuit.comp("mcu.dbg").expect("header exists").id;
    assert_ne!(after, before, "the added component really renumbers the header");
    assert_eq!(added.design.links[0].id, id, "component renumbering preserves link identity");
    let LinkEnd::Comp(c) = added.design.links[0].a else { panic!("the header end remains a component"); };
    assert_eq!(c.comp, after, "the end rebinds to the current component id by path");
    assert_eq!(comp_name(&added.design, c), "ledctl.mcu.dbg", "the rebound end still names the debug header");

    let units = &added.design.units;
    for (text, unit) in [
        ("mount(probe, {on: ledctl})\nmount(probe, {at: [1mm, 0mm, 0mm]})\n", BoardId(1)),
        ("mount(ledctl, {on: probe})\nmount(probe, {on: ledctl})\n", BoardId(1)),
        ("mount(probe, {on: probe})\n", BoardId(1)),
        ("let dbg = mate(ledctl.mcu.dbg, probe.j)\nmount(ledctl, {on: dbg})\nmount(probe, {on: ledctl})\n", BoardId(1)),
        ("mount(probe, {on: absent})\n", BoardId(1)),
        ("for x in [0, 1] { mate(ledctl.mcu.dbg, probe.j) }\n", BoardId(0)),
        ("if true { mount(probe, {on: ledctl}) }\n", BoardId(0)),
        ("fn f() { mate(ledctl.mcu.dbg, probe.j) }\n", BoardId(0)),
        ("other(ledctl)\n", BoardId(0)),
        ("let [x] = mate(ledctl.mcu.dbg, probe.j)\n", BoardId(0)),
        ("let x = mount(probe, {on: ledctl})\n", BoardId(0)),
    ] {
        let got = sys_doc::lower(doc, text, units, None);
        assert_eq!(got.diags.len(), 1, "one bad statement produces one diagnostic: {text} {:?}", got.diags);
        let span = source_span(&got.diags[0], doc);
        assert!(span.end > span.start, "the bad statement has a nonempty source location: {text}");
        let mount = got.mounts.iter().find(|m| m.unit == unit).expect("every unit has a mount");
        assert_eq!((&mount.parent, mount.xform), (&Frame::World, Xform3::IDENTITY), "the invalid mount leaves its unit at the origin: {text}");
        if text.starts_with("for ") || text.starts_with("if ") || text.starts_with("fn ") || text.starts_with("other(") || text.starts_with("let [") || text.starts_with("let x = mount") {
            assert_eq!(got.diags[0].message, "the system document takes declarations", "non-declarations have the specified diagnostic");
            assert!(got.links.is_empty(), "nested calls are not evaluated");
        }
    }
    let got = sys_doc::lower(doc, "var ledctl = 0\nmate(ledctl.mcu.dbg, probe.j)\n", units, None);
    assert_eq!(got.diags.len(), 1, "a variable declaration is rejected once and skipped");
    assert_eq!(got.links.len(), 1, "a skipped variable declaration does not shadow a unit");
    let text = "mate(ledctl.missing, probe.j)\nmate(ledctl.mcu.dbg, probe.j)\nmount(probe, {on: ledctl})\n";
    let got = sys_doc::lower(doc, text, units, None);
    assert_eq!(got.diags.len(), 1, "a missing end does not hide later declarations");
    assert_eq!(got.links[0].id, LinkId(1), "an unresolved link consumes its statement-order id");
    let mut map = SourceMap::new();
    map.follow(&SpanEdit {
        adds: got.spans.into_iter().map(|(sel, span)| (sel, SourceRange { doc, span })).collect(),
        ..SpanEdit::default()
    });
    let first = map.locate(&Sel::Link(LinkId(0))).expect("missing link is mapped");
    let delete = SpanEdit { edits: vec![TextEdit { doc, span: 0..first.span.end + 1, text: String::new() }], ..SpanEdit::default() };
    map.follow(&delete);
    assert!(map.locate(&Sel::Link(LinkId(0))).is_none(), "deleting a link statement drops its map entry");
    assert_eq!(map.locate(&Sel::Link(LinkId(1))).expect("surviving link stays mapped").span.start, 0, "the surviving link follows the deleted line");
    let ledger = SysLedger::of_map(&map, doc);
    let remaining = &text[(first.span.end + 1) as usize..];
    let text = format!("{remaining}mate(ledctl.mcu.dbg, probe.j)\n");
    let got = sys_doc::lower(doc, &text, units, Some(&ledger));
    assert_eq!(got.links.iter().map(|l| l.id).collect::<Vec<_>>(), vec![LinkId(1), LinkId(2)], "the ledger preserves starts and allocates above its maximum");
    let missing_unit = sys_doc::lower(doc, PAIR, &units[..1], Some(&SysLedger { links: vec![(0, LinkId(8))] }));
    assert_eq!(missing_unit.diags.len(), 2, "a missing unit dangles its link and its mount independently");
    assert!(missing_unit.links.is_empty(), "a failed unit leaves no link with stale references");
    assert_eq!(missing_unit.spans[0].0, Sel::Link(LinkId(8)), "the link reserves its old identity while the unit is absent");
    let returned = sys_doc::lower(doc, PAIR, units, Some(&SysLedger { links: vec![(0, LinkId(8))] }));
    assert_eq!(returned.links[0].id, LinkId(8), "the returned unit restores the original link identity");
    let malformed = "mate(ledctl.mcu.dbg, probe.j\nmount(probe, {on: ledctl})\n";
    let got = sys_doc::lower(doc, malformed, units, None);
    assert_eq!(got.diags.len(), 1, "one malformed line gives one syntax diagnostic");
    assert_eq!(got.mounts[1].parent, Frame::Unit(BoardId(0)), "a syntax error does not hide the next mount");

    let extra = sources.add("second.sys.splash", "mount(ledctl, {on: probe})\n").expect("second document has a distinct name");
    let got = evaluate_from(Some(&added), &sources, &libs, &NO_CANCEL).expect("a second system document does not fail evaluation");
    assert_eq!(got.system.diags.len(), 1, "a second system document gives exactly one diagnostic");
    source_span(&got.system.diags[0], extra);
    assert_eq!(got.design.links, added.design.links, "the second document does not replace links");
    assert_eq!(got.design.assembly.mounts, added.design.assembly.mounts, "the second document is ignored");
}

#[test]
fn projects_without_a_system_document_keep_the_default_system() {
    let fixture = fab_elec_fixtures::docs("ledctl").expect("controller documents exist");
    let mut sources = Sources::new();
    // Stock projects now include an empty system document; retain coverage of absent documents.
    for doc in fixture.docs().iter().filter(|d| d.kind != fab_elec_lang::DocKind::System) {
        sources.add(doc.name.clone(), doc.text.clone()).expect("controller document names are distinct");
    }
    assert!(sources.docs().iter().all(|d| d.kind != fab_elec_lang::DocKind::System), "this input has no system document");
    let ev = evaluate(&sources, &std_libs()).expect("controller documents evaluate");
    assert!(ev.design.links.is_empty(), "a project without system text has no links");
    assert_eq!(ev.design.assembly.mounts.len(), ev.design.units.len(), "every unit has one default mount");
    for unit in &ev.design.units {
        let mount = ev.design.mount(unit.id).expect("each unit has a mount");
        assert_eq!((&mount.parent, mount.xform), (&Frame::World, Xform3::IDENTITY), "{} is at the world origin", unit.name);
    }
    assert_eq!(ev.system.doc, None, "there is no evaluated system document");
    assert!(ev.system.diags.is_empty(), "an absent system document has no diagnostics");
    assert_eq!(ev.system.took, Duration::ZERO, "an absent system document has zero measured time");
    assert_eq!(sys_doc::write(&ev.design), "", "a default system writes no text");
}
