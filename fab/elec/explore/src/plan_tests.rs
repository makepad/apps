use super::{delete, delete_parts, mirror_part, mirror_parts, plan, turn_part, turn_parts, Gesture, Planned};
use crate::edit::{Target, Toward};
use crate::sheet::{Frame, Mover, PartPlace, SheetLayout, TagKind};
use fab_elec_circuit::{Circuit, CircuitOp, Flow, NetStyle, PortSide, SheetHint};
use fab_elec_core::{CompId, NetId, NetRef, PinIx, SchPt, Sel};
use fab_elec_design::{fixture, DesignOp, Resolved};

fn pin((comp, pin): (CompId, PinIx)) -> Target {
    Target::Pin { comp, pin, toward: Toward::Across }
}

fn end((comp, pin): (CompId, PinIx), net: NetId) -> Target {
    Target::WireEnd { comp, pin, net }
}

fn wire(net: NetId) -> Target {
    Target::Wire { net, run: 0, seg: 0, at: [0, 0] }
}

fn tag(net: NetId) -> Target {
    Target::Tag { net, kind: TagKind::Label }
}

fn nothing(chip: &str) -> Planned {
    Planned::Nothing { chip: chip.into() }
}

fn edit<const N: usize>(circuit: &Circuit, label: &str, ops: [CircuitOp; N]) -> Planned {
    Planned::Edit(Resolved {
        ops: ops.into_iter().map(|op| DesignOp::Circuit { unit: circuit.unit, op }).collect(),
        place: vec![],
        label: label.into(),
    })
}

#[test]
fn selection_keys_turn_the_mover_and_delete_the_selected_part() {
    let mut unit = fixture::probe::probe();
    let c = std::sync::Arc::make_mut(&mut unit.circuit);
    let head = c.comp("led.r").expect("selection setup: resistor").id;
    let member = c.comp("led.d").expect("selection setup: diode").id;
    let block = c.comps[head.ix()].block;
    let mut sheet = SheetLayout {
        parts: vec![PartPlace::default(); c.comps.len()],
        movers: vec![Mover::Itself; c.comps.len()],
        frames: vec![Frame::default(); c.tree.len()],
        ..SheetLayout::default()
    };
    sheet.parts[head.ix()] = PartPlace { placed: true, at: [17, 26], turn: 1, mirror: true, block: block.0, ..PartPlace::default() };
    sheet.frames[block.ix()] = Frame { placed: true, origin: [10, 30], ..Frame::default() };
    sheet.movers[member.ix()] = Mover::Head(head.0);
    let hinted = |hint| CircuitOp::Hint { path: "led.r".into(), hint: Some(hint) };
    assert_eq!(turn_part(c, &sheet, member, false), edit(c, "Turn R1", [hinted(SheetHint {
        at: Some(SchPt::new(7, 4)), turn: 0, mirror: true, ..SheetHint::default()
    })]), "turning an unhinted chain member pins its head at the drawn block-local point and turns from its drawn angle");
    assert_eq!(mirror_part(c, &sheet, member, false), edit(c, "Mirror R1", [hinted(SheetHint {
        at: Some(SchPt::new(7, 4)), turn: 3, mirror: false, ..SheetHint::default()
    })]), "X on an unhinted chain member mirrors its head left-right in place from its drawn orientation \
        (at an odd turn, the part's own vertical flip and a half turn)");

    let hint = SheetHint { at: Some(SchPt::new(4, -2)), flow: Some(Flow::Down), mirror: true,
        pins: vec![("1".into(), PortSide::Top)], ..SheetHint::default() };
    c.hints.parts.push(("led.r".into(), hint.clone()));
    for (from, clockwise, counter_clockwise) in [(0, 3, 1), (1, 0, 2), (2, 1, 3), (3, 2, 0)] {
        c.hints.parts.last_mut().unwrap().1.turn = from;
        sheet.parts[head.ix()].turn = from;
        for (shift, turn) in [(false, clockwise), (true, counter_clockwise)] {
            assert_eq!(turn_part(c, &sheet, head, shift),
                edit(c, "Turn R1", [hinted(SheetHint { turn, ..hint.clone() })]),
                "R from {from} with shift={shift} changes only the hint's turn by one quarter");
        }
    }
    sheet.parts[head.ix()].turn = 0;
    for mirror in [true, false] {
        c.hints.parts.last_mut().unwrap().1 = SheetHint { turn: 3, mirror, ..hint.clone() };
        for (vertical, label, turn) in [(false, "Mirror R1", 1), (true, "Flip R1", 3)] {
            assert_eq!(mirror_part(c, &sheet, head, vertical),
                edit(c, label, [hinted(SheetHint { mirror: !mirror, turn, ..hint.clone() })]),
                "{label} from turn 3, mirror={mirror} reflects on the sheet's own axis (a hint mirrors before it turns), \
                uses the latest hint and preserves its placement, flow and pin sides");
        }
    }
    sheet.movers[member.ix()] = Mover::Rides { host: head.0, port: 0 };
    assert_eq!(turn_part(c, &sheet, member, false), nothing("D1 turns with R1"),
        "a pin rider names the part it turns with and writes no hint");
    for vertical in [false, true] {
        assert_eq!(mirror_part(c, &sheet, member, vertical), nothing("D1 mirrors with R1"),
            "mirroring a pin rider on either axis names its host and writes no hint");
    }

    let pin_id = (head, c.comps[head.ix()].part.pin("1").expect("selection setup: resistor pin"));
    let net = c.net_of(pin_id.0, pin_id.1).expect("selection setup: connected resistor pin");
    let disconnect = edit(c, "Disconnect R1.1 from 3V3", [CircuitOp::Disconnect { pin: pin_id }]);
    for target in [pin(pin_id), end(pin_id, net)] {
        assert_eq!(delete(c, target, Some(member)), disconnect, "a pin or end disconnects before Delete considers the selection");
    }
    c.comps[head.ix()].conns.retain(|&(pin, _)| pin != pin_id.1);
    assert_eq!(delete(c, pin(pin_id), Some(member)), nothing(""), "a free pin protects the selection from Delete");
    for target in [wire(net), Target::None] {
        assert_eq!(delete(c, target, Some(member)), edit(c, "Delete D1", [CircuitOp::Remove(member)]),
            "away from a pin or end, Delete removes the selected part itself in one step");
        assert_eq!(delete(c, target, None), nothing(""), "without a selection or endpoint, Delete does nothing");
    }
    assert_eq!(delete_parts(c, &[member, head, member]),
        edit(c, "Delete 2 parts", [CircuitOp::Remove(head), CircuitOp::Remove(member)]),
        "group Delete plans one undo step removing each selected part once");
    assert_eq!(delete_parts(c, &[member]), edit(c, "Delete D1", [CircuitOp::Remove(member)]),
        "a one-part selection keeps the part's name in history");
    assert_eq!(delete_parts(c, &[]), nothing(""), "an empty selection creates no undo step");
    assert_eq!(delete_parts(c, &[CompId(u32::MAX), head]), nothing(""),
        "a missing component cancels the group deletion before any part is removed");
}

