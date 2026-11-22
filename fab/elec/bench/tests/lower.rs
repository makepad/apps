use fab_elec_bench::bench::{Firmware, ScopeProbe, Stub, Supply};
use fab_elec_bench::expect::Probe;
use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::input::{LedColor, LowerInput, LowerNet, LowerPart, LowerPin, PartKind};
use fab_elec_bench::logic::{LogicConn, LogicDriver, LogicKind};
use fab_elec_bench::{lower, lower_circuit, lower_input, BenchSpec, ModelLibs, SimCmd};
use fab_elec_circuit::{Circuit, Component, Net, NetKind, Quantity, QtyUnit, Value};
use fab_elec_core::ids::{BlockId, BoardId, ClassId, CompId, CompRef, NetId, NetRef, PinIx, PinRef};
use fab_elec_design::{Design, Tip};
use fab_elec_sim::live::{LiveCmd, Terminal};
use fab_elec_parts::{Fields, GpioCard, Limits, PartDef, PinDef, PinKind, VRange};
use fab_elec_sim::{run, Analysis, Control, GpioParams, PinMode, PinPull, Record, TranSpec};
use std::sync::Arc;

fn input_net(id: u32) -> NetRef {
    NetRef { board: BoardId(0), net: NetId(id) }
}

fn input_part(id: u32, path: &str, kind: PartKind, pins: &[(&str, u32)]) -> LowerPart {
    LowerPart {
        id: CompRef { board: BoardId(0), comp: CompId(id) }, path: path.into(), kind, tol: None,
        pins: pins.iter().map(|&(name, n)| LowerPin { name: name.into(), net: Some(input_net(n)), nc: false }).collect(),
        noise: false,
    }
}

fn led_resistance(design: &Design) -> f64 {
    design.unit(BoardId(0)).expect("ledctl design has board 0").circuit
        .comp("lamps[0].r").expect("ledctl design has lamp 0's resistor")
        .value.as_ref().expect("lamp resistor has a value")
        .qty.expect("lamp resistor value has an SI quantity").si
}

/// A driven-low GPIO has its low-side driver and pad leakage in parallel to VSS.
fn pin_conductance(libs: &ModelLibs) -> f64 {
    1.0 / libs.pin_ron + 1.0 / libs.pin_leak
}

fn pin_voltage(libs: &ModelLibs, current: f64) -> f64 {
    current / pin_conductance(libs)
}

fn on_current(libs: &ModelLibs, v33: f64, r: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, v33 / r);
    for _ in 0..64 {
        let i = 0.5 * (lo + hi);
        let volts = i * r + libs.led_vf(LedColor::Green, i).expect("generic library has green") + pin_voltage(libs, i);
        if volts < v33 {
            lo = i;
        } else {
            hi = i;
        }
    }
    0.5 * (lo + hi)
}

/// Scalar regulator output current: enable, smoothed dropout target and both current clamps.
/// The card uses the default voltage/resistance parameters and the part's current limit.
fn reg_io(vi: f64, vo: f64, ve: f64, ilim: f64) -> f64 {
    let smin = |a: f64, b: f64, w: f64| 0.5 * (a + b - ((a - b) * (a - b) + w * w).sqrt());
    let smax = |a: f64, b: f64, w: f64| -smin(-a, -b, w);
    let on = 0.5 * (1.0 + ((ve - 1.0) / 0.05).tanh());
    let target = on * smin(3.3, vi - 0.25, 0.02);
    let isink = 1e-3; // The default regulator card's sink limit.
    let (wl, ws) = (1e-3 * ilim, 1e-3 * isink);
    smax(smin((target - vo) / 0.05, ilim, wl), -isink * on, ws)
}

fn mean(record: &Record, values: &[f64], start: f64, end: f64) -> f64 {
    assert_eq!(record.at.len(), values.len(), "average has one value per recorded time");
    assert!(
        record.at.first().is_some_and(|&t| t <= start)
            && record.at.last().is_some_and(|&t| t >= end),
        "record covers the complete averaging window"
    );
    let mut area = 0.0;
    for (t, v) in record.at.windows(2).zip(values.windows(2)) {
        let a = t[0].max(start);
        let b = t[1].min(end);
        if b <= a {
            continue;
        }
        let at = |time| v[0] + (v[1] - v[0]) * (time - t[0]) / (t[1] - t[0]);
        area += 0.5 * (at(a) + at(b)) * (b - a);
    }
    area / (end - start)
}

