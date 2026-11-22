//! The probe flags' rules (named connections): which pin a flag anchors at (its `at`, else its
//! label), where it sits at its stub, how its size follows the zoom and where a carried flag lands;
//! and ledavr's flags on its sheet and its board.

use fab_elec_circuit::SheetHints;
use fab_elec_explore::{layout, rows::SheetRows, Opts};
use fab_elec_core::NetId;
use fab_elec_view::board::nodes::{net_pads, pad_anchor, probe_anchor};
use fab_elec_view::canvas::Tokens;
use fab_elec_view::explorer::liveix::SceneLive;
use fab_elec_view::explorer::scene::{Scene, SceneState};
use fab_elec_view::live::flags::{anchor_pin, flag_scale, margin_callouts, nearest_pin, Anchor, PinSpot, SCALE_MAX, SCALE_MIN};
use fab_elec_view::live::probe::{place_probe_op, rename_keeps, rename_probe_op};
use fab_elec_view::snapshot::ViewAction;
use makepad_widgets_core::{dvec2, Rect};

#[test]
fn a_flag_anchors_at_the_pin_it_names_else_a_small_part() {
    let spots = [
        PinSpot { refdes: "U1", number: "10", part_pins: 32 },
        PinSpot { refdes: "D1", number: "1", part_pins: 2 },
        PinSpot { refdes: "R4", number: "2", part_pins: 2 },
        PinSpot { refdes: "U1", number: "12", part_pins: 32 },
    ];
    assert_eq!(anchor_pin("U1.12", &spots), Some(3), "a <refdes>.<pin> label anchors at that pin");
    assert_eq!(anchor_pin("U1.12\nPD6", &spots), Some(3), "a pin label with the pin's name on its second line too");
    assert_eq!(anchor_pin("R4 PD6", &spots), Some(2), "a label naming a part anchors at that part's pin");
    assert_eq!(anchor_pin("LED0", &spots), Some(1), "a net's name anchors at its smallest part's pin, the first such");
    assert_eq!(anchor_pin("U1.99", &spots), Some(0), "a pin the net does not have: the named part's own pin on the net");
    assert_eq!(anchor_pin("anything", &[]), None, "no pins, no pin anchor");
    assert_eq!(spots[2].name(), "R4.2", "a pin is named as a probe's at names it");
    assert_eq!(nearest_pin([[0.0, 0.0], [10.0, 0.0], [10.0, 4.0]], [9.0, 3.0]), Some(2), "a carried flag lands at the nearest pin");
    assert_eq!(nearest_pin([], [0.0, 0.0]), None, "no pins, nowhere to land");

    let out = |anchor: Anchor| anchor.out.map(|v| (v * 1e9).round() / 1e9);
    assert_eq!(out(Anchor::pin([10.0, 5.0], [3.0, 0.0], None)), [1.0, 0.0], "a pin points its own way, as a unit vector");
    assert_eq!(out(Anchor::pin([10.0, 5.0], [0.0, 0.0], Some([0.0, 0.0, 10.0, 10.0]))), [1.0, 0.0],
        "a pin of no length points away from its part's centre");
}

/// The stub runs out of the pin its own way and the flag sits at its end, centred on it; flags of
/// close pins stagger; the flag grows with zoom sublinearly between a floor and a ceiling.
/// On the board a probe's callout sits in the margin outside the board, never over it: beyond the
/// side nearest its pad, stacked along it so callouts never meet; with no room anywhere it shows
/// only on hover (None). Its ring grows with zoom sublinearly.
#[test]
fn a_board_callout_sits_in_the_margin_beside_its_side_stacks_and_grows_with_zoom_sublinearly() {
    let clip = Rect { pos: dvec2(0.0, 0.0), size: dvec2(600.0, 400.0) };
    let board = Rect { pos: dvec2(150.0, 100.0), size: dvec2(300.0, 200.0) };
    let size = dvec2(84.0, 26.0);
    // three pads near the top edge, close together; one near the bottom
    let points = [(dvec2(200.0, 110.0), size), (dvec2(210.0, 112.0), size), (dvec2(220.0, 115.0), size), (dvec2(300.0, 290.0), size)];
    let placed = margin_callouts(&points, board, clip, &[]);
    let meets = |a: &Rect, b: &Rect| a.pos.x < b.pos.x + b.size.x && b.pos.x < a.pos.x + a.size.x && a.pos.y < b.pos.y + b.size.y && b.pos.y < a.pos.y + a.size.y;
    for (i, rect) in placed.iter().enumerate() {
        let rect = rect.unwrap_or_else(|| panic!("callout {i} finds room in the margin"));
        assert!(!meets(&rect, &board), "callout {i} stays off the board: {rect:?}");
        assert!(rect.pos.x >= clip.pos.x && rect.pos.y >= clip.pos.y && rect.pos.x + rect.size.x <= clip.size.x && rect.pos.y + rect.size.y <= clip.size.y, "callout {i} stays in the pane: {rect:?}");
        for (j, other) in placed.iter().enumerate().skip(i + 1) {
            assert!(!meets(&rect, &other.unwrap()), "callouts {i} and {j} never meet");
        }
    }
    assert!(placed[..3].iter().all(|r| r.unwrap().pos.y + size.y <= board.pos.y), "pads near the top edge get callouts above the board");
    assert!(placed[3].unwrap().pos.y >= board.pos.y + board.size.y, "a pad near the bottom edge gets one below it");
    let (a, b) = (placed[0].unwrap(), placed[1].unwrap());
    assert!(a.pos.x < b.pos.x, "stacked along the edge in the order of their pads");
    // a board that fills the pane leaves no margin: every callout shows only on hover
    let full = Rect { pos: dvec2(-10.0, -10.0), size: dvec2(620.0, 420.0) };
    assert!(margin_callouts(&points, full, clip, &[]).iter().all(Option::is_none), "no margin: callouts on hover only");
    // a float in the margin keeps callouts off it
    let strip = Rect { pos: dvec2(0.0, 0.0), size: dvec2(600.0, 100.0) };
    assert!(margin_callouts(&points[..1], board, clip, &[strip])[0].is_none_or(|r| !meets(&r, &strip)), "a keep-out rectangle is never covered");

    assert_eq!(flag_scale(10.0, 10.0), 1.0, "at the pane's reference zoom a flag is its own size");
    assert!((flag_scale(14.4, 10.0) - 1.2).abs() < 1e-12, "1.44 times the zoom is 1.2 times the size: the square root");
    assert_eq!(flag_scale(40.0, 10.0), SCALE_MAX, "far in, a flag stops at its ceiling");
    assert_eq!(flag_scale(0.1, 10.0), SCALE_MIN, "far out, a flag keeps its readable floor");
    assert_eq!(flag_scale(0.0, 10.0), 1.0, "no zoom yet: its own size");
}