#[test]
fn group_orientation_carries_each_pin_about_one_grid_point() {
    let mut unit = fixture::probe::probe();
    let c = std::sync::Arc::make_mut(&mut unit.circuit);
    let selected = ["j", "led.r", "led.d"].map(|path| c.comp(path).expect("probe selection").id);
    let [header, resistor, diode] = selected;
    let mut sheet = SheetLayout {
        parts: vec![PartPlace::default(); c.comps.len()],
        movers: vec![Mover::Itself; c.comps.len()],
        frames: vec![Frame { placed: true, origin: [30, 60], ..Frame::default() }; c.tree.len()],
        ..SheetLayout::default()
    };
    for (id, x, y, turn, mirror) in [(header, 2, 9, 1, true), (resistor, 9, -3, 2, false), (diode, -4, 4, 3, false)] {
        let comp = &c.comps[id.ix()];
        sheet.parts[id.ix()] = PartPlace {
            placed: true, at: [30 + x, 60 - y], turn, mirror, block: comp.block.0, ..PartPlace::default()
        };
        c.hints.parts.push((comp.path.clone(), SheetHint {
            at: Some(SchPt::new(x, y)), turn, mirror, ..SheetHint::default()
        }));
    }
    assert_eq!(turn_parts(c, &sheet, &selected, false), nothing("turn parts of one block"),
        "turning the probe's root header with its LED block makes no edit");
    for (vertical, note) in [(false, "mirror parts of one block"), (true, "flip parts of one block")] {
        assert_eq!(mirror_parts(c, &sheet, &selected, vertical), nothing(note),
            "a reflection across blocks names the requested operation and makes no edit");
    }

    // Place the header beside the two LED parts. The bounding-box centre is (2.5, 3) in hint space.
    let block = c.comps[resistor.ix()].block;
    c.comps[header.ix()].block = block;
    sheet.parts[header.ix()].block = block.0;
    c.comps[header.ix()].path = "led.j".into();
    c.hints.parts[0].0 = "led.j".into();
    c.hints.parts[0].1.flow = Some(Flow::Down);
    c.hints.parts[0].1.pins.push(("1".into(), PortSide::Top));
    let original = c.hints.parts.clone();
    let clockwise = turn_parts(c, &sheet, &selected, false);
    c.hints.parts[0].1.at = None;
    c.hints.parts.truncate(2);
    assert_eq!(turn_parts(c, &sheet, &selected, false), clockwise,
        "a style-only hint and an unhinted part use their drawn block-local positions");
    c.hints.parts.clone_from(&original);

    let pose = |p: SchPt, turn, mirror| {
        let [x, y] = crate::wiretree::rot([p.x, p.y], turn, mirror);
        SchPt::new(x, y)
    };
    let pin_at = |hint: &SheetHint, offset| {
        let at = hint.at.expect("a placed part has a grid point");
        SchPt::new(at.x, -at.y).add(pose(offset, hint.turn, hint.mirror))
    };
    let centre = SchPt::new(2, -3); // Pins use Y up.
    for (label, quarters, reflect, repeats) in [
        ("Turn", 3, false, 4), ("Turn", 1, false, 4), ("Mirror", 0, true, 2), ("Flip", 2, true, 2),
    ] {
        for _ in 0..repeats {
            let planned = if reflect { mirror_parts(c, &sheet, &selected, quarters == 2) }
                else { turn_parts(c, &sheet, &selected, quarters == 1) };
            let Planned::Edit(edit) = planned else { panic!("{label} must produce one group edit") };
            assert_eq!(edit.label, format!("{label} 3 parts"), "history names the operation and the selection size");
            assert_eq!(edit.ops.len(), 3, "the edit has one hint for each independent part");
            for op in edit.ops {
                let DesignOp::Circuit { op: CircuitOp::Hint { path, hint: Some(hint) }, .. } = op
                    else { panic!("a group orientation only writes part hints") };
                let i = original.iter().position(|(p, _)| p == &path).expect("the hint belongs to a selected part");
                for pin in &c.comps[selected[i].ix()].part.symbol.units[0].pins {
                    let before = pin_at(&c.hints.parts[i].1, pin.at);
                    assert_eq!(pin_at(&hint, pin.at), centre.add(pose(before.sub(centre), quarters, reflect)),
                        "{label} carries {path} pin {:?} rigidly about the snapped centre", pin.pin);
                }
                c.hints.parts[i].1 = hint;
            }
        }
        assert_eq!(c.hints.parts, original, "{repeats} applications of {label} restore every source hint");
    }

    sheet.movers[resistor.ix()] = Mover::Head(header.0);
    sheet.movers[diode.ix()] = Mover::Rides { host: resistor.0, port: 0 };
    assert_eq!(turn_parts(c, &sheet, &[diode, resistor], false), edit(c, "Turn 2 parts", [CircuitOp::Hint {
        path: c.comps[header.ix()].path.clone(), hint: Some(SheetHint { turn: 0, ..original[0].1.clone() }),
    }]), "a rider and its chain member turn their shared head once about its own position");
}

