//! Circuit renames follow board names without replacing their owning statements.

use fab_elec_circuit::CircuitOp;
use fab_elec_core::{CompRef, NetRef, Sel};
use fab_elec_design::{DesignOp, TextEdit};
use fab_elec_fixtures::docs;
use fab_elec_lang::{NO_CANCEL, evaluate, evaluate_from, splice, splice::SpanEdit, std_libs, Evaluated, Sources, UnitState};

const CIRCUIT: &str = "ledctl.ckt.splash";
const BOARD: &str = "ledctl.pcb.splash";

fn text<'a>(sources: &'a Sources, name: &str) -> &'a str {
    &sources.doc(sources.find(name).expect("the fixture document exists")).expect("the document is loaded").text
}

fn set_text(sources: &mut Sources, name: &str, text: String) {
    let doc = sources.doc(sources.find(name).expect("the fixture document exists")).expect("the document is loaded");
    sources.apply(&[TextEdit { doc: doc.id, span: 0..doc.text.len() as u32, text }]).expect("the fixture extension applies");
}

fn renamed(sources: &Sources, prev: &Evaluated, what: Sel, to: &str) -> (Sources, Evaluated, SpanEdit) {
    let change = splice(sources, &prev.map, &prev.design, &[DesignOp::Circuit {
        unit: prev.units[0].id, op: CircuitOp::Rename { what, to: to.into() },
    }]).expect("the circuit rename splices");
    assert!(change.reevaluate, "a rename still requires evaluation");
    let board_doc = sources.find(BOARD).expect("the fixture has a board document");
    let edits: Vec<_> = change.edits.iter().filter(|e| e.doc == board_doc).collect();
    assert!(edits.windows(2).all(|pair| pair[0].span.end <= pair[1].span.start),
        "board name edits are sorted and disjoint");
    for edit in edits {
        let before = &text(sources, BOARD)[edit.span.start as usize..edit.span.end as usize];
        assert!(before.starts_with('"') && before.ends_with('"') && edit.text.starts_with('"') && edit.text.ends_with('"'),
            "a board edit replaces a whole string token: {before:?} -> {:?}", edit.text);
    }
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("the circuit and board edits apply together");
    let mut followed = prev.clone();
    followed.map.follow(&change);
    let next = evaluate_from(Some(&followed), &edited, &std_libs(), &NO_CANCEL).expect("the renamed documents evaluate");
    assert_eq!(next.units[0].state, UnitState::Built, "the renamed unit builds instead of keeping its old build");
    assert!(!next.units[0].reused, "the renamed unit is rebuilt from its changed documents");
    let before = prev.design.units[0].board.as_ref().expect("the fixture has a board");
    let after = next.design.units[0].board.as_ref().expect("the renamed unit retains its board");
    assert_eq!(after.placed.iter().map(|p| (p.id, p.at, p.rot, p.side)).collect::<Vec<_>>(),
        before.placed.iter().map(|p| (p.id, p.at, p.rot, p.side)).collect::<Vec<_>>(),
        "every placement keeps its item id, position, rotation and side");
    assert_eq!(after.next_item, before.next_item, "renaming allocates no new board item ids");
    let items = |ev: &Evaluated| {
        let mut items: Vec<_> = ev.map.of_doc(board_doc).into_iter().filter(|(sel, _)| matches!(sel, Sel::Item(..))).collect();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        items
    };
    assert_eq!(items(&next), items(&followed), "every board item keeps its id at its followed statement start");
    let inverse = change.inverse(sources, &prev.map);
    let mut restored = edited.clone();
    restored.apply(&inverse.edits).expect("the inverse circuit and board edits apply");
    for doc in sources.docs() {
        assert_eq!(restored.doc(doc.id).expect("the restored document exists").text, doc.text,
            "the inverse restores every byte of {}", doc.name);
    }
    followed.map.follow(&inverse);
    assert_eq!(items(&followed), items(prev), "the inverse restores every board item's source range");
    (edited, next, change)
}