/// ledavr's chase bench probes the eight LED nets ("D1 PD6": LED D1 on PD6) and the button: each
/// flag anchors at a pin of the part its label names, pointing out of it; a probe's `at` moves its
/// flag to the pin it names, and a carried flag's landing writes that `at` (one undo step) unless it
/// is there already.
#[test]
fn ledavr_flags_anchor_at_their_parts_and_follow_their_at() {
    let sources = fab_elec_fixtures::docs("ledavr").expect("ledavr's documents");
    let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("ledavr evaluates");
    let design = &evaluated.design;
    let unit = design.units.iter().position(|unit| unit.circuit.net("LED0").is_some()).expect("ledavr's unit with the LEDs");
    let circuit = &design.units[unit].circuit;
    let sheet = layout(design, unit, &SheetHints::default(), &Opts::default());
    let rows = SheetRows::new().sync(circuit);
    let scene = Scene::build(design, unit, &sheet, &rows, Tokens::for_theme(true), &SceneState::default(), &SceneLive::default());
    let chase = evaluated.benches.iter().find(|bench| bench.name == "chase").expect("ledavr's chase bench");
    let mut pins = Vec::new();
    let mut flags = 0;
    for probe in &chase.probes {
        let net = circuit.nets.iter().position(|net| net.name == probe.of.target).expect("a probed net is on the sheet") as u32;
        let anchor = scene.probe_anchor(circuit, net, &probe.label, 0.0, &mut pins)
            .unwrap_or_else(|| panic!("{}: a probe on the sheet has a flag", probe.label));
        let pin = pins.iter().find(|pin| pin.at == anchor.at).unwrap_or_else(|| panic!("{}: the flag anchors at a pin", probe.label));
        let part = &circuit.comps[pin.node as usize].refdes;
        if let Some(named) = probe.label.split_whitespace().next().filter(|word| word.starts_with('D')) {
            assert_eq!(part, named, "{}: the flag anchors at the LED its label names", probe.label);
        }
        let (dx, dy) = (anchor.at[0] - (pin.body[0] + pin.body[2]) * 0.5, anchor.at[1] - (pin.body[1] + pin.body[3]) * 0.5);
        assert!(dx * anchor.out[0] + dy * anchor.out[1] > 0.0, "{}: the flag points out of {part}", probe.label);
        flags += 1;
    }
    assert_eq!(flags, 9, "every chase probe has a flag: eight LEDs and the button");

    // D1's net (LED0) also reaches the MCU: an `at` naming that pin moves the flag there.
    let probe = chase.probes.iter().find(|probe| probe.label.starts_with("D1")).expect("D1's probe");
    let net = circuit.nets.iter().position(|net| net.name == probe.of.target).expect("D1's net") as u32;
    let led = scene.probe_anchor(circuit, net, &probe.label, 0.0, &mut pins).expect("D1's flag");
    let (other, name) = {
        let other = pins.iter().find(|pin| pin.at != led.at).expect("LED0 has a pin besides D1's");
        let comp = &circuit.comps[other.node as usize];
        (other.at, format!("{}.{}", comp.refdes, comp.part.pins[other.pin as usize].pads[0]))
    };
    let moved = scene.probe_anchor(circuit, net, &name, 0.0, &mut pins).expect("a placed flag");
    assert_eq!(moved.at, other, "{name}: a flag placed at a pin anchors there");
    let Some(ViewAction::Ops { ops, label, .. }) = place_probe_op("chase", &chase.probes, &probe.of.target, &name) else {
        panic!("landing at another pin places the flag");
    };
    assert_eq!(label, format!("Move probe D1 PD6 to {name}"), "one undo step named for the move");
    let change = fab_elec_lang::splice(&sources, &evaluated.map, design, &ops).expect("the place splices");
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("the place's edit applies");
    let reloaded = fab_elec_lang::evaluate(&edited, &fab_elec_lang::std_libs()).expect("ledavr evaluates from the edited text");
    let chase = reloaded.benches.iter().find(|bench| bench.name == "chase").expect("the chase bench");
    let placed = chase.probes.iter().find(|p| p.of == probe.of).expect("D1's probe after the move");
    assert_eq!((placed.at.as_deref(), placed.label.as_str()), (Some(name.as_str()), "D1 PD6"), "the place is saved with the probe, its name kept");
    assert_eq!(place_probe_op("chase", &chase.probes, &probe.of.target, &name), None, "landing where it is writes nothing");

    // Renaming a lane whose name placed its flag keeps the flag where it is: the same step pins
    // the pin as its `at`.
    let d2 = chase.probes.iter().find(|p| p.label.starts_with("D2")).expect("D2's probe");
    let keep = rename_keeps(&reloaded.design, d2, "Row two").expect("D2's name places its flag: a rename keeps it");
    assert!(keep.starts_with("D2."), "{keep}: the pin is D2's");
    assert_eq!(rename_keeps(&reloaded.design, d2, "D2 again"), None, "a name naming the same place pins nothing");
    assert_eq!(rename_keeps(&reloaded.design, placed, "Row one"), None, "a placed flag stays by its at");
    let Some(ViewAction::Ops { ops, .. }) = rename_probe_op("chase", &d2.of, &d2.label, "Row two", Some(&keep)) else {
        panic!("a new name renames the probe");
    };
    let change = fab_elec_lang::splice(&edited, &reloaded.map, &reloaded.design, &ops).expect("the rename and its pin splice in one step");
    let mut renamed = edited.clone();
    renamed.apply(&change.edits).expect("the rename's edits apply");
    let after = fab_elec_lang::evaluate(&renamed, &fab_elec_lang::std_libs()).expect("ledavr evaluates after the rename");
    let chase = after.benches.iter().find(|bench| bench.name == "chase").expect("the chase bench");
    let d2 = chase.probes.iter().find(|p| p.of == d2.of).expect("D2's probe after the rename");
    assert_eq!((d2.label.as_str(), d2.at.as_deref()), ("Row two", Some(keep.as_str())), "renamed, its flag pinned where it stood");
}