#[test]
fn naming_a_net_follows_the_wire_or_connected_pin() {
    let mut unit = fixture::probe::probe();
    let c = std::sync::Arc::make_mut(&mut unit.circuit);
    let header = c.comp("j").expect("label setup: probe header");
    let comp = header.id;
    let data = (comp, header.part.pin("2").expect("label setup: data pin"));
    let free = (comp, header.part.pin("3").expect("label setup: pin to disconnect"));
    let net = c.net("SWDIO").expect("label setup: SWDIO").id;
    c.comps[comp.ix()].conns.retain(|&(pin, _)| pin != free.1);
    let name = |target, to: &str| plan(c, &Gesture::NameNet { target, to: to.into() });
    let expected = edit(c, "Name the net DATA", [CircuitOp::Rename {
        what: Sel::Net(NetRef { board: c.unit, net }), to: "DATA".into(),
    }]);
    for target in [wire(net), pin(data), end(data, NetId(u32::MAX))] {
        assert_eq!(name(target, "DATA"), expected, "a wire, pin or wire end renames its connected net");
        assert_eq!(name(target, "SWDIO"), nothing(""), "the current name leaves the source alone");
    }
    assert_eq!(name(pin(free), "DATA"), nothing("wire the pin first: a label names a net"),
        "a free pin explains why it cannot take a label");
    for target in [Target::None, Target::Canvas { block: 0, cell: [0, 0] }, tag(net), wire(NetId(u32::MAX))] {
        assert_eq!(name(target, "DATA"), nothing(""), "a label needs a pin or an existing wire");
    }
}

