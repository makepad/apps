//! Schematic bus parts resolve through part data and stay out of the analog circuit.

use fab_elec_bench::bench::Supply;
use fab_elec_bench::bus::BusConn;
use fab_elec_bench::{lower_circuit, BenchSpec, Firmware, ModelLibs, SimCmd};
use fab_elec_circuit::{Circuit, Component, Net, NetKind, QtyUnit, Quantity, Value};
use fab_elec_core::{BlockId, BoardId, ClassId, CompId, CompRef, NetId, NetRef, PinIx};
use fab_elec_mcu_emu::board::Board;
use fab_elec_mcu_emu::Pull;
use fab_elec_mcu_parts::{Netlist, PartKind as BusKind, PinRef};
use fab_elec_parts::{Fields, GpioCard, PartDef, PartKind, PinDef, PinKind, Symbol};
use std::sync::Arc;

fn net(i: u32) -> NetRef { NetRef { board: BoardId(0), net: NetId(i) } }

fn component(id: u32, path: &str, sim: &str, pins: &[(&str, u32)]) -> Component {
    Component {
        id: CompId(id), path: path.into(), refdes: path.into(), ref_kind: Default::default(),
        part: Arc::new(PartDef {
            name: path.into(), kind: PartKind::Ic, symbol: Symbol::default(), footprint: None,
            body: None, emitter: None, gpio: None, lifecycle: None, approved: false, fields: Fields(vec![("sim".into(), sim.into())]),
            pins: pins.iter().map(|&(name, _)| PinDef {
                name: name.into(), pads: Vec::new(), unit: 0, limits: None, alternates: Vec::new(), delay: None,
                kind: if ["VDD", "GND", "VIO"].contains(&name) { PinKind::PowerIn } else { PinKind::Bidi },
            }).collect(),
        }),
        value: None, conns: pins.iter().enumerate().map(|(i, &(_, n))| (PinIx(i as u16), NetId(n))).collect(),
        nc: Vec::new(), block: BlockId(0), dnp: false, fields: Fields::default(),
    }
}

fn value(comp: &mut Component, si: f64, unit: QtyUnit) {
    comp.value = Some(Value { text: si.to_string(), qty: Some(Quantity { si, unit }), tol: None });
}

fn card(c_pin: Option<f64>) -> GpioCard {
    // Synthetic card data: capacitance and input threshold are explicit.
    GpioCard { r_high: 25.0, r_low: 25.0, pull_up: 30e3, vih: 0.7, vil: 0.3, leak: 1e-6, edge: None, c_pin }
}

fn schematic() -> (Circuit, BenchSpec) {
    let mut fw = component(0, "U1", "mcu", &[
        ("SDA", 2), ("SCL", 3), ("INT", 4), ("RST", 5), ("VDD", 1), ("GND", 0), ("XTAL1", 6), ("XTAL2", 7),
    ]);
    let def = Arc::make_mut(&mut fw.part);
    def.fields.0.push(("model".into(), "TEST_MCU".into()));
    def.gpio = Some(card(Some(5e-12)));
    // The component's sim and addr win over the library fields. Pin order and case deliberately
    // differ from the model, so the output must be ordered by the model's pin list.
    let mut touch = component(1, "U2", "passive", &[("rst", 5), ("Int", 4), ("scl", 3), ("SDA", 2), ("VDD", 1), ("GND", 0), ("VIO", 1)]);
    let def = Arc::make_mut(&mut touch.part);
    def.fields.0.extend([("model".into(), "CST820".into()), ("bus.addr".into(), "22".into())]);
    def.gpio = Some(GpioCard { vih: 0.8, ..card(Some(3e-12)) });
    def.pins.push(PinDef { name: "NC".into(), kind: PinKind::Passive, ..def.pins[0].clone() });
    touch.nc.push(PinIx(7));
    touch.fields = Fields(vec![("sim".into(), "bus".into()), ("bus.addr".into(), "21".into())]);
    let mut parts = vec![fw, touch];
    for (path, n) in [("R29", 2), ("R30", 3), ("R31", 4), ("R32", 2)] {
        let mut r = component(parts.len() as u32, path, "resistor", &[("1", n), ("2", 1)]);
        Arc::make_mut(&mut r.part).kind = PartKind::Resistor;
        value(&mut r, 10e3, QtyUnit::Ohm);
        parts.push(r);
    }
    let mut decouple = component(parts.len() as u32, "C1", "capacitor", &[("1", 1), ("2", 0)]);
    value(&mut decouple, 100e-9, QtyUnit::Farad);
    parts.push(decouple);
    let mut crystal = component(parts.len() as u32, "Y1", "passive", &[("1", 6), ("2", 7)]);
    Arc::make_mut(&mut crystal.part).kind = PartKind::Crystal;
    value(&mut crystal, 16e6, QtyUnit::Hertz);
    parts.push(crystal);
    parts.push(component(parts.len() as u32, "J1", "passive", &[("1", 2), ("2", 3)]));
    let circuit = Circuit {
        unit: BoardId(0), name: "handheld".into(), comps: parts,
        nets: ["GND", "3V3", "SDA", "SCL", "INT", "RST", "X1", "X2"].into_iter().enumerate().map(|(i, name)| Net {
            id: NetId(i as u32), name: name.into(), class: ClassId(0), volts: None, scope: None,
            kind: if i == 0 { NetKind::Ground } else if i == 1 { NetKind::Power } else { NetKind::Signal },
        }).collect(),
        ..Circuit::default()
    };
    let bench = BenchSpec {
        supplies: vec![Supply { net: net(1), v: 3.3, limit: None, ramp: None }],
        firmware: vec![Firmware { path: "U1".into(), doc: "handheld.fw.splash".into(), params: Vec::new() }],
        ..BenchSpec::default()
    };
    (circuit, bench)
}