#[test]
fn ledctl_lowers_and_solves_dc() {
    let design = fab_elec_fixtures::ledctl();
    let libs = ModelLibs::generic();
    let bench = ledctl_bench(10e3, [0.0; 8]);
    let mut lowered = lower(&design, BoardId(0), &bench, &libs);
    let circuit = &design.unit(BoardId(0)).expect("ledctl design has board 0").circuit;
    assert!(lowered.diags.is_empty(), "fixture lowers without diagnostics: {:?}", lowered.diags);
    let pins: usize = circuit.comps.iter().map(|comp| comp.part.pins.len()).sum();
    assert_eq!(lowered.index.counts(), (26, pins, 30), "fixture net, pin and part counts");
    assert_eq!(
        lowered.index.counts(),
        (lowered.plan.nets.len(), lowered.plan.pins.len(), lowered.plan.parts.len()),
        "value index and plan have identical dimensions"
    );
    assert_eq!(lowered.index.generation, design.generation, "value index retains the design generation");
    assert_eq!(lowered.leds.len(), 8, "every lamp's LED is a light tap");
    for tap in &lowered.leds {
        let net = lowered.plan.pin_net[tap.anode as usize].map(|n| lowered.plan.nets[n as usize].as_str());
        assert!(net.is_some_and(|n| n.starts_with("lamps[") && n.ends_with("].d.A")), "an LED tap reads its anode pin: {net:?}");
    }
    for (position, net) in circuit.nets.iter().enumerate() {
        assert_eq!(
            lowered.index.net(NetRef { board: BoardId(0), net: net.id }),
            Some(position),
            "design net {} retains its position in the value index", net.name
        );
    }
    for comp in &circuit.comps {
        if comp.part.name.starts_with("R_") {
            assert_eq!(
                lowered.index.command(&SimCmd::SetParam {
                    comp: CompRef { board: BoardId(0), comp: comp.id }, value: 1234.0,
                }),
                [LiveCmd::SetParam { generation: design.generation, device: comp.path.clone(), key: "value".into(), value: 1234.0 }],
                "resistor {} tunes its named kernel device", comp.path
            );
        }
    }
    let button = circuit.comp("btn.sw").expect("ledctl design has the button");
    assert_eq!(
        lowered.index.command(&SimCmd::SetParam {
            comp: CompRef { board: BoardId(0), comp: button.id }, value: 1.0,
        }),
        [LiveCmd::SetParam { generation: design.generation, device: "btn.sw#press".into(), key: "dc".into(), value: 1.0 }],
        "button command tunes its press source's DC level"
    );
    assert_eq!(lowered.index.momentary(), [CompRef { board: BoardId(0), comp: button.id }],
        "the button is the design's one momentary input: a view offers a press for it and nothing else");
    let led = circuit.comp("lamps[3].d").expect("ledctl design has LED 3");
    let anode = PinRef {
        board: BoardId(0),
        comp: led.id,
        pin: led.part.pin("A").expect("LED 3 has an anode pin"),
    };
    let pin = lowered.index.pin(anode).expect("LED 3's anode is indexed");
    assert_eq!(
        lowered.plan.pins[pin],
        vec![Terminal { device: "lamps[3].d".into(), terminal: 0 }],
        "LED 3's design anode maps only to diode terminal 0"
    );

    lowered.circuit.opts.reltol = 1e-10;
    lowered.circuit.opts.vntol = 1e-13;
    let mut record = Record::default();
    run(&lowered.circuit, &Analysis::Op, &mut record, &Control::default())
        .unwrap_or_else(|diag| panic!("fixture DC operating point converges: {diag}"));
    let v33 = record.last("3V3");
    assert!((3.2..=3.3).contains(&v33), "regulated 3V3 rail is in range: {v33}");
    let r = led_resistance(&design);
    let expected = on_current(&libs, v33, r);
    let mut leds = 0.0;
    for i in 0..8 {
        let current = (v33 - record.last(&format!("lamps[{i}].d.A"))) / r;
        assert!(
            (current / expected - 1.0).abs() < 1e-7,
            "LED {i} DC current agrees with diode and GPIO closed form: {current} vs {expected}"
        );
        let vpin = record.last(&format!("LED{i}"));
        let want_pin = pin_voltage(&libs, expected);
        assert!(
            (vpin / want_pin - 1.0).abs() < 1e-7,
            "LED {i} sink voltage includes the GPIO driver and pad leakage: {vpin} vs {want_pin}"
        );
        leds += current;
    }
    let pullups: f64 = ["BTN", "mcu.NRST", "mcu.SWDIO"].into_iter()
        .map(|pin| (v33 - record.last(pin)) / libs.pin_pull).sum();
    let vbus = record.last("VBUS");
    let io = reg_io(vbus, v33, vbus, 0.6);
    // Output pad leakage goes to VSS and is already included in the LED currents.
    let load = leds + libs.mcu_iq + pullups;
    assert!(
        (io - load).abs() < 1e-9,
        "regulator output supplies LEDs, MCU and pull-ups within 1 nA: {io} vs {load}"
    );
}