#[test]
fn a_global_label_ties_a_free_pin_by_name_and_flags_signal_nets() {
    let mut unit = fixture::probe::probe();
    let c = std::sync::Arc::make_mut(&mut unit.circuit);
    let header = c.comp("j").expect("global setup: probe header");
    let comp = header.id;
    let data = (comp, header.part.pin("2").expect("global setup: data pin"));
    let free = (comp, header.part.pin("3").expect("global setup: pin to disconnect"));
    let swdio = c.net("SWDIO").expect("global setup: SWDIO").id;
    c.comps[comp.ix()].conns.retain(|&(pin, _)| pin != free.1);
    let tie = |name: &str| CircuitOp::ConnectNamed { pin: free, name: name.into() };
    let flag = |name: &str| CircuitOp::NetStyle { name: name.into(), style: Some(NetStyle::Label) };
    let global = |c: &Circuit, target, name: &str| plan(c, &Gesture::Global { target, name: name.into() });
    assert_eq!(global(c, pin(free), " VREF "), edit(c, "Tie J1.3 to VREF", [tie("VREF"), flag("VREF")]),
        "a new name ties the free pin and draws the new signal net as flags");
    assert_eq!(global(c, pin(free), "GND"), edit(c, "Tie J1.3 to GND", [tie("GND")]),
        "a rail's name joins the rail, which keeps its own tags");
    assert_eq!(global(c, pin(data), "SWDIO"), edit(c, "Show SWDIO as labels", [flag("SWDIO")]),
        "a pin already on that net only changes how the net draws");
    assert!(matches!(global(c, end(data, swdio), "VREF"), Planned::Nothing { chip } if chip.contains("is on SWDIO")),
        "a connected pin is not moved off its net by a label");
    assert_eq!(global(c, pin(free), "  "), nothing("enter a net name"), "a blank name is asked for");
    for target in [Target::None, wire(swdio), tag(swdio)] {
        assert_eq!(global(c, target, "VREF"), nothing(""), "a global label goes on a pin");
    }
    c.hints.nets.push(("SWDIO".into(), NetStyle::Label));
    assert_eq!(global(c, pin(free), "SWDIO"), edit(c, "Tie J1.3 to SWDIO", [tie("SWDIO")]),
        "a net already drawn as flags is joined without a second style entry");
    assert_eq!(global(c, pin(data), "SWDIO"), nothing(""), "nothing is left to do on a flagged net's pin");
}

#[test]
fn naming_a_wire_end_uses_the_start_pins_connection() {
    let mut unit = fixture::probe::probe();
    let c = std::sync::Arc::make_mut(&mut unit.circuit);
    let header = c.comp("j").expect("wire name setup: probe header");
    let [on, free, loose] = ["2", "1", "4"].map(|name| (header.id, header.part.pin(name).expect("probe pin")));
    let block = header.block;
    let [swdio, swclk, nrst] = ["SWDIO", "SWCLK", "NRST"].map(|name| c.net(name).expect("probe net").id);
    c.comps[free.0.ix()].conns.retain(|&(pin, _)| pin != free.1);
    c.nets[nrst.ix()].scope = Some(block);
    c.hints.nets.push(("SWCLK".into(), NetStyle::Label));
    let name_end = |from, name: &str| plan(c, &Gesture::NameEnd { from, name: name.into() });
    for from in [pin(free), pin(loose), pin(on), end(on, swdio)] {
        assert_eq!(name_end(from, "  "), nothing(""), "a blank wire-end name closes silently");
    }
    for (p, label) in [(free, "J1.1"), (loose, "J1.4")] {
        for name in ["DATA", "SWDIO", "SWCLK", "GND"] {
            let tie = CircuitOp::ConnectNamed { pin: p, name: name.into() };
            let label = format!("Tie {label} to {name}");
            let expected = if matches!(name, "SWCLK" | "GND") { edit(c, &label, [tie]) } else {
                edit(c, &label, [tie, CircuitOp::NetStyle { name: name.into(), style: Some(NetStyle::Label) }])
            };
            assert_eq!(name_end(pin(p), name), expected, "{label} uses the global tie and flags only unflagged signals");
        }
    }
    assert_eq!(name_end(end(loose, nrst), "NRST"), nothing(""), "a loose pin's own name adds no flag");
    for from in [pin(on), end(on, swdio)] {
        assert_eq!(name_end(from, " SWDIO "), nothing(""), "the connected pin's own name makes no edit");
        assert_eq!(name_end(from, " DATA "), edit(c, "Name the net DATA", [CircuitOp::Rename {
            what: Sel::Net(NetRef { board: c.unit, net: swdio }), to: "DATA".into(),
        }]), "a new name renames the connected net from either end");
    }
    assert_eq!(name_end(pin(on), "SWCLK"), Planned::Refused {
        chip: "SWDIO and SWCLK are two nets: join them in the text".into(),
        sites: [swdio, swclk].map(|net| Sel::Net(NetRef { board: c.unit, net })).to_vec(),
    }, "naming another net from a connected pin refuses with both net sites");
    assert_eq!(name_end(end(on, swdio), "SWCLK"), edit(c, "Move J1.2 from SWDIO to SWCLK", [
        CircuitOp::Disconnect { pin: on }, CircuitOp::Connect { pin: on, net: swclk },
    ]), "naming another net from a wire end moves just that pin's connection");
}