#[test]
fn a_schematic_bus_lowers_beside_firmware_and_binds() {
    let (circuit, bench) = schematic();
    let lowered = lower_circuit(&circuit, &bench, 0, &ModelLibs::generic());
    let fw = &lowered.firmware[0];
    let bus = &fw.bus;
    assert!(fw.logic.parts.is_empty(), "bus parts belong to BusSide regardless of the firmware chip");
    assert_eq!(bus.parts.len(), 1, "the one schematic bus part lowers");
    let part = &bus.parts[0];
    assert_eq!((&*part.path, part.comp, &*part.model, part.kind),
        ("U2", CompRef { board: BoardId(0), comp: CompId(1) }, "CST820", BusKind::Cst820 { addr: 21 }),
        "the binding preserves component identity and resolves component fields over library fields");
    assert_eq!(part.pins, [
        ("SDA".into(), BusConn::Net(0)), ("SCL".into(), BusConn::Net(1)),
        ("INT".into(), BusConn::Net(2)), ("RST".into(), BusConn::Net(3)),
    ], "pins match without case and follow model order; supply, ground, VIO and NC are excluded");
    assert_eq!(bus.nets.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["SDA", "SCL", "INT", "RST"],
        "bus nets retain schematic net order");
    for n in &bus.nets {
        assert_eq!(n.chip, [n.name.clone()], "{}: the net retains the firmware pin name", n.name);
        assert_eq!(n.vih, Some(0.7), "{}: the net uses the firmware card's input threshold", n.name);
        assert!((n.farads.expect("both participants carry c_pin") - 8e-12).abs() < 1e-24,
            "{}: capacitance sums firmware and bus pins; the connector contributes nothing", n.name);
        assert!(!lowered.circuit.nodes.iter().any(|node| node.name == n.name),
            "{}: a bus signal has no analog node, GPIO, pad capacitance or insulation leak", n.name);
    }
    for (i, resistance, names) in [(0, 5e3, vec!["R29", "R32"]), (1, 10e3, vec!["R30"]), (2, 10e3, vec!["R31"])] {
        let pull = bus.nets[i].pull.as_ref().expect("the schematic pull is retained");
        assert!(pull.high, "{}: a resistor to the participant's supply pulls high", bus.nets[i].name);
        assert!((pull.ohms - resistance).abs() < 1e-9, "{}: parallel pulls combine by conductance", bus.nets[i].name);
        assert_eq!(pull.resistors, names, "{}: every consumed resistor is listed", bus.nets[i].name);
        for name in names {
            assert!(lowered.diags.iter().any(|d| d.message == format!("{name}: pull-up of {}, digital side", bus.nets[i].name)),
                "{name}: a note explains where the resistor went");
            assert!(!lowered.circuit.devices.r.iter().any(|r| r.name == name), "{name}: a consumed pull is absent from the circuit");
            let id = circuit.comp(name).expect("the pull is a schematic component").id;
            assert!(lowered.index.command(&SimCmd::SetParam { comp: CompRef { board: BoardId(0), comp: id }, value: 20e3 }).is_empty(),
                "{name}: a consumed pull has no analog tuning command");
        }
    }
    assert!(bus.nets[3].pull.is_none(), "RST: no undeclared pull is invented");
    assert_eq!(lowered.diags.len(), 4, "only the four consumed pulls produce notes: {:?}", lowered.diags);
    assert!(lowered.tol.is_empty(), "consumed pull resistors leave no analog tolerance sites");
    assert!(lowered.plan.parts[1].is_empty(), "the bus part has no analog device");
    assert!(lowered.circuit.devices.c.iter().any(|c| c.name == "C1"), "supply-to-ground decoupling stays in the circuit");
    assert!(lowered.cosim_pins("U1").is_empty(), "the bus GPIOs are absent from the analog co-simulation pin table");
    let desc = bus.board(123);
    assert_eq!(desc.seed, 123, "board conversion retains the caller's seed");
    assert_eq!(desc.parts[0].refdes.as_ref(), "U2", "board conversion owns the component path");
    assert_eq!(desc.nets[0].pins, [PinRef::Chip("SDA".into()), PinRef::Part { part: 0, pin: "SDA".into() }],
        "board conversion joins chip and model pin by name");
    assert_eq!(desc.nets[0].pull, Pull::Up, "board conversion retains pull direction");
    assert_eq!(desc.nets[0].rise_ps, Some((5e3_f64 * 8e-12 * (1.0_f64 / (1.0 - 0.7)).ln() * 1e12).ceil() as u64),
        "SDA: 5 kOhm * 8 pF * ln(1 / (1 - 0.7)) rounds up to 48,159 ps");
    assert_eq!(desc.nets[1].rise_ps, Some(96_318),
        "SCL: 10 kOhm * 8 pF * ln(1 / (1 - 0.7)) rounds up to 96,318 ps; doubling resistance doubles the unrounded rise");
    assert_eq!(desc.nets[3].rise_ps, None, "RST: an absent pull leaves rise time unknown");
    let mut board = Netlist::new(desc).expect("the schematic description must construct a netlist");
    board.bind(&["SDA", "SCL", "INT", "RST"], 1_000_000).expect("the netlist must bind the named firmware pins");
}

