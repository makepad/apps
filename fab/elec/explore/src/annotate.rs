use crate::plan::{nothing, Planned};
use crate::SheetLayout;
use fab_elec_circuit::{Circuit, CircuitOp, RefKind};
use fab_elec_design::{DesignOp, Resolved};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// Parts less than this many grid steps below the top of a row belong to it.
const ROW_BAND: u32 = 8;

fn split_ref(refdes: &str) -> (&str, &str) {
    let prefix = refdes.trim_end_matches(|c: char| c.is_ascii_digit());
    (prefix, &refdes[prefix.len()..])
}

/// Annotate: stored refs for the parts numbered automatically, in sheet order. Read each row
/// left to right, then visit undrawn parts in component order. Stored and shared refs keep
/// their numbers. O(n log n) time and O(n) space in components, once per request.
pub fn annotate(circuit: &Circuit, sheet: &SheetLayout) -> Planned {
    let shared = circuit.comps.iter().filter(|c| c.ref_kind == RefKind::Shared).count();
    if circuit.comps.iter().all(|c| c.ref_kind != RefKind::Auto) {
        return match shared {
            0 => nothing("every part has its reference"),
            1 => nothing("1 part keeps its automatic ref: it is made by a shared or library call"),
            _ => nothing(&format!("{shared} parts keep automatic refs: each is made by a shared or library call")),
        };
    }
    let mut taken = HashSet::new();
    let mut drawn = Vec::new();
    let mut undrawn = Vec::new();
    for comp in &circuit.comps {
        if comp.ref_kind != RefKind::Auto {
            let (prefix, digits) = split_ref(&comp.refdes);
            if let Ok(n) = digits.parse::<usize>() {
                taken.insert((prefix, n));
            }
        }
        if let Some(place) = sheet.parts.get(comp.id.ix()).filter(|p| p.placed) {
            drawn.push((place.at, comp));
        } else {
            undrawn.push(comp);
        }
    }
    drawn.sort_unstable_by_key(|(at, _)| Reverse(at[1]));
    let mut remaining = drawn.as_mut_slice();
    while let Some((top, _)) = remaining.first() {
        let len = remaining.partition_point(|(at, _)| top[1].abs_diff(at[1]) < ROW_BAND);
        let (row, rest) = remaining.split_at_mut(len);
        row.sort_unstable_by_key(|(at, comp)| (at[0], comp.id));
        remaining = rest;
    }

    let mut next = HashMap::new();
    let mut ops = Vec::new();
    for comp in drawn.into_iter().map(|(_, comp)| comp).chain(undrawn) {
        if comp.ref_kind != RefKind::Auto { continue; }
        let (prefix, _) = split_ref(&comp.refdes);
        let n = next.entry(prefix).or_insert(1);
        while taken.contains(&(prefix, *n)) { *n += 1; }
        let op = CircuitOp::SetRef { comp: comp.id, to: Some(format!("{prefix}{n}")) };
        ops.push(DesignOp::Circuit { unit: circuit.unit, op });
        *n += 1;
    }
    let parts = if ops.len() == 1 { "part" } else { "parts" };
    let mut label = format!("Annotate {} {parts}", ops.len());
    match shared {
        0 => {}
        1 => label.push_str(" (1 keeps its automatic ref)"),
        _ => label.push_str(&format!(" ({shared} keep automatic refs)")),
    }
    Planned::Edit(Resolved { ops, place: vec![], label })
}

#[cfg(test)]
#[path = "annotate_tests.rs"]
mod tests;
