//! Lower a design unit through classification and the kernel-device binding table.

use crate::bench::{BenchSource, BenchSpec, ScopeProbe, SourceKind, Supply};
use crate::expect::Probe;
use crate::bind::{bind, bind_circuit};
use crate::bus::{BusPart, BusSide};
use crate::input::{Drive, LedColor, LowerInput, LowerPart, PartKind, Pull};
use crate::logic::{LogicConn, LogicDriver, LogicNet, LogicSide};
use crate::models::{ModelLibs, LED_RATED_A};
use crate::tolerance::{noise_seed, TolSite};
use crate::values::{Tune, ValueIndex};
use fab_elec_circuit::Circuit;
use fab_elec_core::ids::{BoardId, CompRef, NetRef};
use fab_elec_design::Design;
use fab_elec_sim::cosim::CoSimPin;
use fab_elec_sim::live::{Terminal, ValuePlan};
use fab_elec_sim::expr::Expr;
use fab_elec_sim::analysis::logic::Pads;
use fab_elec_mcu_rt::ports::PinRole;
use fab_elec_sim::{BKind, CtlKind, GpioParams, ModelKind, PinMode, PinPull, SimCircuit, Source, Wave};
use std::collections::{HashMap, HashSet};

/// A bench supply's output resistance in ohms: keeps it solvable beside another source on the net.
const SUPPLY_R: f64 = 1e-3;

/// An op-amp section's internal pole resistance, ohms: sets the scale of its gain stage (`gm =
/// a0 / OPAMP_R0`), not any terminal's behaviour.
const OPAMP_R0: f64 = 1e6;


/// The step of a resistor's simulated thermal noise, seconds: white to half the 96 kHz this sets
/// (past what is heard), and the run steps at least this often while the noise sounds.
const NOISE_DT: f64 = 1.0 / 96_000.0;

/// The conductance (siemens) that holds an op-amp section's internal node within its rails: at
/// the largest drive (`gm` times the supply) the node passes a rail by a few tens of millivolts.
const OPAMP_CLAMP: f64 = 10.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Lowered {
    pub circuit: SimCircuit,
    pub plan: ValuePlan,
    pub index: ValueIndex,
    pub diags: Vec<LowerDiag>,
    /// The LEDs: where a view reads each one's light output (live-rows.md, part rows' y).
    pub leds: Vec<LedTap>,
    /// Nominal dissipated watts at full glow, in the same order as the part rows.
    pub p_rated: Vec<f32>,
    /// Finite, non-degenerate R, C and L tolerance sites, in input part order, for ensemble draws.
    pub tol: Vec<TolSite>,
    /// Parts running firmware, in bench order.
    pub firmware: Vec<FwBinding>,
}

/// What a firmware part's build says of its pins (`fab_elec_fw::Built`), for Logic mode's facts
/// (`Lowered::logic_pads`): the pins it ever uses as analog and each declared pin's role, by the
/// chip's pin names.
pub struct PinFacts<'a> {
    pub part: &'a str,
    pub analog: &'a [String],
    pub pins: &'a [(String, PinRole)],
}

/// A part running firmware, as the co-simulation maker takes it: `cosim_pins(&part)` gives its GPIO
/// rows, `rails` the rows without a GPIO.
#[derive(Clone, Debug, PartialEq)]
pub struct FwBinding {
    /// Component path (`mcu.u`).
    pub part: String,
    /// The part data's `model`: the chip the firmware runs on, with a component field taking
    /// precedence over the part's. `None` when the part names none.
    pub chip: Option<String>,
    /// The firmware document's file name.
    pub doc: String,
    /// Compile-time constants by name, in the bench's order.
    pub params: Vec<(String, f64)>,
    /// The MCU clock in Hz: the crystal's value on the part's pins; `None` without one.
    pub f_clk: Option<f64>,
    /// The pins without a GPIO (supply and crystal pins), in package pin order: the pin name and the
    /// name of the node it sits on (the stub's VDD and VSS pins included). An unconnected pin has no row.
    pub rails: Vec<(String, String)>,
    /// The logic parts the host runs beside this part's machine, and their digital nets; empty
    /// without any (`crate::logic`).
    pub logic: LogicSide,
    /// The bus parts the host binds beside this part's machine; empty without any (`crate::bus`).
    pub bus: BusSide,
    /// The program's connections the bench declares (`b.net`), in bench order.
    pub net: Vec<crate::net::NetEndpoint>,
}

impl FwBinding {
    /// The speakers the firmware plays through: the bus's amplifiers (model `AMP`). A board's
    /// speaker is heard by default once its device runs, so a run of a binding with one starts
    /// at the audio rate (the host) and the app hears it (Listen). O(bus parts).
    pub fn speakers(&self) -> impl Iterator<Item = &BusPart> {
        self.bus.parts.iter().filter(|part| part.model.eq_ignore_ascii_case("AMP"))
    }
}

impl Lowered {
    /// The largest step the circuit's own sources allow: a noise source draws a new value every
    /// sample, and a run stepping over its samples would not make that noise. Infinite without one.
    /// O(sources).
    pub fn source_step(&self) -> f64 {
        let d = &self.circuit.devices;
        d.v.iter().chain(&d.i).filter_map(|s| match s.src.wave {
            Wave::Noise { dt, .. } => Some(dt),
            _ => None,
        }).fold(f64::INFINITY, f64::min)
    }

    /// The pads' facts for Logic mode's lowering rule (`fab_elec_sim::analysis::logic::Pads`), from
    /// each firmware part's build (`facts`):
    /// - logic pins: every pad except a firmware part's pins its build uses as analog, and except
    ///   every pad of a firmware part `facts` does not name (no build, no facts: its pins count as
    ///   analog); a stub's pads and the logic parts' outputs are logic pins;
    /// - pulls: those the firmware declares on its inputs;
    /// - write-only: the logic parts' outputs, and the firmware pins declared only an output or a
    ///   PWM.
    /// O(pads x firmware parts).
    pub fn logic_pads(&self, facts: &[PinFacts]) -> Pads {
        let mut pads = Pads::default();
        for g in &self.circuit.devices.gpio {
            let Some(fw) = self.firmware.iter().find(|fw| g.name.strip_prefix(fw.part.as_str()).is_some_and(|rest| rest.starts_with('#'))) else {
                pads.digital.insert(g.name.clone());
                continue;
            };
            let Some(f) = facts.iter().find(|f| f.part == fw.part) else { continue };
            let pin = g.name[fw.part.len() + 1..].strip_suffix(".io").unwrap_or_default();
            if f.analog.iter().any(|a| a == pin) {
                continue;
            }
            pads.digital.insert(g.name.clone());
            let roles: Vec<PinRole> = f.pins.iter().filter(|(p, _)| p == pin).map(|&(_, r)| r).collect();
            for r in &roles {
                if let PinRole::Input { pull } = *r {
                    let pull = match pull { 1 => PinPull::Up, 2 => PinPull::Down, _ => PinPull::None };
                    pads.pulls.insert(g.name.clone(), pull);
                }
            }
            if !roles.is_empty() && roles.iter().all(|r| matches!(r, PinRole::Output | PinRole::Pwm)) {
                pads.write_only.insert(g.name.clone());
            }
        }
        for fw in &self.firmware {
            for part in &fw.logic.parts {
                for (pin, conn) in &part.pins {
                    if let crate::logic::LogicConn::Out(_) = conn {
                        pads.write_only.insert(format!("{}#{pin}.io", part.path));
                    }
                }
            }
        }
        pads
    }

    /// The pin table of a part's co-simulated process: one row per signal pin its stub lowered as
    /// a GPIO (`{part}#{pin}.io`), in package pin order, as `CoSimDevice` takes it. Empty for a part
    /// that lowered no GPIO.
    pub fn cosim_pins(&self, part: &str) -> Vec<CoSimPin> {
        self.cosim_pins_in(&self.circuit, part)
    }

    /// `cosim_pins` of another lowering of this scope's circuit (Logic mode's): the rows whose
    /// GPIO it still has.
    fn cosim_pins_in(&self, c: &SimCircuit, part: &str) -> Vec<CoSimPin> {
        let prefix = format!("{part}#");
        let node = |n: u32| c.nodes[n as usize].name.clone();
        let row = |name: String, g: &fab_elec_sim::circuit::Gpio| CoSimPin {
            name, gpio: g.name.clone(), pad: node(g.pad), vss: node(g.vss), vdd: node(g.vdd), vih: g.params.vih, vil: g.params.vil,
        };
        let mut rows: Vec<CoSimPin> = c.devices.gpio.iter().filter_map(|g| {
            let pin = g.name.strip_prefix(&prefix)?.strip_suffix(".io")?;
            Some(row(pin.to_string(), g))
        }).collect();
        // the outputs of the logic parts beside it, after its own pins (`{logic}.{pin}`)
        if let Some(fw) = self.firmware.iter().find(|fw| fw.part == part) {
            for logic in &fw.logic.parts {
                for (pin, conn) in &logic.pins {
                    let LogicConn::Out(name) = conn else { continue };
                    let gpio = format!("{}#{pin}.io", logic.path);
                    if let Some(g) = c.devices.gpio.iter().find(|g| g.name == gpio) {
                        rows.push(row(name.clone(), g));
                    }
                }
            }
        }
        rows
    }
}

