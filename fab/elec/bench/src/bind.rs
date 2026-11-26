//! Classify a design unit or a circuit of its own into the model-binding table's ordered input.

use crate::bench::BenchSpec;
use crate::expect::Probe;
use crate::input::{LedColor, LowerInput, LowerNet, LowerPart, LowerPin, PartKind, Taper};
use crate::lower::LowerDiag;
use crate::models::{Bind, ModelLibs};
use fab_elec_circuit::{Circuit, Component, NetKind, PortKind, QtyUnit};
use fab_elec_core::ids::{BoardId, CompRef, NetId, NetRef, PinIx};
use fab_elec_design::Design;
use fab_elec_parts::{PartKind as DefinedPartKind, PinKind};
use fab_elec_sim::{GpioParams, ModelKind};

pub fn bind(design: &Design, scope: BoardId, bench: &BenchSpec, libs: &ModelLibs) -> (LowerInput, Vec<LowerDiag>) {
    let Some(unit) = design.units.iter().find(|unit| unit.id == scope) else {
        let input = LowerInput {
            nets: Vec::new(),
            ground: NetRef { board: scope, net: NetId(0) },
            parts: Vec::new(),
            supplies: bench.supplies.clone(),
            sources: bench.sources.clone(),
            firmware: bench.firmware.iter().cloned().map(|fw| (fw, None)).collect(),
            probes: bench.probes.clone(),
        };
        let diags = vec![LowerDiag { part: None, net: None, path: String::new(), message: format!("no unit {} in the design", scope.0) }];
        return (input, diags);
    };
    bind_circuit(&unit.circuit, scope, bench, libs)
}

pub fn bind_circuit(circuit: &Circuit, scope: BoardId, bench: &BenchSpec, libs: &ModelLibs) -> (LowerInput, Vec<LowerDiag>) {
    let mut input = LowerInput {
        nets: Vec::new(),
        // An empty input has no ground; this placeholder is never lowered to a node.
        ground: NetRef { board: scope, net: NetId(0) },
        parts: Vec::new(),
        supplies: bench.supplies.clone(),
        sources: bench.sources.clone(),
        firmware: bench.firmware.iter().cloned().map(|fw| (fw, None)).collect(),
        probes: bench.probes.clone(),
    };
    let mut diags = Vec::new();
    let ground = circuit.nets.iter().find(|net| net.kind == NetKind::Ground).or_else(|| {
        diags.push(LowerDiag { part: None, net: None, path: String::new(), message: "no ground net".to_owned() });
        circuit.nets.first()
    });
    let Some(ground) = ground else {
        return (input, diags);
    };
    input.ground = NetRef { board: scope, net: ground.id };
    let supplies = declared_supplies(circuit);
    let heard = declared_heard(circuit, scope, bench);
    input.nets = circuit.nets.iter().map(|net| LowerNet {
        id: NetRef { board: scope, net: net.id },
        name: net.name.clone(),
        supply: net.id != ground.id && net.kind != NetKind::Ground && supplies.contains(&net.id),
        heard: net.id != ground.id && net.kind != NetKind::Ground && heard.binary_search(&net.id).is_ok(),
    }).collect();
    for comp in &circuit.comps {
        let id = CompRef { board: scope, comp: comp.id };
        let mut f_clk = None;
        let kind = classify(comp, circuit, bench, libs, &mut f_clk, &mut |message| diags.push(LowerDiag {
            part: Some(id),
            net: None,
            path: comp.path.clone(),
            message: format!("{}: {message}", comp.path),
        }));
        for (fw, clock) in &mut input.firmware {
            if fw.path == comp.path {
                *clock = f_clk;
            }
        }
        let tol = match &kind {
            PartKind::Resistor { ohms: si } | PartKind::Capacitor { farads: si } | PartKind::Inductor { henries: si } => {
                comp.tolerance().map(|t| t.bounds(*si))
            }
            _ => None,
        };
        let pins = comp.part.pins.iter().enumerate().map(|(i, pin)| LowerPin {
            name: pin.name.clone(),
            net: comp.conns.iter().find(|(ix, _)| *ix == PinIx(i as u16))
                .map(|(_, net)| NetRef { board: scope, net: *net }),
            nc: comp.nc.contains(&PinIx(i as u16)),
        }).collect();
        // a resistor's `noise: thermal` field simulates its thermal noise (a noise source's)
        let noise = match comp.fields.get("noise").or_else(|| comp.part.fields.get("noise")) {
            None => false,
            Some(text) if text.trim().eq_ignore_ascii_case("thermal") && matches!(kind, PartKind::Resistor { .. }) => true,
            Some(text) => {
                diags.push(LowerDiag {
                    part: Some(id),
                    net: None,
                    path: comp.path.clone(),
                    message: format!("{}: noise '{text}': only a resistor's thermal noise is simulated; it is quiet", comp.path),
                });
                false
            }
        };
        input.parts.push(LowerPart { id, path: comp.path.clone(), kind, tol, pins, noise });
    }
    for stub in bench.stubs.iter().filter(|stub| !circuit.comps.iter().any(|comp| comp.path == stub.path)) {
        diags.push(LowerDiag {
            part: None,
            net: None,
            path: stub.path.clone(),
            message: format!("{}: stub names no component in the scope; its drives reach nothing", stub.path),
        });
    }
    (input, diags)
}