#[test]
fn component_rename_changes_one_board_token_and_preserves_placements() {
    let mut sources = docs("ledctl").expect("the controller fixture exists");
    let circuit = text(&sources, CIRCUIT).to_string() + "let extra = e.net({name: \"mcu.dbg.extra\"})\n";
    set_text(&mut sources, CIRCUIT, circuit);
    let via = "via(\"mcu.dbg.extra\", [1mm, 1mm], {drill: 0.3mm pad: 0.6mm})\n";
    let board = text(&sources, BOARD).to_string() + via;
    set_text(&mut sources, BOARD, board);
    let prev = evaluate(&sources, &std_libs()).expect("the fixture evaluates");
    let unit = &prev.design.units[0];
    let comp = unit.circuit.comp("mcu.dbg").expect("the debug header exists").id;
    let (edited, next, change) = renamed(&sources, &prev, Sel::Comp(CompRef { board: unit.id, comp }), "debug_header");
    let board_doc = sources.find(BOARD).expect("the fixture has a board document");
    let board_edits: Vec<_> = change.edits.iter().filter(|e| e.doc == board_doc).collect();
    assert_eq!(board_edits.len(), 1, "renaming dbg changes exactly one board token");
    assert_eq!(&text(&sources, BOARD)[board_edits[0].span.start as usize..board_edits[0].span.end as usize],
        "\"mcu.dbg\"", "the sole board edit targets the old component path");
    assert_eq!(board_edits[0].text, "\"mcu.debug_header\"", "the sole board edit writes the new component path");
    let mut expected = sources.clone();
    set_text(&mut expected, BOARD, text(&sources, BOARD).replace("\"mcu.dbg\"", "\"mcu.debug_header\""));
    set_text(&mut expected, CIRCUIT, text(&sources, CIRCUIT)
        .replace("let dbg    =", "let debug_header    =").replace("e.side(dbg,", "e.side(debug_header,"));
    assert_eq!(edited, expected, "only the circuit binding, its use and the placement token change");
    assert!(next.design.units[0].board.as_ref().expect("the board survives").placed.iter().any(|p| p.path == "mcu.debug_header"),
        "the rebuilt board places the renamed component");
    assert!(text(&edited, BOARD).contains(via), "the via keeps its document-given net name mcu.dbg.extra");
    assert!(next.design.units[0].circuit.net("mcu.dbg.extra").is_some(),
        "the document-given net keeps its name after its matching component prefix is renamed");
}

#[test]
fn block_rename_follows_indexed_paths_pads_origins_and_derived_nets() {
    let mut sources = docs("ledctl").expect("the controller fixture exists");
    let circuit = text(&sources, CIRCUIT).replace("let d = e.led(", "let local = e.net()\n        let signals = e.bus(\"LOCAL\", [@READY])\n        let d = e.led(")
        + "let signal = e.net({name: \"lamps[0].signal\"})\n";
    set_text(&mut sources, CIRCUIT, circuit.clone());
    let board = text(&sources, BOARD)
        .replace("[\"lamps[0].r:2\", \"lamps[0].d:2\"])",
            "[\"lamps[0].r:2\", arc([28mm, 24mm], \"lamps[0].d:2\")], {block: \"lamps[0]\"})")
        + "via(\"lamps[0].local\", [1mm, 1mm], {drill: 0.3mm pad: 0.6mm})\n"
        + "via(\"lamps[0].READY\", [2mm, 1mm], {drill: 0.3mm pad: 0.6mm})\n";
    let untouched = concat!(
        "// \"lamps[0].d\" is a comment, not a path site\n",
        "text(\"lamps[0].d\", \"F.SilkS\", [1mm, 2mm], {size: 1mm thick: 0.1mm})\n",
        "hole(\"lamps[0].r:2\", [1mm, 3mm], 1mm)\n",
        "zone(\"VBUS\", [\"B.Cu\"], @board, {name: \"lamps[0].d.A\"})\n",
        "via(\"lamps[0].signal\", [3mm, 1mm], {drill: 0.3mm pad: 0.6mm})\n",
    );
    set_text(&mut sources, BOARD, format!("{board}{untouched}"));
    let prev = evaluate(&sources, &std_libs()).expect("the extended fixture evaluates");
    let unit = &prev.design.units[0];
    let block = unit.circuit.tree.iter().find(|b| b.path == "lamps[0]").expect("the indexed block exists").id;
    for (name, kind) in [("lamps[0].d.A", "pin net"), ("lamps[0].local", "bound net"), ("lamps[0].READY", "bus member")] {
        let net = unit.circuit.net(name).unwrap_or_else(|| panic!("the {kind} {name} exists before renaming"));
        assert_eq!(net.scope, Some(block), "the {kind} {name} records its containing block lamps[0]");
    }
    for name in ["GND", "lamps[0].signal"] {
        let net = unit.circuit.net(name).unwrap_or_else(|| panic!("the document-given net {name} exists before renaming"));
        assert_eq!(net.scope, None, "the document-given name {name} has no scope that follows a path rename");
    }
    let (edited, next, change) = renamed(&sources, &prev, Sel::Block(unit.id, block), "indicators");
    let board_doc = sources.find(BOARD).expect("the fixture has a board document");
    assert_eq!(change.edits.iter().filter(|e| e.doc == board_doc).count(), 43,
        "exactly 43 board tokens follow: sixteen placements, sixteen pads, eight track nets, one origin and two local nets");
    let mut expected = sources.clone();
    set_text(&mut expected, BOARD, board.replace("\"lamps[", "\"indicators[") + untouched);
    set_text(&mut expected, CIRCUIT, circuit.replace("let lamps =", "let indicators =")
        .replace("e.group(\"leds\", lamps)", "e.group(\"leds\", indicators)"));
    assert_eq!(edited, expected, "only the collected binding, its use and typed board names change");
    let circuit = &next.design.units[0].circuit;
    for i in 0..8 {
        assert!(circuit.comp(&format!("indicators[{i}].d")).is_some(), "placement path {i} keeps its loop index");
        assert!(circuit.net(&format!("indicators[{i}].d.A")).is_some(), "pin-derived net {i} follows its component path");
    }
    assert!(circuit.net("indicators[0].local").is_some(), "an unnamed bound net follows its block prefix");
    assert!(circuit.net("indicators[0].READY").is_some(), "a bus member follows its block prefix");
    assert!(circuit.net("GND").is_some() && circuit.net("VBUS").is_some(), "explicit top-level net names stay unchanged");
    assert!(circuit.net("lamps[0].signal").is_some(), "the document-given net keeps its name after its matching block prefix is renamed");
    assert!(text(&edited, BOARD).contains("via(\"lamps[0].signal\","), "the via keeps its document-given net name lamps[0].signal");
}