#[test]
fn a_circuit_of_its_own_lowers_like_its_unit() {
    let design = fab_elec_fixtures::ledctl();
    let libs = ModelLibs::generic();
    let bench = ledctl_bench(10e3, [0.3; 8]);
    let mut circuit = design.unit(BoardId(0)).expect("ledctl design has board 0").circuit.as_ref().clone();
    circuit.unit = BoardId(900);
    assert!(design.unit(circuit.unit).is_none(), "the circuit's own scope belongs to no design unit");
    let unit = lower(&design, BoardId(0), &bench, &libs);
    let own = lower_circuit(&circuit, &bench, design.generation, &libs);
    assert!(unit.diags.is_empty(), "design unit lowers without diagnostics: {:?}", unit.diags);
    assert!(own.diags.is_empty(), "circuit of its own lowers without diagnostics: {:?}", own.diags);
    assert_eq!(own.plan.nets, unit.plan.nets, "own scope keeps the design unit's net names and order");
    assert_eq!(own.circuit.nodes, unit.circuit.nodes, "own scope keeps the design unit's kernel nodes and order");
    assert_eq!(own.plan.parts, unit.plan.parts, "own scope keeps the design unit's device lists and order");
    assert_eq!(own.circuit.devices, unit.circuit.devices, "own scope keeps the design unit's device parameters and connections");
    assert_eq!(own.index.generation, design.generation, "own scope retains the supplied generation");

    let mut unit_record = Record::default();
    run(&unit.circuit, &Analysis::Op, &mut unit_record, &Control::default())
        .unwrap_or_else(|diag| panic!("design unit's DC operating point converges: {diag}"));
    let mut own_record = Record::default();
    run(&own.circuit, &Analysis::Op, &mut own_record, &Control::default())
        .unwrap_or_else(|diag| panic!("own scope's DC operating point converges: {diag}"));
    for node in &unit.circuit.nodes {
        let expected = unit_record.last(&node.name);
        let actual = own_record.last(&node.name);
        assert!((actual - expected).abs() <= 1e-12,
            "own scope's node {} agrees with the design unit within 1e-12 V: {actual} vs {expected}", node.name);
    }

    let net = circuit.net("3V3").expect("ledctl circuit has the 3V3 net").id;
    assert!(own.index.net(NetRef { board: BoardId(900), net }).is_some(),
        "own scope's 3V3 net is numbered under BoardId(900)");
    assert!(own.index.net(NetRef { board: BoardId(0), net }).is_none(),
        "own scope's index does not resolve the same net id under BoardId(0)");

    let mut foreign_bench = bench.clone();
    foreign_bench.supplies.push(Supply {
        net: NetRef { board: BoardId(0), net }, v: 3.3, limit: None, ramp: None,
    });
    let foreign = lower_circuit(&circuit, &foreign_bench, design.generation, &libs);
    assert!(foreign.diags.iter().any(|diag| diag.path == "bench.supplies[0]" && diag.message.contains("outside the input")),
        "a supply from another scope is diagnosed at bench.supplies[0]: {:?}", foreign.diags);
    assert_eq!(foreign.circuit.nodes, own.circuit.nodes, "a supply from another scope adds no node");
    assert_eq!(foreign.circuit.devices, own.circuit.devices, "a supply from another scope adds no device");

    let mut no_ground = circuit.clone();
    for net in &mut no_ground.nets {
        if net.kind == NetKind::Ground {
            net.kind = NetKind::Signal;
        }
    }
    let no_ground = lower_circuit(&no_ground, &bench, design.generation, &libs);
    assert!(no_ground.diags.iter().any(|diag| diag.message.contains("no ground net")),
        "a circuit without a ground-kind net reports no ground net: {:?}", no_ground.diags);
    assert_eq!(no_ground.plan.nets.first().map(String::as_str), Some("0"),
        "a circuit without a ground-kind net grounds its first net");
}

#[test]
fn led_pwm_average() {
    let design = fab_elec_fixtures::ledctl();
    let libs = ModelLibs::generic();
    let bench = ledctl_bench(10e3, [0.3; 8]);
    let lowered = lower(&design, BoardId(0), &bench, &libs);
    assert!(lowered.diags.is_empty(), "PWM fixture lowers without diagnostics: {:?}", lowered.diags);
    let mut record = Record::default();
    let analysis = Analysis::Tran(TranSpec { tstop: 1e-3, max_step: Some(2e-7), uic: false });
    run(&lowered.circuit, &analysis, &mut record, &Control::default())
        .unwrap_or_else(|diag| panic!("PWM transient completes with default options: {diag}"));
    let r = led_resistance(&design);
    let rail = record.v("3V3");
    let anode = record.v("lamps[0].d.A");
    let currents: Vec<_> = rail.iter().zip(&anode).map(|(v33, va)| (v33 - va) / r).collect();
    let v33 = mean(&record, &rail, 5e-4, 1e-3);
    let average = mean(&record, &currents, 5e-4, 1e-3);
    let expected = 0.7 * on_current(&libs, v33, r);
    assert!(
        (average / expected - 1.0).abs() < 0.01,
        "LED mean current is 70% of its closed-form on current within 1%: {average} vs {expected}"
    );
    for k in 5..=10 {
        let rising = k as f64 * 1e-4;
        assert!(
            record.at.iter().any(|&t| (t - rising).abs() < 1e-12),
            "PWM period {k} rising edge is a recorded breakpoint at {rising}"
        );
        if k < 10 {
            let falling = rising + 3e-5;
            assert!(
                record.at.iter().any(|&t| (t - falling).abs() < 1e-12),
                "PWM period {k} falling edge is a recorded breakpoint at {falling}"
            );
        }
    }
}

