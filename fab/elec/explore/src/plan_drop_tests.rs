use super::{plan_drop, Dropped, PinId, Template, TemplateKind};
use crate::edit::{Target, Toward};
use crate::plan::Planned;
use crate::sheet::TagKind;
use fab_elec_circuit::{BlockDef, BlockDefId, Circuit, CircuitOp, End, NetKind, SheetHint};
use fab_elec_core::{BlockId, CompId, CompRef, NetId, PinIx, SchPt, Sel};
use fab_elec_design::{fixture, DesignOp, Resolved};
use fab_elec_parts::{PinKind, VRange};
use std::sync::Arc;

fn pin((comp, pin): PinId, toward: Toward) -> Target {
    Target::Pin { comp, pin, toward }
}

fn wire(net: NetId) -> Target {
    Target::Wire { net, run: 0, seg: 0, at: [0, 0] }
}

fn tag(net: NetId) -> Target {
    Target::Tag { net, kind: TagKind::Label }
}

fn edit(circuit: &Circuit, label: &str, op: CircuitOp) -> Planned {
    Planned::Edit(Resolved {
        ops: vec![DesignOp::Circuit { unit: circuit.unit, op }], place: vec![], label: label.into(),
    })
}

fn add(circuit: &Circuit, template: &Template, after: Option<CompId>, ends: Vec<End>, label: &str) -> Planned {
    edit(circuit, label, CircuitOp::AddPart { stmt: template.stmt.clone(), after, ends, hint: None })
}

fn series(circuit: &Circuit, template: &Template, net: NetId, moved: Vec<PinId>, label: &str) -> Planned {
    edit(circuit, label, CircuitOp::InsertSeries { net, stmt: template.stmt.clone(), moved })
}

fn cycle(circuit: &Circuit, template: &Template, at: Target, sides: Option<&[Vec<PinId>; 2]>, expected: &[Planned], rule: &str) {
    for pick in (0..expected.len() * 2).chain([usize::MAX]) {
        let got = plan_drop(circuit, template, at, pick, sides);
        assert_eq!(got.count, expected.len(), "{rule}: candidate count");
        assert_eq!(got.pick, pick % expected.len(), "{rule}: pick wraps");
        assert_eq!(got.planned, expected[pick % expected.len()], "{rule}: candidate order at pick {pick}");
    }
}