#[test]
fn net_rename_follows_every_net_site_and_leaves_other_strings_alone() {
    for (case, declaration, old, to, new) in [
        ("explicit", "let gnd  = e.net({name: \"GND\"  kind: @ground})", "GND", "return \"rail\"", "return \"rail\""),
        ("bound", "let gnd  = e.net({kind: @ground})", "gnd", "return_rail", "return_rail"),
        ("indirect", "let ground_name = \"GND\"\nlet gnd  = e.net({name: ground_name kind: @ground})", "GND", "return_handle", "GND"),
    ] {
        let mut sources = docs("ledctl").expect("the controller fixture exists");
        let circuit = text(&sources, CIRCUIT).replace("let gnd  = e.net({name: \"GND\"  kind: @ground})", declaration);
        set_text(&mut sources, CIRCUIT, circuit);
        let board = (text(&sources, BOARD).to_string()
            + "let host = track(\"GND\", \"F.Cu\", 0.25mm, [[1mm, 1mm], [3mm, 1mm]])\n"
            + "hole(\"contact\", [3mm, 1mm], 0.4mm, {pad: 0.8mm net: \"GND\" plated: true})\n"
            + "tuning(\"GND\", skew(\"GND\", 0.1mm), {pair: \"GND\" host: [host] amp: [0.2mm, 0.8mm] pitch: 0.5mm})\n")
            .replace("\"GND\"", &format!("\"{old}\""));
        let untouched = concat!(
            "// \"GND\" remains in this comment\n",
            "text(\"GND\", \"F.SilkS\", [1mm, 2mm], {size: 1mm thick: 0.1mm})\n",
            "zone(\"VBUS\", [\"B.Cu\"], @board, {name: \"GND\"})\n",
            "zone(nil, [\"F.Cu\"], @board)\n",
            "hole(\"GND\", [1mm, 3mm], 1mm, {net: nil})\n",
        );
        set_text(&mut sources, BOARD, format!("{board}{untouched}"));
        let prev = evaluate(&sources, &std_libs()).unwrap_or_else(|errors| panic!("{case}: the net fixture evaluates: {errors:?}"));
        let unit = &prev.design.units[0];
        let net = unit.circuit.net(old).unwrap_or_else(|| panic!("{case}: the original net exists")).id;
        let (edited, next, change) = renamed(&sources, &prev, Sel::Net(NetRef { board: unit.id, net }), to);
        let board_doc = sources.find(BOARD).expect("the fixture has a board document");
        assert_eq!(change.edits.iter().filter(|e| e.doc == board_doc).count(), if old == new { 0 } else { 8 },
            "{case}: both vias, the zone, track, hole and all three tuning net sites follow a changed net name");
        let quoted = format!("\"{}\"", new.replace('"', "\\\""));
        assert_eq!(text(&edited, BOARD), board.replace(&format!("\"{old}\""), &quoted) + untouched,
            "{case}: other nets, labels, comments, names and nil net options stay byte for byte unchanged");
        assert!(next.design.units[0].circuit.net(new).is_some(), "{case}: the rebuilt circuit has the requested net name");
        assert!(next.design.units[0].circuit.net("VBUS").is_some(), "{case}: another explicit net stays unchanged");
    }
}
