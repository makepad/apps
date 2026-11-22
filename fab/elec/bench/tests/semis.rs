use fab_elec_bench::bind::bind;
use fab_elec_bench::input::{LowerInput, LowerNet, LowerPart, LowerPin, PartKind};
use fab_elec_bench::{lower_input, BenchSpec, LowerDiag, ModelLibs};
use fab_elec_circuit::{Circuit, Component, Net, NetKind};
use fab_elec_core::ids::{BlockId, BoardId, ClassId, CompId, CompRef, NetId, NetRef, PinIx, PinRef};
use fab_elec_design::{Design, Unit, UnitKind};
use fab_elec_parts::{Fields, PartDef, PinDef, PinKind};
use fab_elec_sim::live::Terminal;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::{run, Analysis, Control, ModelKind, Record, Source};
use std::sync::Arc;

fn net(id: u32) -> NetRef {
    NetRef { board: BoardId(0), net: NetId(id) }
}

fn part(id: u32, path: &str, kind: PartKind, pins: &[(&str, u32)]) -> LowerPart {
    LowerPart {
        id: CompRef { board: BoardId(0), comp: CompId(id) },
        path: path.into(),
        kind,
        tol: None,
        pins: pins.iter().map(|&(name, id)| LowerPin { name: name.into(), net: Some(net(id)), nc: false }).collect(),
        noise: false,
    }
}

fn resistor(id: u32, path: &str, a: u32, b: u32, ohms: f64) -> LowerPart {
    part(id, path, PartKind::Resistor { ohms }, &[("1", a), ("2", b)])
}

/// A minimal design exercises the field and prefix classifier independently of the OP fixture.
fn bind_part(
    name: &str, pins: &[&str], fields: &[(&str, &str)], part_fields: &[(&str, &str)], libs: &ModelLibs,
) -> (LowerInput, Vec<LowerDiag>) {
    let make_fields = |values: &[(&str, &str)]| Fields(values.iter().map(|&(k, v)| (k.into(), v.into())).collect());
    let component = Component {
        id: CompId(0), path: "device".into(), refdes: String::new(), ref_kind: Default::default(),
        part: Arc::new(PartDef {
            name: name.into(), kind: fab_elec_parts::PartKind::Transistor,
            symbol: Default::default(), footprint: None, body: None, emitter: None,
            pins: pins.iter().map(|name| PinDef {
                name: (*name).into(), pads: Vec::new(), kind: PinKind::Passive,
                unit: 0, limits: None, alternates: Vec::new(), delay: None,
            }).collect(),
            fields: make_fields(part_fields),
            gpio: None,
        }),
        value: None,
        conns: pins.iter().enumerate().map(|(i, _)| (PinIx(i as u16), NetId(0))).collect(),
        nc: Vec::new(), block: BlockId(0), dnp: false, fields: make_fields(fields),
    };
    let design = Design {
        units: vec![Unit {
            id: BoardId(0), name: "semiconductors".into(), kind: UnitKind::Part,
            circuit: Arc::new(Circuit {
                comps: vec![component],
                nets: vec![Net { id: NetId(0), name: "0".into(), class: ClassId(0), volts: None, kind: NetKind::Ground, scope: None }],
                ..Circuit::default()
            }),
            supplied: Vec::new(), board: None, factory: None,
        }],
        ..Design::default()
    };
    bind(&design, BoardId(0), &BenchSpec::default(), libs)
}