/// With no bench stubs (the app's bench before a bench is chosen) every MCU pin is an input, and
/// the debug header's lines touch only those inputs and the header: each pin's leakage, and each
/// net nothing else holds at DC its insulation resistance, give the operating point its DC paths.
#[test]
fn ledctl_without_stubs_solves_and_runs() {
    let design = fab_elec_fixtures::ledctl();
    let libs = ModelLibs::generic();
    let lowered = lower(&design, BoardId(0), &BenchSpec::default(), &libs);
    assert!(lowered.diags.is_empty(), "ledctl lowers with the default bench: {:?}", lowered.diags);
    let pin = lowered.circuit.devices.gpio.iter().find(|p| p.name == "mcu.u#PA13.io")
        .expect("an MCU input pin (PA13, on SWDIO) leaks to VSS");
    assert_eq!(pin.params.r_leak, libs.pin_leak, "the pin leakage is the library's");
    assert_eq!(pin.mode, PinMode::Input(PinPull::None), "the default bench leaves PA13 unpulled");
    let mut record = Record::default();
    let op = run(&lowered.circuit, &Analysis::Op, &mut record, &Control::default())
        .unwrap_or_else(|diag| panic!("the default bench's operating point converges: {diag}"));
    assert!(op.aid.is_some(), "the operating point reports how it converged");
    let swdio = record.last("mcu.SWDIO");
    assert!(swdio.is_finite() && swdio.abs() < 1e-3, "an undriven, unpulled SWDIO line rests at VSS through the leakage: {swdio}");
    let analysis = Analysis::Tran(TranSpec { tstop: 1e-4, max_step: Some(1e-5), uic: false });
    run(&lowered.circuit, &analysis, &mut Record::default(), &Control::default())
        .unwrap_or_else(|diag| panic!("the default bench's transient runs: {diag}"));
}

#[test]
fn stubs_and_firmware_for_a_missing_component_are_diagnosed() {
    let design = fab_elec_fixtures::ledctl();
    let bench = BenchSpec {
        stubs: vec![Stub { path: "gone.u".into(), pins: vec![], rail: None }],
        firmware: vec![Firmware {
            path: "missing.u".into(), doc: "missing.fw.splash".into(), params: vec![],
        }],
        ..BenchSpec::default()
    };
    let lowered = lower(&design, BoardId(0), &bench, &ModelLibs::generic());
    assert!(lowered.firmware.is_empty(), "a missing component never gets a firmware binding");
    assert!(lowered.diags.iter().any(|diag| {
        diag.part.is_none() && diag.path == "missing.u"
            && diag.message == "missing.u: firmware names no component in the scope; nothing runs it"
    }), "the missing firmware component is diagnosed at its requested path: {:?}", lowered.diags);
    assert!(lowered.diags.iter().any(|diag| {
        diag.part.is_none() && diag.path == "gone.u"
            && diag.message == "gone.u: stub names no component in the scope; its drives reach nothing"
    }), "the missing stubbed component is diagnosed at its requested path: {:?}", lowered.diags);
}

#[test]
fn firmware_chip_uses_component_then_part_model_and_diagnoses_absence() {
    let component = |id, path: &str, kind, pins: &[(&str, u32)]| Component {
        id: CompId(id), path: path.into(), refdes: path.into(), ref_kind: Default::default(),
        part: Arc::new(PartDef {
            name: path.into(), kind, symbol: Default::default(), footprint: None,
            body: None, emitter: None, gpio: None, fields: Fields::default(),
            pins: pins.iter().map(|&(name, _)| PinDef {
                name: name.into(), pads: Vec::new(), unit: 0, limits: None, alternates: Vec::new(), delay: None,
                kind: if ["VCC", "GND"].contains(&name) { PinKind::PowerIn } else { PinKind::Bidi },
            }).collect(),
        }),
        value: None, conns: pins.iter().enumerate().map(|(i, &(_, net))| (PinIx(i as u16), NetId(net))).collect(),
        nc: Vec::new(), block: BlockId(0), dnp: false, fields: Fields::default(),
    };
    let mcu = component(0, "mcu.u", fab_elec_parts::PartKind::Mcu,
        &[("VCC", 1), ("GND", 0), ("XTAL1", 2), ("XTAL2", 3)]);
    let mut crystal = component(1, "mcu.y", fab_elec_parts::PartKind::Crystal, &[("1", 2), ("2", 3)]);
    crystal.value = Some(Value {
        text: "16MHz".into(), qty: Some(Quantity { si: 16e6, unit: QtyUnit::Hertz }), tol: None,
    });
    let mut circuit = Circuit {
        comps: vec![mcu, crystal],
        nets: ["GND", "VCC", "X1", "X2"].into_iter().enumerate().map(|(i, name)| Net {
            id: NetId(i as u32), name: name.into(), class: ClassId(0), volts: None, scope: None,
            kind: if i == 0 { NetKind::Ground } else if i == 1 { NetKind::Power } else { NetKind::Signal },
        }).collect(),
        ..Circuit::default()
    };
    let bench = BenchSpec {
        supplies: vec![Supply { net: input_net(1), v: 5.0, limit: None, ramp: None }],
        firmware: vec![Firmware { path: "mcu.u".into(), doc: "chip.fw.splash".into(), params: Vec::new() }],
        ..BenchSpec::default()
    };
    let libs = ModelLibs::generic();
    let fields = |model: Option<&str>| Fields(model.into_iter().map(|model| ("model".into(), model.into())).collect());
    for (part_model, component_model, expected) in [
        (Some("ATMEGA328P"), None, Some("ATMEGA328P")),
        (Some("ATMEGA328P"), Some("ESP32-S3"), Some("ESP32-S3")),
        (None, Some("ESP32-S3"), Some("ESP32-S3")),
        (None, None, None),
    ] {
        let mcu = &mut circuit.comps[0];
        Arc::make_mut(&mut mcu.part).fields = fields(part_model);
        mcu.fields = fields(component_model);
        let lowered = lower_circuit(&circuit, &bench, 0, &libs);
        assert_eq!(lowered.firmware.len(), 1, "a missing chip still produces its firmware binding");
        assert_eq!(lowered.firmware[0].part, "mcu.u", "the binding names its component");
        assert_eq!(lowered.firmware[0].chip.as_deref(), expected,
            "component model {component_model:?} takes precedence over part model {part_model:?}");
        assert_eq!(lowered.diags.len(), usize::from(expected.is_none()),
            "only an absent model produces exactly one diagnostic: {:?}", lowered.diags);
        if expected.is_none() {
            let diag = &lowered.diags[0];
            assert_eq!(diag.part, Some(CompRef { board: BoardId(0), comp: CompId(0) }), "the problem selects the MCU");
            assert_eq!(diag.net, None, "the missing chip is a part problem");
            assert_eq!(diag.path, "mcu.u", "the problem locates the firmware part");
            assert_eq!(diag.message, "mcu.u: no chip model: its part data names none (`model`)",
                "the diagnostic names the part and its missing model field");
        }
    }
}