/// The scope in Logic mode (A-L1) for one firmware part: the analog circuit with the indicator
/// loads in and the key matrices out, its value plan (an LED a load stands for reads the load's
/// current), the logic plan with the matrices' nets and keys, the co-simulation rows of that
/// circuit, the loads, and the keys by the press sources they stand for
/// (`ValueIndex::logic_keys`).
#[derive(Clone, Debug)]
pub struct LogicMode {
    pub circuit: SimCircuit,
    pub values: ValuePlan,
    pub plan: fab_elec_mcu_cosim::LogicPlan,
    pub pins: Vec<CoSimPin>,
    pub loads: Vec<fab_elec_sim::analysis::logic::Load>,
    pub keys: Vec<String>,
    /// The timing cells, and the threshold inverters left analog with why.
    pub cells: Vec<fab_elec_sim::analysis::logic::CellTaken>,
    pub analog_cells: Vec<(String, String)>,
}

impl Lowered {
    /// Logic mode's timing cells on this lowering (`fab_elec_sim::analysis::logic::with_cells`):
    /// each threshold inverter whose input is a plain RC runs as a cell, its output pin read on
    /// the cell. Returns the cells and the inverters left analog with why. The firmware's logic
    /// plan (`logic_mode`) is not made: a co-simulated part runs pin by pin. Cost: `with_cells`.
    pub fn with_cells(&mut self) -> (Vec<fab_elec_sim::analysis::logic::CellTaken>, Vec<(String, String)>) {
        let (circuit, cells, analog) = fab_elec_sim::analysis::logic::with_cells(&self.circuit);
        self.circuit = circuit;
        cell_pins(&mut self.plan, &self.circuit);
        (cells, analog)
    }

    /// Logic mode for the firmware part `part` running at `f_clk` hertz (`logic_pads` for the
    /// facts): a matrix net driven by the part's own pad becomes a plan net its pin drives and
    /// reads, one driven by a logic part's output a plan net of that output; a key is named by
    /// its press source; a net's release goes in whole ticks, at least one. Fails when a matrix
    /// pad belongs to neither. Cost: the sim's `lower_logic`, then O(matrix nets x logic pins).
    pub fn logic_mode(&self, part: &str, facts: &[PinFacts], f_clk: f64) -> Result<LogicMode, String> {
        use fab_elec_mcu_cosim::LogicKey;
        use fab_elec_sim::analysis::logic::{lower as judge, lower_logic};
        let fw = self.firmware.iter().find(|fw| fw.part == part).ok_or_else(|| format!("no firmware part {part}"))?;
        let pads = self.logic_pads(facts);
        let report = judge(&self.circuit, &pads, None);
        let logic = lower_logic(&self.circuit, &pads, &report)?;
        let mut side = fw.logic.clone();
        let (mut keys, mut release) = (Vec::new(), Vec::new());
        let own = format!("{part}#");
        for m in &logic.matrices {
            let first = side.nets.len();
            for net in &m.nets {
                let index = side.nets.len() as u32;
                let g = self.circuit.devices.gpio.iter().find(|g| g.name == net.pad).ok_or_else(|| format!("no pad {}", net.pad))?;
                let name = self.circuit.nodes[g.pad as usize].name.clone();
                let driver = if let Some(pin) = net.pad.strip_prefix(&own).and_then(|p| p.strip_suffix(".io")) {
                    LogicDriver::Machine(pin.to_string())
                } else {
                    let found = side.parts.iter_mut().enumerate().find_map(|(k, lp)| {
                        let pin = net.pad.strip_prefix(&lp.path)?.strip_prefix('#')?.strip_suffix(".io")?;
                        let conn = lp.pins.iter_mut().find(|(name, conn)| name == pin && matches!(conn, LogicConn::Out(_)))?;
                        conn.1 = LogicConn::Net(index);
                        Some(LogicDriver::Part { part: k as u32, pin: pin.to_string() })
                    });
                    found.ok_or_else(|| format!("matrix pad {} is neither {part}'s nor a logic output", net.pad))?
                };
                side.nets.push(LogicNet { name, driver });
                if net.release > 0.0 {
                    release.push((index, ((net.release * f_clk).round() as u64).max(1)));
                }
            }
            for k in &m.keys {
                keys.push(LogicKey { name: k.press.clone(), a: (first + k.a) as u32, b: (first + k.b) as u32, diode: k.diode, pressed: k.pressed });
            }
        }
        let mut plan = side.plan();
        let names = keys.iter().map(|k| k.name.clone()).collect();
        plan.keys = keys;
        plan.release = release;
        let pins = self.cosim_pins_in(&logic.circuit, part);
        // an LED a load stands for: its anode's current is the load pad's supply current (a
        // source's vdd terminal, a sink's pad terminal)
        let mut values = self.plan.clone();
        for led in &self.leds {
            let Some(terms) = values.pins.get_mut(led.anode as usize) else { continue };
            if let Some(l) = logic.loads.iter().find(|l| terms.iter().any(|t| l.devices.contains(&t.device))) {
                *terms = vec![Terminal { device: l.pad.clone(), terminal: if l.high { 1 } else { 0 } }];
            }
        }
        cell_pins(&mut values, &logic.circuit);
        Ok(LogicMode {
            circuit: logic.circuit, values, plan, pins, loads: logic.loads, keys: names, cells: logic.cells, analog_cells: logic.analog_cells,
        })
    }
}

/// The pins of what each timing cell of `circuit` took read the cell: a threshold inverter's
/// output pin, read on its down switch, its output's current, and a pin of a resistor or
/// capacitor it absorbed what that part carries (`Cell::terminal_of`). Cost: O(cells x the
/// plan's terminals x the cell's parts), once a lowering.
fn cell_pins(values: &mut ValuePlan, circuit: &SimCircuit) {
    for cell in &circuit.devices.cell {
        for terms in &mut values.pins {
            if terms.iter().any(|t| t.device == cell.down.0) {
                *terms = vec![Terminal { device: cell.name.clone(), terminal: 0 }];
                continue;
            }
            // a terminal number past a `Terminal`'s byte (a cell of over 125 parts) stays unread
            for t in terms.iter_mut() {
                if let Some(terminal) = cell.terminal_of(&t.device, t.terminal.into()).and_then(|n| u8::try_from(n).ok()) {
                    *t = Terminal { device: cell.name.clone(), terminal };
                }
            }
        }
    }
}

/// An LED in a lowering: its part index, the pin index its forward current enters (the anode)
/// and the current its light output is full at (the model's rated point).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LedTap {
    pub part: u32,
    pub anode: u32,
    pub i_rated: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LowerDiag {
    pub part: Option<CompRef>,
    /// The net the note is about (a net with one connection, or none with a DC path to ground).
    pub net: Option<NetRef>,
    pub path: String,
    pub message: String,
}

/// Classify and lower one design unit, retaining its generation and diagnostic order.
pub fn lower(design: &Design, scope: BoardId, bench: &BenchSpec, libs: &ModelLibs) -> Lowered {
    let (input, mut diags) = bind(design, scope, bench, libs);
    let mut lowered = lower_with_packages(&input, design.generation, libs, design.unit(scope).map(|unit| unit.circuit.as_ref()));
    diags.append(&mut lowered.diags);
    lowered.diags = diags;
    attach_nets(&mut lowered, bench);
    lowered
}

/// Each firmware binding takes the connections the bench declares for its part.
fn attach_nets(lowered: &mut Lowered, bench: &BenchSpec) {
    for fw in &mut lowered.firmware {
        fw.net = bench.nets.iter().filter(|net| net.path == fw.part).cloned().collect();
    }
}

/// Lowers a circuit that is its own scope (a block bench): no design and no other units.
/// `circuit.unit` is the scope the bench's refs carry; its ground is the first ground-kind net (a
/// circuit without one gets a diag and grounds its first net, as a design unit does). A ref of
/// another scope is outside the input and gets its diag on its source or probe. The result runs
/// through `bench_run` like a design scope's; `generation` numbers its values (`ValueIndex`).
pub fn lower_circuit(circuit: &Circuit, bench: &BenchSpec, generation: u64, libs: &ModelLibs) -> Lowered {
    let (input, mut diags) = bind_circuit(circuit, circuit.unit, bench, libs);
    let mut lowered = lower_with_packages(&input, generation, libs, Some(circuit));
    diags.append(&mut lowered.diags);
    lowered.diags = diags;
    attach_nets(&mut lowered, bench);
    lowered
}