#[test]
fn discrete_semiconductors_lower_and_conduct() -> Result<(), String> {
    let mut libs = ModelLibs::generic();
    let input = LowerInput {
        nets: ["0", "supply", "diode", "npn.c", "npn.b", "nmos.on", "nmos.off", "pnp.c", "pnp.b", "pmos.d"]
            .into_iter().enumerate().map(|(i, name)| LowerNet { id: net(i as u32), name: name.into(), supply: false }).collect(),
        ground: net(0),
        supplies: Vec::new(),
        sources: Vec::new(),
        firmware: Vec::new(),
        probes: Vec::new(),
        parts: vec![
            resistor(0, "diode.load", 1, 2, 1000.0),
            part(1, "diode.device", PartKind::Diode { model: "D_GENERIC".into() }, &[("k", 0), ("a", 2)]),
            resistor(2, "npn.load", 1, 3, 330.0),
            resistor(3, "npn.drive", 1, 4, 4700.0),
            part(4, "npn.device", PartKind::Bjt { pnp: false, model: "Q_NPN_GENERIC".into() },
                &[("e", 0), ("c", 3), ("b", 4)]),
            resistor(5, "nmos.on.load", 1, 5, 330.0),
            part(6, "nmos.on.device", PartKind::Mosfet { pmos: false, model: "Q_NMOS_GENERIC".into() },
                &[("s", 0), ("g", 1), ("d", 5)]),
            resistor(7, "nmos.off.load", 1, 6, 330.0),
            part(8, "nmos.off.device", PartKind::Mosfet { pmos: false, model: "Q_NMOS_GENERIC".into() },
                &[("B", 0), ("D", 6), ("S", 0), ("G", 0)]),
            resistor(9, "pnp.load", 7, 0, 330.0),
            resistor(10, "pnp.drive", 8, 0, 4700.0),
            part(11, "pnp.device", PartKind::Bjt { pnp: true, model: "Q_PNP_GENERIC".into() },
                &[("B", 8), ("E", 1), ("C", 7)]),
            resistor(12, "pmos.load", 9, 0, 330.0),
            part(13, "pmos.device", PartKind::Mosfet { pmos: true, model: "Q_PMOS_GENERIC".into() },
                &[("G", 0), ("D", 9), ("S", 1)]),
        ],
    };
    let mut lowered = lower_input(&input, 7, &libs);
    assert!(lowered.diags.is_empty(), "all connected discrete devices lower without diagnostics: {:?}", lowered.diags);
    assert_eq!(lowered.circuit.devices.d.len(), 1, "one kernel diode represents the discrete diode");
    assert_eq!(lowered.circuit.devices.q.len(), 2, "one kernel BJT represents each polarity");
    assert_eq!(lowered.circuit.devices.m.len(), 3, "one kernel MOSFET represents each on/off device");
    assert_eq!(lowered.circuit.models.len(), 5, "each generic card is registered once, including the shared NMOS card");
    assert!(lowered.leds.is_empty(), "ordinary diodes do not create LED light taps");
    assert_eq!(lowered.index.counts(), (lowered.plan.nets.len(), lowered.plan.pins.len(), lowered.plan.parts.len()),
        "semiconductor value index and live plan have identical dimensions");

    for part in &input.parts {
        let comp = lowered.index.comp(part.id).ok_or_else(|| format!("{} has a component value index", part.path))?;
        assert_eq!(lowered.plan.parts[comp], vec![part.path.clone()], "{} lowers to one device named by its path", part.path);
        assert_eq!(lowered.plan.part_pins[comp].1 as usize, part.pins.len(), "{} power covers every design pin", part.path);
        for (i, pin) in part.pins.iter().enumerate() {
            let pin_ref = PinRef { board: part.id.board, comp: part.id.comp, pin: PinIx(i as u16) };
            let at = lowered.index.pin(pin_ref).ok_or_else(|| format!("{}.{} has a pin value index", part.path, pin.name))?;
            assert_eq!(lowered.plan.pin_net[at], pin.net.and_then(|n| lowered.index.net(n)).map(|n| n as u32),
                "{}.{} keeps its net in the power plan", part.path, pin.name);
            let terminals = match &part.kind {
                PartKind::Diode { .. } => match pin.name.to_ascii_uppercase().as_str() {
                    "A" => vec![0], "K" => vec![1], _ => Vec::new(),
                },
                PartKind::Bjt { .. } => match pin.name.to_ascii_uppercase().as_str() {
                    "C" => vec![0], "B" => vec![1], "E" => vec![2, 3], _ => Vec::new(),
                },
                PartKind::Mosfet { .. } => match pin.name.to_ascii_uppercase().as_str() {
                    "D" => vec![0], "G" => vec![1], "B" => vec![3],
                    "S" if part.pins.iter().any(|p| p.name.eq_ignore_ascii_case("B")) => vec![2],
                    "S" => vec![2, 3], _ => Vec::new(),
                },
                _ => vec![i as u8],
            };
            let expected: Vec<_> = terminals.into_iter().map(|terminal| Terminal { device: part.path.clone(), terminal }).collect();
            assert_eq!(lowered.plan.pins[at], expected, "{}.{} maps its named terminals regardless of pin order or case", part.path, pin.name);
        }
    }
    for mos in &lowered.circuit.devices.m {
        assert_eq!((mos.w, mos.l), (100e-6, 100e-6), "{} uses equal 100 um dimensions so beta equals kp", mos.name);
        assert_eq!(mos.b, mos.s, "{} bulk is on its source net", mos.name);
    }

    // Bench stimulus is ideal: all six branches share exactly 5 V.
    lowered.circuit.v("supply.source", "supply", "0", Source::dc(5.0));
    lowered.circuit.opts.reltol = 1e-10;
    lowered.circuit.opts.vntol = 1e-13;
    let mut record = Record::default();
    run(&lowered.circuit, &Analysis::Op, &mut record, &Control::default())
        .map_err(|diag| format!("discrete semiconductor operating point converges: {diag}"))?;
    let spec = LiveSpec { playing: false, reltol: Some(1e-10), ..LiveSpec::default() };
    let mut live = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .map_err(|diag| format!("discrete semiconductor live operating point converges: {diag}"))?;
    let values = live.values(1.0);
    let current = |path: &str, name: &str| -> Result<f64, String> {
        let part = input.parts.iter().find(|p| p.path == path).ok_or_else(|| format!("{path} is in the OP fixture"))?;
        let pin = part.pins.iter().position(|p| p.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("{path} has pin {name}"))?;
        lowered.index.i(&values, PinRef { board: part.id.board, comp: part.id.comp, pin: PinIx(pin as u16) })
            .map(f64::from).ok_or_else(|| format!("{path}.{name} has a published terminal current"))
    };
    let drop = record.last("diode");
    assert!((0.55..0.8).contains(&drop), "generic diode forward drop is between 0.55 and 0.8 V: {drop}");
    let diode_i = current("diode.device", "A")?;
    let expected = (5.0 - drop) / 1000.0;
    assert!((diode_i / expected - 1.0).abs() < 1e-6, "diode pin current matches the 1 kohm load within 1e-6 relative: {diode_i} vs {expected}");
    let vce = record.last("npn.c");
    assert!((0.0..0.3).contains(&vce), "NPN low-side switch saturates below 0.3 V: {vce}");
    let ic = current("npn.device", "C")?;
    let expected = (5.0 - vce) / 330.0;
    assert!((ic / expected - 1.0).abs() < 0.05, "NPN collector current matches the 330 ohm load within 5%: {ic} vs {expected}");
    let vds = record.last("nmos.on");
    assert!((0.0..0.2).contains(&vds), "NMOS with a 5 V gate conducts below 0.2 V VDS: {vds}");
    let off_i = current("nmos.off.device", "D")?;
    assert!(off_i.abs() < 1e-6, "NMOS with a grounded gate draws less than 1 uA: {off_i}");
    assert!((record.last("pnp.c") - 5.0).abs() < 0.3, "PNP high-side switch drives its load within 0.3 V of 5 V");
    assert!((record.last("pmos.d") - 5.0).abs() < 0.3, "PMOS high-side switch drives its load within 0.3 V of 5 V");
    for (comp, &(first, count)) in lowered.plan.part_pins.iter().enumerate() {
        let mut expected = 0.0_f64;
        let mut magnitude = 0.0_f64;
        let mut sum_i = 0.0_f64;
        for pin in first..first + count {
            let i = f64::from(values.branch_i[pin as usize]);
            sum_i += i;
            if let Some(net) = lowered.plan.pin_net[pin as usize] {
                let power = f64::from(values.net_v[net as usize]) * i;
                expected += power;
                magnitude += power.abs();
            }
        }
        assert!(sum_i.abs() < 1e-8, "component {comp} terminal currents conserve charge including tied bulk/substrate current: {sum_i}");
        assert!((f64::from(values.part_p[comp]) - expected).abs() <= 1e-6 * magnitude + 1e-12,
            "component {comp} power equals the sum of its pin voltage times current");
    }

    let cases: [(&str, &str, PartKind, &[&str]); 5] = [
        ("D_small", "diode", PartKind::Diode { model: "D_GENERIC".into() }, &["k", "a"]),
        ("Q_NPN_small", "npn", PartKind::Bjt { pnp: false, model: "Q_NPN_GENERIC".into() }, &["e", "b", "c"]),
        ("Q_PNP_small", "pnp", PartKind::Bjt { pnp: true, model: "Q_PNP_GENERIC".into() }, &["c", "e", "b"]),
        ("Q_NMOS_small", "nmos", PartKind::Mosfet { pmos: false, model: "Q_NMOS_GENERIC".into() }, &["s", "g", "d"]),
        ("Q_PMOS_small", "pmos", PartKind::Mosfet { pmos: true, model: "Q_PMOS_GENERIC".into() }, &["g", "d", "s"]),
    ];
    for (name, sim, expected, pins) in cases {
        for (part_name, fields, part_fields) in [
            (name, Vec::new(), Vec::new()),
            ("unclassified", vec![("sim", sim)], Vec::new()),
            ("unclassified", Vec::new(), vec![("sim", sim)]),
        ] {
            let (bound, diags) = bind_part(part_name, pins, &fields, &part_fields, &libs);
            assert!(diags.is_empty(), "{sim} binds by prefix or sim field without diagnostics: {diags:?}");
            assert_eq!(bound.parts[0].kind, expected, "{sim} classification retains its polarity and generic model name");
        }
    }

    let (missing, diags) = bind_part("Q_NMOS_small", &["D", "S"], &[], &[], &libs);
    assert!(diags.iter().any(|d| d.message.contains("pin G is missing")), "missing MOSFET gate diagnostic names G");
    assert_eq!(missing.parts[0].kind, PartKind::Passive, "MOSFET without G is classified passive");
    let passive = lower_input(&missing, 0, &libs);
    assert!(passive.circuit.devices.m.is_empty() && passive.plan.parts[0].is_empty(), "passive missing-gate part creates no kernel device or live device tap");
    let mut missing = missing;
    missing.parts[0].kind = PartKind::Mosfet { pmos: false, model: "Q_NMOS_GENERIC".into() };
    let missing = lower_input(&missing, 0, &libs);
    assert!(missing.diags.iter().any(|d| d.message.contains("pin G is missing")) && missing.circuit.devices.m.is_empty(),
        "direct LowerInput also diagnoses a missing gate and skips the MOSFET");

    let (unknown, diags) = bind_part("D_small", &["A", "K"], &[("model", "absent")], &[], &libs);
    assert!(diags.iter().any(|d| d.message.contains("unknown model card 'absent'")), "unknown model field diagnostic names the requested card");
    assert_eq!(unknown.parts[0].kind, PartKind::Diode { model: "D_GENERIC".into() }, "unknown model field falls back to the generic diode");
    let fallback = lower_input(&unknown, 0, &libs);
    assert!(fallback.diags.is_empty(), "resolved generic fallback lowers without duplicate diagnostics");
    assert_eq!(fallback.circuit.devices.d[0].model, "D_GENERIC", "unknown model field still lowers a generic kernel diode");

    libs.cards.push(("D_CUSTOM".into(), ModelKind::Diode, vec![("is".into(), 1e-10), ("n".into(), 1.2)]));
    let (custom, diags) = bind_part("D_small", &["A", "K"], &[], &[("model", "D_CUSTOM")], &libs);
    assert!(diags.is_empty(), "a known part model field binds without diagnostics: {diags:?}");
    let custom = lower_input(&custom, 0, &libs);
    assert_eq!(custom.circuit.devices.d[0].model, "D_CUSTOM", "a known part model field selects its named kernel card");
    assert_eq!(custom.circuit.find_model("D_CUSTOM").and_then(|m| m.get("is")), Some(1e-10), "custom card parameters reach the kernel unchanged");
    let (override_part, diags) = bind_part("D_small", &["A", "K"], &[("model", "D_GENERIC")], &[("model", "D_CUSTOM")], &libs);
    assert!(diags.is_empty(), "component model override binds without diagnostics: {diags:?}");
    assert_eq!(override_part.parts[0].kind, PartKind::Diode { model: "D_GENERIC".into() }, "component model field overrides the part definition's card");

    let (mut unconnected, _) = bind_part("Q_NMOS_small", &["D", "G", "S", "B"], &[], &[], &libs);
    unconnected.parts[0].pins[3].net = None;
    let unconnected = lower_input(&unconnected, 0, &libs);
    assert!(unconnected.diags.iter().any(|d| d.message.contains("pin B is not connected")) && unconnected.circuit.devices.m.is_empty(),
        "an explicit unconnected bulk is diagnosed and does not silently tie to source");
    Ok(())
}