/// An MCU on an ideal clock lowers its crystal oscillator at DC: the crystal's nets, which only the
/// crystal (no model) and its load capacitors touch, get their DC path from the amplifier's bias
/// through the feedback resistor, so they need no insulation leak and get no note. At the operating
/// point both rest at half the supply, with the library's feedback resistance between the pins.
#[test]
fn an_ideal_clock_s_crystal_nets_rest_at_its_oscillator_s_bias() {
    let libs = ModelLibs::generic();
    let (net, part) = (input_net, input_part);
    let input = LowerInput {
        nets: ["0", "VCC", "X1", "X2"].into_iter().enumerate()
            .map(|(i, name)| LowerNet { id: net(i as u32), name: name.into(), supply: false }).collect(),
        ground: net(0),
        supplies: vec![Supply { net: net(1), v: 5.0, limit: None, ramp: None }],
        sources: Vec::new(),
        firmware: Vec::new(),
        probes: Vec::new(),
        parts: vec![
            part(0, "u", PartKind::Firmware { chip: Some("ATMEGA328P".into()), rails: vec![0, 1, 2, 3], crystal: vec![2, 3], pads: libs.gpio_params(), c_pin: None, supply: None },
                &[("VCC", 1), ("GND", 0), ("PB6", 2), ("PB7", 3)]),
            part(1, "y", PartKind::Passive, &[("1", 2), ("2", 3), ("3", 0)]),
            part(2, "c1", PartKind::Capacitor { farads: 22e-12 }, &[("1", 2), ("2", 0)]),
            part(3, "c2", PartKind::Capacitor { farads: 22e-12 }, &[("1", 3), ("2", 0)]),
        ],
    };
    let lowered = lower_input(&input, 0, &libs);
    assert!(lowered.diags.is_empty(), "the crystal's nets are held by the oscillator, never noted: {:?}", lowered.diags);
    let r = &lowered.circuit.devices.r;
    assert!(!r.iter().any(|r| r.name.ends_with("#leak")), "no net needs the insulation leak: {:?}",
        r.iter().map(|r| &r.name).collect::<Vec<_>>());
    let feedback: f64 = r.iter().filter(|r| r.name == "u#PB6.rf" || r.name == "u#PB7.rf").map(|r| r.r).sum();
    assert_eq!(feedback, libs.osc_rf, "the crystal pins see the feedback resistor between them");
    let mut record = Record::default();
    run(&lowered.circuit, &Analysis::Op, &mut record, &Control::default())
        .unwrap_or_else(|diag| panic!("the operating point converges: {diag}"));
    let vcc = record.last("VCC");
    assert!(vcc > 4.9, "the supply holds VCC: {vcc}");
    for x in ["X1", "X2"] {
        let v = record.last(x);
        assert!((v - vcc / 2.0).abs() < 1e-6, "{x} rests at half the supply ({}): {v}", vcc / 2.0);
    }
}

