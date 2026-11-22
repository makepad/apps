//! Pure Library drop plans shared by sheet gestures and the host's action resolvers.

use crate::edit::{Target, Toward};
use crate::plan::{edit, nothing, Aim, Pin, Planned, Targets};
use fab_elec_circuit::{Block, Circuit, CircuitOp, Component, End, Net, NetKind, SheetHint};
use fab_elec_core::{BlockId, CompId, CompRef, NetId, PinIx, SchPt, Sel};
use fab_elec_parts::PinKind;
use std::cmp::Ordering;
use std::fmt;

/// A component and its pin index, as supplied by the sheet's cut net tree.
pub type PinId = (CompId, PinIx);

/// A Library entry as a drop plan needs it; the view fills it from the language's catalog entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    /// The AddPart template: one Splash statement with `_n*` placeholders, defaults filled in.
    pub stmt: String,
    /// The binding stem (`r`, `c`, `led_driver`), for chip text.
    pub stem: String,
    /// Which drop idioms this entry supports.
    pub kind: TemplateKind,
    /// The entry's pins (name, kind) in placeholder order: slot k is `_nk`.
    pub pins: Vec<(String, PinKind)>,
}

/// The electrical role of a Library template when it is dropped.
#[derive(Clone, Debug, PartialEq)]
pub enum TemplateKind {
    /// A two-terminal passive other than a capacitor (resistor, inductor, ferrite, fuse...).
    Passive,
    /// A two-terminal capacitor: on a supply pin it decouples.
    Capacitor,
    /// A library part.
    Part,
    /// A circuit block.
    Block,
}

/// A drop plan: the chosen candidate and how many legal candidates Tab cycles through.
#[derive(Clone, Debug, PartialEq)]
pub struct Dropped {
    /// The edits to preview and commit, or the immediate reason nothing changes.
    pub planned: Planned,
    /// The candidate shown (`pick` modulo `count`), 0-based.
    pub pick: usize,
    /// How many candidates the target offers (at least 1).
    pub count: usize,
    /// A shared block's last path segment and instance count, for the editing chip.
    pub instances: Option<(String, usize)>,
}

/// Plan a Library drop without evaluation or changing the circuit. `sides` supplies the two
/// pin sets cut from the drawn net tree at a wire segment; Tab advances `pick`.
/// Pin state and lone tags use the wiring planner's single cached lone-net scan.
/// Besides the result, a pin drop gathers its host's rails in one small list. Work is
/// linear in host pins, plus sorting rails, scanning a named net and counting block instances;
/// wires scan their sides and find their first pin, canvas drops scan components once.
pub fn plan_drop(
    circuit: &Circuit, template: &Template, at: Target, pick: usize, sides: Option<&[Vec<PinId>; 2]>,
) -> Dropped {
    let mut targets = Targets { circuit, lone: [None; 2] };
    let (mut dropped, block) = match at {
        Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. } => {
            let Some(p) = targets.pin(comp, pin) else { return empty() };
            let Some(host) = circuit.comps.get(comp.ix()) else { return empty() };
            let toward = match at { Target::Pin { toward, .. } => toward, _ => Toward::Across };
            (on_pin(circuit, template, host, p, toward, pick), Some(host.block))
        }
        Target::Tag { .. } => match targets.aim(at) {
            Some(Aim::Pin(p)) => {
                let Some(host) = circuit.comps.get(p.id.0.ix()) else { return empty() };
                (on_pin(circuit, template, host, p, Toward::Across, pick), Some(host.block))
            }
            Some(Aim::Net(net)) => {
                let first = first_pin(circuit, net.id);
                if first.is_some_and(|(host, pin)| host.part.pins.get(pin.ix()).is_none()) { return empty(); }
                let planned = add(circuit, template, first.map(|(host, _)| host.id), vec![End::Net(net.id)],
                    None, format!("Add {} on {}", template.stem, net.name));
                (chosen(planned, 0, 1), first.map(|(host, _)| host.block))
            }
            _ => return empty(),
        },
        Target::Wire { net, .. } => {
            let Some(net) = circuit.nets.get(net.ix()) else { return empty() };
            let first = first_pin(circuit, net.id);
            if first.is_some_and(|(host, pin)| host.part.pins.get(pin.ix()).is_none()) { return empty(); }
            let dropped = if two_terminal(template) {
                on_wire(circuit, template, net, pick, sides)
            } else if uses_slots(template) {
                let kind = first.and_then(|(host, pin)| host.part.pins.get(pin.ix()).map(|p| p.kind));
                slots(circuit, template, first.map(|(host, _)| host.id), End::Net(net.id), &net.name, kind, pick)
            } else {
                empty()
            };
            (dropped, first.map(|(host, _)| host.block))
        }
        Target::Canvas { block, cell } => {
            let Some(block) = circuit.tree.get(block as usize) else { return empty() };
            let after = last_part(circuit, block.id);
            let hint = SheetHint { at: Some(SchPt::new(cell[0], cell[1])), ..Default::default() };
            let planned = add(circuit, template, after, vec![], Some(hint), format!("Add {}", template.stem));
            (chosen(planned, 0, 1), Some(block.id))
        }
        Target::None => return empty(),
    };
    dropped.instances = block.and_then(|id| {
        let block = circuit.tree.get(id.ix())?;
        let count = shared_instances(circuit, block)?;
        Some((block.path.rsplit('.').next()?.into(), count))
    });
    dropped
}

