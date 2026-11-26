use fab_elec_bench::bench::{BenchSource, SourceKind, Stub, Supply};
use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::input::{Drive, LowerInput, LowerNet, LowerPart, LowerPin, PartKind};
use fab_elec_bench::{lower, lower_input, ModelLibs};
use fab_elec_core::ids::{BoardId, CompId, CompRef, NetId, NetRef, PinRef};
use fab_elec_sim::live::Terminal;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::{run, Analysis, Control, Record, SimCircuit, TranSpec};

fn net(id: u32) -> NetRef {
    NetRef { board: BoardId(0), net: NetId(id) }
}

fn resistor_input(b: u32, sources: Vec<BenchSource>) -> LowerInput {
    LowerInput {
        nets: ["0", "a", "b"].into_iter().enumerate()
            .map(|(i, name)| LowerNet { id: net(i as u32), name: name.into(), supply: false, heard: false }).collect(),
        ground: net(0),
        parts: vec![LowerPart {
            id: CompRef { board: BoardId(0), comp: CompId(0) },
            path: "r".into(),
            kind: PartKind::Resistor { ohms: 1e3 },
            tol: None,
            pins: vec![
                LowerPin { name: "1".into(), net: Some(net(1)), nc: false },
                LowerPin { name: "2".into(), net: Some(net(b)), nc: false },
            ],
            noise: false,
        }],
        supplies: Vec::new(),
        sources,
        firmware: Vec::new(),
        probes: Vec::new(),
    }
}

fn simulate(circuit: &SimCircuit, analysis: Analysis, context: &str) -> Result<Record, String> {
    let mut record = Record::default();
    run(circuit, &analysis, &mut record, &Control::default())
        .map_err(|diag| format!("{context}: {diag}"))?;
    Ok(record)
}

fn last_voltage(record: &Record, name: &str) -> Result<f64, String> {
    let column = record.names.iter().position(|n| n == name)
        .ok_or_else(|| format!("record is missing net {name}"))?;
    record.x.last().and_then(|row| row.get(column)).copied()
        .ok_or_else(|| format!("record has no final voltage for {name}"))
}

