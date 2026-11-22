//! Circuit renames follow into literal link ends; removal leaves a located dangling link.

use fab_elec_circuit::CircuitOp;
use fab_elec_core::{CompRef, DocId, LinkId, NetRef, Sel};
use fab_elec_design::{DesignOp, LinkEnd, TextEdit};
use fab_elec_lang::{NO_CANCEL, check_system, evaluate, evaluate_from, splice, std_libs, sys_doc, Evaluated, Sources, SpanEdit, UnitState};

const PAIR: &str = include_str!("../../fixtures/pair.sys.splash");

fn text(sources: &Sources, doc: DocId) -> &str {
    &sources.doc(doc).expect("the fixture document exists").text
}

fn replace(sources: &mut Sources, name: &str, replacement: &str) {
    let doc = sources.find(name).expect("the fixture document exists");
    sources.apply(&[TextEdit { doc, span: 0..text(sources, doc).len() as u32, text: replacement.into() }])
        .expect("the test setup replaces a whole document");
}

fn rename_sources(system: &str) -> Sources {
    let mut sources = fab_elec_fixtures::docs("pair").expect("the pair has documents");
    replace(&mut sources, "pair.sys.splash", system);
    sources
}

fn built(ev: &Evaluated) {
    assert!(ev.units.iter().all(|u| u.state == UnitState::Built),
        "every circuit rebuilds instead of retaining an old unit: {:?}", ev.units);
    assert!(ev.system.diags.is_empty(), "all system ends still resolve: {:?}", ev.system.diags);
}

fn undo(sources: &Sources, prev: &Evaluated, edited: &Sources, change: &SpanEdit) {
    let inverse = change.inverse(sources, &prev.map);
    let mut restored = edited.clone();
    restored.apply(&inverse.edits).expect("the inverse edits apply across documents");
    for doc in sources.docs() {
        assert_eq!(text(&restored, doc.id), doc.text.as_ref(), "the inverse restores every byte of {}", doc.name);
    }
    let mut map = prev.map.clone();
    map.follow(change);
    map.follow(&inverse);
    assert_eq!(map.of_doc(sources.find("pair.sys.splash").expect("system document exists")),
        prev.map.of_doc(sources.find("pair.sys.splash").expect("system document exists")),
        "the inverse restores all system statement ranges and identities");
}

#[test]
fn component_rename_changes_only_identifiers_and_keeps_authored_bindings() {
    let libs = std_libs();
    let bound = concat!(
        "let dbg = mate(unit(\"ledctl\").mcu /* dbg stays in this comment */\n    .dbg, probe.j)\n",
        "wire(probe.j.pin(\"1\"), ledctl.mcu.dbg.pin(\"1\", 0))\n",
        "mount(probe, {on: dbg at: [10mm, 0mm, -8mm]})\n",
    );
    for system in [PAIR, bound] {
        let sources = rename_sources(system);
        let prev = evaluate(&sources, &libs).expect("the original pair evaluates");
        built(&prev);
        let unit = prev.design.unit_by_name("ledctl").expect("the controller exists");
        let comp = unit.circuit.comp("mcu.dbg").expect("the mated component exists").id;
        let change = splice(&sources, &prev.map, &prev.design, &[DesignOp::Circuit {
            unit: unit.id,
            op: CircuitOp::Rename { what: Sel::Comp(CompRef { board: unit.id, comp }), to: "debug_port".into() },
        }]).expect("renaming the mated component splices");
        assert!(change.reevaluate, "a component rename requests re-evaluation");
        let circuit = sources.find("ledctl.ckt.splash").expect("the circuit document exists");
        let doc = sources.find("pair.sys.splash").expect("the system document exists");
        let board = sources.find("ledctl.pcb.splash").expect("the board document exists");
        let circuit_edits: Vec<_> = change.edits.iter().filter(|e| e.doc == circuit).collect();
        let system_edits: Vec<_> = change.edits.iter().filter(|e| e.doc == doc).collect();
        let board_edits: Vec<_> = change.edits.iter().filter(|e| e.doc == board).collect();
        assert_eq!(circuit_edits.len(), 2, "the circuit changes only the dbg binding and e.side use");
        assert_eq!(system_edits.len(), prev.design.links.len(), "each link has exactly one renamed end token");
        assert_eq!(board_edits.len(), 1, "the board changes only the component's placement path");
        assert_eq!((&text(&sources, board)[board_edits[0].span.start as usize..board_edits[0].span.end as usize], board_edits[0].text.as_str()),
            ("\"mcu.dbg\"", "\"mcu.debug_port\""), "the placement path token follows the rename");
        assert_eq!(change.edits.len(), circuit_edits.len() + system_edits.len() + board_edits.len(), "no other document is edited");
        for edit in circuit_edits.iter().chain(&system_edits) {
            assert_eq!(&text(&sources, edit.doc)[edit.span.start as usize..edit.span.end as usize], "dbg",
                "each edit replaces just the old identifier token");
            assert_eq!(edit.text, "debug_port", "each edit writes just the new identifier token");
        }
        let mut edited = sources.clone();
        edited.apply(&change.edits).expect("the circuit and system edits apply together");
        assert_eq!(text(&edited, circuit), text(&sources, circuit).replace("let dbg    =", "let debug_port    =")
            .replace("e.side(dbg,", "e.side(debug_port,"), "every other circuit byte stays unchanged");
        assert_eq!(text(&edited, doc), system.replace(".dbg", ".debug_port"),
            "unit references, bindings, comments, pin arguments and mounts stay byte-identical");
        let mut followed = prev.clone();
        followed.map.follow(&change);
        let got = evaluate_from(Some(&followed), &edited, &libs, &NO_CANCEL).expect("the renamed pair re-evaluates");
        built(&got);
        let renamed = got.design.unit(unit.id).expect("the controller survives").circuit.comp("mcu.debug_port")
            .expect("the renamed component exists");
        let link = got.design.link(LinkId(0)).expect("the first link retains id zero");
        assert_eq!(link.a, LinkEnd::Comp(CompRef { board: unit.id, comp: renamed.id }),
            "the first link resolves to the renamed component's current id");
        assert_eq!(got.design.assembly.mounts, prev.design.assembly.mounts, "rename preserves the mount and its parent");
        if system == bound {
            let link = got.design.link(LinkId(1)).expect("the second link keeps its id");
            let LinkEnd::Pin(pin) = link.b else { panic!("the second link's renamed end remains a pin"); };
            assert_eq!((pin.board, pin.comp), (unit.id, renamed.id), "end b also follows the renamed component");
            assert_eq!(pin.pin.ix(), 0, "the pin name and occurrence still select the first pin");
            assert!(text(&edited, doc).starts_with("let dbg = mate("), "the authored link binding keeps the old name");
            assert!(sys_doc::write(&got.design).starts_with("let debug_port = mate("),
                "the canonical writer derives a new binding name, unlike the authored splice");
        }
        undo(&sources, &prev, &edited, &change);

        let net = unit.circuit.nets.iter().find(|n| n.name == "BTN").expect("the button net exists").id;
        let change = splice(&sources, &prev.map, &prev.design, &[DesignOp::Circuit {
            unit: unit.id,
            op: CircuitOp::Rename { what: Sel::Net(NetRef { board: unit.id, net }), to: "BUTTON".into() },
        }]).expect("a net rename still splices");
        assert!(change.edits.iter().all(|e| e.doc == circuit), "net renames do not edit component link paths");
    }
}

