//! Electrical rules check, first version (E1 4.2): the pin-kind conflict matrix, undriven inputs
//! and power inputs, unconnected pins, single-pin nets, duplicate references and missing
//! footprints. EROUTE owns ERC from here (the reference's matrix defaults come from spec S13; the
//! voltage-domain and budget checks move to `erc.splash` prelude fns).
//!
//! Supply tracing costs O(supplied nets + pins + parts × passes) per circuit evaluation.

use crate::check::{CheckKind, Violation};
use crate::{Circuit, NetKind};
use fab_elec_core::{CompRef, NetId, NetRef, PinRef, Sel, Severity};
use fab_elec_parts::{PartKind, PinKind};

/// The nets supplied by the bench, a power output or a jack, including rails fed through series parts.
fn power_driven(c: &Circuit, supplied: &[NetId]) -> Vec<bool> {
    let mut driven = vec![false; c.nets.len()];
    for net in supplied {
        if let Some(rail) = driven.get_mut(net.ix()) {
            *rail = true;
        }
    }
    let mut series = Vec::new();
    for comp in &c.comps {
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
    let net_pins = c.net_pins();
    let driven = power_driven(c, supplied);
    for (ni, pins) in net_pins.0.iter().enumerate() {
        let net = &c.nets[ni];
        let kinds: Vec<PinKind> = pins.iter().filter_map(|(comp, pin)| c.pin_kind(*comp, *pin)).collect();
        if pins.len() == 1 {
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
        }
        let drivers = kinds.iter().filter(|k| k.drives() || **k == PinKind::Passive).count();
        if kinds.contains(&PinKind::PowerIn) && !driven[ni] {
            out.push(
                Violation::new(CheckKind::PowerNotDriven, Severity::Error, unit, vec![net_sel(net.id)], "erc", format!("{}: no driver", net.name))
                    .hint("connect a supply jack or a power output, or add a power flag"),
            );
        } else if kinds.contains(&PinKind::In) && drivers == 0 {
            out.push(Violation::new(CheckKind::UndrivenInput, Severity::Error, unit, vec![net_sel(net.id)], "erc", format!("input on {} with nothing driving it", net.name)));
        }
    }
    for comp in &c.comps {
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
    let mut by_ref: Vec<(&str, fab_elec_core::CompId)> = c.comps.iter().map(|k| (k.refdes.as_str(), k.id)).collect();
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
                fields: Default::default(), emitter: None, gpio: None,
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
}