fn two_terminal(template: &Template) -> bool {
    matches!(template.kind, TemplateKind::Passive | TemplateKind::Capacitor) && template.pins.len() == 2
}

/// One-pin offers attach their sole slot, including catalog functions classified as passives.
fn uses_slots(template: &Template) -> bool {
    template.pins.len() == 1 || matches!(template.kind, TemplateKind::Part | TemplateKind::Block)
}

fn pin_end(p: Pin<'_>) -> End {
    match p.state.net() {
        Some(net) => End::Net(net.id),
        None => End::Pin(p.id.0, p.id.1),
    }
}

fn on_pin(circuit: &Circuit, template: &Template, host: &Component, p: Pin<'_>, toward: Toward, pick: usize) -> Dropped {
    let Some(pin) = host.part.pins.get(p.id.1.ix()) else { return empty() };
    if uses_slots(template) {
        return slots(circuit, template, Some(host.id), pin_end(p), p, Some(pin.kind), pick);
    }
    if !two_terminal(template) { return empty(); }
    let dropped_on = p.state.net();
    // A ground pin is often PowerIn: a pin decouples only when it is (or would be) on a supply.
    let supply = match dropped_on {
        Some(net) => net.kind == NetKind::Power || (pin.kind == PinKind::PowerIn && net.kind != NetKind::Ground),
        None => pin.kind == PinKind::PowerIn,
    };
    // Rails and ground never include the dropped-on net: a part with both ends on it does nothing.
    let mut ground: Option<(PinIx, &Net)> = None;
    let mut rails: Vec<&Net> = Vec::new();
    for &(pin, net) in &host.conns {
        let Some(kind) = host.part.pins.get(pin.ix()).map(|pin| pin.kind) else { continue };
        let Some(net) = circuit.nets.get(net.ix()) else { continue };
        if dropped_on.is_some_and(|on| on.id == net.id) { continue; }
        if net.kind == NetKind::Ground && ground.is_none_or(|(first, _)| pin < first) {
            ground = Some((pin, net));
        }
        if kind == PinKind::PowerIn && net.kind == NetKind::Power {
            rails.push(net);
        }
    }
    // Highest voltage first, rails without volts last, then by id; duplicates end up adjacent.
    rails.sort_unstable_by(|a, b| {
        let volts = match (a.volts, b.volts) {
            (Some(va), Some(vb)) => vb.max.total_cmp(&va.max),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        volts.then_with(|| a.id.cmp(&b.id))
    });
    rails.dedup_by_key(|net| net.id);
    pin_candidates(circuit, template, p, toward, supply, &rails, ground.map(|(_, net)| net), pick)
}

#[derive(Clone, Copy)]
enum Idiom { Up, Down, Series }

fn pin_candidates(
    circuit: &Circuit, template: &Template, p: Pin<'_>, toward: Toward, supply: bool,
    rails: &[&Net], ground: Option<&Net>, pick: usize,
) -> Dropped {
    let decouple = template.kind == TemplateKind::Capacitor && supply;
    let no_supply = !decouple && toward == Toward::Up && rails.is_empty();
    let no_ground = (decouple || toward == Toward::Down) && ground.is_none();
    let net = p.state.net();
    let count = rails.len() + usize::from(ground.is_some()) + usize::from(net.is_some())
        + usize::from(no_supply) + usize::from(no_ground);
    if count == 0 { return empty(); }
    let pick = pick % count;
    let mut index = pick;
    let refused = |chip: String| Planned::Refused {
        chip, sites: vec![Sel::Comp(CompRef { board: circuit.unit, comp: p.id.0 })],
    };
    let order = if decouple || toward == Toward::Down {
        [Idiom::Down, Idiom::Up, Idiom::Series]
    } else if toward == Toward::Up {
        [Idiom::Up, Idiom::Down, Idiom::Series]
    } else {
        [Idiom::Series, Idiom::Up, Idiom::Down]
    };
    for idiom in order {
        let len = match idiom {
            Idiom::Up => rails.len() + usize::from(no_supply),
            Idiom::Down => usize::from(ground.is_some()) + usize::from(no_ground),
            Idiom::Series => usize::from(net.is_some()),
        };
        if index >= len { index -= len; continue; }
        let planned = match idiom {
            Idiom::Up if no_supply => refused(format!("no supply on {}", p.refdes)),
            Idiom::Down if no_ground => refused(format!("no ground on {}", p.refdes)),
            Idiom::Up => {
                let Some(rail) = rails.get(index) else { return empty() };
                add(circuit, template, Some(p.id.0), vec![pin_end(p), End::Net(rail.id)], None,
                    format!("Pull {p} up to {}", rail.name))
            }
            Idiom::Down => {
                let Some(ground) = ground else { return empty() };
                let label = if decouple {
                    match net {
                        Some(net) => format!("Decouple {} to {}", net.name, ground.name),
                        None => format!("Decouple {p} to {}", ground.name),
                    }
                } else {
                    format!("Pull {p} down to {}", ground.name)
                };
                add(circuit, template, Some(p.id.0), vec![pin_end(p), End::Net(ground.id)], None, label)
            }
            Idiom::Series => {
                let Some(net) = net else { return empty() };
                edit(circuit, [CircuitOp::InsertSeries { net: net.id, stmt: template.stmt.clone(), moved: vec![p.id] }],
                    format!("Put {} in series with {p}", template.stem))
            }
        };
        return chosen(planned, pick, count);
    }
    empty()
}

fn on_wire(circuit: &Circuit, template: &Template, net: &Net, pick: usize, sides: Option<&[Vec<PinId>; 2]>) -> Dropped {
    let Some(sides) = sides else { return empty() };
    if sides.iter().any(Vec::is_empty) { return empty(); }
    let mut drivers = [false; 2];
    for (i, side) in sides.iter().enumerate() {
        for &(comp, pin) in side {
            let Some(kind) = circuit.pin_kind(comp, pin) else { return empty() };
            drivers[i] |= matches!(kind, PinKind::PowerOut | PinKind::Out);
        }
    }
    let default = if drivers[0] != drivers[1] {
        usize::from(drivers[0])
    } else {
        usize::from(sides[1].len() < sides[0].len())
    };
    let pick = pick % 2;
    let planned = edit(circuit, [CircuitOp::InsertSeries {
        net: net.id, stmt: template.stmt.clone(), moved: sides[default ^ pick].clone(),
    }], format!("Put {} in series on {}", template.stem, net.name));
    chosen(planned, pick, 2)
}

fn complements(target: Option<PinKind>, slot: PinKind) -> bool {
    match target {
        Some(PinKind::Out | PinKind::PowerOut | PinKind::OpenCollector | PinKind::OpenEmitter) => {
            matches!(slot, PinKind::In | PinKind::PowerIn | PinKind::Bidi | PinKind::Passive)
        }
        Some(PinKind::In | PinKind::PowerIn) => {
            matches!(slot, PinKind::Out | PinKind::PowerOut | PinKind::Bidi | PinKind::Passive)
        }
        _ => false,
    }
}

fn slots(
    circuit: &Circuit, template: &Template, after: Option<CompId>, end: End, name: impl fmt::Display,
    kind: Option<PinKind>, pick: usize,
) -> Dropped {
    let count = template.pins.iter().filter(|(_, kind)| *kind != PinKind::NoConnect).count();
    if count == 0 { return empty(); }
    let pick = pick % count;
    let group = |matched| template.pins.iter().enumerate().filter(move |(_, (_, slot))| {
        *slot != PinKind::NoConnect && complements(kind, *slot) == matched
    });
    let Some((slot, (pin, _))) = group(true).chain(group(false)).nth(pick) else { return empty() };
    let mut ends = Vec::with_capacity(slot + 1);
    ends.resize_with(slot, || End::Open);
    ends.push(end);
    let planned = add(circuit, template, after, ends, None, format!("Add {} with {pin} on {name}", template.stem));
    chosen(planned, pick, count)
}

fn first_pin(circuit: &Circuit, net: NetId) -> Option<(&Component, PinIx)> {
    circuit.comps.iter().find_map(|comp| {
        comp.conns.iter().find(|(_, n)| *n == net).map(|(pin, _)| (comp, *pin))
    })
}

/// Find the last part owned directly by the block for insertion on its canvas. O(components).
pub(crate) fn last_part(circuit: &Circuit, block: BlockId) -> Option<CompId> {
    circuit.comps.iter().filter(|comp| comp.block == block).map(|comp| comp.id).max()
}

/// Count the instances affected by an edit to a shared definition. O(blocks) for a definition.
pub(crate) fn shared_instances(circuit: &Circuit, block: &Block) -> Option<usize> {
    let def = block.def?;
    let count = circuit.tree.iter().filter(|b| b.def == Some(def)).count();
    (count > 1).then_some(count)
}

fn add(circuit: &Circuit, template: &Template, after: Option<CompId>, ends: Vec<End>, hint: Option<SheetHint>, label: String) -> Planned {
    edit(circuit, [CircuitOp::AddPart { stmt: template.stmt.clone(), after, ends, hint }], label)
}

fn chosen(planned: Planned, pick: usize, count: usize) -> Dropped {
    Dropped { planned, pick, count, instances: None }
}

fn empty() -> Dropped {
    chosen(nothing(""), 0, 1)
}

#[cfg(test)]
#[path = "plan_drop_tests.rs"]
mod tests;