#[test]
fn block_and_indexed_paths_keep_every_other_segment() {
    let libs = std_libs();
    let indexed = concat!(
        "let ledctl = mate(unit(\"ledctl\").mcu.dbg, probe.j)\n",
        "wire(unit(\"ledctl\").lamps[0].d.pin(\"A\"), unit(\"ledctl\").lamps[7].d.pin(\"K\"))\n",
        "wire(probe.led.d.pin(\"A\"), probe.led.d.pin(\"K\"))\n",
        "mount(probe, {on: ledctl at: [10mm, 0mm, -8mm]})\n",
    );
    for (system, path, to) in [(PAIR, "mcu", "control"), (indexed, "lamps[0].d", "diode")] {
        let sources = rename_sources(system);
        let prev = evaluate(&sources, &libs).expect("the original pair evaluates");
        built(&prev);
        let unit = prev.design.unit_by_name("ledctl").expect("the controller exists");
        let what = if path == "mcu" {
            Sel::Block(unit.id, unit.circuit.tree.iter().find(|b| b.path == path).expect("the mated block instance exists").id)
        } else {
            Sel::Comp(CompRef { board: unit.id, comp: unit.circuit.comp(path).expect("the indexed component exists").id })
        };
        let change = splice(&sources, &prev.map, &prev.design, &[DesignOp::Circuit {
            unit: unit.id, op: CircuitOp::Rename { what, to: to.into() },
        }]).expect("the block instance or shared component binding can be renamed");
        assert!(change.reevaluate, "renaming a path segment requests re-evaluation");
        let doc = sources.find("pair.sys.splash").expect("the system document exists");
        let edits: Vec<_> = change.edits.iter().filter(|e| e.doc == doc).collect();
        let (old, expected) = if path == "mcu" {
            assert_eq!(edits.len(), 1, "the block rename changes one system token");
            ("mcu", system.replace(".mcu.", ".control."))
        } else {
            assert_eq!(edits.len(), 2, "the shared binding rename changes both indexed ends");
            ("d", system.replace("].d.pin", "].diode.pin"))
        };
        for edit in edits {
            assert_eq!(&text(&sources, doc)[edit.span.start as usize..edit.span.end as usize], old,
                "the edit selects only the renamed path segment, excluding any index");
            assert_eq!(edit.text, to, "the replacement is only the new identifier");
        }
        let mut edited = sources.clone();
        edited.apply(&change.edits).expect("the path edits apply together");
        assert_eq!(text(&edited, doc), expected, "unit spelling, other segments, indices, pin suffixes and other units stay unchanged");
        let mut followed = prev.clone();
        followed.map.follow(&change);
        let got = evaluate_from(Some(&followed), &edited, &libs, &NO_CANCEL).expect("the renamed paths re-evaluate");
        built(&got);
        assert_eq!(got.design.links.iter().map(|l| l.id).collect::<Vec<_>>(),
            prev.design.links.iter().map(|l| l.id).collect::<Vec<_>>(), "all links retain their statement identities");
        let circuit = &got.design.unit(unit.id).expect("the controller survives").circuit;
        if path == "mcu" {
            let comp = circuit.comp("control.dbg").expect("the component follows its renamed block");
            assert_eq!(got.design.link(LinkId(0)).expect("the mate survives").a,
                LinkEnd::Comp(CompRef { board: unit.id, comp: comp.id }), "the mate follows the renamed block instance");
        } else {
            let link = got.design.link(LinkId(1)).expect("the indexed wire survives");
            for (end, path, name) in [(link.a, "lamps[0].diode", "A"), (link.b, "lamps[7].diode", "K")] {
                let LinkEnd::Pin(pin) = end else { panic!("the indexed wire retains a pin end"); };
                let comp = circuit.comp(path).expect("the indexed renamed component exists");
                assert_eq!((pin.board, pin.comp), (unit.id, comp.id), "the indexed pin end resolves to {path}");
                assert_eq!(comp.part.pin_def(pin.pin).expect("the pin exists").name, name, "{path} retains its pin name");
            }
            assert_eq!(got.design.link(LinkId(2)), prev.design.link(LinkId(2)), "another unit's identically named component stays unchanged");
        }
        undo(&sources, &prev, &edited, &change);
    }
}