#[test]
fn wiring_plans_follow_the_design_table() {
    // The Rust probe uses the shared fixture parts without evaluating any documents.
    let probe = fixture::probe::probe();
    let mut c = (*probe.circuit).clone();
    let pin_id = |path, name| {
        let comp = c.comp(path).expect("rules 1-18 setup: fixture component");
        (comp.id, comp.part.pin(name).expect("rules 1-18 setup: fixture pin"))
    };
    let a = pin_id("j", "1");
    let b = pin_id("led.r", "1");
    let la = pin_id("j", "3");
    let lb = pin_id("led.r", "2");
    let p = pin_id("j", "2");
    let q = pin_id("j", "4");
    let same = pin_id("led.d", "A");
    let port = pin_id("led.d", "K");
    let spare = pin_id("j", "5");
    let n = c.net("SWDIO").expect("rules 1-18 setup: SWDIO").id;
    let m = c.net("SWCLK").expect("rules 1-18 setup: SWCLK").id;
    let nl = c.net("NRST").expect("rules 1-18 setup: first loose net").id;
    let ml = c.net("led.A").expect("rules 1-18 setup: second loose net").id;
    let bound = c.net("GND").expect("rules 1-18 setup: port net").id;
    for comp in &mut c.comps {
        comp.conns.clear();
        comp.nc.clear();
    }
    for (id, net) in [(p, n), (q, m), (same, n), (la, nl), (lb, ml), (port, bound)] {
        c.comps[id.0.ix()].conns.push((id.1, net));
    }
    c.comps[b.0.ix()].nc.push(b.1);
    for (net, id, name) in [(nl, la, "loose_a"), (ml, lb, "loose_b"), (bound, port, "GND")] {
        c.nets[net.ix()].name = name.into();
        c.nets[net.ix()].scope = Some(c.comps[id.0.ix()].block);
    }
    let block = c.comps[port.0.ix()].block;
    c.tree[block.ix()].ports.push(("ground".into(), vec![bound]));
    let before = c.clone();
    let drag = |from, to| plan(&c, &Gesture::Wire { from, to });
    let canvas = Target::Canvas { block: 0, cell: [3, 4] };

    for id in [a, la, p] {
        for to in [pin(id), end(id, m), Target::Pin { comp: id.0, pin: id.1, toward: Toward::Up }] {
            assert_eq!(drag(pin(id), to), nothing(""), "rule 1: the same pin changes nothing in every state");
        }
    }
    for to in [Target::None, canvas] {
        for from in [a, la, p] {
            assert_eq!(drag(pin(from), to), nothing("release on a pin, wire or tag"),
                "rule 2: releasing a new wire on empty space asks for a target");
        }
        assert_eq!(drag(end(p, n), to), edit(&c, "Disconnect J1.2 from SWDIO", [CircuitOp::Disconnect { pin: p }]),
            "rule 9: an empty drop disconnects the carried pin");
        assert_eq!(drag(end(la, nl), to), edit(&c, "Disconnect J1.3 from loose_a", [CircuitOp::Disconnect { pin: la }]),
            "rule 9: an empty drop also disconnects a loose pin");
    }

    for to in [pin(b), end(b, n)] {
        assert_eq!(drag(pin(p), to), edit(&c, "Wire R1.1 to SWDIO", [CircuitOp::Connect { pin: b, net: n }]),
            "rule 3: ON to FREE connects the free pin without an explicit mark clear");
        assert_eq!(drag(pin(a), to), edit(&c, "Wire J1.1 to R1.1", [CircuitOp::ConnectPins { a, b }]),
            "rule 5: FREE to FREE emits ConnectPins, even across blocks and with a marked pin");
        assert_eq!(drag(pin(la), to), edit(&c, "Wire J1.3 to R1.1", [CircuitOp::Connect { pin: b, net: nl }]),
            "rule 5: LOOSE to FREE reuses the loose source's net");
        assert_eq!(drag(end(p, n), to), edit(&c, "Move SWDIO from J1.2 to R1.1", [
            CircuitOp::Disconnect { pin: p }, CircuitOp::Connect { pin: b, net: n },
        ]), "rule 10: a carried connection moves to a free pin");
        assert_eq!(drag(end(la, nl), to), edit(&c, "Move loose_a from J1.3 to R1.1", [
            CircuitOp::Disconnect { pin: la }, CircuitOp::Connect { pin: b, net: nl },
        ]), "rule 10: a loose source's connection moves to a free pin");
    }
    for to in [pin(lb), end(lb, ml), wire(ml), tag(ml)] {
        assert_eq!(drag(pin(p), to), edit(&c, "Wire R1.2 to SWDIO", [
            CircuitOp::Disconnect { pin: lb }, CircuitOp::Connect { pin: lb, net: n },
        ]), "rule 3: ON to LOOSE disconnects the destination first, including its wire and tag");
        assert_eq!(drag(pin(a), to), edit(&c, "Wire J1.1 to R1.2", [CircuitOp::Connect { pin: a, net: ml }]),
            "rule 5: FREE to LOOSE reuses the destination's net, including its wire and tag");
        assert_eq!(drag(pin(la), to), edit(&c, "Wire J1.3 to R1.2", [
            CircuitOp::Disconnect { pin: lb }, CircuitOp::Connect { pin: lb, net: nl },
        ]), "rule 5: LOOSE to LOOSE preserves the source net, including a destination wire or tag");
        assert_eq!(drag(end(p, n), to), edit(&c, "Move SWDIO from J1.2 to R1.2", [
            CircuitOp::Disconnect { pin: p }, CircuitOp::Disconnect { pin: lb }, CircuitOp::Connect { pin: lb, net: n },
        ]), "rule 10: moving onto a loose pin disconnects both pins, including its wire and tag");
        assert_eq!(drag(end(la, nl), to), edit(&c, "Move loose_a from J1.3 to R1.2", [
            CircuitOp::Disconnect { pin: la }, CircuitOp::Disconnect { pin: lb }, CircuitOp::Connect { pin: lb, net: nl },
        ]), "rule 10: a loose source also moves its connection onto a loose destination");
    }
    for to in [pin(same), end(same, m), wire(n), tag(n)] {
        assert_eq!(drag(pin(a), to), edit(&c, "Wire J1.1 to SWDIO", [CircuitOp::Connect { pin: a, net: n }]),
            "rule 4: FREE connects to an ON pin, wire or tag");
        assert_eq!(drag(pin(b), to), edit(&c, "Wire R1.1 to SWDIO", [CircuitOp::Connect { pin: b, net: n }]),
            "rule 4: connecting a marked pin relies on Connect to clear its mark");
        assert_eq!(drag(pin(la), to), edit(&c, "Wire J1.3 to SWDIO", [
            CircuitOp::Disconnect { pin: la }, CircuitOp::Connect { pin: la, net: n },
        ]), "rule 4: LOOSE disconnects before connecting to an ON pin, wire or tag");
        assert_eq!(drag(pin(p), to), nothing("J1.2 is already on SWDIO"),
            "rule 6: a new wire onto the same net changes nothing");
        assert_eq!(drag(end(p, n), to), nothing("J1.2 is already on SWDIO"),
            "rule 11: carrying onto the same net changes nothing");
    }
    for to in [pin(q), end(q, n), wire(m), tag(m)] {
        assert_eq!(drag(pin(p), to), Planned::Refused {
            chip: "SWDIO and SWCLK are two nets: join them in the text".into(),
            sites: vec![Sel::Net(NetRef { board: c.unit, net: n }), Sel::Net(NetRef { board: c.unit, net: m })],
        }, "rule 7: two nets are refused with both nets as sites, even for a singleton authored net");
        assert_eq!(drag(end(p, n), to), edit(&c, "Move J1.2 from SWDIO to SWCLK", [
            CircuitOp::Disconnect { pin: p }, CircuitOp::Connect { pin: p, net: m },
        ]), "rule 12: a carried pin changes net without merging the nets");
        assert_eq!(drag(end(la, nl), to), edit(&c, "Move J1.3 from loose_a to SWCLK", [
            CircuitOp::Disconnect { pin: la }, CircuitOp::Connect { pin: la, net: m },
        ]), "rule 12: a carried loose pin changes net");
    }
    for to in [pin(p), end(p, m)] {
        assert_eq!(drag(end(p, n), to), nothing(""), "rule 8: carrying back to the same pin changes nothing");
    }
    for to in [pin(la), end(la, m), wire(nl), tag(nl)] {
        assert_eq!(drag(pin(la), to), nothing(""), "rule 1: a loose pin's wire and tag stand for itself");
        assert_eq!(drag(end(la, nl), to), nothing(""), "rule 8: a carried loose pin's wire and tag stand for itself");
    }
    for from in [wire(n), tag(n), wire(nl), tag(nl), canvas, Target::None] {
        assert_eq!(drag(from, pin(b)), nothing(""), "rules 1-12: only pins and wire ends start a wire gesture");
    }

    for target in [pin(p), end(p, m), pin(same)] {
        let chip = if target == pin(same) { "D1.A is on SWDIO" } else { "J1.2 is on SWDIO" };
        assert_eq!(plan(&c, &Gesture::NoConnect(target)), nothing(chip),
            "rule 13: Q on a connected pin changes nothing and trusts the circuit's net");
    }
    assert_eq!(plan(&c, &Gesture::NoConnect(pin(q))), nothing("J1.4 is on SWCLK"),
        "rule 13: a lone authored net is still connected");
    for target in [pin(b), end(b, n)] {
        assert_eq!(plan(&c, &Gesture::NoConnect(target)), edit(&c, "Clear R1.1's not-connected mark", [
            CircuitOp::NoConnect { pin: b, nc: false },
        ]), "rule 14: Q clears a free pin's mark");
    }
    for target in [pin(a), end(a, n)] {
        assert_eq!(plan(&c, &Gesture::NoConnect(target)), edit(&c, "Mark J1.1 not connected", [
            CircuitOp::NoConnect { pin: a, nc: true },
        ]), "rule 15: Q marks a free pin");
    }
    for target in [pin(la), end(la, m)] {
        assert_eq!(plan(&c, &Gesture::NoConnect(target)), edit(&c, "Mark J1.3 not connected", [
            CircuitOp::Disconnect { pin: la }, CircuitOp::NoConnect { pin: la, nc: true },
        ]), "rule 15: Q disconnects a loose pin before marking it");
    }
    for target in [wire(n), tag(n), wire(nl), tag(nl), canvas, Target::None] {
        assert_eq!(plan(&c, &Gesture::NoConnect(target)), nothing(""), "rule 16: Q only edits a pin or wire end");
    }
    for target in [end(p, n), end(p, m), end(p, NetId(u32::MAX))] {
        assert_eq!(plan(&c, &Gesture::Delete(target)), edit(&c, "Disconnect J1.2 from SWDIO", [CircuitOp::Disconnect { pin: p }]),
            "rule 17: Delete disconnects the circuit's current net even with a stale wire-end net");
        assert_eq!(drag(target, canvas), edit(&c, "Disconnect J1.2 from SWDIO", [CircuitOp::Disconnect { pin: p }]),
            "rule 9: carrying a stale wire end uses current connectivity");
    }
    assert_eq!(plan(&c, &Gesture::Delete(end(la, nl))), edit(&c, "Disconnect J1.3 from loose_a", [CircuitOp::Disconnect { pin: la }]),
        "rule 17: Delete also disconnects a loose wire end");
    assert_eq!(plan(&c, &Gesture::Delete(pin(p))), edit(&c, "Disconnect J1.2 from SWDIO", [CircuitOp::Disconnect { pin: p }]),
        "rule 17: Delete on a connected pin disconnects it (the pin wins the pick below 8 px)");
    assert_eq!(plan(&c, &Gesture::Delete(pin(la))), edit(&c, "Disconnect J1.3 from loose_a", [CircuitOp::Disconnect { pin: la }]),
        "rule 17: Delete on a loose pin disconnects it");
    for target in [pin(a), pin(b), wire(n), tag(n), wire(nl), tag(nl), canvas, Target::None] {
        assert_eq!(plan(&c, &Gesture::Delete(target)), nothing(""), "rule 18: Delete acts only on a connected pin or its wire end");
    }
    assert_eq!(plan(&c, &Gesture::Delete(end(a, n))), nothing(""), "rule 17: a stale wire end of a free pin changes nothing");
    assert_eq!(drag(end(a, n), pin(b)), nothing(""), "rules 8-12: a free pin cannot carry a stale connection");

    for target in [pin(port), end(port, bound), wire(bound), tag(bound)] {
        assert_eq!(drag(pin(a), target), edit(&c, "Wire J1.1 to GND", [CircuitOp::Connect { pin: a, net: bound }]),
            "rule 4: a lone derived net bound to a block port is ON, including its wire and tag");
    }
    assert_eq!(plan(&c, &Gesture::NoConnect(pin(port))), nothing("D1.K is on GND"),
        "rule 13: a port-bound singleton derived net is connected");

    let missing_comp = (CompId(u32::MAX), PinIx(0));
    let missing_pin = (a.0, PinIx(u16::MAX));
    for target in [pin(missing_comp), pin(missing_pin), end(missing_comp, n), end(missing_pin, n)] {
        assert_eq!(drag(target, pin(b)), nothing(""), "rules 1-12: a missing source component or pin changes nothing");
        assert_eq!(drag(pin(p), target), nothing(""), "rules 1-7: a missing destination is not empty canvas");
        assert_eq!(drag(end(p, n), target), nothing(""), "rules 8-12: a missing destination never disconnects the source");
        assert_eq!(plan(&c, &Gesture::NoConnect(target)), nothing(""), "rules 13-16: Q on a missing component or pin changes nothing");
        assert_eq!(plan(&c, &Gesture::Delete(target)), nothing(""), "rules 17-18: Delete on a missing component or pin changes nothing");
    }
    for target in [wire(NetId(u32::MAX)), tag(NetId(u32::MAX))] {
        assert_eq!(drag(pin(a), target), nothing(""), "rule 4: a missing net cannot be connected");
        assert_eq!(drag(end(p, n), target), nothing(""), "rule 12: a missing net cannot receive a carried connection");
    }
    assert_eq!(c, before, "rules 1-18: planning leaves the circuit unchanged");

    let mut changed = c.clone();
    changed.comps[la.0.ix()].nc.push(la.1);
    assert_eq!(plan(&changed, &Gesture::NoConnect(pin(la))), edit(&changed, "Clear J1.3's not-connected mark", [
        CircuitOp::NoConnect { pin: la, nc: false },
    ]), "rule 14: a marked loose pin clears its mark before rule 15 can disconnect it");
    changed.comps[p.0.ix()].nc.push(p.1);
    assert_eq!(plan(&changed, &Gesture::NoConnect(pin(p))), nothing("J1.2 is on SWDIO"),
        "rule 13: being ON takes precedence over a stale not-connected mark");
    changed.comps[b.0.ix()].nc.clear();
    assert_eq!(plan(&changed, &Gesture::NoConnect(pin(b))), edit(&changed, "Mark R1.1 not connected", [
        CircuitOp::NoConnect { pin: b, nc: true },
    ]), "rule 15: marking a generated part's pin is left to the language");

    changed.comps[la.0.ix()].nc.clear();
    changed.comps[spare.0.ix()].conns.push((spare.1, nl));
    assert_eq!(plan(&changed, &Gesture::NoConnect(pin(la))), nothing("J1.3 is on loose_a"),
        "rule 13: two different pins of the same component make a derived net connected");
    for to in [pin(la), end(la, nl), wire(nl), tag(nl)] {
        assert_eq!(plan(&changed, &Gesture::Wire { from: pin(a), to }), edit(&changed, "Wire J1.1 to loose_a", [
            CircuitOp::Connect { pin: a, net: nl },
        ]), "rule 4: a derived net with a second pin is ON, including its wire and tag");
    }
    changed.comps[spare.0.ix()].conns.retain(|(pin, _)| *pin != spare.1);
    changed.comps[b.0.ix()].conns.push((b.1, nl));
    assert_eq!(plan(&changed, &Gesture::NoConnect(pin(la))), nothing("J1.3 is on loose_a"),
        "rule 13: a pin on another component also prevents a derived net from being lone");
}