#[test]
fn drop_plans_pick_the_idiom_by_direction() {
    // The same Rust probe as plan_tests: one fixture, no document evaluation.
    let probe = fixture::probe::probe();
    let mut c = (*probe.circuit).clone();
    let host = c.comp("j").expect("A setup: fixture host").id;
    let receiver = c.comp("led.r").expect("C setup: fixture receiver").id;
    let passive = c.comp("led.d").expect("C setup: fixture passive").id;
    let block = c.comps[receiver.ix()].block;
    let low = c.net("3V3").expect("A setup: low rail").id;
    let high = c.net("SWCLK").expect("A setup: second rail").id;
    let gnd = c.net("GND").expect("A setup: ground").id;
    let signal = c.net("SWDIO").expect("A setup: signal").id;
    let loose = c.net("NRST").expect("A setup: loose net").id;
    for comp in &mut c.comps {
        comp.conns.clear();
        comp.nc.clear();
    }
    c.nets[high.ix()].name = "5V".into();
    c.nets[high.ix()].kind = NetKind::Power;
    c.nets[high.ix()].volts = Some(VRange::fixed(5.0));
    c.nets[loose.ix()].name = "loose".into();
    for net in [low, high, gnd, loose] { c.nets[net.ix()].scope = Some(block); }
    for b in &mut c.tree { b.ports.clear(); }
    c.block_defs.push(BlockDef {
        id: BlockDefId(0), name: "driver".into(), ports: vec![], params: vec![],
        sheet: Default::default(), template: None, planned: false, role: None,
    });
    c.tree[block.ix()].def = Some(BlockDefId(0));
    c.tree[block.ix()].path = "bank.led_driver".into();
    let mut second = c.tree[block.ix()].clone();
    second.id = BlockId(c.tree.len() as u32);
    second.path = "bank.other_driver".into();
    c.comps[passive.ix()].block = second.id;
    c.tree.push(second);
    c.comps[host.ix()].block = block;
    c.comps[host.ix()].refdes = "U2".into();
    let part = Arc::make_mut(&mut c.comps[host.ix()].part);
    let prototype = part.pins[0].clone();
    part.pins = [
        ("VDD", PinKind::PowerIn), ("PB7", PinKind::Out), ("VCC", PinKind::PowerIn),
        ("FREE", PinKind::Passive), ("GND", PinKind::PowerIn), ("SUP", PinKind::PowerIn),
        ("LOOSE", PinKind::Passive), ("RAIL", PinKind::Passive), ("DUP", PinKind::PowerIn),
    ].into_iter().map(|(name, kind)| {
        let mut pin = prototype.clone();
        pin.name = name.into();
        pin.kind = kind;
        pin
    }).collect();
    // Unsorted connections, duplicate supplies and a lone ground still describe real rails.
    c.comps[host.ix()].conns = vec![
        (PinIx(2), high), (PinIx(0), low), (PinIx(4), gnd), (PinIx(1), signal),
        (PinIx(5), low), (PinIx(6), loose), (PinIx(7), low), (PinIx(8), high),
    ];
    c.comps[receiver.ix()].conns = vec![(PinIx(0), signal), (PinIx(1), signal)];
    let part = Arc::make_mut(&mut c.comps[receiver.ix()].part);
    part.pins[0].kind = PinKind::In;
    part.pins[1].kind = PinKind::PowerOut;
    c.comps[passive.ix()].conns = vec![(PinIx(0), signal), (PinIx(1), signal)];
    for p in &mut Arc::make_mut(&mut c.comps[passive.ix()].part).pins { p.kind = PinKind::Passive; }
    let p = (host, PinIx(1));
    let free = (host, PinIx(3));
    let supply = (host, PinIx(5));
    let lone = (host, PinIx(6));
    let rail_pin = (host, PinIx(7));
    let input = (receiver, PinIx(0));
    let output = (receiver, PinIx(1));
    let load = (passive, PinIx(0));
    let r = Template {
        stmt: "let r = e.r(10kohm, _n0, _n1, {package: \"0402\"})".into(), stem: "r".into(),
        kind: TemplateKind::Passive, pins: vec![("1".into(), PinKind::Passive), ("2".into(), PinKind::Passive)],
    };
    let cap = Template {
        stmt: "let c = e.c(100nF, _n0, _n1, {package: \"0402\"})".into(), stem: "c".into(),
        kind: TemplateKind::Capacitor, pins: r.pins.clone(),
    };
    let before = c.clone();
    let up_high = add(&c, &r, Some(host), vec![End::Net(signal), End::Net(high)], "Pull U2.PB7 up to 5V");
    let up_low = add(&c, &r, Some(host), vec![End::Net(signal), End::Net(low)], "Pull U2.PB7 up to 3V3");
    let down = add(&c, &r, Some(host), vec![End::Net(signal), End::Net(gnd)], "Pull U2.PB7 down to GND");
    let split = series(&c, &r, signal, vec![p], "Put r in series with U2.PB7");
    for (toward, expected) in [
        (Toward::Up, [up_high.clone(), up_low.clone(), down.clone(), split.clone()]),
        (Toward::Down, [down.clone(), up_high.clone(), up_low.clone(), split.clone()]),
        (Toward::Across, [split.clone(), up_high.clone(), up_low.clone(), down.clone()]),
    ] {
        cycle(&c, &r, pin(p, toward), None, &expected, "A: direction and distinct rails in descending voltage order");
    }
    cycle(&c, &r, Target::WireEnd { comp: host, pin: p.1, net: NetId(u32::MAX) }, None,
        &[split.clone(), up_high.clone(), up_low.clone(), down.clone()], "A: wire ends are Across and use current connectivity");
    let decouple = add(&c, &cap, Some(host), vec![End::Net(low), End::Net(gnd)], "Decouple 3V3 to GND");
    for toward in [Toward::Up, Toward::Down, Toward::Across] {
        cycle(&c, &cap, pin(supply, toward), None, &[
            decouple.clone(),
            add(&c, &cap, Some(host), vec![End::Net(low), End::Net(high)], "Pull U2.SUP up to 5V"),
            series(&c, &cap, low, vec![supply], "Put c in series with U2.SUP"),
        ], "A: capacitor decouples a PowerIn first in every direction, excludes the dropped-on rail");
    }
    cycle(&c, &cap, pin(rail_pin, Toward::Up), None, &[
        decouple.clone(),
        add(&c, &cap, Some(host), vec![End::Net(low), End::Net(high)], "Pull U2.RAIL up to 5V"),
        series(&c, &cap, low, vec![rail_pin], "Put c in series with U2.RAIL"),
    ], "A: any pin ON a Power net is a supply pin");
    cycle(&c, &cap, pin(p, Toward::Down), None, &[
        add(&c, &cap, Some(host), vec![End::Net(signal), End::Net(gnd)], "Pull U2.PB7 down to GND"),
        add(&c, &cap, Some(host), vec![End::Net(signal), End::Net(high)], "Pull U2.PB7 up to 5V"),
        add(&c, &cap, Some(host), vec![End::Net(signal), End::Net(low)], "Pull U2.PB7 up to 3V3"),
        series(&c, &cap, signal, vec![p], "Put c in series with U2.PB7"),
    ], "A: a signal capacitor keeps pull-down wording");

    let mut missing = c.clone();
    missing.comps[host.ix()].conns.retain(|(_, net)| *net != low && *net != high);
    cycle(&missing, &r, pin(p, Toward::Up), None, &[
        Planned::Refused { chip: "no supply on U2".into(), sites: vec![Sel::Comp(CompRef { board: c.unit, comp: host })] },
        down.clone(), split.clone(),
    ], "A: missing supply refuses first, Tab reaches ground and series");
    cycle(&missing, &r, pin(p, Toward::Across), None, &[split.clone(), down.clone()],
        "A: a missing non-default idiom adds no refusal");
    let mut missing = c.clone();
    missing.comps[host.ix()].conns.retain(|(_, net)| *net != gnd);
    let no_ground = Planned::Refused {
        chip: "no ground on U2".into(), sites: vec![Sel::Comp(CompRef { board: c.unit, comp: host })],
    };
    cycle(&missing, &r, pin(p, Toward::Down), None,
        &[no_ground.clone(), up_high.clone(), up_low.clone(), split.clone()], "A: missing ground refuses first, Tab reaches rails and series");
    cycle(&missing, &cap, pin(supply, Toward::Across), None, &[
        no_ground,
        add(&c, &cap, Some(host), vec![End::Net(low), End::Net(high)], "Pull U2.SUP up to 5V"),
        series(&c, &cap, low, vec![supply], "Put c in series with U2.SUP"),
    ], "A: missing decoupling ground refuses in any direction");
    let mut ties = c.clone();
    ties.nets[high.ix()].volts = ties.nets[low.ix()].volts;
    cycle(&ties, &r, pin(p, Toward::Up), None, &[up_low.clone(), up_high.clone(), down.clone(), split.clone()],
        "A: equal rail voltages use net id order");
    ties.nets[high.ix()].volts = None;
    cycle(&ties, &r, pin(p, Toward::Up), None, &[up_low.clone(), up_high.clone(), down.clone(), split.clone()],
        "A: a rail without voltage sorts last");
    ties.nets[low.ix()].volts = None;
    cycle(&ties, &r, pin(p, Toward::Up), None, &[up_low.clone(), up_high.clone(), down.clone(), split.clone()],
        "A: rails without voltage use net id order");
    let mut grounds = c.clone();
    let other_ground = grounds.net("led.A").expect("A setup: second ground").id;
    grounds.nets[other_ground.ix()].name = "AGND".into();
    grounds.nets[other_ground.ix()].kind = NetKind::Ground;
    grounds.comps[host.ix()].conns.push((free.1, other_ground));
    cycle(&grounds, &r, pin(p, Toward::Down), None, &[
        add(&c, &r, Some(host), vec![End::Net(signal), End::Net(other_ground)], "Pull U2.PB7 down to AGND"),
        up_high.clone(), up_low.clone(), split.clone(),
    ], "A: ground is the first host pin by pin order, regardless of pin kind or connection order");

    let free_high = add(&c, &r, Some(host), vec![End::Pin(free.0, free.1), End::Net(high)], "Pull U2.FREE up to 5V");
    let free_low = add(&c, &r, Some(host), vec![End::Pin(free.0, free.1), End::Net(low)], "Pull U2.FREE up to 3V3");
    let free_down = add(&c, &r, Some(host), vec![End::Pin(free.0, free.1), End::Net(gnd)], "Pull U2.FREE down to GND");
    for toward in [Toward::Up, Toward::Across] {
        cycle(&c, &r, pin(free, toward), None, &[free_high.clone(), free_low.clone(), free_down.clone()],
            "B: FREE uses End::Pin and offers no series");
    }
    cycle(&c, &r, pin(free, Toward::Down), None, &[free_down, free_high, free_low], "B: FREE respects Down");
    let mut free_supply = c.clone();
    free_supply.comps[host.ix()].conns.retain(|(pin, _)| *pin != supply.1);
    cycle(&free_supply, &cap, pin(supply, Toward::Across), None, &[
        add(&c, &cap, Some(host), vec![End::Pin(host, supply.1), End::Net(gnd)], "Decouple U2.SUP to GND"),
        add(&c, &cap, Some(host), vec![End::Pin(host, supply.1), End::Net(high)], "Pull U2.SUP up to 5V"),
        add(&c, &cap, Some(host), vec![End::Pin(host, supply.1), End::Net(low)], "Pull U2.SUP up to 3V3"),
    ], "B: a FREE supply decouples by pin name with no series");
    let loose_plans = [
        series(&c, &r, loose, vec![lone], "Put r in series with U2.LOOSE"),
        add(&c, &r, Some(host), vec![End::Net(loose), End::Net(high)], "Pull U2.LOOSE up to 5V"),
        add(&c, &r, Some(host), vec![End::Net(loose), End::Net(low)], "Pull U2.LOOSE up to 3V3"),
        add(&c, &r, Some(host), vec![End::Net(loose), End::Net(gnd)], "Pull U2.LOOSE down to GND"),
    ];
    cycle(&c, &r, pin(lone, Toward::Across), None, &loose_plans, "A: LOOSE keeps its net end and can split");
    cycle(&c, &r, tag(loose), None, &loose_plans, "D: a lone derived tag is its pin, Across");

    for (sides, default, rule) in [
        ([vec![p], vec![input, load]], 1, "C: the side without a driver wins over the smaller side"),
        ([vec![input, load], vec![output]], 0, "C: PowerOut also drives the opposite side"),
        ([vec![input, load], vec![(passive, PinIx(1))]], 1, "C: neither driver uses the smaller side"),
        ([vec![input], vec![load]], 0, "C: neither driver and equal sizes use side zero"),
        ([vec![p, input], vec![output]], 1, "C: both drivers use the smaller side"),
        ([vec![p], vec![output]], 0, "C: both drivers and equal sizes use side zero"),
    ] {
        let expected = [
            series(&c, &r, signal, sides[default].clone(), "Put r in series on SWDIO"),
            series(&c, &r, signal, sides[default ^ 1].clone(), "Put r in series on SWDIO"),
        ];
        cycle(&c, &r, wire(signal), Some(&sides), &expected, rule);
    }
    let nothing = Planned::Nothing { chip: String::new() };
    let mut isolated = c.clone();
    isolated.comps[host.ix()].conns.clear();
    cycle(&isolated, &r, pin(free, Toward::Across), None, &[nothing.clone()],
        "A/B: no legal idiom gives one Nothing candidate");
    cycle(&c, &r, wire(signal), None, &[nothing.clone()], "C: no cut tree means no series plan");
    for sides in [[vec![], vec![p]], [vec![p], vec![]]] {
        cycle(&c, &r, wire(signal), Some(&sides), &[nothing.clone()], "C: either empty side means no series plan");
    }
    cycle(&c, &r, tag(low), None, &[add(&c, &r, Some(host), vec![End::Net(low)], "Add r on 3V3")],
        "D: a rail tag attaches slot zero and leaves other ends open");
    cycle(&c, &r, tag(signal), None, &[add(&c, &r, Some(host), vec![End::Net(signal)], "Add r on SWDIO")],
        "D: a named signal tag also attaches slot zero");
    let canvas = Target::Canvas { block: block.0, cell: [-7, 12] };
    cycle(&c, &r, canvas, None, &[edit(&c, "Add r", CircuitOp::AddPart {
        stmt: r.stmt.clone(), after: Some(receiver), ends: vec![],
        hint: Some(SheetHint { at: Some(SchPt::new(-7, 12)), ..Default::default() }),
    })], "E: canvas uses local cells and the highest component id in that block");
    cycle(&c, &r, Target::Canvas { block: 0, cell: [0, 0] }, None, &[edit(&c, "Add r", CircuitOp::AddPart {
        stmt: r.stmt.clone(), after: None, ends: vec![],
        hint: Some(SheetHint { at: Some(SchPt::new(0, 0)), ..Default::default() }),
    })], "E: an empty block has no insertion predecessor");

    let marker = Template {
        stmt: "let marker = mod.project.marker(_n0)".into(), stem: "marker".into(),
        kind: TemplateKind::Passive, pins: vec![("1".into(), PinKind::PowerOut)],
    };
    for (at, end, name) in [
        (pin(supply, Toward::Across), End::Net(low), "U2.SUP"),
        (pin(free, Toward::Up), End::Pin(host, free.1), "U2.FREE"),
        (wire(low), End::Net(low), "3V3"),
    ] {
        cycle(&c, &marker, at, None, &[add(&c, &marker, Some(host), vec![end],
            &format!("Add marker with 1 on {name}"))], "a one-pin catalog function connects its sole slot");
    }
    cycle(&c, &marker, canvas, None, &[edit(&c, "Add marker", CircuitOp::AddPart {
        stmt: marker.stmt.clone(), after: Some(receiver), ends: vec![],
        hint: Some(SheetHint { at: Some(SchPt::new(-7, 12)), ..Default::default() }),
    })], "a one-pin part on empty canvas is placed unconnected in one edit");

    let mut device = Template {
        stmt: "let led_driver = driver({out: _n1, input: _n2, power: _n3, io: _n4, sense: _n5, source: _n6, tri: _n7})".into(),
        stem: "led_driver".into(), kind: TemplateKind::Part,
        pins: [
            ("nc", PinKind::NoConnect), ("out", PinKind::Out), ("input", PinKind::In),
            ("power", PinKind::PowerIn), ("io", PinKind::Bidi), ("sense", PinKind::Passive),
            ("source", PinKind::PowerOut), ("tri", PinKind::TriState),
        ].into_iter().map(|(name, kind)| (name.into(), kind)).collect(),
    };
    for kind in [TemplateKind::Part, TemplateKind::Block] {
        device.kind = kind;
        for (at, after, end, name, order, rule) in [
            (pin(p, Toward::Up), host, End::Net(signal), "U2.PB7", [2, 3, 4, 5, 1, 6, 7], "F: driver complements first"),
            (pin(input, Toward::Down), receiver, End::Net(signal), "R1.1", [1, 4, 5, 6, 2, 3, 7], "F: receiver complements first"),
            (pin(free, Toward::Across), host, End::Pin(host, free.1), "U2.FREE", [1, 2, 3, 4, 5, 6, 7], "F: passive FREE target uses slot order and End::Pin"),
            (Target::WireEnd { comp: host, pin: p.1, net: low }, host, End::Net(signal), "U2.PB7", [2, 3, 4, 5, 1, 6, 7], "F: wire end uses the pin's kind and current net"),
            (wire(signal), host, End::Net(signal), "SWDIO", [2, 3, 4, 5, 1, 6, 7], "F: wire uses its first pin's kind without sides"),
        ] {
            let expected: Vec<_> = order.into_iter().map(|slot| {
                let mut ends = vec![End::Open; slot];
                ends.push(end.clone());
                add(&c, &device, Some(after), ends, &format!("Add led_driver with {} on {name}", device.pins[slot].0))
            }).collect();
            cycle(&c, &device, at, None, &expected, rule);
        }
        cycle(&c, &device, tag(low), None, &[add(&c, &device, Some(host), vec![End::Net(low)], "Add led_driver on 3V3")],
            "D: Part and Block tags attach slot zero regardless of its kind");
    }
    device.pins = vec![("nc".into(), PinKind::NoConnect)];
    for at in [pin(p, Toward::Up), wire(signal)] {
        cycle(&c, &device, at, None, &[nothing.clone()], "F: no usable slot gives one Nothing candidate");
    }
    device.pins.clear();
    cycle(&c, &device, pin(free, Toward::Across), None, &[nothing.clone()], "F: no slots gives one Nothing candidate");

    for at in [
        Target::None, pin((CompId(u32::MAX), PinIx(0)), Toward::Up), pin((host, PinIx(u16::MAX)), Toward::Down),
        Target::WireEnd { comp: CompId(u32::MAX), pin: PinIx(0), net: signal },
        wire(NetId(u32::MAX)), tag(NetId(u32::MAX)), Target::Canvas { block: u32::MAX, cell: [0, 0] },
    ] {
        assert_eq!(plan_drop(&c, &r, at, usize::MAX, None), Dropped {
            planned: nothing.clone(), pick: 0, count: 1, instances: None,
        }, "G: missing targets give one empty candidate");
    }
    let mut missing_net = c.clone();
    missing_net.comps[host.ix()].conns.iter_mut().find(|(pin, _)| *pin == p.1)
        .expect("G setup: connected target").1 = NetId(u32::MAX);
    assert_eq!(plan_drop(&missing_net, &r, pin(p, Toward::Across), 0, None).planned, nothing,
        "G: a missing connected net is not a FREE pin");
    for at in [pin(p, Toward::Up), canvas, wire(signal), tag(low), tag(loose)] {
        assert_eq!(plan_drop(&c, &r, at, 0, None).instances, Some(("led_driver".into(), 2)),
            "instances: pin, canvas, wire and tag report the target block's last path segment and shared count");
    }
    assert_eq!(plan_drop(&c, &r, Target::Canvas { block: 0, cell: [0, 0] }, 0, None).instances, None,
        "instances: the root has no definition");
    let mut unique = c.clone();
    unique.tree.pop();
    assert_eq!(plan_drop(&unique, &r, pin(p, Toward::Up), 0, None).instances, None,
        "instances: a definition with one instance adds no chip suffix");
    assert_eq!(c, before, "A-G: drop planning leaves the circuit unchanged");
}