/// Two chained shift registers beside an MCU running firmware: the nets only the MCU's pins and
/// their inputs share (data, clock, latch, the cascade) are digital and lower nothing, so their
/// edges never reach the circuit; inputs on the registers' own supply and ground are tied; each
/// output on another net is a GPIO pad on the register's supply and a co-simulation row of the MCU
/// beside the MCU's own pins. Nothing is noted.
#[test]
fn shift_registers_beside_firmware_lower_only_their_outputs() {
    let libs = ModelLibs::generic();
    let (net, part) = (input_net, input_part);
    let names = ["0", "VCC", "DATA", "CLK", "LATCH", "CHAIN", "LED0", "LED15", "PD6"];
    // a register's pins: those not named are unconnected
    let register = |id, path, wired: &[(&str, u32)]| {
        let mut register = part(id, path, PartKind::Logic { kind: LogicKind::ShiftLatch8, pads: libs.gpio_params() },
            &[("SRCLK", 3), ("RCLK", 4), ("OE", 0), ("SRCLR", 1), ("VCC", 1), ("GND", 0)]);
        for &(pin, _) in LogicKind::ShiftLatch8.pins() {
            if !register.pins.iter().any(|p| p.name == pin) {
                let net = wired.iter().find(|w| w.0 == pin).map(|w| net(w.1));
                register.pins.push(LowerPin { name: pin.into(), net, nc: false });
            }
        }
        register
    };
    let input = LowerInput {
        nets: names.into_iter().enumerate().map(|(i, name)| LowerNet { id: net(i as u32), name: name.into(), supply: false }).collect(),
        ground: net(0),
        supplies: vec![Supply { net: net(1), v: 5.0, limit: None, ramp: None }],
        sources: Vec::new(),
        firmware: vec![(Firmware { path: "u".into(), doc: "fw.splash".into(), params: Vec::new() }, Some(16e6))],
        probes: Vec::new(),
        parts: vec![
            part(0, "u", PartKind::Firmware { chip: Some("ATMEGA328P".into()), rails: vec![0, 1], crystal: Vec::new(), pads: libs.gpio_params(), c_pin: None, supply: None },
                &[("VCC", 1), ("GND", 0), ("PB3", 2), ("PB5", 3), ("PB2", 4), ("PD6", 8)]),
            register(1, "s1", &[("SER", 2), ("QA", 6), ("QHS", 5)]),
            register(2, "s2", &[("SER", 5), ("QH", 7)]),
            part(3, "r0", PartKind::Resistor { ohms: 1e3 }, &[("1", 6), ("2", 0)]),
            part(4, "r15", PartKind::Resistor { ohms: 1e3 }, &[("1", 7), ("2", 0)]),
            part(5, "r6", PartKind::Resistor { ohms: 1e3 }, &[("1", 8), ("2", 0)]),
            part(6, "j", PartKind::Passive, &[("1", 2), ("2", 3)]),
        ],
    };
    let lowered = lower_input(&input, 0, &libs);
    assert!(lowered.diags.is_empty(), "nothing is noted: {:?}", lowered.diags);

    let gpio: Vec<&str> = lowered.circuit.devices.gpio.iter().map(|g| g.name.as_str()).collect();
    for pin in ["PB3", "PB5", "PB2"] {
        assert!(!gpio.contains(&format!("u#{pin}.io").as_str()), "{pin}: a pin on a digital net gets no pad: {gpio:?}");
    }
    assert!(gpio.contains(&"u#PD6.io"), "PD6, on an analog net, keeps its pad: {gpio:?}");
    assert!(gpio.contains(&"s1#QA.io") && gpio.contains(&"s2#QH.io"), "outputs on analog nets are pads: {gpio:?}");
    assert!(!gpio.iter().any(|g| g.contains("#QHS") || g.contains("#SER")), "the cascade and inputs lower nothing: {gpio:?}");
    let touches = |node: &str| {
        let n = lowered.circuit.nodes.iter().position(|n| n.name == node).expect("the node exists") as u32;
        lowered.circuit.devices.c.iter().any(|c| c.a == n || c.b == n) || lowered.circuit.devices.gpio.iter().any(|g| g.pad == n)
    };
    for node in ["DATA", "CLK", "LATCH", "CHAIN"] {
        assert!(!touches(node), "{node}: a digital net has no pad and no pin capacitance");
    }

    let fw = &lowered.firmware[0];
    assert!(fw.bus.parts.is_empty() && fw.bus.nets.is_empty(), "a logic-only scope has an empty bus side");
    let nets: Vec<(&str, &LogicDriver)> = fw.logic.nets.iter().map(|n| (n.name.as_str(), &n.driver)).collect();
    assert_eq!(nets, [
        ("DATA", &LogicDriver::Machine("PB3".into())), ("CLK", &LogicDriver::Machine("PB5".into())),
        ("LATCH", &LogicDriver::Machine("PB2".into())), ("CHAIN", &LogicDriver::Part { part: 0, pin: "QHS".into() }),
    ], "the digital nets and their drivers, in net order");
    let s1 = &fw.logic.parts[0];
    let conn = |name: &str| &s1.pins.iter().find(|(pin, _)| pin == name).expect("a logic pin").1;
    assert_eq!((conn("SER"), conn("OE"), conn("SRCLR")), (&LogicConn::Net(0), &LogicConn::Tied(false), &LogicConn::Tied(true)),
        "an input reads its digital net, its own ground low and its own supply high");
    assert_eq!((conn("QA"), conn("QB")), (&LogicConn::Out("s1.QA".into()), &LogicConn::Open), "an output into the circuit is a row; an absent one is open");
    let rows: Vec<String> = lowered.cosim_pins("u").into_iter().map(|row| row.name).collect();
    assert_eq!(rows, ["PD6", "s1.QA", "s2.QH"], "the MCU's rows: its own pins, then the registers' outputs");
}