/// The nets the design declares supplies: every power net, and every net on a power-output port,
/// a block's (its definition's port kind) or a part's pin.
fn declared_supplies(circuit: &Circuit) -> Vec<NetId> {
    let mut out: Vec<NetId> = circuit.nets.iter().filter(|net| net.kind == NetKind::Power).map(|net| net.id).collect();
    for block in &circuit.tree {
        let Some(def) = block.def.and_then(|id| circuit.block_defs.iter().find(|def| def.id == id)) else { continue };
        for (port, nets) in &block.ports {
            if def.ports.iter().any(|p| p.name == *port && p.kind == PortKind::PowerOut) {
                out.extend(nets.iter().copied());
            }
        }
    }
    for comp in &circuit.comps {
        for (ix, net) in &comp.conns {
            if comp.part.pins.get(ix.0 as usize).is_some_and(|pin| pin.kind == PinKind::PowerOut) {
                out.push(*net);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The output jack channels and bench voltage probe points on this board, in net order.
fn declared_heard(circuit: &Circuit, scope: BoardId, bench: &BenchSpec) -> Vec<NetId> {
    let mut heard = Vec::new();
    for jack in crate::audio::output_jacks(circuit) {
        heard.extend(jack.channels());
    }
    let mut point = |net: NetRef| {
        if net.board == scope {
            heard.push(net.net);
        }
    };
    for probe in &bench.probes {
        match probe.probe {
            Probe::V(net) => point(net),
            Probe::VDiff(a, b) => {
                point(a);
                point(b);
            }
            Probe::I(_) | Probe::P(_) => {}
        }
    }
    heard.sort_unstable();
    heard.dedup();
    heard
}

fn classify(
    comp: &Component, circuit: &Circuit, bench: &BenchSpec, libs: &ModelLibs,
    f_clk: &mut Option<f64>, diag: &mut impl FnMut(String),
) -> PartKind {
    if bench.firmware.iter().any(|fw| fw.path == comp.path) {
        let mut rails = Vec::new();
        let mut crystal = Vec::new();
        let mut crystals: Vec<&Component> = Vec::new();
        for (i, pin) in comp.part.pins.iter().enumerate() {
            let net = comp.conns.iter().find(|(ix, _)| *ix == PinIx(i as u16)).map(|(_, net)| net);
            let mut crystal_pin = false;
            // A crystal's case pin sits on ground: sharing ground makes no pin a crystal pin.
            let ground = net.is_some_and(|net| circuit.nets.iter().any(|n| n.id == *net && n.kind == NetKind::Ground));
            if let Some(net) = net.filter(|_| !ground) {
                for crystal in circuit.comps.iter().filter(|c| {
                    c.part.kind == DefinedPartKind::Crystal && c.conns.iter().any(|(_, n)| n == net)
                }) {
                    crystal_pin = true;
                    if !crystals.iter().any(|c| c.id == crystal.id) {
                        crystals.push(crystal);
                    }
                }
            }
            if matches!(pin.kind, PinKind::PowerIn | PinKind::PowerOut) || crystal_pin {
                rails.push(i);
            }
            if crystal_pin {
                crystal.push(i);
            }
        }
        if crystals.len() > 1 {
            diag("more than one crystal on its pins".to_owned());
        }
        *f_clk = crystals.first().and_then(|c| c.value.as_ref()).and_then(|v| v.qty)
            .filter(|v| v.unit == QtyUnit::Hertz).map(|v| v.si);
        if f_clk.is_none() {
            diag("no crystal frequency on its pins; its firmware has no clock".to_owned());
        }
        let chip = comp.fields.get("model").or_else(|| comp.part.fields.get("model")).map(str::to_owned);
        return PartKind::Firmware { chip, rails, crystal, pads: pads(comp, libs), c_pin: comp.part.gpio.as_ref().and_then(|c| c.c_pin), supply: supply(comp).map(str::to_owned) };
    }
    if comp.dnp {
        return PartKind::Passive;
    }
    let stub = bench.stubs.iter().find(|stub| stub.path == comp.path);
    let binding = if let Some(sim) = comp.fields.get("sim").or_else(|| comp.part.fields.get("sim")) {
        match sim {
            "resistor" => Some(Bind::Resistor),
            "capacitor" => Some(Bind::Capacitor),
            "inductor" => Some(Bind::Inductor),
            "led" => Some(Bind::Led),
            "diode" => Some(Bind::Diode),
            "npn" => Some(Bind::Npn),
            "pnp" => Some(Bind::Pnp),
            "nmos" => Some(Bind::Nmos),
            "pmos" => Some(Bind::Pmos),
            "regulator" => Some(Bind::Regulator),
            "mcu" => Some(Bind::Mcu),
            "usb" => Some(Bind::UsbPower),
            "button" => Some(Bind::Button),
            "opamp" => Some(Bind::OpAmp),
            "pot" => Some(Bind::Pot),
            "schmitt" => Some(Bind::Schmitt),
            "shift_latch8" => Some(Bind::ShiftLatch8),
            "bus" => Some(Bind::Bus),
            "passive" => Some(Bind::Passive),
            _ => {
                if stub.is_none() {
                    diag(format!("unknown simulation model '{sim}'"));
                }
                None
            }
        }
    } else {
        libs.rules.iter().find(|(prefix, _)| comp.part.name.starts_with(prefix.as_str()))
            .map(|(_, binding)| *binding)
            .or_else(|| match comp.part.kind {
                DefinedPartKind::Mcu => Some(Bind::Mcu),
                DefinedPartKind::Crystal => Some(Bind::Passive),
                _ => None,
            })
    };
    if let Some(stub) = stub {
        let kind = match binding {
            Some(Bind::Resistor) => Some("resistor"),
            Some(Bind::Capacitor) => Some("capacitor"),
            Some(Bind::Inductor) => Some("inductor"),
            Some(Bind::Led) => Some("led"),
            Some(Bind::Diode) => Some("diode"),
            Some(Bind::Npn) => Some("npn"),
            Some(Bind::Pnp) => Some("pnp"),
            Some(Bind::Nmos) => Some("nmos"),
            Some(Bind::Pmos) => Some("pmos"),
            Some(Bind::Regulator) => Some("regulator"),
            Some(Bind::UsbPower) => Some("usb power"),
            Some(Bind::Button) => Some("button"),
            Some(Bind::OpAmp) => Some("op-amp"),
            Some(Bind::Pot) => Some("pot"),
            Some(Bind::Schmitt) => Some("Schmitt inverter"),
            Some(Bind::ShiftLatch8) => Some("shift register"),
            Some(Bind::Bus) => Some("bus part"),
            Some(Bind::Mcu | Bind::Passive) | None => None,
        };
        if let Some(kind) = kind {
            diag(format!("its {kind} model is replaced by the bench stub"));
        }
        return PartKind::Stub {
            drives: stub.pins.clone(), rail: stub.rail, mcu: binding == Some(Bind::Mcu), pads: pads(comp, libs), supply: supply(comp).map(str::to_owned),
        };
    }
    let Some(binding) = binding else {
        return PartKind::Other(comp.part.name.clone());
    };
    match binding {
        Bind::Resistor | Bind::Capacitor | Bind::Inductor => {
            let (unit, quantity) = match binding {
                Bind::Resistor => (QtyUnit::Ohm, "resistance"),
                Bind::Capacitor => (QtyUnit::Farad, "capacitance"),
                Bind::Inductor => (QtyUnit::Henry, "inductance"),
                _ => unreachable!(),
            };
            let Some(value) = comp.value.as_ref().and_then(|value| value.qty).filter(|value| value.unit == unit) else {
                diag(format!("no {quantity} value; not simulated"));
                return PartKind::Passive;
            };
            match binding {
                Bind::Resistor => PartKind::Resistor { ohms: value.si },
                Bind::Capacitor => PartKind::Capacitor { farads: value.si },
                Bind::Inductor => PartKind::Inductor { henries: value.si },
                _ => unreachable!(),
            }
        }
        Bind::Led => {
            let text = comp.value.as_ref().map(|value| value.text.as_str()).unwrap_or("");
            let color = LedColor::of_name(text).unwrap_or_else(|| {
                diag(format!("LED colour '{text}' unknown; simulated as red"));
                LedColor::Red
            });
            PartKind::Led { color }
        }
        Bind::Diode | Bind::Npn | Bind::Pnp | Bind::Nmos | Bind::Pmos => {
            let (kind, names): (ModelKind, &[&str]) = match binding {
                Bind::Diode => (ModelKind::Diode, &["A", "K"]),
                Bind::Npn => (ModelKind::Npn, &["C", "B", "E"]),
                Bind::Pnp => (ModelKind::Pnp, &["C", "B", "E"]),
                Bind::Nmos => (ModelKind::Nmos, &["D", "G", "S"]),
                Bind::Pmos => (ModelKind::Pmos, &["D", "G", "S"]),
                _ => return PartKind::Passive,
            };
            let mut complete = true;
            for name in names {
                if !comp.part.pins.iter().any(|pin| pin.name.eq_ignore_ascii_case(name)) {
                    diag(format!("pin {name} is missing; not simulated"));
                    complete = false;
                }
            }
            if !complete {
                return PartKind::Passive;
            }
            let requested = comp.fields.get("model").or_else(|| comp.part.fields.get("model"));
            let Some(model) = libs.discrete_model(requested, kind, diag) else {
                return PartKind::Passive;
            };
            let model = model.to_owned();
            match kind {
                ModelKind::Diode => PartKind::Diode { model },
                ModelKind::Npn | ModelKind::Pnp => PartKind::Bjt { pnp: kind == ModelKind::Pnp, model },
                ModelKind::Nmos | ModelKind::Pmos => PartKind::Mosfet { pmos: kind == ModelKind::Pmos, model },
                _ => PartKind::Passive,
            }
        }
        Bind::Regulator => {
            let limits = comp.part.pins.iter().find(|pin| pin.name == "VOUT").and_then(|pin| pin.limits.as_ref());
            let Some(volts) = limits.and_then(|limits| limits.volts).filter(|volts| volts.min == volts.max) else {
                diag("no fixed VOUT voltage; not simulated".to_owned());
                return PartKind::Passive;
            };
            PartKind::Regulator {
                vout: volts.min,
                ilim: limits.and_then(|limits| limits.current).unwrap_or(libs.reg_ilim),
            }
        }
        Bind::Mcu => PartKind::Stub { drives: Vec::new(), rail: None, mcu: true, pads: pads(comp, libs), supply: supply(comp).map(str::to_owned) },
        Bind::UsbPower => {
            let volts = comp.part.pins.iter().find(|pin| pin.name == "VBUS")
                .and_then(|pin| pin.limits.as_ref()).and_then(|limits| limits.volts)
                .filter(|volts| volts.min == volts.max).map(|volts| volts.min)
                .unwrap_or(libs.usb_volts);
            PartKind::UsbPower { volts }
        }
        Bind::Button => PartKind::Button { pressed: bench.pressed.contains(&comp.path) },
        Bind::OpAmp => opamp(comp, diag),
        Bind::Pot => pot(comp, bench, diag),
        Bind::Schmitt => schmitt(comp, diag),
        Bind::ShiftLatch8 => PartKind::Logic { kind: crate::logic::LogicKind::ShiftLatch8, pads: pads(comp, libs) },
        Bind::Bus => bus(comp, diag),
        Bind::Passive => PartKind::Passive,
    }
}

/// Component fields override part fields, including each individual bus parameter. The factory
/// owns the keys and validation; the bench never selects a model family or invents a parameter.
fn bus(comp: &Component, diag: &mut impl FnMut(String)) -> PartKind {
    let Some(model) = comp.fields.get("model").or_else(|| comp.part.fields.get("model")) else {
        diag("no bus model key; not simulated".into());
        return PartKind::Passive;
    };
    let mut params = Vec::new();
    let fields = comp.fields.0.iter().chain(comp.part.fields.0.iter().filter(|(key, _)| comp.fields.get(key).is_none()));
    for (key, text) in fields {
        let Some(name) = key.strip_prefix("bus.") else { continue };
        let Ok(number) = text.trim().parse::<f64>() else {
            diag(format!("{key} '{text}' is not a number; not simulated"));
            return PartKind::Passive;
        };
        params.push((name.to_owned(), number));
    }
    match fab_elec_mcu_parts::model(model, &params) {
        Ok(kind) => PartKind::Bus {
            model: model.to_owned(), kind, c_pin: comp.part.gpio.as_ref().and_then(|c| c.c_pin),
            rails: comp.part.pins.iter().enumerate()
                .filter(|(_, pin)| matches!(pin.kind, PinKind::PowerIn | PinKind::PowerOut)).map(|(i, _)| i).collect(),
        },
        Err(why) => {
            diag(format!("{why}; not simulated"));
            PartKind::Passive
        }
    }
}

/// An op-amp's sections by their pin names: `OUT`, `INP` and `INN` for a single op-amp, each with
/// the section's letter (`OUTA`, `INPA`, `INNA`, then B, C and D) on a multiple one. Its supply is
/// VDD or VCC and VSS, VEE or GND. Without a section or a supply it is not simulated.
fn opamp(comp: &Component, diag: &mut impl FnMut(String)) -> PartKind {
    let pin = |name: &str| comp.part.pins.iter().position(|p| p.name.eq_ignore_ascii_case(name));
    let sections: Vec<[usize; 3]> = ["", "A", "B", "C", "D"].iter().filter_map(|s| {
        Some([pin(&format!("OUT{s}"))?, pin(&format!("INP{s}"))?, pin(&format!("INN{s}"))?])
    }).collect();
    if sections.is_empty() {
        diag("no op-amp section (pins OUT, INP and INN, or OUTA, INPA and INNA ..); not simulated".to_owned());
        return PartKind::Passive;
    }
    let supply = ["VDD", "VCC"].iter().any(|n| pin(n).is_some()) && ["VSS", "VEE", "GND"].iter().any(|n| pin(n).is_some());
    if !supply {
        diag("no supply pins (VDD or VCC, and VSS, VEE or GND); not simulated".to_owned());
        return PartKind::Passive;
    }
    PartKind::OpAmp { sections }
}

/// A Schmitt-trigger inverter's sections by their pin names: `A` in and `Y` out for a single
/// one, `A1` and `Y1` to `A6` and `Y6` for a hex. Its supply is VDD or VCC and VSS or GND.
/// Without a section or a supply it is not simulated.
fn schmitt(comp: &Component, diag: &mut impl FnMut(String)) -> PartKind {
    let pin = |name: &str| comp.part.pins.iter().position(|p| p.name.eq_ignore_ascii_case(name));
    let sections: Vec<[usize; 2]> = ["", "1", "2", "3", "4", "5", "6"].iter().filter_map(|s| {
        Some([pin(&format!("A{s}"))?, pin(&format!("Y{s}"))?])
    }).collect();
    if sections.is_empty() {
        diag("no Schmitt inverter section (pins A and Y, or A1 and Y1 ..); not simulated".to_owned());
        return PartKind::Passive;
    }
    let supply = ["VDD", "VCC"].iter().any(|n| pin(n).is_some()) && ["VSS", "GND"].iter().any(|n| pin(n).is_some());
    if !supply {
        diag("no supply pins (VDD or VCC, and VSS or GND); not simulated".to_owned());
        return PartKind::Passive;
    }
    PartKind::Schmitt { sections }
}

/// A pot: its value is the track's resistance; its `pos` field (0 to 1, default 0.5) is where the
/// wiper starts, unless the bench turns it (`b.turn`), and its `taper` field (`lin` or `log`,
/// default `lin`) how the A-W half follows the turn. A component's own fields win over its part's.
fn pot(comp: &Component, bench: &BenchSpec, diag: &mut impl FnMut(String)) -> PartKind {
    let Some(ohms) = comp.value.as_ref().and_then(|v| v.qty).filter(|v| v.unit == QtyUnit::Ohm).map(|v| v.si)
        .filter(|r| r.is_finite() && *r > 0.0)
    else {
        diag("no resistance value; not simulated".to_owned());
        return PartKind::Passive;
    };
    let field = |key: &str| comp.fields.get(key).or_else(|| comp.part.fields.get(key));
    let turned = bench.turned.iter().find(|(path, _)| *path == comp.path).map(|&(_, pos)| pos);
    let pos = match (turned, field("pos")) {
        (Some(pos), _) => pos,
        (None, None) => 0.5,
        (None, Some(text)) => match text.trim().parse::<f64>() {
            Ok(p) if (0.0..=1.0).contains(&p) => p,
            _ => {
                diag(format!("pos '{text}' is not a number from 0 to 1; the wiper starts at 0.5"));
                0.5
            }
        },
    };
    let taper = match field("taper") {
        None => Taper::Linear,
        Some(text) => Taper::of_name(text).unwrap_or_else(|| {
            diag(format!("taper '{text}' is not lin or log; simulated as lin"));
            Taper::Linear
        }),
    };
    PartKind::Pot { ohms, pos, taper }
}

/// A stubbed part's pad electrics: its part's GPIO card when it has one, else the library's generic
/// profile. A card's leakage is rated at the highest voltage its supply pin (`supply`, VDD, VCC or VIO, the
/// lowering's order) is rated for, a datasheet's test condition; without one it keeps the generic
/// leakage.
fn pads(comp: &Component, libs: &ModelLibs) -> GpioParams {
    let Some(card) = &comp.part.gpio else {
        return libs.gpio_params();
    };
    let supply = supply(comp).into_iter().chain(["VDD", "VCC", "VIO"])
        .find_map(|name| comp.part.pins.iter().find(|pin| pin.name.eq_ignore_ascii_case(name)));
    let rated = supply.and_then(|pin| pin.limits.as_ref()).and_then(|limits| limits.volts).map(|volts| volts.max);
    libs.card_params(card, rated)
}

fn supply(comp: &Component) -> Option<&str> {
    comp.fields.get("supply").or_else(|| comp.part.fields.get("supply"))
}
