//! Electrical rules for component pins and block ports. Block checks and the schematic share
//! the voltage, demand and capacity of each rail. Grouping declarations walks their ancestry,
//! so its cost is linear in blocks, ports, bindings, pins and nets times the tree depth.
//! Pin supply propagation and sorting findings retain their separate costs.

use crate::check::{CheckKind, Measure, Violation};
use crate::{Block, Circuit, NetKind, PortDef, PortKind, QtyUnit, Quantity};
use fab_elec_core::{BlockId, BoardId, CompId, CompRef, NetId, NetRef, PinIx, PinRef, Sel, Severity};
use fab_elec_parts::{PartKind, PinKind, VRange};
use std::collections::HashSet;

/// The outermost block declaring an output on this rail, or an independent component pin.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum RailSource {
    Block(BlockId),
    Pin(CompId, PinIx),
}

impl RailSource {
    fn selector(self, board: BoardId) -> Sel {
        match self {
            Self::Block(b) => Sel::Block(board, b),
            Self::Pin(comp, pin) => Sel::Pin(PinRef { board, comp, pin }),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RailSummary {
    pub volts: Option<VRange>,
    pub volts_conflict: bool,
    pub sources: Vec<RailSource>,
    pub budget: Option<f64>,
    pub demand: f64,
    pub has_demand: bool,
}

fn ancestors(c: &Circuit, first: Option<BlockId>) -> impl Iterator<Item = BlockId> + '_ {
    std::iter::successors(first, |id| c.tree.get(id.ix()).and_then(|b| b.parent))
}

fn planned_blocks(c: &Circuit) -> Vec<bool> {
    c.tree.iter().map(|b| ancestors(c, Some(b.id)).any(|id| {
        c.tree[id.ix()].def.is_some_and(|d| c.block_defs[d.0 as usize].planned)
    })).collect()
}

fn ports(c: &Circuit) -> impl Iterator<Item = (&Block, &PortDef, &[NetId])> {
    c.tree.iter().filter_map(|b| b.def.map(|d| (b, &c.block_defs[d.0 as usize]))).flat_map(|(b, d)| {
        d.ports.iter().zip(&b.ports).map(move |(port, (_, nets))| (b, port, nets.as_slice()))
    })
}

fn intersect(a: Option<VRange>, b: Option<VRange>) -> Option<VRange> {
    match (a, b) {
        (Some(a), Some(b)) => Some(VRange { min: a.min.max(b.min), max: a.max.min(b.max) }),
        _ => a.or(b),
    }
}

/// One entry per net, in net order. A source's wrapper and backing pin describe the same
/// supply at different boundaries, so the lowest declared current limit applies.
pub fn rail_summaries(c: &Circuit) -> Vec<RailSummary> {
    let mut rails = vec![RailSummary::default(); c.nets.len()];
    let mut port_volts = vec![None; rails.len()];
    let mut pin_volts = vec![None; rails.len()];
    let mut outputs = HashSet::new();
    let mut consumers = HashSet::new();
    for (b, port, nets) in ports(c) {
        match port.kind {
            PortKind::PowerOut => outputs.extend(nets.iter().map(|&n| (b.id, n))),
            PortKind::PowerIn if port.current.is_some() => consumers.extend(nets.iter().map(|&n| (b.id, n))),
            _ => {}
        }
    }
    let source_block = |b, n| ancestors(c, Some(b)).filter(|&id| outputs.contains(&(id, n))).last();
    let mut seen = HashSet::new();
    let mut source = |rail: &mut RailSummary, n: NetId, id: RailSource, current: Option<f64>| {
        if seen.insert((n, id)) {
            rail.sources.push(id);
        }
        if let Some(current) = current {
            rail.budget = Some(rail.budget.map_or(current, |v| v.min(current)));
        }
    };
    for (b, port, nets) in ports(c) {
        for &n in nets {
            let rail = &mut rails[n.ix()];
            match port.kind {
                PortKind::PowerOut => {
                    let id = source_block(b.id, n).expect("a bound output is itself a source");
                    source(rail, n, RailSource::Block(id), port.current);
                    port_volts[n.ix()] = intersect(port_volts[n.ix()], port.volts);
                }
                PortKind::PowerIn => {
                    if let Some(load) = port.current {
                        if !ancestors(c, b.parent).any(|id| consumers.contains(&(id, n))) {
                            rail.demand += load;
                            rail.has_demand = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let planned = planned_blocks(c);
    for comp in &c.comps {
        if planned.get(comp.block.ix()) == Some(&true) || comp.part.kind == PartKind::PowerFlag {
            continue;
        }
        for &(p, n) in &comp.conns {
            let Some(pin) = comp.part.pin_def(p).filter(|p| p.kind == PinKind::PowerOut) else { continue };
            let id = source_block(comp.block, n).map_or(RailSource::Pin(comp.id, p), RailSource::Block);
            let limits = pin.limits.as_ref();
            source(&mut rails[n.ix()], n, id, limits.and_then(|l| l.current));
            pin_volts[n.ix()] = intersect(pin_volts[n.ix()], limits.and_then(|l| l.volts));
        }
    }
    for (i, rail) in rails.iter_mut().enumerate() {
        rail.volts_conflict = intersect(port_volts[i], pin_volts[i]).is_some_and(|v| v.min > v.max);
        if !rail.volts_conflict {
            rail.volts = c.nets[i].volts.or(port_volts[i]).or(pin_volts[i]);
        }
        if rail.sources.len() != 1 {
            rail.budget = None;
        }
    }
    rails
}

/// The nets supplied by a declared source, bench or jack, including rails fed through series parts.
fn power_driven(c: &Circuit, mut driven: Vec<bool>, planned: &[bool]) -> Vec<bool> {
    let mut series = Vec::new();
    for comp in &c.comps {
        if planned.get(comp.block.ix()) == Some(&true) { continue; }
        let usb = comp.fields.get("sim").or_else(|| comp.part.fields.get("sim")) == Some("usb");
        let two_pin_series = comp.part.pins.len() == 2 && matches!(comp.part.kind, PartKind::Resistor | PartKind::Inductor | PartKind::Fuse);
        let mut ends = [None; 2];
        for &(pin, net) in &comp.conns {
            let Some(def) = comp.part.pin_def(pin) else { continue };
            let Some(supplied) = driven.get_mut(net.ix()) else { continue };
            if def.kind == PinKind::PowerOut || (usb && (def.name.eq_ignore_ascii_case("VBUS") || def.name.eq_ignore_ascii_case("GND"))) {
                *supplied = true;
            }
            if two_pin_series {
                ends[pin.ix()] = Some(net.ix());
            }
        }
        if let [Some(a), Some(b)] = ends {
            series.push((a, b));
        }
    }
    loop {
        let mut changed = false;
        for &(a, b) in &series {
            let to = match (driven[a], driven[b]) {
                (true, false) => b,
                (false, true) => a,
                _ => continue,
            };
            if matches!(c.nets[to].kind, NetKind::Power | NetKind::Ground) {
                driven[to] = true;
                changed = true;
            }
        }
        if !changed {
            return driven;
        }
    }
}

pub fn erc(c: &Circuit) -> Vec<Violation> {
    erc_supplied(c, &[])
}

/// Check the circuit with these nets driven by the bench. Each supplied net adds one lookup.
pub fn erc_supplied(c: &Circuit, supplied: &[NetId]) -> Vec<Violation> {
    let unit = Some(c.unit);
    let pin_sel = |comp, pin| Sel::Pin(PinRef { board: c.unit, comp, pin });
    let net_sel = |net| Sel::Net(NetRef { board: c.unit, net });
    let mut out = Vec::new();
    let rails = rail_summaries(c);
    let planned = planned_blocks(c);
    let mut endpoints = vec![0; rails.len()];
    let mut signal_driver = vec![false; rails.len()];
    let mut direct_power = vec![false; rails.len()];
    for n in supplied {
        if let Some(value) = direct_power.get_mut(n.ix()) { *value = true; }
    }
    for (block, port, nets) in ports(c).filter(|(b, _, _)| planned[b.id.ix()]) {
        if nets.is_empty() {
            out.push(Violation::new(CheckKind::OpenPort, Severity::Warning, unit, vec![Sel::Block(c.unit, block.id)],
                &port.name, format!("{}.{} is not connected", block.path, port.name)));
        }
        for n in nets {
            endpoints[n.ix()] += 1;
            match port.kind {
                PortKind::PowerOut => direct_power[n.ix()] = true,
                PortKind::Out | PortKind::Bidi | PortKind::Passive | PortKind::Bus { .. } | PortKind::Pair | PortKind::Protocol(_) => signal_driver[n.ix()] = true,
                _ => {}
            }
        }
    }
    let mut net_pins = c.net_pins();
    for (i, pins) in net_pins.0.iter_mut().enumerate() {
        pins.retain(|(id, _)| planned.get(c.comps[id.ix()].block.ix()) != Some(&true));
        endpoints[i] += pins.len();
        direct_power[i] |= pins.iter().any(|&(id, p)| c.pin_kind(id, p) == Some(PinKind::PowerOut));
    }
    let driven = power_driven(c, direct_power.clone(), &planned);
    for (ni, pins) in net_pins.0.iter().enumerate() {
        let net = &c.nets[ni];
        let kinds: Vec<PinKind> = pins.iter().filter_map(|(comp, pin)| c.pin_kind(*comp, *pin)).collect();
        if pins.len() == 1 && endpoints[ni] == 1 {
            out.push(
                Violation::new(CheckKind::SinglePinNet, Severity::Warning, unit, vec![net_sel(net.id)], "erc", format!("net {} has one pin", net.name))
                    .hint("connect it, or mark the pin not connected"),
            );
        }
        let count = |k: PinKind| kinds.iter().filter(|x| **x == k).count();
        if count(PinKind::Out) > 1 || count(PinKind::PowerOut) > 1 || (count(PinKind::Out) > 0 && count(PinKind::PowerOut) > 0) {
            let items: Vec<Sel> = pins
                .iter()
                .filter(|(cm, p)| matches!(c.pin_kind(*cm, *p), Some(PinKind::Out | PinKind::PowerOut)))
                .map(|(cm, p)| pin_sel(*cm, *p))
                .collect();
            out.push(Violation::new(CheckKind::PinConflict, Severity::Error, unit, items, "erc", format!("net {} has more than one output driving it", net.name)));
        } else if rails[ni].sources.len() > 1 {
            let items = rails[ni].sources.iter().map(|s| s.selector(c.unit)).collect();
            out.push(Violation::new(CheckKind::PinConflict, Severity::Error, unit, items, &net.name,
                format!("{} has multiple independent power sources", net.name)));
        }
        let rail = &rails[ni];
        if rail.volts_conflict {
            let mut items: Vec<_> = rail.sources.iter().map(|s| s.selector(c.unit)).collect();
            items.push(net_sel(net.id));
            out.push(Violation::new(CheckKind::VoltageDomain, Severity::Error, unit, items, "erc",
                format!("{} has sources with incompatible voltages", net.name)));
        }
        if let Some(cap) = rail.budget.filter(|cap| rail.demand > cap * (1.0 + 1e-9)) {
            let amps = |si| Measure::Qty(Quantity { si, unit: QtyUnit::Ampere });
            let items = vec![rail.sources[0].selector(c.unit), net_sel(net.id)];
            out.push(Violation::new(CheckKind::RailBudget, Severity::Error, unit, items, "erc",
                format!("{} draws more current than its source supplies", net.name))
                .measures(amps(rail.demand), amps(cap)));
        }
        let drivers = kinds.iter().filter(|k| k.drives() || **k == PinKind::Passive).count();
        if kinds.contains(&PinKind::PowerIn) && !driven[ni] {
            out.push(
                Violation::new(CheckKind::PowerNotDriven, Severity::Error, unit, vec![net_sel(net.id)], "erc", format!("{}: no driver", net.name))
                    .hint("connect a supply jack or a power output, or add a power flag"),
            );
        } else if kinds.contains(&PinKind::In) && drivers == 0 && !signal_driver[ni] {
            out.push(Violation::new(CheckKind::UndrivenInput, Severity::Error, unit, vec![net_sel(net.id)], "erc", format!("input on {} with nothing driving it", net.name)));
        }
    }
    for (block, port, nets) in ports(c) {
        for &n in nets {
            let items = || vec![Sel::Block(c.unit, block.id), net_sel(n)];
            if port.kind == PortKind::PowerIn {
                if let (Some(req), Some(v)) = (port.volts, rails[n.ix()].volts) {
                    if !req.contains(&v) {
                        out.push(Violation::new(CheckKind::VoltageDomain, Severity::Error, unit, items(), &port.name,
                            format!("{} is outside the voltage range of {}.{}", c.nets[n.ix()].name, block.path, port.name))
                            .measures(Measure::VRange(v), Measure::VRange(req)));
                    }
                }
                if planned[block.id.ix()] && !direct_power[n.ix()] {
                    out.push(Violation::new(CheckKind::PowerNotDriven, Severity::Error, unit, items(), &port.name,
                        format!("{}.{} has no power source on {}", block.path, port.name, c.nets[n.ix()].name)));
                }
            } else if planned[block.id.ix()] && !matches!(port.kind, PortKind::PowerIn | PortKind::PowerOut | PortKind::Ground) && endpoints[n.ix()] < 2 {
                out.push(Violation::new(CheckKind::OpenPort, Severity::Warning, unit, items(), &port.name,
                    format!("{}.{} has no other endpoint on {}", block.path, port.name, c.nets[n.ix()].name)));
            }
        }
    }
    for comp in &c.comps {
        if planned.get(comp.block.ix()) == Some(&true) { continue; }
        for (pi, pin) in comp.part.pins.iter().enumerate() {
            let pix = fab_elec_core::PinIx(pi as u16);
            let connected = comp.conns.iter().any(|(p, _)| *p == pix);
            if !connected && !comp.nc.contains(&pix) && !matches!(pin.kind, PinKind::NoConnect | PinKind::Free) {
                out.push(
                    Violation::new(CheckKind::UnconnectedPin, Severity::Error, unit, vec![pin_sel(comp.id, pix)], "erc", format!("{} pin {} is not connected", comp.refdes, pin.name))
                        .hint("connect it, or list it in the part's nc"),
                );
            }
        }
        // The power flag exists only on the schematic and needs no footprint.
        if comp.part.footprint.is_none() && !comp.dnp && comp.part.kind != PartKind::PowerFlag {
            out.push(Violation::new(
                CheckKind::MissingFootprint,
                Severity::Error,
                unit,
                vec![Sel::Comp(CompRef { board: c.unit, comp: comp.id })],
                "erc",
                format!("{} ({}) has no footprint", comp.refdes, comp.part.name),
            ));
        }
    }
    let mut by_ref: Vec<(&str, fab_elec_core::CompId)> = c.comps.iter().filter(|k| planned.get(k.block.ix()) != Some(&true)).map(|k| (k.refdes.as_str(), k.id)).collect();
    by_ref.sort();
    for w in by_ref.windows(2) {
        if w[0].0 == w[1].0 {
            let items = vec![Sel::Comp(CompRef { board: c.unit, comp: w[0].1 }), Sel::Comp(CompRef { board: c.unit, comp: w[1].1 })];
            out.push(Violation::new(CheckKind::DuplicateRefdes, Severity::Error, unit, items, "erc", format!("reference {} twice", w[0].0)));
        }
    }
    out.sort_by_key(|v| v.id);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Component, Net};
    use fab_elec_core::{CompId, PinIx};
    use fab_elec_parts::{PartDef, PinDef};
    use std::sync::Arc;

    #[test]
    fn bench_supply_drives_both_sides_of_a_series_resistor() {
        let mut circuit = Circuit::default();
        let rails = [NetId(0), NetId(1)];
        for (id, name) in rails.into_iter().zip(["VIN", "VA"]) {
            circuit.nets.push(Net { id, name: name.into(), class: Default::default(), volts: None, kind: NetKind::Power, scope: None });
        }
        for (refdes, kind, pin_kind) in [("R1", PartKind::Resistor, PinKind::Passive), ("U1", PartKind::Ic, PinKind::PowerIn)] {
            let pins = ["1", "2"].map(|name| PinDef {
                name: name.into(), pads: vec![], kind: pin_kind, unit: 0, limits: None, alternates: vec![], delay: None,
            });
            let part = Arc::new(PartDef {
                name: refdes.into(), kind, pins: pins.into(), symbol: Default::default(), footprint: None, body: None,
                fields: Default::default(), emitter: None, gpio: None, lifecycle: None, approved: false,
            });
            circuit.comps.push(Component {
                id: CompId(circuit.comps.len() as u32), path: refdes.into(), refdes: refdes.into(), ref_kind: Default::default(),
                part, conns: vec![(PinIx(0), rails[0]), (PinIx(1), rails[1])], nc: vec![], block: Default::default(),
                value: None, dnp: false, fields: Default::default(),
            });
        }
        let mut findings = erc(&circuit);
        let before = findings.len();
        findings.retain(|v| v.kind != CheckKind::PowerNotDriven);
        assert_eq!(before - findings.len(), 2, "both power inputs lack a driver with the bench disconnected");
        assert_eq!(erc_supplied(&circuit, &rails[..1]), findings,
            "supplying VIN clears the missing drivers on VIN and VA and preserves the other findings");
    }

    fn circuit() -> Circuit {
        use crate::{Block, BlockSource};
        let mut c = Circuit::default();
        c.tree.push(Block { id: BlockId(0), path: String::new(), parent: None, def: None,
            params: vec![], ports: vec![], source: BlockSource::Code });
        for (i, name) in ["VDD", "IRQ"].into_iter().enumerate() {
            c.nets.push(Net { id: NetId(i as u32), name: name.into(), class: Default::default(), volts: None,
                kind: if i == 0 { NetKind::Power } else { NetKind::Signal }, scope: None });
        }
        c
    }

    fn block(c: &mut Circuit, parent: BlockId, name: &str, planned: bool, kind: PortKind, current: Option<f64>) -> BlockId {
        use crate::{BlockDef, BlockDefId, BlockSource};
        let def = BlockDefId(c.block_defs.len() as u32);
        let id = BlockId(c.tree.len() as u32);
        let port = PortDef { name: "p".into(), kind, current, volts: None, default: None };
        c.block_defs.push(BlockDef { id: def, name: name.into(), ports: vec![port], planned, role: None,
            params: vec![], sheet: Default::default(), template: None });
        c.tree.push(Block { id, parent: Some(parent), path: name.into(), def: Some(def), params: vec![],
            ports: vec![("p".into(), vec![NetId(0)])], source: BlockSource::Code });
        id
    }

    fn pin(c: &mut Circuit, block: BlockId, kind: PinKind, current: Option<f64>) -> CompId {
        let id = CompId(c.comps.len() as u32);
        let name = format!("U{}", id.0 + 1);
        let part = PartDef { name: "Chip".into(), kind: PartKind::Ic, pins: vec![PinDef {
            name: "P".into(), pads: vec![], unit: 0, kind, limits: Some(fab_elec_parts::Limits { current, ..Default::default() }),
            alternates: vec![], delay: None,
        }], footprint: None, symbol: Default::default(), body: None, fields: Default::default(), gpio: None,
            emitter: None, lifecycle: None, approved: false };
        c.comps.push(Component { id, block, refdes: name.clone(), path: name, ref_kind: Default::default(), part: Arc::new(part),
            conns: vec![(PinIx(0), NetId(0))], nc: vec![], value: None, dnp: true, fields: Default::default() });
        id
    }

    #[test]
    fn block_checks_flag_voltage_domain_mismatch_and_rail_overbudget() {
        let mut c = circuit();
        block(&mut c, BlockId(0), "supply", true, PortKind::PowerOut, Some(0.3));
        block(&mut c, BlockId(0), "load", true, PortKind::PowerIn, Some(0.2));
        block(&mut c, BlockId(0), "other", true, PortKind::PowerIn, Some(0.3));
        let req = VRange { min: 3.0, max: 3.6 };
        c.block_defs[1].ports[0].volts = Some(req);
        c.nets[0].volts = Some(VRange::fixed(5.0));
        let findings = erc(&c);
        let voltage = findings.iter().find(|v| v.kind == CheckKind::VoltageDomain).expect("5 V must violate the input voltage range");
        assert_eq!((voltage.severity, voltage.actual, voltage.required),
            (Severity::Error, Some(Measure::VRange(VRange::fixed(5.0))), Some(Measure::VRange(req))), "the whole required voltage interval must survive");
        let budget = findings.iter().find(|v| v.kind == CheckKind::RailBudget).expect("the two loads must exceed the supply");
        let amps = |si| Some(Measure::Qty(Quantity { si, unit: QtyUnit::Ampere }));
        assert_eq!((budget.severity, budget.actual, budget.required), (Severity::Error, amps(0.5), amps(0.3)), "the finding must report total demand and source capacity");
        c.nets[0].volts = Some(VRange::fixed(3.3));
        c.block_defs[0].ports[0].current = Some(0.5);
        assert!(erc(&c).is_empty(), "a compatible rail at its current budget must pass");
    }

    #[test]
    fn block_checks_avoid_double_counting_nested_power_in() {
        let mut c = circuit();
        let wrapper = block(&mut c, BlockId(0), "voice", false, PortKind::PowerIn, Some(0.1));
        block(&mut c, wrapper, "osc", true, PortKind::PowerIn, Some(0.025));
        block(&mut c, wrapper, "env", true, PortKind::PowerIn, Some(0.025));
        assert_eq!(rail_summaries(&c)[0].demand, 0.1, "the wrapper already budgets both children");
        c.block_defs[0].ports[0].current = None;
        assert_eq!(rail_summaries(&c)[0].demand, 0.05, "an unbudgeted wrapper must pass through both children's demand");
    }

    #[test]
    fn block_checks_count_realized_source_once_and_flag_independent_sources() {
        let mut c = circuit();
        let wrapper = block(&mut c, BlockId(0), "supply", false, PortKind::PowerOut, None);
        let ldo = block(&mut c, wrapper, "reg", false, PortKind::PowerOut, Some(0.6));
        c.block_defs[1].ports[0].volts = Some(VRange::fixed(3.3));
        let p = pin(&mut c, ldo, PinKind::PowerOut, Some(0.6));
        block(&mut c, BlockId(0), "load", true, PortKind::PowerIn, Some(0.9));
        let over = erc(&c);
        let v = over.iter().find(|v| v.kind == CheckKind::RailBudget).expect("900 mA must overload a 600 mA LDO");
        let amps = |si| Some(Measure::Qty(Quantity { si, unit: QtyUnit::Ampere }));
        assert_eq!((v.actual, v.required), (amps(0.9), amps(0.6)), "the LDO and its VOUT pin must count once");
        c.block_defs[2].ports[0].current = Some(0.5);
        assert!(erc(&c).is_empty(), "a 500 mA load on this LDO must pass");
        let rating = |c: &mut Circuit, limit| Arc::make_mut(&mut c.comps[p.ix()].part).pins[0].limits.as_mut().unwrap().current = Some(limit);
        rating(&mut c, 0.25);
        assert_eq!(rail_summaries(&c)[0].budget, Some(0.25), "a weaker backing pin must constrain its wrapper");
        rating(&mut c, 0.6);
        c.block_defs[0].ports[0].current = Some(0.5);
        assert_eq!(rail_summaries(&c)[0].budget, Some(0.5), "a derated wrapper must constrain its descendant");
        let other = block(&mut c, BlockId(0), "other", true, PortKind::PowerOut, Some(0.8));
        c.block_defs[3].ports[0].volts = Some(VRange::fixed(5.0));
        let rail = &rail_summaries(&c)[0];
        assert_eq!(rail.sources, [RailSource::Block(wrapper), RailSource::Block(other)], "the independent source and wrapper must both be identified");
        assert_eq!(rail.budget, None, "a shorted rail must have no available budget");
        let findings = erc(&c);
        assert!(findings.iter().any(|v| v.kind == CheckKind::PinConflict && v.items == [Sel::Block(c.unit, wrapper), Sel::Block(c.unit, other)]), "the conflict must name the competing blocks");
        assert!(findings.iter().any(|v| v.kind == CheckKind::VoltageDomain), "the incompatible output voltages must also be reported");
        assert!(!findings.iter().any(|v| v.kind == CheckKind::RailBudget), "a source conflict has no defined capacity to check");
    }

    #[test]
    fn block_checks_list_open_ports_and_skip_planned_block_insides() {
        let mut c = circuit();
        let source = block(&mut c, BlockId(0), "supply", true, PortKind::PowerOut, None);
        let child = block(&mut c, source, "unimplemented", false, PortKind::In, None);
        c.tree[child.ix()].ports[0].1.clear();
        let hidden = pin(&mut c, child, PinKind::PowerOut, None);
        c.comps[hidden.ix()].conns.clear();
        c.comps[hidden.ix()].dnp = false;
        let irq = block(&mut c, BlockId(0), "signal", true, PortKind::Out, None);
        c.tree[irq.ix()].ports[0].1 = vec![NetId(1)];
        let real = pin(&mut c, BlockId(0), PinKind::PowerIn, None);
        c.comps[real.ix()].refdes = c.comps[hidden.ix()].refdes.clone();
        let findings = erc(&c);
        assert_eq!(findings.len(), 2, "only the two open ports should be reported: {findings:?}");
        assert!(findings.iter().all(|v| v.kind == CheckKind::OpenPort && v.severity == Severity::Warning), "planned internals and the driven real pin must produce no component findings");
        assert!(findings.iter().any(|v| v.items == [Sel::Block(c.unit, child)]), "the unbound port must select its block");
        assert!(findings.iter().any(|v| v.items.contains(&Sel::Block(c.unit, irq)) && v.items.contains(&Sel::Net(NetRef { board: c.unit, net: NetId(1) }))), "the dangling signal must select its block and net");
        let input = pin(&mut c, BlockId(0), PinKind::In, None);
        c.comps[input.ix()].conns[0].1 = NetId(1);
        assert_eq!(erc(&c).len(), 1, "a planned signal output must drive a realized input and count as its other endpoint");
        c.block_defs[0].ports[0].kind = PortKind::PowerIn;
        assert!(erc(&c).iter().any(|v| v.kind == CheckKind::PowerNotDriven && v.items.contains(&Sel::Block(c.unit, source))), "an undriven planned power input must identify its block");
        assert!(!erc_supplied(&c, &[NetId(0)]).iter().any(|v| v.kind == CheckKind::PowerNotDriven), "an external supply must drive both power inputs");
        c.comps[real.ix()].conns.clear();
        assert!(!erc_supplied(&c, &[NetId(0)]).iter().any(|v| v.kind == CheckKind::OpenPort && v.items.contains(&Sel::Block(c.unit, source))),
            "a lone planned power input on a bench-supplied rail must not be reported as an open signal port");
    }
}