#[test]
fn removal_leaves_the_link_text_and_mount_until_the_component_returns() {
    let sources = fab_elec_fixtures::docs("pair").expect("the pair has documents");
    let libs = std_libs();
    let prev = evaluate(&sources, &libs).expect("the original pair evaluates");
    built(&prev);
    // Unlike dbg's e.side use, the probe header has no remaining circuit uses after Remove.
    let unit = prev.design.unit_by_name("probe").expect("the probe exists");
    let comp = unit.circuit.comp("j").expect("the mated header exists").id;
    let change = splice(&sources, &prev.map, &prev.design, &[DesignOp::Circuit {
        unit: unit.id, op: CircuitOp::Remove(comp),
    }]).expect("removing a mated component is permitted");
    assert!(change.reevaluate, "removing a component requests re-evaluation");
    let circuit = sources.find("probe.ckt.splash").expect("the probe circuit exists");
    let doc = sources.find("pair.sys.splash").expect("the system document exists");
    assert_eq!(change.edits.len(), 1, "removal deletes only the circuit statement");
    assert_eq!(change.edits[0].doc, circuit, "the deletion belongs to the probe circuit");
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("the removal applies");
    assert_eq!(text(&edited, doc), PAIR, "removal leaves the entire system document unchanged");
    let mut followed = prev.clone();
    followed.map.follow(&change);
    let mut got = evaluate_from(Some(&followed), &edited, &libs, &NO_CANCEL).expect("a dangling link does not fail evaluation");
    assert!(got.units.iter().all(|u| u.state == UnitState::Built), "the circuit rebuilds after removal: {:?}", got.units);
    assert!(got.design.unit(unit.id).expect("the probe survives").circuit.comp("j").is_none(), "the header is actually removed");
    assert!(got.design.links.is_empty(), "the dangling link keeps no stale component id");
    assert_eq!(got.system.diags.len(), 1, "only the dangling link reports a diagnostic: {:?}", got.system.diags);
    assert_eq!(check_system(&edited, &got.design, &got.map), Ok(()), "a dangling link is text the model shares, not a mismatch for the pre-save check");
    assert_eq!(got.system.diags[0].sel.len(), 1, "the dangling link has one source location");
    let Sel::Source { doc: at, start, end } = got.system.diags[0].sel[0] else { panic!("the dangling link diagnostic is source-located"); };
    assert_eq!(at, doc, "the diagnostic selects the system document");
    assert_eq!(&text(&edited, doc)[start as usize..end as usize], "probe.j", "the diagnostic selects the missing end within the link statement");
    assert_eq!(got.design.assembly.mounts, prev.design.assembly.mounts, "the independent mount still stands");
    assert_eq!(got.map.locate(&Sel::Link(LinkId(0))), prev.map.locate(&Sel::Link(LinkId(0))),
        "the dangling statement keeps its link id in the ledger");
    let inverse = change.inverse(&sources, &prev.map);
    edited.apply(&inverse.edits).expect("the inverse restores the circuit text");
    got.map.follow(&inverse);
    let restored = evaluate_from(Some(&got), &edited, &libs, &NO_CANCEL).expect("the restored header re-evaluates");
    built(&restored);
    assert_eq!(restored.design.links, prev.design.links, "restoring the component restores the original link and id");
    assert_eq!(restored.design.assembly.mounts, prev.design.assembly.mounts, "restoring the header leaves the mount unchanged");
    for doc in sources.docs() {
        assert_eq!(text(&edited, doc.id), doc.text.as_ref(), "restoration preserves every byte of {}", doc.name);
    }
}