/// Lower a scope with unique net and part ids, preserving its net, part and pin ordering.
pub fn lower_input(input: &LowerInput, generation: u64, libs: &ModelLibs) -> Lowered {
    lower_with_packages(input, generation, libs, None)
}

/// Perceptual full-glow reference in watts when the part has no known package rating.
pub const DEFAULT_P_RATED: f32 = 0.5;

/// Nominal dissipation for chip passives and indicators, or a leaded resistor.
/// Package codes are whole groups of digits (including names such as R_0603_1608Metric).
fn nominal_watts(kind: &PartKind, footprint: Option<&fab_elec_parts::Footprint>) -> f32 {
    let Some(fp) = footprint else { return DEFAULT_P_RATED };
    if !matches!(kind, PartKind::Resistor { .. } | PartKind::Capacitor { .. }
        | PartKind::Inductor { .. } | PartKind::Led { .. }) {
        return DEFAULT_P_RATED;
    }
    if matches!(kind, PartKind::Resistor { .. }) && (fp.attrs.mount == fab_elec_parts::FpMount::Thru
        || fp.pads.iter().any(|p| p.kind == fab_elec_parts::PadKind::Thru)) {
        return 0.25;
    }
    let name = fp.name.to_ascii_lowercase();
    let packages = name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|field| !field.ends_with("metric"));
    for code in packages.flat_map(|field| field.split(|c: char| !c.is_ascii_digit())) {
        match code {
            "0402" => return 0.063,
            "0603" => return 0.1,
            "0805" => return 0.125,
            "1206" => return 0.25,
            _ => {}
        }
    }
    DEFAULT_P_RATED
}

/// Classified input omits package geometry. A design or circuit lowering still has its library parts.
fn lower_with_packages(input: &LowerInput, generation: u64, libs: &ModelLibs, source: Option<&Circuit>) -> Lowered {
    let mut circuit = SimCircuit::new();
    let mut plan = ValuePlan::default();
    let mut index = ValueIndex::new(generation);
    let mut diags = Vec::new();
    let mut leds = Vec::new();
    let mut p_rated = Vec::with_capacity(input.parts.len());
    let mut tol = Vec::new();
    let mut buses = crate::bus::buses(input);
    diags.append(&mut buses.diags);
    let mut nodes = HashMap::new();
    for net in &input.nets {
        let name = if net.id == input.ground { "0" } else { net.name.as_str() };
        nodes.insert(net.id, name);
        // Keep design/value indices stable, but a bus signal has no analog unknown or device.
        if !buses.nets.contains(&net.id) {
            circuit.node(name);
            if net.supply {
                circuit.declare_supply(name);
            }
        }
        index.add_net(net.id);
        plan.nets.push(name.to_owned());
    }
    let ground = nodes.get(&input.ground).copied().unwrap_or("0");
    let mut supplies = HashMap::new();
    for (i, supply) in input.supplies.iter().enumerate() {
        let path = format!("bench.supplies[{i}]");
        let Some(&node) = nodes.get(&supply.net) else {
            bench_diag(&mut diags, path, "net is outside the input; not simulated".to_owned());
            continue;
        };
        let problem = if supply.net == input.ground {
            Some("cannot supply the ground net; not simulated".to_owned())
        } else if !supply.v.is_finite() {
            Some(format!("voltage must be finite, got {}; not simulated", supply.v))
        } else if let Some(limit) = supply.limit.filter(|v| !v.is_finite() || *v <= 0.0) {
            Some(format!("current limit must be finite and > 0 A, got {limit}; not simulated"))
        } else if let Some(ramp) = supply.ramp.filter(|v| !v.is_finite() || *v < 0.0) {
            Some(format!("ramp must be finite and >= 0 s, got {ramp}; not simulated"))
        } else if supplies.contains_key(&supply.net) {
            Some(format!("a second supply on {node} is ignored"))
        } else {
            None
        };
        if let Some(problem) = problem {
            bench_diag(&mut diags, path, problem);
        } else {
            supplies.insert(supply.net, (i, supply));
        }
    }
    let mut usb_supplies = HashSet::new();
    let digital = crate::logic::digital(input);
    for (i, note) in &digital.notes {
        let part = &input.parts[*i];
        diags.push(LowerDiag { part: Some(part.id), net: None, path: part.path.clone(), message: format!("{}: {note}", part.path) });
    }
    for (part_index, part) in input.parts.iter().enumerate() {
        let footprint = source.and_then(|c| c.comps.get(part_index)).and_then(|comp| comp.part.footprint.as_deref());
        p_rated.push(nominal_watts(&part.kind, footprint));
        let consumed = buses.consumed.contains(&part_index);
        let tune = match &part.kind {
            _ if consumed => None,
            PartKind::Resistor { .. } | PartKind::Capacitor { .. } | PartKind::Inductor { .. } => {
                Some(Tune::device(part.path.clone(), "value"))
            }
            PartKind::UsbPower { .. } => Some(Tune::device(format!("{}#vbus", part.path), "dc")),
            PartKind::Button { .. } => Some(Tune::device(format!("{}#press", part.path), "dc")),
            PartKind::Pot { ohms, taper, .. } => Some(Tune::Pot {
                a: format!("{}#a", part.path), b: format!("{}#b", part.path), ohms: *ohms, taper: *taper, end: libs.pot_end,
            }),
            _ => None,
        };
        let Ok(pin_count) = u16::try_from(part.pins.len()) else {
            diags.push(LowerDiag {
                part: Some(part.id), net: None, path: part.path.clone(),
                message: format!("{}: pin count does not fit PinIx; not simulated", part.path),
            });
            let (_, first_pin) = index.add_comp(part.id, 0, None);
            plan.part_pins.push((first_pin as u32, 0));
            plan.parts.push(Vec::new());
            continue;
        };
        if let (Some((lo, hi)), Some(Tune::Device { device, key })) = (part.tol, &tune) {
            let nominal = match &part.kind {
                PartKind::Resistor { ohms } => Some(*ohms),
                PartKind::Capacitor { farads } => Some(*farads),
                PartKind::Inductor { henries } => Some(*henries),
                _ => None,
            };
            if let Some(nominal) = nominal.filter(|n| {
                lo.is_finite() && n.is_finite() && hi.is_finite() && lo <= *n && *n <= hi && lo < hi
            }) {
                tol.push(TolSite {
                    part: part_index as u32, path: part.path.clone(), device: device.clone(), key: key.clone(),
                    nominal, lo, hi,
                });
            }
        }
        let (comp, first_pin) = index.add_comp(part.id, pin_count, tune);
        if matches!(part.kind, PartKind::Button { .. }) {
            index.add_momentary(part.id);
        }
        if let PartKind::Pot { pos, .. } = part.kind {
            index.add_knob(part.id, pos);
        }
        plan.part_pins.push((first_pin as u32, part.pins.len() as u32));
        plan.pin_net.extend(part.pins.iter().map(|p| p.net.and_then(|r| index.net(r)).map(|i| i as u32)));
        let supply = if matches!(&part.kind, PartKind::UsbPower { .. }) {
            part.pins.iter().find(|pin| pin.name.eq_ignore_ascii_case("VBUS"))
                .and_then(|pin| pin.net).and_then(|net| supplies.get(&net))
                .map(|(_, supply)| *supply)
        } else {
            None
        };
        if let Some(supply) = supply {
            usb_supplies.insert(supply.net);
        }
        let mut skip = Vec::new();
        if digital.host.as_ref().is_some_and(|(path, _)| *path == part.path) { skip.extend_from_slice(&digital.skip); }
        if buses.host.as_ref().is_some_and(|(path, _)| *path == part.path) { skip.extend_from_slice(&buses.skip); }
        let mut binding = PartBinding {
            part,
            libs,
            nodes: &nodes,
            ground,
            supply,
            circuit: &mut circuit,
            diags: &mut diags,
            pins: vec![Vec::new(); part.pins.len()],
            devices: Vec::new(),
            anode: None,
            skip: &skip,
            outs: digital.outs.get(&part_index).map_or(&[], Vec::as_slice),
        };
        if !consumed { binding.lower(); }
        if let Some(a) = binding.anode {
            leds.push(LedTap { part: comp as u32, anode: (first_pin + a) as u32, i_rated: LED_RATED_A });
        }
        plan.pins.extend(binding.pins);
        plan.parts.push(binding.devices);
    }
    hold_oscillator_inputs(&mut circuit);
    for (i, supply) in input.supplies.iter().enumerate() {
        if supplies.get(&supply.net).map(|(i, _)| *i) != Some(i) || usb_supplies.contains(&supply.net) {
            continue;
        }
        let Some(&node) = nodes.get(&supply.net) else {
            continue;
        };
        let name = format!("bench#supply{i}");
        let src = format!("{name}.src");
        circuit.v(&name, &src, ground, supply_source(supply));
        if let Some(limit) = supply.limit {
            supply_limit(&mut circuit, &format!("{name}.limit"), (&src, ground, node), supply.v, limit, SUPPLY_R);
        } else {
            circuit.r(&format!("{name}.r"), &src, node, SUPPLY_R);
        }
    }
    for (i, source) in input.sources.iter().enumerate() {
        lower_source(i, source, &nodes, &mut circuit, &mut diags);
    }
    // before the floating nets are held: a resistive tip is a DC path of the net it probes
    for (i, probe) in input.probes.iter().enumerate() {
        lower_tip(i, probe, &nodes, ground, &mut circuit, &mut diags);
    }
    let floating = hold_floating_nets(&mut circuit, libs.board_leak);
    let digital_nets = digital.nets.union(&buses.nets).copied().collect();
    net_notes(input, &floating, &digital_nets, libs.board_leak, &mut diags);
    let firmware: Vec<FwBinding> = input.firmware.iter().filter_map(|(fw, f_clk)| {
        let Some(part) = input.parts.iter().find(|part| part.path == fw.path) else {
            bench_diag(&mut diags, fw.path.clone(),
                "firmware names no component in the scope; nothing runs it".to_owned());
            return None;
        };
        let PartKind::Firmware { chip, rails, .. } = &part.kind else { return None };
        if chip.is_none() {
            diags.push(LowerDiag {
                part: Some(part.id), net: None, path: part.path.clone(),
                message: format!("{}: no chip model: its part data names none (`model`)", part.path),
            });
        }
        let vdd = supply_pin(part, &nodes).map(|(i, _)| i);
        let vss = rail_pin(part, &nodes, &["VSS", "GND"]).map(|(i, _)| i);
        let rails = part.pins.iter().enumerate().filter_map(|(i, pin)| {
            if !rails.contains(&i) && Some(i) != vdd && Some(i) != vss {
                return None;
            }
            let node = nodes.get(&pin.net?)?;
            Some((pin.name.clone(), (*node).to_owned()))
        }).collect();
        let logic = digital.host.as_ref().filter(|(path, _)| *path == fw.path).map(|(_, side)| side.clone()).unwrap_or_default();
        let bus = buses.host.as_ref().filter(|(path, _)| *path == fw.path).map(|(_, side)| side.clone()).unwrap_or_default();
        Some(FwBinding {
            part: fw.path.clone(), chip: chip.clone(), doc: fw.doc.clone(), params: fw.params.clone(), f_clk: *f_clk, rails, logic, bus,
            net: Vec::new(),
        })
    }).collect();
    if let Some(binding) = firmware.first() {
        index.board_inputs(&binding.bus);
    }
    Lowered { circuit, plan, index, diags, leds, p_rated, tol, firmware }
}