/// ledavr's chase probes on its board: each flag anchors at a pad of its net (an LED's at the pad of
/// the LED its label names), pointing out of its part; the pad names a carried flag can land on are
/// the pin names a probe's `at` writes, and the same `at` anchors the flag at that pad.
#[test]
fn ledavr_board_flags_anchor_at_their_pads() {
    let mut sources = fab_elec_fixtures::docs("ledavr").expect("ledavr's documents");
    sources.add("ledavr.pcb.splash", include_str!("../../fixtures/ledavr.pcb.splash")).expect("ledavr's board document");
    let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("ledavr evaluates");
    let board = evaluated.design.units.iter().find_map(|unit| unit.board.clone().filter(|_| unit.circuit.net("LED0").is_some()))
        .expect("ledavr's board with the LEDs");
    let chase = evaluated.benches.iter().find(|bench| bench.name == "chase").expect("ledavr's chase bench");
    let mut flags = 0;
    for probe in &chase.probes {
        let net = NetId(board.nets.iter().position(|net| net.name == probe.of.target).expect("a probed net is on the board") as u32);
        let anchor = probe_anchor(&board, net, &probe.label)
            .unwrap_or_else(|| panic!("{}: a probe on the board has a flag", probe.label));
        if let Some(named) = probe.label.split_whitespace().next().filter(|word| word.starts_with('D')) {
            let led = board.placed.iter().find(|placed| placed.refdes == named).expect("the LED a label names is placed");
            assert!((0..led.footprint.pads.len()).any(|pad| pad_anchor(led, pad) == Some(anchor)),
                "{}: the flag anchors at a pad of {named}", probe.label);
        }
        let body = anchor.body.expect("a pad's flag knows its part");
        let (dx, dy) = (anchor.at[0] - (body[0] + body[2]) * 0.5, anchor.at[1] - (body[1] + body[3]) * 0.5);
        assert!(dx * anchor.out[0] + dy * anchor.out[1] > 0.0, "{}: the flag points out of its part", probe.label);
        let pads = net_pads(&board, net);
        assert!(pads.iter().any(|(_, a)| *a == anchor), "{}: its pad is one a carry can land on", probe.label);
        for (name, pad) in &pads {
            assert_eq!(probe_anchor(&board, net, name).as_ref(), Some(pad), "{}: an at of {name} anchors at that pad", probe.label);
        }
        flags += 1;
    }
    assert_eq!(flags, 9, "every chase probe has a flag on the board: eight LEDs and the button");
}
