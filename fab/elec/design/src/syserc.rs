//! System ERC (C4, E2.13), first version: link ends that resolve to nothing or leave pins
//! unpaired, system nets whose members declare conflicting volts, and power inputs on a system
//! net with no power output anywhere on it. ELANG and ECABLE extend it (sources, grounds,
//! ratings, pin maps checked against geometry) from wave 2.
//!
//! Cost: O(nets + link pins).

use crate::{sysnets, Design, LinkEnd, PinMap};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{Sel, Severity};
use fab_elec_parts::PinKind;

pub fn system_erc(d: &Design) -> Vec<Violation> {
    let mut out = Vec::new();
    for l in &d.links {
        let pairs = d.link_pairs(l);
        let has_pin = |p: fab_elec_core::PinRef| d.unit(p.board).and_then(|u| u.circuit.comps.get(p.comp.ix())).is_some_and(|k| p.pin.ix() < k.part.pins.len());
        if pairs.iter().any(|(a, b)| !has_pin(*a) || !has_pin(*b)) {
            out.push(
                Violation::new(CheckKind::LinkPinMap, Severity::Error, None, vec![Sel::Link(l.id)], "link pins", "the pin map names a pin its part does not have".into())
                    .hint("fix the pin map"),
            );
        }
        if pairs.is_empty() {
            out.push(Violation::new(CheckKind::LinkMismatch, Severity::Error, None, vec![Sel::Link(l.id)], "link", "the link joins no pins".into()));
            continue;
        }
        if let (LinkEnd::Comp(a), LinkEnd::Comp(b), PinMap::Derived) = (l.a, l.b, &l.pinmap) {
            let count = |c: fab_elec_core::CompRef| d.unit(c.board).and_then(|u| u.circuit.comps.get(c.comp.ix())).map_or(0, |k| k.part.pins.len());
            if count(a) != pairs.len() || count(b) != pairs.len() {
                out.push(
                    Violation::new(CheckKind::LinkPinMap, Severity::Warning, None, vec![Sel::Link(l.id)], "link", format!("{} of the connectors' pins pair up by name", pairs.len()))
                        .hint("give the pin map explicitly"),
                );
            }
        }
    }
    for sn in sysnets(d) {
        if sn.members.len() < 2 {
            continue;
        }
        let items: Vec<Sel> = sn.members.iter().map(|m| Sel::Net(*m)).collect();
        let declared: Vec<_> = sn.members.iter().filter_map(|m| d.unit(m.board)?.circuit.nets.get(m.net.ix())?.volts).collect();
        if !declared.is_empty() && sn.volts.is_none() {
            out.push(Violation::new(CheckKind::SystemVolts, Severity::Error, None, items.clone(), "system", format!("{} declare different voltages", sn.aliases.join(", "))));
        }
        let power_in = sn.members.iter().any(|m| {
            d.unit(m.board).is_some_and(|u| u.circuit.pins_of(m.net).iter().any(|(c, p)| u.circuit.pin_kind(*c, *p) == Some(PinKind::PowerIn)))
        });
        if power_in && sn.sources.is_empty() {
            out.push(Violation::new(CheckKind::SystemSource, Severity::Error, None, items, "system", format!("power inputs on {} with no source in the system", sn.aliases.join(", "))));
        }
    }
    out.sort_by_key(|v| v.id);
    out
}