fn supply_pin<'a>(part: &LowerPart, nodes: &HashMap<NetRef, &'a str>) -> Option<(usize, &'a str)> {
    let supply = match &part.kind {
        PartKind::Stub { supply, .. } | PartKind::Firmware { supply, .. } => supply.as_deref(),
        _ => None,
    };
    supply.and_then(|name| rail_pin(part, nodes, &[name]))
        .or_else(|| rail_pin(part, nodes, &["VDD", "VCC", "VIO"]))
}

fn rail_pin<'a>(part: &LowerPart, nodes: &HashMap<NetRef, &'a str>, names: &[&str]) -> Option<(usize, &'a str)> {
    names.iter().find_map(|name| {
        part.pins.iter().enumerate().find_map(|(i, pin)| {
            if pin.name.eq_ignore_ascii_case(name) {
                pin.net.and_then(|net| nodes.get(&net).copied()).map(|node| (i, node))
            } else {
                None
            }
        })
    })
}

/// A note located on each design net that is an open end or has no DC path to ground (a
/// resistor deleted from a live bench leaves one): one connection only (one part pin, bench
/// supply or bench source terminal in all), or a node `hold_floating_nets` had to hold with its
/// insulation leak. Neither stops the run; the note says where to look. O(pins).
fn net_notes(input: &LowerInput, floating: &[String], digital: &HashSet<NetRef>, leak: f64, diags: &mut Vec<LowerDiag>) {
    let mut count: HashMap<NetRef, (u32, Option<(&str, &str)>)> = HashMap::new();
    for part in &input.parts {
        for pin in &part.pins {
            if let Some(net) = pin.net {
                let c = count.entry(net).or_default();
                c.0 += 1;
                c.1.get_or_insert((part.path.as_str(), pin.name.as_str()));
            }
        }
    }
    let terminals = input.supplies.iter().map(|s| s.net).chain(input.sources.iter().flat_map(|s| [s.a, s.b]));
    for net in terminals {
        count.entry(net).or_default().0 += 1;
    }
    for net in &input.nets {
        // a digital net is not in the circuit: its leak holds a node nothing reads
        if net.id == input.ground || digital.contains(&net.id) {
            continue;
        }
        let note = |message: String| LowerDiag { part: None, net: Some(net.id), path: net.name.clone(), message };
        if floating.iter().any(|f| *f == net.name) {
            diags.push(note(format!("{}: no DC path to ground; its insulation leak ({leak:e} ohm) holds it", net.name)));
        } else if let Some((1, Some((part, pin)))) = count.get(&net.id).copied() {
            diags.push(note(format!("{}: one connection only ({part} {pin}): an open end", net.name)));
        }
    }
}

/// Gives each node with no DC path to ground its insulation resistance to ground, `leak`
/// (`{net}#leak`): a real net is never an open circuit, and one that only capacitors, control
/// inputs or connector pins touch would leave the operating point singular. Conducting at DC:
/// R, L, V, switches (on or off), junctions (D; Q's base-collector, base-emitter and substrate;
/// M's drain-source and body; its gate is insulated), a regulator's input and output, E and H
/// outputs, a GPIO's pad to its low rail (its leakage), a behavioural V, R or L, and a behavioural
/// current that reads either of its own terminals (a conductance, as an op-amp's output is); not
/// C, I, F or G outputs, control inputs, or a regulator's enable. Returns the held nodes' names.
/// O(devices).
fn hold_floating_nets(circuit: &mut SimCircuit, leak: f64) -> Vec<String> {
    fn root(parent: &mut [usize], mut a: usize) -> usize {
        while parent[a] != a {
            parent[a] = parent[parent[a]];
            a = parent[a];
        }
        a
    }
    let mut parent: Vec<usize> = (0..circuit.nodes.len()).collect();
    let mut join = |a: u32, b: u32| {
        let (ra, rb) = (root(&mut parent, a as usize), root(&mut parent, b as usize));
        if ra != rb {
            parent[ra] = rb;
        }
    };
    let d = &circuit.devices;
    d.r.iter().for_each(|x| join(x.a, x.b));
    d.l.iter().for_each(|x| join(x.a, x.b));
    d.v.iter().for_each(|x| join(x.p, x.n));
    d.sw.iter().for_each(|x| join(x.p, x.n));
    d.d.iter().for_each(|x| join(x.a, x.k));
    for q in &d.q {
        join(q.b, q.c);
        join(q.b, q.e);
        join(q.s, q.c);
    }
    for m in &d.m {
        join(m.d, m.s);
        join(m.b, m.s);
    }
    for g in &d.reg {
        join(g.vin, g.gnd);
        join(g.vout, g.gnd);
    }
    d.gpio.iter().for_each(|g| join(g.pad, g.vss));
    for b in &d.b {
        let conducts = match b.kind {
            BKind::V | BKind::R | BKind::L => true,
            BKind::I => b.reads.iter().any(|&(_, node)| node == b.p || node == b.n),
            _ => false,
        };
        if conducts {
            join(b.p, b.n);
        }
    }
    for e in &d.ctl {
        if matches!(e.kind, CtlKind::Vcvs | CtlKind::Ccvs) {
            join(e.p, e.n);
        }
    }
    let ground = root(&mut parent, 0);
    let floating: Vec<String> = (1..circuit.nodes.len())
        .filter(|&i| root(&mut parent, i) != ground)
        .map(|i| circuit.nodes[i].name.clone())
        .collect();
    for net in &floating {
        circuit.r(&format!("{net}#leak"), net, "0", leak);
    }
    floating
}