fn field(c: &mut Circuit, key: &str, text: &str) {
    let fields = &mut c.comps[1].fields.0;
    fields.retain(|(k, _)| k != key);
    fields.push((key.into(), text.into()));
}

#[test]
fn unmodeled_pins_and_series_paths() {
    let (mut base, bench) = schematic();
    for name in ["FROM", "MIDDLE", "TO", "BIAS"] {
        base.nets.push(Net { id: NetId(base.nets.len() as u32), name: name.into(), ..base.nets[2].clone() });
    }
    let mut peer = base.comps[1].clone();
    peer.id = CompId(base.comps.len() as u32);
    peer.path = "U3".into();
    peer.refdes = peer.path.clone();
    // Use the part's address 22 beside U2's address 21.
    peer.fields.0.retain(|(key, _)| key != "bus.addr");
    peer.conns[0].1 = NetId(10);
    base.comps.push(peer);
    let first = &mut base.comps[1];
    first.conns[0].1 = NetId(8);
    let def = Arc::make_mut(&mut first.part);
    let bias = PinIx(def.pins.len() as u16);
    def.pins.push(PinDef { name: "BIAS".into(), kind: PinKind::Passive, ..def.pins[0].clone() });
    first.conns.push((bias, NetId(11)));
    for (path, sim, a, b, si, unit) in [
        ("Cseries", "capacitor", 8, 9, 1e-6, QtyUnit::Farad),
        ("Rseries", "resistor", 9, 10, 10e3, QtyUnit::Ohm),
        ("Cbias", "capacitor", 11, 0, 100e-9, QtyUnit::Farad),
    ] {
        let mut part = component(base.comps.len() as u32, path, sim, &[("1", a), ("2", b)]);
        value(&mut part, si, unit);
        base.comps.push(part);
    }

    let libs = ModelLibs::generic();
    let lowered = lower_circuit(&base, &bench, 0, &libs);
    let side = &lowered.firmware[0].bus;
    assert_eq!(side.parts.len(), 2, "both series-coupled parts lower: {:?}", lowered.diags);
    let desc = side.board(0);
    let from = desc.nets.iter().find(|n| n.name.as_ref() == "FROM").expect("the first endpoint names the bus net");
    assert_eq!(from.pins, [PinRef::Part { part: 0, pin: "RST".into() }, PinRef::Part { part: 1, pin: "RST".into() }],
        "the series path joins exactly the two model pins");
    Netlist::new(desc).expect("the two parts can share the bus after coupling");
    assert!(!side.nets.iter().any(|n| matches!(n.name.as_str(), "TO" | "MIDDLE" | "BIAS")),
        "the series path and the unmodeled pin introduce no extra bus nets");
    for message in ["U2: pin BIAS has no bus model; left to the circuit",
        "FROM: series coupling through Cseries, Rseries; component values are not simulated on the bus"] {
        assert_eq!(lowered.diags.iter().filter(|d| d.message == message).count(), 1, "one note explains {message}");
    }
    assert!(lowered.circuit.devices.c.iter().any(|c| c.name == "Cbias"), "the analog bias capacitor stays in the circuit");
    assert!(lowered.circuit.devices.c.iter().any(|c| c.name == "Cseries"), "the physical series capacitor stays in the circuit");
    assert!(lowered.circuit.devices.r.iter().any(|r| r.name == "Rseries"), "the physical series resistor stays in the circuit");

    let cases: &[(&str, fn(&mut Circuit), &str)] = &[
        ("extra pin on host GPIO", |c| c.comps[1].conns.last_mut().unwrap().1 = NetId(5),
            "U2: pin BIAS is not a model pin; not simulated"),
        ("extra pin on peer model pin", |c| c.comps[1].conns.last_mut().unwrap().1 = NetId(10),
            "U2: pin BIAS is not a model pin; not simulated"),
        ("middle net branches", |c| c.comps.push(component(c.comps.len() as u32, "J2", "passive", &[("1", 9)])),
            "FROM: Cseries couples a bus net to the circuit; not simulated yet"),
        ("chain ends on host GPIO", |c| c.comps.iter_mut().find(|p| p.path == "Rseries").unwrap().conns[1].1 = NetId(5),
            "FROM: Cseries couples a bus net to the circuit; not simulated yet"),
    ];
    for &(property, change, refusal) in cases {
        let mut circuit = base.clone();
        change(&mut circuit);
        let lowered = lower_circuit(&circuit, &bench, 0, &libs);
        assert!(lowered.diags.iter().any(|d| d.message == refusal), "{property}: missing {refusal}: {:?}", lowered.diags);
        assert!(!lowered.firmware[0].bus.parts.iter().any(|p| p.path == "U2"), "{property}: the affected model is refused");
        assert!(!lowered.diags.iter().any(|d| d.message.contains("series coupling through")), "{property}: no accepted series path");
    }
}