/// Every colour's LED card holds its junction's charge: alone on its net and held in reverse, an
/// LED passes no DC (its leakage only) yet an AC path, its depletion capacitance, so a lone LED
/// net neither floats in a transient nor settles through the LED at rest.
#[test]
fn every_led_colour_is_a_capacitor_in_reverse_and_passes_no_dc() -> Result<(), String> {
    use fab_elec_bench::input::LedColor;
    use fab_elec_sim::AcSweep;
    let libs = ModelLibs::generic();
    let colours = [
        LedColor::Red, LedColor::Orange, LedColor::Yellow, LedColor::Green, LedColor::Blue,
        LedColor::White, LedColor::Infrared, LedColor::YellowGreen,
    ];
    let input = LowerInput {
        nets: std::iter::once("0".to_string()).chain((0..colours.len()).map(|i| format!("lamp{i}")))
            .enumerate().map(|(i, name)| LowerNet { id: net(i as u32), name, supply: false }).collect(),
        ground: net(0),
        supplies: Vec::new(),
        sources: Vec::new(),
        firmware: Vec::new(),
        probes: Vec::new(),
        parts: colours.iter().enumerate()
            .map(|(i, &color)| part(i as u32, &format!("lamp{i}.d"), PartKind::Led { color }, &[("A", i as u32 + 1), ("K", 0)]))
            .collect(),
    };
    let mut lowered = lower_input(&input, 7, &libs);
    // Alone on its net, each anode is noted as an open end, and its LED is still simulated.
    assert!(lowered.diags.len() == colours.len()
        && lowered.diags.iter().enumerate().all(|(i, d)| d.message.contains(&format!("one connection only (lamp{i}.d A)"))),
        "every colour's LED lowers, its lone anode noted: {:?}", lowered.diags);
    // Each LED held 1 V in reverse, with a 1 V AC swing on top: DC through it is leakage alone,
    // AC is its capacitance there, cjo / (1 + 1 V / vj)^m.
    for i in 0..colours.len() {
        lowered.circuit.v(&format!("bias{i}"), &format!("lamp{i}"), "0", Source::ac(-1.0, 1.0, 0.0));
    }
    let mut op = Record::default();
    run(&lowered.circuit, &Analysis::Op, &mut op, &Control::default()).map_err(|d| format!("the reverse-biased LEDs settle: {d}"))?;
    let f = 1e6;
    let mut ac = Record::default();
    run(&lowered.circuit, &Analysis::Ac(AcSweep::List(vec![f])), &mut ac, &Control::default()).map_err(|d| format!("the LEDs' AC response solves: {d}"))?;
    for (i, &color) in colours.iter().enumerate() {
        let card = libs.leds.iter().find(|c| c.color == color).ok_or_else(|| format!("{color:?} has a card"))?;
        assert!(card.cjo > 0.0 && card.tt > 0.0 && card.bv > 1.0, "{color:?}: the card states its capacitance, transit time and breakdown: {card:?}");
        let branch = format!("bias{i}#branch");
        let dc = op.last(&branch);
        assert!(dc.abs() < 1e-9, "{color:?}: 1 V of reverse passes no DC but leakage: {dc} A");
        let at = ac.index(&branch);
        let i_ac = ac.ac[0][at].norm();
        let c = card.cjo / (1.0 + 1.0 / card.vj).powf(card.m);
        let want = 2.0 * std::f64::consts::PI * f * c;
        assert!((i_ac / want - 1.0).abs() < 0.02, "{color:?}: at 1 MHz it passes its junction capacitance's current: {i_ac} A, {want} A for {c} F");
    }
    Ok(())
}