fn bench_diag(diags: &mut Vec<LowerDiag>, path: String, message: String) {
    diags.push(LowerDiag { part: None, net: None, message: format!("{path}: {message}"), path });
}

/// A voltage probe's tip load, `tip.r` and `tip.c` in parallel, across its two points: a probe of
/// one net's from that net to ground, a differential probe's between its nets. The elements are
/// named by what the probe reads (`bench#probe:{net}.r`, `bench#probe:{a}-{b}.c`; a bench probes
/// each at most once), so a reorder or a label edit keeps them, and a Replace carries the tip
/// capacitor's charge by name. An ideal tip lowers nothing, and neither does its bandwidth: that
/// is the channel's filter. A current or power probe has no tip load. O(1).
fn lower_tip(
    i: usize, probe: &ScopeProbe, nodes: &HashMap<NetRef, &str>, ground: &str,
    circuit: &mut SimCircuit, diags: &mut Vec<LowerDiag>,
) {
    let (r, c) = (probe.tip.r, probe.tip.c);
    if r.is_none() && c.is_none() {
        return;
    }
    let path = format!("bench.probes[{i}]");
    let (a, b) = match probe.probe {
        Probe::V(net) => (net, None),
        Probe::VDiff(a, b) => (a, Some(b)),
        Probe::I(_) | Probe::P(_) => {
            bench_diag(diags, path, "a current or power probe has no tip load; not simulated".to_owned());
            return;
        }
    };
    let node = |net: NetRef| nodes.get(&net).copied();
    let (Some(na), Some(nb)) = (node(a), b.map_or(Some(ground), node)) else {
        bench_diag(diags, path, "probe net is outside the input; its tip load is not simulated".to_owned());
        return;
    };
    let name = if b.is_some() { format!("bench#probe:{na}-{nb}") } else { format!("bench#probe:{na}") };
    if let Some(r) = r {
        if r.is_finite() && r > 0.0 {
            circuit.r(&format!("{name}.r"), na, nb, r);
        } else {
            bench_diag(diags, path.clone(), format!("tip resistance must be finite and > 0 ohms, got {r}; not simulated"));
        }
    }
    if let Some(c) = c {
        if c.is_finite() && c > 0.0 {
            circuit.c(&format!("{name}.c"), na, nb, c);
        } else {
            bench_diag(diags, path, format!("tip capacitance must be finite and > 0 F, got {c}; not simulated"));
        }
    }
}

/// The Schmitt inverter sections' switch models (`PartBinding::schmitt`): the down switch's `p` is
/// a section's output.
const SCHMITT_DOWN: &str = "SW_SCHMITT_DOWN";
const SCHMITT_UP: &str = "SW_SCHMITT_UP";

/// Each Schmitt section with its low supply on ground whose own output reaches its input through
/// resistors (an RC oscillator) has its input held at 0 V for the operating point, as a power-up
/// finds its timing capacitor: empty, and released at the run's start. Solved freely, an
/// oscillator's input and output would meet at the threshold, between the switches' states, and
/// never leave it. Any other input (a pull-up and a key, another section's output, a diode gate)
/// takes its operating point: held at 0 V, it would short whatever drives it to ground there and
/// pull down the rail the sections share. A path ends at a rail and at another section's output
/// (a driven node feeds nothing back). Cost: O(resistors) a section.
fn hold_oscillator_inputs(circuit: &mut SimCircuit) {
    let n = circuit.nodes.len();
    let mut on: Vec<Vec<u32>> = vec![Vec::new(); n];
    for r in &circuit.devices.r {
        on[r.a as usize].push(r.b);
        on[r.b as usize].push(r.a);
    }
    let sections: Vec<(u32, u32)> = circuit.devices.sw.iter()
        .filter(|s| s.model == SCHMITT_DOWN && s.n == 0 && s.cn == 0 && s.cp != 0).map(|s| (s.cp, s.p)).collect();
    let outs: HashSet<u32> = sections.iter().map(|&(_, out)| out).collect();
    for &(input, out) in &sections {
        let mut seen = vec![false; n];
        seen[input as usize] = true;
        let mut todo = vec![input];
        let mut fed_back = false;
        while let Some(x) = todo.pop() {
            if x == out {
                fed_back = true;
                break;
            }
            if x != input && (x == 0 || circuit.nodes[x as usize].supply || outs.contains(&x)) {
                continue;
            }
            for &y in &on[x as usize] {
                if !std::mem::replace(&mut seen[y as usize], true) {
                    todo.push(y);
                }
            }
        }
        if fed_back {
            circuit.nodes[input as usize].ic = Some(0.0);
        }
    }
}

fn supply_source(supply: &Supply) -> Source {
    match supply.ramp {
        Some(ramp) if ramp > 0.0 => Source::wave(Wave::Pwl(vec![(0.0, 0.0), (ramp, supply.v)])),
        _ => Source::dc(supply.v),
    }
}

fn supply_limit(circuit: &mut SimCircuit, name: &str, nodes: (&str, &str, &str), v: f64, limit: f64, rout: f64) {
    let card = format!("{name}.model");
    circuit.model(&card, ModelKind::Reg, &[
        ("vref", v + 1.0), ("vdo", 0.0), ("rout", rout), ("iq", 0.0),
        ("ilim", limit), ("isink", limit), ("ven", -1.0), ("wen", 0.05), ("w", 0.02),
    ]);
    let (vin, gnd, vout) = nodes;
    circuit.reg(name, vin, gnd, vout, gnd, &card);
}