#[test]
fn bus_refusals_and_explicit_unknown_capacitance() {
    let (base, bench) = schematic();
    let libs = ModelLibs::generic();
    let cases: &[(&str, fn(&mut Circuit), &str)] = &[
        ("unknown key", |c| field(c, "model", "missing"), "U2: missing: unknown part model key; not simulated"),
        ("missing key", |c| Arc::make_mut(&mut c.comps[1].part).fields.0.retain(|(k, _)| k != "model"),
            "U2: no bus model key; not simulated"),
        ("missing parameter", |c| {
            c.comps[1].fields.0.retain(|(k, _)| k != "bus.addr");
            Arc::make_mut(&mut c.comps[1].part).fields.0.retain(|(k, _)| k != "bus.addr");
        }, "U2: CST820: missing parameter addr; not simulated"),
        ("extra parameter", |c| field(c, "bus.extra", "1"), "U2: CST820: unused parameter extra; not simulated"),
        ("non-number", |c| field(c, "bus.addr", "many"), "U2: bus.addr 'many' is not a number; not simulated"),
        ("invalid number", |c| field(c, "bus.addr", "128"), "U2: CST820: parameter addr must be an integer in 0..=127; not simulated"),
        ("non-finite number", |c| field(c, "bus.addr", "NaN"), "U2: CST820: parameter addr must be an integer in 0..=127; not simulated"),
        ("extra pin", |c| Arc::make_mut(&mut c.comps[1].part).pins[0].name = "EXTRA".into(),
            "U2: pin EXTRA is not a model pin; not simulated"),
        ("missing pin", |c| {
            Arc::make_mut(&mut c.comps[1].part).pins[0].name = "NC".into();
            c.comps[1].conns.retain(|(p, _)| *p != PinIx(0));
        }, "U2: no RST pin; not simulated"),
        ("coupled capacitor", |c| {
            let mut cap = component(c.comps.len() as u32, "C12", "capacitor", &[("1", 4), ("2", 0)]);
            value(&mut cap, 1e-9, QtyUnit::Farad);
            c.comps.push(cap);
        }, "INT: C12 couples a bus net to the circuit; not simulated yet"),
        ("opposing pulls", |c| {
            let mut r = component(c.comps.len() as u32, "R33", "resistor", &[("1", 2), ("2", 0)]);
            value(&mut r, 10e3, QtyUnit::Ohm);
            c.comps.push(r);
        }, "SDA: pull-up R29, R32 and pull-down R33 conflict; not simulated"),
        ("logic sharing", |c| c.comps.push(component(c.comps.len() as u32, "U3", "shift_latch8", &[("SER", 4)])),
            "INT: U3 shares a net with a bus part; not simulated yet"),
        ("all peers on a coupled net", |c| {
            let mut peer = c.comps[1].clone();
            peer.id = CompId(c.comps.len() as u32);
            peer.path = "U4".into();
            peer.refdes = "U4".into();
            c.comps.push(peer);
            let mut cap = component(c.comps.len() as u32, "C12", "capacitor", &[("1", 4), ("2", 0)]);
            value(&mut cap, 1e-9, QtyUnit::Farad);
            c.comps.push(cap);
        }, "INT: C12 couples a bus net to the circuit; not simulated yet"),
    ];
    for &(property, change, message) in cases {
        let mut c = base.clone();
        change(&mut c);
        let lowered = lower_circuit(&c, &bench, 0, &libs);
        assert!(lowered.diags.iter().any(|d| d.message == message), "{property}: exact refusal: {message}; got {:?}", lowered.diags);
        assert!(lowered.firmware[0].bus.parts.is_empty(), "{property}: the rejected part lowers nothing");
        assert!(lowered.firmware[0].bus.nets.is_empty(), "{property}: no orphan bus nets survive");
        assert!(lowered.plan.parts[1].is_empty(), "{property}: the rejected bus part has no analog model");
        assert!(lowered.circuit.devices.r.iter().any(|r| r.name == "R29"), "{property}: a rejected part consumes no pull");
    }
    for no_card in [false, true] {
        let mut c = base.clone();
        Arc::make_mut(&mut c.comps[1].part).gpio = if no_card { None } else { Some(card(None)) };
        let lowered = lower_circuit(&c, &bench, 0, &libs);
        assert_eq!(lowered.firmware[0].bus.parts.len(), 1, "missing c_pin is a note, not a refusal");
        for n in &lowered.firmware[0].bus.nets {
            assert_eq!(n.farads, None, "{}: an unknown contribution stays unknown", n.name);
            assert_eq!(lowered.diags.iter().filter(|d| d.message == format!("{}: no c_pin for U2; bus pin capacitance is unknown", n.name)).count(), 1,
                "{}: exactly one capacitance note names the missing participant", n.name);
        }
        assert!(lowered.firmware[0].bus.board(0).nets.iter().all(|n| n.rise_ps.is_none()),
            "an unknown c_pin leaves every affected net's rise time unknown");
    }
    let mut without_gpio = base.clone();
    without_gpio.comps[0].conns.retain(|(pin, _)| *pin != PinIx(0));
    let lowered = lower_circuit(&without_gpio, &bench, 0, &libs);
    let side = &lowered.firmware[0].bus;
    assert_eq!((side.nets[0].farads, side.nets[0].vih), (Some(3e-12), None),
        "SDA without a firmware GPIO retains pin capacitance but no input threshold");
    assert_eq!(side.board(0).nets[0].rise_ps, None,
        "a bus model's threshold is not available in its part data and must not be guessed");
    let mut counted = base.clone();
    counted.comps[0].conns.iter_mut().find(|(pin, _)| *pin == PinIx(3)).expect("RST is connected").1 = NetId(2);
    let lowered = lower_circuit(&counted, &bench, 0, &libs);
    let sda = &lowered.firmware[0].bus.nets[0];
    assert_eq!(sda.chip, ["SDA", "RST"], "all firmware pins sharing a bus net are retained in package order");
    assert!((sda.farads.expect("the three pins have cards") - 13e-12).abs() < 1e-24,
        "capacitance counts every pin, including two pins of the same part");
    Arc::make_mut(&mut counted.comps[0].part).gpio = None;
    Arc::make_mut(&mut counted.comps[1].part).gpio = None;
    let lowered = lower_circuit(&counted, &bench, 0, &libs);
    assert_eq!(lowered.diags.iter().filter(|d| d.message == "SDA: no c_pin for U1, U2; bus pin capacitance is unknown").count(), 1,
        "one note lists every missing part once, even when two of its pins share the net");

    let mut down = base.clone();
    down.comps[4].conns[1].1 = NetId(0);
    let lowered = lower_circuit(&down, &bench, 0, &libs);
    assert!(!lowered.firmware[0].bus.nets[2].pull.as_ref().expect("INT has its pull").high,
        "a resistor to ground is a pull-down");
    assert!(lowered.diags.iter().any(|d| d.message == "R31: pull-down of INT, digital side"), "a consumed pull-down is noted");
    assert_eq!(lowered.firmware[0].bus.board(0).nets[2].pull, Pull::Down, "pull-down direction survives board conversion");
    assert_eq!(lowered.firmware[0].bus.board(0).nets[2].rise_ps, None,
        "a pull-down has no pull-up rise time even with known capacitance and threshold");

    let mut c = base.clone();
    c.comps[1].conns.iter_mut().for_each(|(pin, n)| {
        if *pin == PinIx(0) { *n = NetId(1); }
        if *pin == PinIx(1) { *n = NetId(0); }
    });
    let lowered = lower_circuit(&c, &bench, 0, &libs);
    let side = &lowered.firmware[0].bus;
    assert_eq!((&side.parts[0].pins[2].1, &side.parts[0].pins[3].1), (&BusConn::Low, &BusConn::High),
        "model pins on their own ground and supply lower as ties");
    let desc = side.board(0);
    for (name, pull) in [("U2.INT", Pull::Down), ("U2.RST", Pull::Up)] {
        let n = desc.nets.iter().find(|n| n.name.as_ref() == name).expect("a tie becomes its own named net");
        assert_eq!((n.pull, n.pins.len()), (pull, 1), "{name}: the netlist represents a tie with a one-pin pull");
        assert_eq!(n.rise_ps, None, "{name}: a supply tie has no resistor rise time");
    }
    let mut board = Netlist::new(desc).expect("ties must produce a valid board description");
    board.bind(&["SDA", "SCL", "INT", "RST"], 1_000_000).expect("a board with tied pins must bind");
    let mut switched = bench.clone();
    switched.supplies.clear();
    let lowered = lower_circuit(&base, &switched, 0, &libs);
    assert_eq!(lowered.firmware[0].bus.parts.len(), 1, "an unheld rail is treated as held on the bus side");
    assert!(lowered.diags.iter().any(|d| d.message == "U2: supply pin VDD is on a rail not held by a supply; treated as held on the bus side"),
        "the assumed held rail is explicit in the diagnostics");
    let mut without_firmware = bench.clone();
    without_firmware.firmware.clear();
    let lowered = lower_circuit(&base, &without_firmware, 0, &libs);
    assert!(lowered.diags.iter().any(|d| d.message == "U2: a bus part runs beside the board's firmware; this bench runs none, so it is not simulated"),
        "a bus part without firmware is refused explicitly");
}