/// A voltage probe's tip load lowers as its R and C in parallel across the probe's points, named by
/// what the probe reads. A resistive tip is a floating net's DC path (no insulation leak, no note);
/// a capacitive one slows the probed node by the RC it adds. An ideal tip and a bandwidth lower
/// nothing, so their edits leave the circuit as it was (a host retunes nothing), while a load's
/// edit changes the circuit's structure (a host's Replace).
#[test]
fn a_probe_s_tip_load_lowers_across_its_points_and_an_ideal_tip_lowers_nothing() {
    let libs = ModelLibs::generic();
    let (net, part) = (input_net, input_part);
    // IN, a 5 V supply ramped in 1 ns, through 1 kohm to N, which 1 Mohm holds to ground; F sits
    // between 22 pF to N and 22 pF to ground, with no DC path of its own
    let input = |probes: Vec<ScopeProbe>| LowerInput {
        nets: ["0", "IN", "N", "F"].into_iter().enumerate()
            .map(|(i, name)| LowerNet { id: net(i as u32), name: name.into(), supply: false }).collect(),
        ground: net(0),
        supplies: vec![Supply { net: net(1), v: 5.0, limit: None, ramp: Some(1e-9) }],
        sources: Vec::new(),
        firmware: Vec::new(),
        probes,
        parts: vec![
            part(0, "r1", PartKind::Resistor { ohms: 1e3 }, &[("1", 1), ("2", 2)]),
            part(1, "r2", PartKind::Resistor { ohms: 1e6 }, &[("1", 2), ("2", 0)]),
            part(2, "c1", PartKind::Capacitor { farads: 22e-12 }, &[("1", 2), ("2", 3)]),
            part(3, "c2", PartKind::Capacitor { farads: 22e-12 }, &[("1", 3), ("2", 0)]),
        ],
    };
    let probe = |probe: Probe, r: Option<f64>, c: Option<f64>, bw: Option<f64>| {
        ScopeProbe { probe, label: String::new(), tip: Tip { r, c, bw } }
    };

    let bare = lower_input(&input(Vec::new()), 0, &libs);
    let ideal = lower_input(&input(vec![
        probe(Probe::V(net(2)), None, None, Some(1e6)),
        probe(Probe::VDiff(net(1), net(2)), None, None, None),
    ]), 0, &libs);
    assert!(bare.circuit.value_changes(&ideal.circuit).is_some_and(|changes| changes.is_empty()),
        "an ideal tip and a bandwidth lower nothing: the circuit is the probeless one");
    assert_eq!(ideal.diags.iter().map(|d| d.path.as_str()).collect::<Vec<_>>(), ["F"],
        "without a resistive tip F rests on its insulation leak: {:?}", ideal.diags);

    let loaded = lower_input(&input(vec![
        probe(Probe::V(net(2)), None, Some(1e-9), Some(1e6)),
        probe(Probe::V(net(3)), Some(10e6), None, None),
        probe(Probe::VDiff(net(1), net(2)), Some(100e6), None, None),
    ]), 0, &libs);
    assert!(loaded.diags.is_empty(), "the resistive tip is F's DC path: {:?}", loaded.diags);
    assert!(ideal.circuit.value_changes(&loaded.circuit).is_none(), "a load's edit changes the circuit's structure");
    let c = &loaded.circuit;
    let ends = |a: u32, b: u32| [c.nodes[a as usize].name.as_str(), c.nodes[b as usize].name.as_str()];
    let rs: Vec<_> = c.devices.r.iter().filter(|r| r.name.starts_with("bench#probe:"))
        .map(|r| (r.name.as_str(), ends(r.a, r.b), r.r)).collect();
    assert_eq!(rs, [("bench#probe:F.r", ["F", "0"], 10e6), ("bench#probe:IN-N.r", ["IN", "N"], 100e6)],
        "a single-ended tip's resistance goes to ground, a differential one's between its nets");
    let cs: Vec<_> = c.devices.c.iter().filter(|c| c.name.starts_with("bench#probe:"))
        .map(|cap| (cap.name.as_str(), ends(cap.a, cap.b), cap.c)).collect();
    assert_eq!(cs, [("bench#probe:N.c", ["N", "0"], 1e-9)], "the tip capacitance is N's to ground");
    assert!(!c.devices.r.iter().any(|r| r.name.ends_with("#leak")), "no net needs the insulation leak");

    // N charges through 1 kohm || 1 Mohm into the tip's 1 nF (and c1 in series with c2, 11 pF):
    // at one time constant it is 1 - 1/e of its final value
    let tau = (1e3 * 1e6 / (1e3 + 1e6)) * (1e-9 + 11e-12);
    let mut record = Record::default();
    let tran = Analysis::Tran(TranSpec { tstop: 5.0 * tau, max_step: Some(tau / 50.0), uic: false });
    run(&loaded.circuit, &tran, &mut record, &Control::default())
        .unwrap_or_else(|diag| panic!("the loaded bench runs: {diag}"));
    let v_final = 5.0 * 1e6 / (1e3 + 1e6);
    let at_tau = record.at_time("N", tau) / v_final;
    assert!((at_tau - (1.0 - (-1.0f64).exp())).abs() < 0.02, "N reaches 1 - 1/e at the tip's time constant: {at_tau}");
}

