//! Plain names for an edit, for a status line ("Re-rendering with R7 = 22k"): a value or knob
//! change by its component's reference, a circuit edit by the values it changed.

use fab_elec_circuit::{Circuit, Component, Quantity};
use fab_elec_core::ids::CompRef;
use fab_elec_lang::units::value_text;
use std::collections::HashMap;

/// A component's name as the schematic shows it: its reference, or its path without one.
fn name(c: &Component) -> &str {
    if c.refdes.is_empty() { &c.path } else { &c.refdes }
}

/// A `SetParam` of `comp` in `circuit`: its new value in the component's own unit ("R7 = 22k"),
/// or a knob's wiper place ("VR1 at 60 %"). O(components).
pub fn param_edit(circuit: &Circuit, comp: CompRef, value: f64, knob: bool) -> String {
    let c = circuit.comps.iter().find(|c| c.id == comp.comp);
    let who = c.map_or("a value", name);
    if knob {
        return format!("{who} at {:.0} %", value * 100.0);
    }
    match c.and_then(|c| c.value.as_ref()?.qty.as_ref()) {
        Some(q) => format!("{who} = {}", value_text(&Quantity { si: value, unit: q.unit })),
        None => format!("{who} = {value}"),
    }
}

/// A press of the momentary input `comp` in `circuit` ("SW5 pressed"). O(components).
pub fn press_edit(circuit: &Circuit, comp: CompRef) -> String {
    let who = circuit.comps.iter().find(|c| c.id == comp.comp).map_or("a key", name);
    format!("{who} pressed")
}

/// The values a circuit edit changed, by component ("R7 = 22k", "R7 = 22k and 2 more"); None
/// when it changed no component's value (wiring, parts, a block). O(components).
pub fn circuit_edit(old: &Circuit, new: &Circuit) -> Option<String> {
    let before: HashMap<_, _> = old.comps.iter().map(|c| (c.id, c)).collect();
    let mut changed = new.comps.iter().filter(|c| before.get(&c.id).is_some_and(|o| o.value != c.value));
    let first = changed.next()?;
    let value = first.value.as_ref().map_or_else(|| "none".to_owned(), |v| match &v.qty {
        Some(q) if v.text.is_empty() => value_text(q),
        _ => v.text.clone(),
    });
    let more = changed.count();
    Some(match more {
        0 => format!("{} = {value}", name(first)),
        n => format!("{} = {value} and {n} more", name(first)),
    })
}