fn lower_source(
    i: usize, source: &BenchSource, nodes: &HashMap<NetRef, &str>,
    circuit: &mut SimCircuit, diags: &mut Vec<LowerDiag>,
) {
    let path = format!("bench.sources[{i}]");
    let (Some(&a), Some(&b)) = (nodes.get(&source.a), nodes.get(&source.b)) else {
        bench_diag(diags, path, "source net is outside the input; not simulated".to_owned());
        return;
    };
    let problem = match &source.kind {
        SourceKind::Vdc(v) if !v.is_finite() => Some(format!("voltage must be finite, got {v}")),
        SourceKind::Idc(i) | SourceKind::LoadI(i) if !i.is_finite() => {
            Some(format!("current must be finite, got {i}"))
        }
        SourceKind::LoadR(r) if !r.is_finite() || *r <= 0.0 => {
            Some(format!("resistance must be finite and > 0 ohms, got {r}"))
        }
        SourceKind::Pulse { low, high, at, width, every, edge } => {
            if !low.is_finite() || !high.is_finite() {
                Some(format!("pulse levels must be finite, got low={low}, v={high}"))
            } else if !at.is_finite() || *at < 0.0 {
                Some(format!("pulse start must be finite and >= 0 s, got {at}"))
            } else if !edge.is_finite() || *edge <= 0.0 {
                Some(format!("pulse edge must be finite and > 0 s, got {edge}"))
            } else if !width.is_finite() || *width <= 0.0 {
                Some(format!("pulse width must be finite and > 0 s, got {width}"))
            } else if every.is_some_and(|p| !p.is_finite() || p < width + 2.0 * edge) {
                Some(format!("pulse period must be finite and at least the width and both edges, got {every:?}"))
            } else {
                None
            }
        }
        SourceKind::LoadStep { from, to, at, rise } => {
            if !from.is_finite() || !to.is_finite() {
                Some(format!("step currents must be finite, got from={from}, to={to}"))
            } else if !at.is_finite() || *at < 0.0 {
                Some(format!("step time must be finite and >= 0 s, got {at}"))
            } else if !rise.is_finite() || *rise <= 0.0 {
                Some(format!("step rise must be finite and > 0 s, got {rise}"))
            } else if !(at + rise).is_finite() || at + rise <= *at {
                Some(format!("step end must be finite and after at={at}, got rise={rise}"))
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(problem) = problem {
        bench_diag(diags, path, format!("{problem}; not simulated"));
        return;
    }
    let name = format!("bench#source{i}");
    match &source.kind {
        SourceKind::Vdc(v) => circuit.v(&name, a, b, Source::dc(*v)),
        SourceKind::Pulse { low, high, at, width, every, edge } => {
            // without a period the kernel's pulse runs once
            let per = every.unwrap_or(f64::INFINITY);
            circuit.v(&name, a, b, Source::wave(Wave::Pulse { v1: *low, v2: *high, td: *at, tr: *edge, tf: *edge, pw: *width, per }))
        }
        SourceKind::Idc(i) | SourceKind::LoadI(i) => circuit.i(&name, a, b, Source::dc(*i)),
        SourceKind::LoadR(r) => circuit.r(&name, a, b, *r),
        SourceKind::LoadStep { from, to, at, rise } => {
            let mut points = vec![(0.0, *from)];
            if *at != 0.0 {
                points.push((*at, *from));
            }
            points.push((at + rise, *to));
            circuit.i(&name, a, b, Source::wave(Wave::Pwl(points)));
        }
    }
}

struct PartBinding<'a> {
    part: &'a LowerPart,
    libs: &'a ModelLibs,
    nodes: &'a HashMap<NetRef, &'a str>,
    ground: &'a str,
    supply: Option<&'a Supply>,
    circuit: &'a mut SimCircuit,
    diags: &'a mut Vec<LowerDiag>,
    pins: Vec<Vec<Terminal>>,
    devices: Vec<String>,
    /// An LED's anode pin, once it is lowered.
    anode: Option<usize>,
    /// A firmware part's GPIO pins on digital nets: they get no GPIO device.
    skip: &'a [usize],
    /// A logic part's outputs into the circuit: (pin index, co-simulation row).
    outs: &'a [(usize, String)],
}

impl<'a> PartBinding<'a> {
    fn diag(&mut self, message: String) {
        self.diags.push(LowerDiag {
            part: Some(self.part.id),
            net: None,
            path: self.part.path.clone(),
            message: format!("{}: {message}", self.part.path),
        });
    }

    fn connected(&mut self, pin: usize) -> Option<&'a str> {
        let Some(pin) = self.part.pins.get(pin) else {
            self.diag(format!("pin {pin} is missing; not simulated"));
            return None;
        };
        let Some(net) = pin.net else {
            // a pin left open on purpose (a spare section's output) is no mistake to report
            if !pin.nc {
                self.diag(format!("pin {} is not connected; not simulated", pin.name));
            }
            return None;
        };
        let node = self.nodes.get(&net).copied();
        if node.is_none() {
            self.diag(format!("pin {} names a net outside the input; not simulated", pin.name));
        }
        node
    }

    fn named<const N: usize>(&mut self, names: [&str; N]) -> Option<[(usize, &'a str); N]> {
        let mut pins = [(0, ""); N];
        let mut complete = true;
        for (slot, name) in names.into_iter().enumerate() {
            if let Some(pin) = self.part.pins.iter().position(|p| p.name.eq_ignore_ascii_case(name)) {
                if let Some(node) = self.connected(pin) {
                    pins[slot] = (pin, node);
                } else {
                    complete = false;
                }
            } else {
                self.diag(format!("pin {name} is missing; not simulated"));
                complete = false;
            }
        }
        complete.then_some(pins)
    }

    fn bind(&mut self, pin: usize, device: &str, terminal: u8) {
        self.pins[pin].push(Terminal { device: device.to_owned(), terminal });
    }

    fn device(&mut self, role: &str) -> String {
        let name = format!("{}#{role}", self.part.path);
        self.devices.push(name.clone());
        name
    }

    fn discrete_model(&mut self, requested: &str, kind: ModelKind) -> Option<&'a str> {
        let libs = self.libs;
        let name = libs.discrete_model(Some(requested), kind, &mut |message| self.diag(message))?;
        if let Some(card) = self.circuit.find_model(name) {
            if card.kind != kind {
                self.diag(format!("model card '{name}' is already registered with another kind; not simulated"));
                return None;
            }
        } else {
            let (_, _, params) = libs.cards.iter().find(|(card, card_kind, _)| {
                card == name && *card_kind == kind
            })?;
            let params: Vec<_> = params.iter().map(|(key, value)| (key.as_str(), *value)).collect();
            self.circuit.model(name, kind, &params);
        }
        Some(name)
    }

    fn lower(&mut self) {
        let part = self.part;
        let path = part.path.as_str();
        match &part.kind {
            PartKind::Resistor { .. } | PartKind::Capacitor { .. } | PartKind::Inductor { .. } => {
                let (Some(a), Some(b)) = (self.connected(0), self.connected(1)) else {
                    return;
                };
                match &part.kind {
                    PartKind::Resistor { ohms } => {
                        self.circuit.r(path, a, b, *ohms);
                        if part.noise {
                            // the same draw every run of this design: a scope shows the same noise
                            self.circuit.thermal_noise(path, NOISE_DT, noise_seed(0, path));
                        }
                    }
                    PartKind::Capacitor { farads } => self.circuit.c(path, a, b, *farads),
                    PartKind::Inductor { henries } => self.circuit.l(path, a, b, *henries),
                    _ => unreachable!(),
                }
                self.devices.push(path.to_owned());
                self.bind(0, path, 0);
                self.bind(1, path, 1);
            }
            PartKind::Led { color } => {
                let Some([(a, anode), (k, cathode)]) = self.named(["A", "K"]) else {
                    return;
                };
                let card = match color {
                    LedColor::Red => "led_red",
                    LedColor::Orange => "led_orange",
                    LedColor::Yellow => "led_yellow",
                    LedColor::Green => "led_green",
                    LedColor::Blue => "led_blue",
                    LedColor::White => "led_white",
                    LedColor::Infrared => "led_infrared",
                    LedColor::YellowGreen => "led_yellow_green",
                };
                if self.circuit.find_model(card).is_none() {
                    let Some(params) = self.libs.led_params(*color) else {
                        self.diag(format!("the model library has no {color:?} LED; not simulated"));
                        return;
                    };
                    self.circuit.model(card, ModelKind::Diode, &params);
                }
                self.circuit.d(path, anode, cathode, card);
                self.devices.push(path.to_owned());
                self.bind(a, path, 0);
                self.bind(k, path, 1);
                self.anode = Some(a);
            }
            PartKind::Diode { model } => {
                let Some([(a, anode), (k, cathode)]) = self.named(["A", "K"]) else {
                    return;
                };
                let Some(card) = self.discrete_model(model, ModelKind::Diode) else {
                    return;
                };
                self.circuit.d(path, anode, cathode, card);
                self.devices.push(path.to_owned());
                self.bind(a, path, 0);
                self.bind(k, path, 1);
            }
            PartKind::Bjt { pnp, model } => {
                let Some([(c, collector), (b, base), (e, emitter)]) = self.named(["C", "B", "E"]) else {
                    return;
                };
                let kind = if *pnp { ModelKind::Pnp } else { ModelKind::Npn };
                let Some(card) = self.discrete_model(model, kind) else {
                    return;
                };
                self.circuit.q4(path, collector, base, emitter, emitter, card);
                self.devices.push(path.to_owned());
                for (pin, terminal) in [(c, 0), (b, 1), (e, 2), (e, 3)] {
                    self.bind(pin, path, terminal);
                }
            }
            PartKind::Mosfet { pmos, model } => {
                let Some([(d, drain), (g, gate), (s, source)]) = self.named(["D", "G", "S"]) else {
                    return;
                };
                let bulk = self.part.pins.iter().position(|pin| pin.name.eq_ignore_ascii_case("B"));
                let (b, bulk) = if let Some(b) = bulk {
                    let Some(bulk) = self.connected(b) else {
                        return;
                    };
                    (b, bulk)
                } else {
                    (s, source)
                };
                let kind = if *pmos { ModelKind::Pmos } else { ModelKind::Nmos };
                let Some(card) = self.discrete_model(model, kind) else {
                    return;
                };
                self.circuit.m(path, drain, gate, source, bulk, card, 100e-6, 100e-6);
                self.devices.push(path.to_owned());
                for (pin, terminal) in [(d, 0), (g, 1), (s, 2), (b, 3)] {
                    self.bind(pin, path, terminal);
                }
            }
            PartKind::Regulator { vout, ilim } => {
                let Some([(vin, vin_node), (gnd, gnd_node), (out, out_node), (en, en_node)]) =
                    self.named(["VIN", "GND", "VOUT", "EN"])
                else {
                    return;
                };
                let card = format!("{path}#model");
                self.circuit.model(&card, ModelKind::Reg, &[
                    ("vref", *vout), ("vdo", self.libs.reg_vdo), ("rout", self.libs.reg_rout),
                    ("iq", self.libs.reg_iq), ("ilim", *ilim),
                ]);
                self.circuit.reg(path, vin_node, gnd_node, out_node, en_node, &card);
                self.devices.push(path.to_owned());
                for (pin, terminal) in [(vin, 0), (gnd, 1), (out, 2), (en, 3)] {
                    self.bind(pin, path, terminal);
                }
            }
            PartKind::Stub { drives, rail, mcu, pads, .. } => self.stub(drives, *rail, *mcu, *pads, &[]),
            PartKind::Firmware { rails, crystal, pads, .. } => {
                let skip: Vec<usize> = rails.iter().chain(self.skip).copied().collect();
                self.stub(&[], None, true, *pads, &skip);
                self.oscillator(crystal);
            }
            PartKind::Logic { pads, .. } => self.logic(*pads),
            PartKind::UsbPower { volts } => {
                let Some([(vbus, bus_node), (gnd, gnd_node)]) = self.named(["VBUS", "GND"]) else {
                    return;
                };
                let src = format!("{path}#src");
                let v = self.device("vbus");
                let source = self.supply.map(supply_source).unwrap_or_else(|| Source::dc(*volts));
                self.circuit.v(&v, &src, gnd_node, source);
                if let Some((supply, limit)) = self.supply.and_then(|s| s.limit.map(|limit| (s, limit))) {
                    let limiter = self.device("limit");
                    supply_limit(self.circuit, &limiter, (&src, gnd_node, bus_node), supply.v, limit, self.libs.usb_cable_r);
                    self.bind(vbus, &limiter, 2);
                    self.bind(gnd, &v, 1);
                    self.bind(gnd, &limiter, 1);
                } else {
                    let r = self.device("cable");
                    self.circuit.r(&r, &src, bus_node, self.libs.usb_cable_r);
                    self.bind(vbus, &r, 1);
                    self.bind(gnd, &v, 1);
                }
            }
            PartKind::Button { pressed } => {
                let Some([(a, a_node), (b, b_node)]) = self.named(["A", "B"]) else {
                    return;
                };
                let ctl = format!("{path}#ctl");
                let press = self.device("press");
                self.circuit.v(&press, &ctl, "0", Source::dc(if *pressed { 1.0 } else { 0.0 }));
                if self.circuit.find_model("button").is_none() {
                    self.circuit.model("button", ModelKind::Sw, &[
                        ("ron", self.libs.button_ron), ("roff", self.libs.button_roff), ("vt", 0.5),
                        ("vh", 0.0), ("vband", 0.1),
                    ]);
                }
                self.circuit.sw(path, a_node, b_node, &ctl, "0", "button", *pressed);
                self.devices.push(path.to_owned());
                self.bind(a, path, 0);
                self.bind(b, path, 1);
            }
            PartKind::OpAmp { sections } => self.opamp(sections),
            PartKind::Schmitt { sections } => self.schmitt(sections),
            PartKind::Pot { ohms, pos, taper } => {
                let Some([(a, a_node), (w, w_node), (b, b_node)]) = self.named(["A", "W", "B"]) else {
                    return;
                };
                let (ra, rb) = taper.halves(*ohms, *pos, self.libs.pot_end);
                for (role, (p, np), (q, nq), r) in [("a", (a, a_node), (w, w_node), ra), ("b", (w, w_node), (b, b_node), rb)] {
                    let dev = self.device(role);
                    self.circuit.r(&dev, np, nq, r);
                    self.bind(p, &dev, 0);
                    self.bind(q, &dev, 1);
                }
            }
            PartKind::Bus { .. } | PartKind::Passive => {}
            PartKind::Other(kind) => self.diag(format!("no simulation model for {kind}; its pins float")),
        }
    }

    /// Each op-amp section as the library's macro-model (`OpAmpCard`), about one unknown a section:
    /// - a transconductance `gm` from its inputs into an internal node `{part}#x{s}`, which `R0`
    ///   and `C0` to ground make the dominant pole (`a0 = gm R0`, the pole at `gbw / a0`);
    /// - a clamp holding that node within the supply pins' voltages (`OPAMP_CLAMP` siemens past
    ///   either rail, nothing between), so the output swings rail to rail and the pole's
    ///   integrator never winds up past a rail;
    /// - the output, a current into the output pin of `ilim tanh((v(x) - v(out)) / (rout ilim))`:
    ///   `rout` for small loads, never more than `ilim` either way.
    ///
    /// The inputs draw nothing. The supply pins carry the sections' quiescent current; the output
    /// current returns through ground, not through the rails (a load current on the rails is not
    /// modelled). Without connected supply pins nothing is lowered (the binding said why).
    fn opamp(&mut self, sections: &[[usize; 3]]) {
        let Some((vdd_pin, vdd)) = self.rail_pin(&["VDD", "VCC"]) else {
            self.diag("supply pin VDD or VCC is not connected; not simulated".to_owned());
            return;
        };
        let Some((vss_pin, vss)) = self.rail_pin(&["VSS", "VEE", "GND"]) else {
            self.diag("supply pin VSS, VEE or GND is not connected; not simulated".to_owned());
            return;
        };
        let card = self.libs.opamp;
        let gm = card.a0 / OPAMP_R0;
        let c0 = card.a0 / (2.0 * std::f64::consts::PI * card.gbw * OPAMP_R0);
        let ground = self.ground;
        let path = self.part.path.clone();
        let mut lowered = 0;
        for (k, &[out, inp, inn]) in sections.iter().enumerate() {
            let (Some(out_node), Some(inp_node), Some(inn_node)) = (self.connected(out), self.connected(inp), self.connected(inn)) else {
                continue;
            };
            // a single op-amp's section has no letter; a multiple one's are a, b, ..
            let s = if sections.len() == 1 { String::new() } else { ((b'a' + k as u8) as char).to_string() };
            let x = format!("{path}#x{s}");
            let gain = self.device(&format!("gm{s}"));
            self.circuit.vctl(&gain, CtlKind::Vccs, ground, &x, inp_node, inn_node, gm);
            let r0 = self.device(&format!("r0{s}"));
            self.circuit.r(&r0, &x, ground, OPAMP_R0);
            let c = self.device(&format!("c0{s}"));
            self.circuit.c(&c, &x, ground, c0);
            let clamp = self.device(&format!("clamp{s}"));
            let mut e = Expr::parse(&format!("{OPAMP_CLAMP}*(uramp(v(x)-v(hi))-uramp(v(lo)-v(x)))")).expect("the clamp parses");
            e.rename(&|n| match n { "x" => x.clone(), "hi" => vdd.to_owned(), _ => vss.to_owned() }, &|d| d.to_owned());
            self.circuit.b(&clamp, BKind::I, &x, ground, e);
            let drive = self.device(&format!("out{s}"));
            let (rout, ilim) = (card.rout, card.ilim);
            let mut e = Expr::parse(&format!("{ilim}*tanh((v(x)-v(o))/{})", rout * ilim)).expect("the output parses");
            e.rename(&|n| if n == "x" { x.clone() } else { out_node.to_owned() }, &|d| d.to_owned());
            self.circuit.b(&drive, BKind::I, ground, out_node, e);
            self.bind(out, &drive, 1);
            lowered += 1;
        }
        if lowered > 0 {
            let iq = self.device("iq");
            self.circuit.i(&iq, vdd, vss, Source::dc(card.iq * lowered as f64));
            self.bind(vdd_pin, &iq, 0);
            self.bind(vss_pin, &iq, 1);
        }
    }

    /// Each Schmitt inverter section as the library's card (`SchmittCard`): two switches with
    /// hysteresis, both controlled by the input against the low supply pin.
    /// - `{part}#dn{s}` from the output to the low supply pin, closing once the input rises past
    ///   `vt_plus` and opening once it falls past `vt_minus`;
    /// - `{part}#up{s}` from the high supply pin to the output, on the negated control and
    ///   thresholds, so it opens and closes the other way round and exactly one is closed at any
    ///   input;
    /// - `{part}#cin{s}` from the input and, when the card has one, `{part}#cout{s}` from the
    ///   output to the low supply pin.
    ///
    /// A switch with hysteresis commits as its control enters its band, as a trigger's own
    /// regeneration does, so an RC oscillator flips at its thresholds and keeps oscillating.
    ///
    /// The kernel locates each switching instant, so an oscillator's edges fall where the
    /// thresholds put them, not on a step. Its supply pins carry the quiescent current.
    ///
    /// An oscillator's input is held at ground for the operating point once every part is lowered
    /// (`hold_oscillator_inputs`). An RC oscillator has no operating point of its own: solved freely, its input and
    /// output would meet at the threshold, between the switches' states, and never leave it. A
    /// driven input moves to its value at the first step, as at a power-up.
    fn schmitt(&mut self, sections: &[[usize; 2]]) {
        let Some((vdd_pin, vdd)) = self.rail_pin(&["VDD", "VCC"]) else {
            self.diag("supply pin VDD or VCC is not connected; not simulated".to_owned());
            return;
        };
        let Some((vss_pin, vss)) = self.rail_pin(&["VSS", "GND"]) else {
            self.diag("supply pin VSS or GND is not connected; not simulated".to_owned());
            return;
        };
        let card = self.libs.schmitt;
        let (vt, vh) = ((card.vt_plus + card.vt_minus) / 2.0, (card.vt_plus - card.vt_minus) / 2.0);
        let (down, up) = (SCHMITT_DOWN, SCHMITT_UP);
        if self.circuit.find_model(down).is_none() {
            self.circuit.model(down, ModelKind::Sw, &[("vt", vt), ("vh", vh), ("ron", card.ron), ("roff", card.roff)]);
            self.circuit.model(up, ModelKind::Sw, &[("vt", -vt), ("vh", vh), ("ron", card.ron), ("roff", card.roff)]);
        }
        let mut lowered = 0;
        for (k, &[inp, out]) in sections.iter().enumerate() {
            let (Some(in_node), Some(out_node)) = (self.connected(inp), self.connected(out)) else {
                continue;
            };
            // a single inverter's section has no number; a hex's are 1 to 6
            let s = if sections.len() == 1 { String::new() } else { (k + 1).to_string() };
            let dn = self.device(&format!("dn{s}"));
            self.circuit.sw(&dn, out_node, vss, in_node, vss, down, false);
            let pull = self.device(&format!("up{s}"));
            self.circuit.sw(&pull, vdd, out_node, vss, in_node, up, true);
            let cin = self.device(&format!("cin{s}"));
            self.circuit.c(&cin, in_node, vss, card.cin);
            if card.cout > 0.0 {
                let cout = self.device(&format!("cout{s}"));
                self.circuit.c(&cout, out_node, vss, card.cout);
            }
            self.bind(out, &dn, 0);
            self.bind(inp, &cin, 0);
            lowered += 1;
        }
        if lowered > 0 {
            let iq = self.device("iq");
            self.circuit.i(&iq, vdd, vss, Source::dc(card.iq * lowered as f64));
            self.bind(vdd_pin, &iq, 0);
            self.bind(vss_pin, &iq, 1);
        }
    }

    fn rail_pin(&self, names: &[&str]) -> Option<(usize, &'a str)> {
        rail_pin(self.part, self.nodes, names)
    }

    fn bind_rail(&mut self, rail: (Option<usize>, &str), device: &str, terminal: u8) {
        if let Some(pin) = rail.0 {
            self.bind(pin, device, terminal);
        }
    }

    /// An ideal-clock MCU's crystal oscillator at DC: its inverting amplifier biases itself at half
    /// its supply through the feedback resistor between the crystal pins, so the crystal's nets
    /// have a DC path. Each connected crystal pin goes through half the feedback resistance
    /// (`{pin}.rf`) to the node `{part}#osc`, which twice the feedback resistance to each rail
    /// (`osc.up`, `osc.down`) holds at half the supply: two crystal pins see `osc_rf` between them
    /// and rest at half the supply, and the bias draws VDD / (4 osc_rf), 1.25 uA at 5 V. Neither
    /// the amplifier's gain nor the oscillation is solved: the clock is ideal. Without a connected
    /// supply pin nothing is lowered (the stub said why). O(pins).
    fn oscillator(&mut self, crystal: &[usize]) {
        let Some((vdd_pin, vdd)) = supply_pin(self.part, self.nodes) else {
            return;
        };
        let vss = self.rail_pin(&["VSS", "GND"])
            .map(|(pin, node)| (Some(pin), node)).unwrap_or((None, self.ground));
        let pins: Vec<(usize, &str)> = crystal.iter()
            .filter_map(|&pin| self.connected(pin).map(|node| (pin, node))).collect();
        if pins.is_empty() {
            return;
        }
        let rf = self.libs.osc_rf;
        let osc = format!("{}#osc", self.part.path);
        for (pin, node) in pins {
            let dev = self.device(&format!("{}.rf", self.part.pins[pin].name));
            self.circuit.r(&dev, node, &osc, rf / 2.0);
            self.bind(pin, &dev, 0);
        }
        let up = self.device("osc.up");
        self.circuit.r(&up, vdd, &osc, 2.0 * rf);
        self.bind(vdd_pin, &up, 0);
        let down = self.device("osc.down");
        self.circuit.r(&down, &osc, vss.1, 2.0 * rf);
        self.bind_rail(vss, &down, 1);
    }

    /// A logic part's outputs into the circuit, each a GPIO pad `{pin}.io` on the part's supply
    /// pins (VCC or VDD, GND or VSS) with its pin capacitance, starting low until the digital
    /// side drives it; its inputs and its outputs on digital nets lower nothing. Without a supply
    /// pin on a net nothing is lowered. O(pins).
    fn logic(&mut self, pads: GpioParams) {
        if self.outs.is_empty() {
            return;
        }
        let Some(vdd) = self.rail_pin(&["VCC", "VDD"]).map(|(pin, node)| (Some(pin), node)) else {
            self.diag("no supply pin (VCC or VDD) on a net; its outputs float".to_owned());
            return;
        };
        let vss = self.rail_pin(&["GND", "VSS"]).map(|(pin, node)| (Some(pin), node)).unwrap_or((None, self.ground));
        for (pin, _) in self.outs {
            let Some(node) = self.connected(*pin) else { continue };
            let name = self.part.pins[*pin].name.clone();
            let dev = self.device(&format!("{name}.io"));
            self.circuit.gpio(&dev, node, vdd.1, vss.1, pads, PinMode::OutLow);
            self.bind(*pin, &dev, 0);
            self.bind_rail(vdd, &dev, 1);
            self.bind_rail(vss, &dev, 2);
            let cin = self.device(&format!("{name}.cin"));
            self.circuit.c(&cin, node, vss.1, self.libs.pin_cin);
            self.bind(*pin, &cin, 0);
            self.bind_rail(vss, &cin, 1);
        }
    }

    fn stub(&mut self, drives: &[(String, Drive)], rail: Option<NetRef>, mcu: bool, pads: GpioParams, skip: &[usize]) {
        let part = self.part;
        for (name, _) in drives {
            if !part.pins.iter().any(|pin| pin.name == *name) {
                self.diag(format!("stub names missing pin {name}"));
            }
        }
        let vdd = if let Some((pin, node)) = supply_pin(part, self.nodes) {
            if rail.is_some() {
                self.diag(format!("stub rail ignored: the part has supply pin {}", part.pins[pin].name));
            }
            (Some(pin), node)
        } else if let Some(rail) = rail {
            let Some(&node) = self.nodes.get(&rail) else {
                self.diag("stub rail is outside the scope; not simulated".to_owned());
                return;
            };
            (None, node)
        } else {
            self.diag("stub needs a supply pin (the `supply` field, then VDD, VCC or VIO) or a rail; not simulated".to_owned());
            return;
        };
        let vss = self.rail_pin(&["VSS", "GND"])
            .map(|(pin, node)| (Some(pin), node)).unwrap_or((None, self.ground));
        if mcu && vdd.0.is_some() && vss.0.is_some() {
            let iq = self.device("iq");
            self.circuit.i(&iq, vdd.1, vss.1, Source::dc(self.libs.mcu_iq));
            self.bind_rail(vdd, &iq, 0);
            self.bind_rail(vss, &iq, 1);
        }
        for (pin, p) in part.pins.iter().enumerate() {
            if Some(pin) == vdd.0 || Some(pin) == vss.0 || p.net.is_none() || skip.contains(&pin) {
                continue;
            }
            let Some(node) = self.connected(pin) else {
                continue;
            };
            let drive = drives.iter().find(|(name, _)| *name == p.name)
                .map(|(_, drive)| drive).unwrap_or(&Drive::Input(Pull::None));
            let mode = match drive {
                Drive::Level(true) => PinMode::OutHigh,
                Drive::Level(false) => PinMode::OutLow,
                Drive::Pwm { duty, .. } if *duty <= 0.0 => PinMode::OutLow,
                Drive::Pwm { duty, .. } if *duty >= 1.0 => PinMode::OutHigh,
                // Retunes preload on the running grid; a level-to-PWM write joins this global grid.
                Drive::Pwm { freq, duty } => PinMode::Pwm { period: 1.0 / freq, high: duty / freq, origin: 0.0 },
                Drive::Input(pull) => PinMode::Input(match pull {
                    Pull::None => PinPull::None,
                    Pull::Up => PinPull::Up,
                    Pull::Down => PinPull::Down,
                }),
            };
            let dev = self.device(&format!("{}.io", p.name));
            self.circuit.gpio(&dev, node, vdd.1, vss.1, pads, mode);
            self.bind(pin, &dev, 0);
            self.bind_rail(vdd, &dev, 1);
            self.bind_rail(vss, &dev, 2);
            // Every pin has its pad capacitance: an input's sets its timing with the pull; a driven
            // pin's is a pole of about 0.1 ns under its drive resistance, which the stepper steps
            // over after an edge (`STIFF_CARRY` in the transient stepper).
            let cin = self.device(&format!("{}.cin", p.name));
            self.circuit.c(&cin, node, vss.1, self.libs.pin_cin);
            self.bind(pin, &cin, 0);
            self.bind_rail(vss, &cin, 1);
        }
    }
}