/// An MCU whose part carries a GPIO card lowers every pad with the card's drivers, pull, edge and
/// thresholds, its leakage rated at the highest voltage its supply pin is rated for, and its pin
/// table carries the card's thresholds; without a rated supply the leakage, and without an edge the
/// edge, are the library's. The same part without a card lowers the library's generic profile.
#[test]
fn a_carded_mcu_s_pads_use_its_card() {
    let libs = ModelLibs::generic();
    let bench = ledctl_bench(10e3, [0.7; 8]);
    let card = GpioCard { r_high: 26.0, r_low: 23.5, pull_up: 36e3, vih: 0.524, vil: 0.418, leak: 1e-6, edge: Some(2e-9), c_pin: Some(8e-12) };
    let with = |card: Option<GpioCard>, rated: Option<f64>| {
        let mut design = fab_elec_fixtures::ledctl();
        let unit = design.units.iter_mut().find(|unit| unit.id == BoardId(0)).expect("ledctl has board 0");
        let comp = Arc::make_mut(&mut unit.circuit).comps.iter_mut().find(|comp| comp.path == "mcu.u")
            .expect("ledctl has its MCU");
        let part = Arc::make_mut(&mut comp.part);
        part.gpio = card;
        let supply = ["VDD", "VCC", "VIO"].iter()
            .find_map(|name| part.pins.iter().position(|pin| pin.name.eq_ignore_ascii_case(name)))
            .expect("the MCU has a supply pin");
        part.pins[supply].limits = rated.map(|max| Limits { volts: Some(VRange { min: 1.8, max }), current: None, power: None });
        let lowered = lower(&design, BoardId(0), &bench, &libs);
        assert!(lowered.diags.is_empty(), "ledctl lowers: {:?}", lowered.diags);
        let pads: Vec<GpioParams> = lowered.circuit.devices.gpio.iter()
            .filter(|g| g.name.starts_with("mcu.u#")).map(|g| g.params).collect();
        assert!(pads.len() >= 8, "the MCU lowers its signal pins as pads: {}", pads.len());
        (pads, lowered.cosim_pins("mcu.u"))
    };

    let (pads, table) = with(Some(card.clone()), Some(3.6));
    let carded = GpioParams { r_on_h: 26.0, r_on_l: 23.5, r_pull: 36e3, r_leak: 3.6 / 1e-6, t_edge: 2e-9, vih: 0.524, vil: 0.418 };
    for (i, pad) in pads.iter().enumerate() {
        assert_eq!(*pad, carded, "pad {i} takes the card, its leakage at the supply's rated 3.6 V; c_pin does not enter GpioParams");
    }
    assert_eq!(table.len(), pads.len(), "every pad is a pin table row");
    for row in &table {
        assert_eq!((row.vih, row.vil), (0.524, 0.418), "{} carries the card's thresholds", row.name);
        assert_eq!(row.vdd, "3V3", "{} names its supply node", row.name);
    }

    let (pads, _) = with(Some(GpioCard { edge: None, ..card }), None);
    let unrated = GpioParams { r_leak: libs.pin_leak, t_edge: libs.pin_edge, ..carded };
    assert!(pads.iter().all(|pad| *pad == unrated), "without a rated supply or an edge, the library's leakage and edge: {:?}", pads[0]);

    let (pads, table) = with(None, Some(3.6));
    assert!(pads.iter().all(|pad| *pad == libs.gpio_params()), "an uncarded part takes the generic profile: {:?}", pads[0]);
    assert!(table.iter().all(|row| (row.vih, row.vil) == (libs.pin_vih, libs.pin_vil)), "and the generic thresholds");
}

#[test]
fn a_parts_supply_field_selects_a_nonstandard_io_rail() {
    let mut design = fab_elec_fixtures::ledctl();
    let unit = design.units.iter_mut().find(|u| u.id == BoardId(0)).expect("fixture has board 0");
    let comp = Arc::make_mut(&mut unit.circuit).comps.iter_mut().find(|c| c.path == "mcu.u")
        .expect("fixture has its MCU");
    let part = Arc::make_mut(&mut comp.part);
    let pin = part.pins.iter_mut().find(|p| p.name == "VDD").expect("fixture MCU has VDD");
    pin.name = "IO_POWER".into();
    part.fields.0.push(("supply".into(), "IO_POWER".into()));
    let bench = ledctl_bench(10e3, [0.0; 8]);
    let libs = ModelLibs::generic();
    let with = lower(&design, BoardId(0), &bench, &libs);
    assert!(!with.diags.iter().any(|d| d.message.contains("stub needs a supply pin")),
        "the named I/O supply must lower: {:?}", with.diags);
    assert!(with.circuit.devices.gpio.iter().any(|p| p.name.starts_with("mcu.u#")),
        "the named supply must produce the MCU's GPIO pads");
    let unit = design.units.iter_mut().find(|u| u.id == BoardId(0)).expect("fixture keeps board 0");
    let comp = Arc::make_mut(&mut unit.circuit).comps.iter_mut().find(|c| c.path == "mcu.u")
        .expect("fixture keeps its MCU");
    Arc::make_mut(&mut comp.part).fields.0.retain(|(key, _)| key != "supply");
    let without = lower(&design, BoardId(0), &bench, &libs);
    assert!(without.diags.iter().any(|d| d.message.contains("stub needs a supply pin")),
        "without the field a nonstandard supply must still be diagnosed: {:?}", without.diags);
}