#[test]
fn bench_stimulus_lowers_and_drives() -> Result<(), String> {
    let design = fab_elec_fixtures::ledctl();
    let libs = ModelLibs::generic();
    let scope = BoardId(0);
    let circuit = &design.unit(scope).ok_or("ledctl is missing board 0")?.circuit;
    let vbus = NetRef { board: scope, net: circuit.net("VBUS").ok_or("ledctl is missing VBUS")?.id };
    let bench = ledctl_bench(1e3, [0.3; 8]);
    let baseline = lower(&design, scope, &bench, &libs);
    assert!(baseline.diags.is_empty(), "unchanged ledctl stimulus has diagnostics: {:?}", baseline.diags);
    for name in ["mcu.u#iq", "mcu.u#PA0.io", "usb.j#vbus", "usb.j#cable"] {
        assert!(baseline.plan.parts.iter().flatten().any(|device| device == name),
            "unchanged ledctl lowering is missing device {name}");
    }

    let mut ramped_bench = bench.clone();
    ramped_bench.supplies.push(Supply { net: vbus, v: 4.5, limit: None, ramp: Some(1e-4) });
    let ramped = lower(&design, scope, &ramped_bench, &libs);
    assert!(ramped.diags.is_empty(), "ramped USB supply has diagnostics: {:?}", ramped.diags);
    assert_eq!(ramped.plan, baseline.plan, "ramped USB supply changed device names, order or pin bindings");
    let record = simulate(&ramped.circuit, Analysis::Op, "ramped USB supply operating point")?;
    let start = last_voltage(&record, "VBUS")?;
    assert!(start.abs() < 0.1, "ramped supply OP must start near 0 V, got VBUS={start} V");
    let record = simulate(&ramped.circuit,
        Analysis::Tran(TranSpec { tstop: 1e-3, max_step: Some(2e-6), uic: false }),
        "ramped USB supply transient to 1 ms")?;
    let end = record.at.last().copied().ok_or("ramped supply transient has no times")?;
    assert!(end >= 1e-3, "ramped supply transient ended at {end} s before 1 ms");
    let bus = last_voltage(&record, "VBUS")?;
    let rail = last_voltage(&record, "3V3")?;
    assert!((bus - 4.5).abs() < 0.05, "ramped VBUS must finish within 50 mV of 4.5 V, got {bus} V");
    assert!((rail / 3.3 - 1.0).abs() < 0.02, "ramped 3V3 must finish within 2% of 3.3 V, got {rail} V");

    let mut limited_bench = ledctl_bench(1e3, [0.0; 8]);
    limited_bench.supplies.push(Supply { net: vbus, v: 5.0, limit: Some(5e-3), ramp: None });
    let limited = lower(&design, scope, &limited_bench, &libs);
    assert!(limited.diags.is_empty(), "limited USB supply has diagnostics: {:?}", limited.diags);
    let usb = circuit.comp("usb.j").ok_or("ledctl is missing usb.j")?;
    let pin = PinRef { board: scope, comp: usb.id, pin: usb.part.pin("VBUS").ok_or("usb.j is missing VBUS")? };
    let pin_index = limited.index.pin(pin).ok_or("usb.j.VBUS has no value index")?;
    assert_eq!(limited.plan.pins.get(pin_index),
        Some(&vec![Terminal { device: "usb.j#limit".into(), terminal: 2 }]),
        "usb.j.VBUS must read the limiter output terminal");
    assert!(!limited.circuit.devices.r.iter().any(|r| r.name == "usb.j#cable"),
        "limited USB supply must replace the cable resistance with the limiter's output resistance");
    let mut live = LiveSim::new(limited.circuit.clone(), limited.plan.clone(), limited.index.generation,
        LiveSpec { playing: false, reltol: Some(1e-8), ..LiveSpec::default() })
        .map_err(|diag| format!("5 mA USB supply operating point: {diag}"))?;
    let values = live.values(1.0);
    let current = f64::from(limited.index.i(&values, pin).ok_or("usb.j.VBUS has no pin current")?);
    let bus = f64::from(limited.index.v(&values, vbus).ok_or("limited supply has no VBUS voltage")?);
    assert!((current / -5e-3 - 1.0).abs() < 0.02,
        "USB VBUS pin current must be -5 mA within 2%, got {current} A at VBUS={bus} V");
    assert!((0.0..4.5).contains(&bus), "5 mA supply must droop well below 5 V, got VBUS={bus} V");

    // Hold the LEDs off to measure the stub rail without the LED load's voltage drop.
    let mut stub_bench = ledctl_bench(1e3, [1.0; 8]);
    stub_bench.stubs.push(Stub {
        path: "reg.u".into(), pins: vec![("VOUT".into(), Drive::Level(true))], rail: Some(vbus),
    });
    let stubbed = lower(&design, scope, &stub_bench, &libs);
    assert_eq!(stubbed.diags.len(), 1, "regulator stub must only report model replacement: {:?}", stubbed.diags);
    let diag = stubbed.diags.first().ok_or("regulator stub is missing its replacement diagnostic")?;
    assert_eq!(diag.path, "reg.u", "replacement diagnostic must name reg.u: {diag:?}");
    assert!(diag.message.contains("its regulator model is replaced by the bench stub"),
        "regulator replacement diagnostic has the wrong message: {diag:?}");
    assert!(!stubbed.circuit.devices.reg.iter().any(|reg| reg.name == "reg.u"),
        "stubbed reg.u must not retain its regulator device");
    let record = simulate(&stubbed.circuit, Analysis::Op, "regulator stub operating point")?;
    let bus = last_voltage(&record, "VBUS")?;
    let rail = last_voltage(&record, "3V3")?;
    assert!((rail - bus).abs() < 0.1,
        "stubbed VOUT must follow its VBUS HIGH rail within 0.1 V, got 3V3={rail} V and VBUS={bus} V");
    stub_bench.stubs.last_mut().ok_or("regulator stub disappeared")?.rail = None;
    let no_rail = lower(&design, scope, &stub_bench, &libs);
    assert!(no_rail.diags.iter().any(|diag| diag.path == "reg.u" && diag.message.contains("stub needs a supply pin")),
        "regulator stub without a rail must diagnose the missing supply: {:?}", no_rail.diags);
    assert!(!no_rail.plan.parts.iter().flatten().any(|name| name.starts_with("reg.u#")),
        "regulator stub without a HIGH rail must lower no reg.u# devices");

    let divider = resistor_input(2, vec![
        BenchSource { a: net(1), b: net(0), kind: SourceKind::Vdc(2.0) },
        BenchSource { a: net(2), b: net(0), kind: SourceKind::LoadR(1e3) },
    ]);
    let divider = lower_input(&divider, 0, &libs);
    assert!(divider.diags.is_empty(), "divider bench sources have diagnostics: {:?}", divider.diags);
    let record = simulate(&divider.circuit, Analysis::Op, "bench voltage and resistance divider")?;
    let b = last_voltage(&record, "b")?;
    assert!((b - 1.0).abs() < 1e-9, "2 V across equal 1 kohm resistors must give V(b)=1 V, got {b} V");

    let mut injection = resistor_input(0, vec![
        BenchSource { a: net(0), b: net(1), kind: SourceKind::Idc(1e-3) },
    ]);
    // The injection circuits have no b branch; omit that otherwise floating node.
    injection.nets.retain(|n| n.id != net(2));
    let injected = lower_input(&injection, 0, &libs);
    assert!(injected.diags.is_empty(), "current injection has diagnostics: {:?}", injected.diags);
    let record = simulate(&injected.circuit, Analysis::Op, "1 mA from ground into a")?;
    let a = last_voltage(&record, "a")?;
    assert!((a - 1.0).abs() < 1e-9, "1 mA from ground into a through 1 kohm must give +1 V, got {a} V");
    injection.sources.push(BenchSource { a: net(1), b: net(0), kind: SourceKind::LoadI(1e-3) });
    let loaded = lower_input(&injection, 0, &libs);
    assert!(loaded.diags.is_empty(), "constant current load has diagnostics: {:?}", loaded.diags);
    let record = simulate(&loaded.circuit, Analysis::Op, "1 mA load drawing the injected current")?;
    let a = last_voltage(&record, "a")?;
    assert!(a.abs() < 1e-9, "equal injection and load currents must leave V(a)=0 V, got {a} V");
    injection.sources = vec![BenchSource {
        a: net(0), b: net(1), kind: SourceKind::LoadStep { from: 0.0, to: 2e-3, at: 1e-4, rise: 1e-5 },
    }];
    let stepped = lower_input(&injection, 0, &libs);
    assert!(stepped.diags.is_empty(), "current step has diagnostics: {:?}", stepped.diags);
    let record = simulate(&stepped.circuit,
        Analysis::Tran(TranSpec { tstop: 2e-4, max_step: Some(5e-6), uic: false }),
        "0 to 2 mA injection transient")?;
    let column = record.names.iter().position(|name| name == "a").ok_or("step record has no a voltage")?;
    assert_eq!(record.at.len(), record.x.len(), "step record must have one voltage row per time");
    let (mut before, mut after) = (0, 0);
    for (&time, row) in record.at.iter().zip(&record.x) {
        let a = *row.get(column).ok_or("step record row has no a voltage")?;
        if time < 1e-4 {
            before += 1;
            assert!(a.abs() < 1e-9, "before 100 us, V(a) must be 0 V, got {a} V at {time} s");
        } else if time >= 1.1e-4 {
            after += 1;
            assert!((a - 2.0).abs() < 1e-6, "from 110 us, V(a) must be 2 V, got {a} V at {time} s");
        } else {
            let expected = 2.0 * (time - 1e-4) / 1e-5;
            assert!((a - expected).abs() < 1e-6,
                "during the current ramp, V(a) must rise linearly to 2 V, got {a} V vs {expected} V at {time} s");
        }
    }
    assert!(before > 0 && after > 0, "step transient needs samples before and after the ramp, got {before} and {after}");
    let end = record.at.last().copied().ok_or("step transient has no times")?;
    assert!(end >= 2e-4, "step transient ended at {end} s before 200 us");

    injection.sources.clear();
    injection.supplies = vec![Supply { net: net(1), v: 2.0, limit: None, ramp: Some(0.0) }];
    let standalone = lower_input(&injection, 0, &libs);
    assert!(standalone.diags.is_empty(), "standalone supply has diagnostics: {:?}", standalone.diags);
    let record = simulate(&standalone.circuit, Analysis::Op, "standalone 2 V supply with a 1 kohm load")?;
    let a = last_voltage(&record, "a")?;
    assert!((a - 2.0).abs() < 1e-5, "standalone supply with zero ramp must start near 2 V, got {a} V");
    injection.supplies.first_mut().ok_or("standalone supply disappeared")?.limit = Some(1e-3);
    let standalone = lower_input(&injection, 0, &libs);
    assert!(standalone.diags.is_empty(), "limited standalone supply has diagnostics: {:?}", standalone.diags);
    let record = simulate(&standalone.circuit, Analysis::Op, "standalone supply limited to 1 mA through 1 kohm")?;
    let a = last_voltage(&record, "a")?;
    assert!((a - 1.0).abs() < 0.02, "standalone 1 mA limit through 1 kohm must give 1 V within 2%, got {a} V");
    assert!(standalone.plan.pins.iter().flatten().all(|terminal| !terminal.device.starts_with("bench#")),
        "standalone supply and limiter must not bind to the resistor's pins");

    let mut invalid = resistor_input(0, vec![
        BenchSource { a: net(99), b: net(0), kind: SourceKind::Vdc(1.0) },
        BenchSource { a: net(1), b: net(0), kind: SourceKind::LoadR(0.0) },
        BenchSource { a: net(1), b: net(0), kind: SourceKind::LoadStep { from: 0.0, to: 1e-3, at: 0.0, rise: 0.0 } },
    ]);
    invalid.supplies = vec![
        Supply { net: net(0), v: 1.0, limit: None, ramp: None },
        Supply { net: net(1), v: f64::NAN, limit: None, ramp: None },
        Supply { net: net(1), v: 1.0, limit: Some(0.0), ramp: None },
        Supply { net: net(1), v: 2.0, limit: None, ramp: None },
        Supply { net: net(1), v: 3.0, limit: None, ramp: None },
    ];
    let invalid = lower_input(&invalid, 0, &libs);
    assert_eq!(invalid.diags.len(), 8, "seven invalid bench items must give seven diagnostics, and the bare net b its note: {:?}", invalid.diags);
    let notes: Vec<_> = invalid.diags.iter().filter(|diag| diag.net.is_some()).collect();
    assert!(notes.len() == 1 && notes[0].net == Some(net(2)) && notes[0].path == "b" && notes[0].part.is_none()
        && notes[0].message == "b: no DC path to ground; its insulation leak (1e12 ohm) holds it",
        "the bare net b is noted on its net, as held by its leak: {:?}", invalid.diags);
    for (path, reason) in [
        ("bench.supplies[0]", "ground"),
        ("bench.supplies[1]", "voltage"),
        ("bench.supplies[2]", "current limit"),
        ("bench.supplies[4]", "a second supply on a is ignored"),
        ("bench.sources[0]", "outside the input"),
        ("bench.sources[1]", "resistance"),
        ("bench.sources[2]", "rise"),
    ] {
        let count = invalid.diags.iter().filter(|diag| diag.path == path).count();
        assert_eq!(count, 1, "{path} must give exactly one diagnostic, got {count}: {:?}", invalid.diags);
        let diag = invalid.diags.iter().find(|diag| diag.path == path)
            .ok_or_else(|| format!("missing diagnostic for {path}"))?;
        assert!(diag.part.is_none() && diag.message.starts_with(&format!("{path}: ")) && diag.message.contains(reason),
            "{path} must prefix its message and explain {reason} without a part binding: {diag:?}");
    }
    assert_eq!(invalid.circuit.devices.v.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), vec!["bench#supply3"],
        "invalid supplies and sources must leave only the first valid supply at its original index");
    assert!(invalid.circuit.devices.i.is_empty(), "invalid load step must create no current source");
    // net b touches nothing, so the lowering holds it at ground through its board leakage
    assert_eq!(invalid.circuit.devices.r.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["r", "bench#supply3.r", "b#leak"],
        "invalid resistance must leave only the part, the valid supply's output resistance and the bare net's leakage");
    assert!(invalid.plan.pins.iter().flatten().all(|terminal| !terminal.device.starts_with("bench#")),
        "bench devices must not bind to any part's pins");
    Ok(())
}
