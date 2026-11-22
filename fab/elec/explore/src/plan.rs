//! Pure sheet edits shared by sheet gestures and the host's action resolvers.

use crate::edit::Target;
use crate::sheet::{self, Mover, SheetLayout};
use fab_elec_circuit::{Circuit, CircuitOp, Net, NetKind, NetStyle, SheetHint};
use fab_elec_core::{CompId, NetId, NetRef, PinIx, SchPt, Sel};
use fab_elec_design::{DesignOp, Resolved};
use std::{collections::{BTreeSet, HashMap}, fmt};

/// A finished gesture on the sheet, by edit targets.
#[derive(Clone, Debug, PartialEq)]
pub enum Gesture {
    /// A drag from `from` (a pin: a new connection; a wire end: carry that pin's connection)
    /// released on `to`.
    Wire { from: Target, to: Target },
    /// A name typed where a wire from a pin or wire end stopped on empty sheet.
    NameEnd { from: Target, name: String },
    /// The name entered on a wire or connected pin.
    NameNet { target: Target, to: String },
    /// A Global label: the name entered on a pin. The pin joins the net of that name (made when
    /// the circuit has none) and a signal net draws as name flags rather than wires.
    Global { target: Target, name: String },
    /// Toggle the no-connect mark on the hovered pin or wire end.
    NoConnect(Target),
    /// The Delete key on the hovered target: a connected pin or its wire end disconnects.
    Delete(Target),
}

/// What a gesture would do.
#[derive(Clone, Debug, PartialEq)]
pub enum Planned {
    /// Edits to preview and commit as one undo step; `label` is the chip text and history name.
    Edit(Resolved),
    /// Nothing changes; `chip` says why (empty when there is nothing to say).
    Nothing { chip: String },
    /// Refused before evaluation: `chip` is the reason, `sites` the chip's "show in Code" targets.
    Refused { chip: String, sites: Vec<Sel> },
}

/// Plan a wiring gesture from current connectivity, without evaluation or changing the circuit.
/// Missing targets change nothing. Only the result allocates; lone-net checks scan connections
/// at most once per named net, with at most two nets to check per gesture.
pub fn plan(circuit: &Circuit, gesture: &Gesture) -> Planned {
    let mut targets = Targets { circuit, lone: [None; 2] };
    match *gesture {
        Gesture::Wire { from, to } => {
            let (comp, pin) = match from {
                Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. } => (comp, pin),
                _ => return nothing(""),
            };
            let Some(a) = targets.pin(comp, pin) else { return nothing("") };
            targets.wire_from(from, a, to)
        }
        Gesture::NameNet { target, ref to } => {
            let net = match label_net(circuit, target) {
                Ok(net) => net,
                Err(chip) => return nothing(chip),
            };
            rename(circuit, net, to)
        }
        Gesture::Global { target: Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. }, ref name } => {
            let Some(p) = targets.pin(comp, pin) else { return nothing("") };
            global(circuit, p, name.trim())
        }
        Gesture::NameEnd { from: from @ (Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. }), ref name } => {
            let name = name.trim();
            if name.is_empty() { return nothing(""); }
            let Some(p) = targets.pin(comp, pin) else { return nothing("") };
            if p.state.net().is_some_and(|net| net.name == name) { return nothing(""); }
            match p.state {
                State::Free | State::Loose(_) => global(circuit, p, name),
                State::On(net) => match circuit.net(name) {
                    Some(other) => targets.wire_from(from, p, Target::Tag { net: other.id, kind: sheet::TagKind::Label }),
                    None => rename(circuit, net, name),
                },
            }
        }
        Gesture::NoConnect(Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. }) => {
            let Some(p) = targets.pin(comp, pin) else { return nothing("") };
            if let State::On(net) = p.state {
                return Planned::Nothing { chip: format!("{p} is on {}", net.name) };
            }
            if p.marked {
                return edit(circuit, [CircuitOp::NoConnect { pin: p.id, nc: false }],
                    format!("Clear {p}'s not-connected mark"));
            }
            let op = CircuitOp::NoConnect { pin: p.id, nc: true };
            let label = format!("Mark {p} not connected");
            if matches!(p.state, State::Loose(_)) {
                edit(circuit, [CircuitOp::Disconnect { pin: p.id }, op], label)
            } else {
                edit(circuit, [op], label)
            }
        }
        // Below 8 px the pin wins the pick over its wire end, and Delete is how the end is reached.
        Gesture::Delete(Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. }) => {
            let Some(p) = targets.pin(comp, pin) else { return nothing("") };
            let Some(net) = p.state.net() else { return nothing("") };
            disconnect(circuit, p, net)
        }
        _ => nothing(""),
    }
}

/// Turn a selected part on its sheet. A free chain turns at its head and a pin rider follows
/// its host. Looks up the mover by index and scans the part hints once.
pub fn turn_part(circuit: &Circuit, sheet: &SheetLayout, selected: CompId, counter_clockwise: bool) -> Planned {
    orient_part(circuit, sheet, selected, "Turn", "turns", |hint| turn_hint(hint, counter_clockwise))
}

/// Mirror the selected mover left-right, or top-bottom, on the sheet's own axes at every turn
/// (as a group mirror does). Like a turn, this scans the part hints once and plans one source edit.
pub fn mirror_part(circuit: &Circuit, sheet: &SheetLayout, selected: CompId, flip_vertical: bool) -> Planned {
    let label = if flip_vertical { "Flip" } else { "Mirror" };
    orient_part(circuit, sheet, selected, label, "mirrors", |hint| {
        GroupOrientation::Mirror { flip_vertical }.apply(hint, [0, 0]);
    })
}

fn turn_hint(hint: &mut SheetHint, counter_clockwise: bool) {
    hint.turn = hint.turn.wrapping_add(if counter_clockwise { 1 } else { 3 }) % 4;
}

fn mirror_hint(hint: &mut SheetHint, flip_vertical: bool) {
    hint.mirror = !hint.mirror;
    if flip_vertical { hint.turn = hint.turn.wrapping_add(2) % 4; }
}

fn orient_part(circuit: &Circuit, sheet: &SheetLayout, selected: CompId, label: &str, rider_verb: &str,
    orient: impl FnOnce(&mut SheetHint)) -> Planned {
    let node = match sheet.movers.get(selected.ix()) {
        Some(Mover::Itself) => selected.ix(),
        Some(&Mover::Head(head)) => head as usize,
        Some(&Mover::Rides { host, .. }) => {
            let (Some(part), Some(head)) = (circuit.comps.get(selected.ix()), circuit.comps.get(host as usize))
                else { return nothing("") };
            return Planned::Nothing { chip: format!("{} {rider_verb} with {}", part.refdes, head.refdes) };
        }
        None => return nothing(""),
    };
    let Some(comp) = circuit.comps.get(node) else { return nothing("") };
    let Some(part) = sheet.parts.get(node).filter(|part| part.placed) else { return nothing("") };
    let mut hint = match circuit.hints.parts.iter().rev().find(|(path, _)| path == &comp.path) {
        Some((_, hint)) => hint.clone(),
        None => {
            let Some([x, y]) = sheet::to_local(sheet, part.block, part.at) else { return nothing("") };
            SheetHint { at: Some(SchPt::new(x, y)), turn: part.turn, mirror: part.mirror, ..SheetHint::default() }
        }
    };
    orient(&mut hint);
    let op = CircuitOp::Hint { path: comp.path.clone(), hint: Some(hint) };
    edit(circuit, [op], format!("{label} {}", comp.refdes))
}

/// A rigid turn or reflection of a group in block-local coordinates (Y down).
#[derive(Clone, Copy)]
pub enum GroupOrientation {
    Turn { counter_clockwise: bool },
    Mirror { flip_vertical: bool },
}

impl GroupOrientation {
    /// Compose one mover's pose and transform its offset from the pivot in constant time.
    pub fn apply(self, hint: &mut SheetHint, [dx, dy]: [i64; 2]) -> [i64; 2] {
        match self {
            Self::Turn { counter_clockwise } => {
                turn_hint(hint, counter_clockwise);
                if counter_clockwise { [dy, -dx] } else { [-dy, dx] }
            }
            Self::Mirror { flip_vertical } => {
                // A hint mirrors before it turns. At odd turns the two part-local axes trade places.
                let local_vertical = flip_vertical ^ (hint.turn & 1 != 0);
                mirror_hint(hint, local_vertical);
                if flip_vertical { [dx, -dy] } else { [-dx, dy] }
            }
        }
    }
}

/// Turn a selection about the grid point at the centre of its movers. Block-local Y runs down.
/// For k selected parts and h stored hints, planning costs O(k log k + h) per key press.
pub fn turn_parts(circuit: &Circuit, sheet: &SheetLayout, selected: &[CompId], counter_clockwise: bool) -> Planned {
    if let [id] = selected { return turn_part(circuit, sheet, *id, counter_clockwise); }
    orient_parts(circuit, sheet, selected, "Turn", GroupOrientation::Turn { counter_clockwise })
}

/// Reflect a selection about a sheet axis through its centre. Like a group turn, this indexes
/// the stored hints once and emits one hint per distinct mover.
pub fn mirror_parts(circuit: &Circuit, sheet: &SheetLayout, selected: &[CompId], flip_vertical: bool) -> Planned {
    if let [id] = selected { return mirror_part(circuit, sheet, *id, flip_vertical); }
    let label = if flip_vertical { "Flip" } else { "Mirror" };
    orient_parts(circuit, sheet, selected, label, GroupOrientation::Mirror { flip_vertical })
}

/// The source part that takes a group edit for this member. Follow one link per attachment,
/// including a flag on a chain member, until reaching the independently placed part.
pub fn group_mover(sheet: &SheetLayout, mut id: CompId) -> Option<CompId> {
    for _ in 0..sheet.movers.len() {
        id = match *sheet.movers.get(id.ix())? {
            Mover::Itself => return Some(id),
            Mover::Head(head) | Mover::Rides { host: head, .. } => CompId(head),
        };
    }
    None
}

fn orient_parts(circuit: &Circuit, sheet: &SheetLayout, selected: &[CompId], label: &str, orient: GroupOrientation) -> Planned {
    let Some(first) = selected.first().and_then(|id| circuit.comps.get(id.ix())) else { return nothing("") };
    let across_blocks = || nothing(&format!("{} parts of one block", label.to_ascii_lowercase()));
    let mut heads = BTreeSet::new();
    for id in selected {
        let Some(comp) = circuit.comps.get(id.ix()) else { return nothing("") };
        if comp.block != first.block { return across_blocks(); }
        let Some(head) = group_mover(sheet, *id) else { return nothing("") };
        heads.insert(head);
    }

    let stored: HashMap<_, _> = circuit.hints.parts.iter().map(|(path, hint)| (path.as_str(), hint)).collect();
    let mut movers = Vec::with_capacity(heads.len());
    let mut lo = [i64::MAX; 2];
    let mut hi = [i64::MIN; 2];
    for head in heads {
        let Some(comp) = circuit.comps.get(head.ix()) else { return nothing("") };
        let Some(part) = sheet.parts.get(head.ix()).filter(|p| p.placed) else { return nothing("") };
        if comp.block != first.block || part.block != first.block.0 { return across_blocks(); }
        let hint = stored.get(comp.path.as_str()).copied().cloned().unwrap_or_else(|| SheetHint {
            turn: part.turn, mirror: part.mirror, ..SheetHint::default()
        });
        let local = hint.at.map(|p| [p.x, p.y]).or_else(|| sheet::to_local(sheet, part.block, part.at));
        let Some(at) = local.map(|p| p.map(i64::from)) else { return nothing("") };
        for axis in 0..2 {
            lo[axis] = lo[axis].min(at[axis]);
            hi[axis] = hi[axis].max(at[axis]);
        }
        movers.push((&comp.path, hint, at));
    }
    // Round half-grid ties toward the lowest-id mover. It turns or reflects with the group,
    // so the same grid pivot is recovered on the next key press.
    let anchor = movers[0].2;
    let centre: [i64; 2] = std::array::from_fn(|i| anchor[i] + (lo[i] + hi[i] - 2 * anchor[i]) / 2);
    let mut ops = Vec::with_capacity(movers.len());
    for (path, mut hint, [x, y]) in movers {
        let [dx, dy] = orient.apply(&mut hint, [x - centre[0], y - centre[1]]);
        let (Ok(x), Ok(y)) = (i32::try_from(centre[0] + dx), i32::try_from(centre[1] + dy))
            else { return nothing("parts would leave the sheet grid") };
        hint.at = Some(SchPt::new(x, y));
        ops.push(CircuitOp::Hint { path: path.clone(), hint: Some(hint) });
    }
    edit(circuit, ops, format!("{label} {} parts", selected.len()))
}

/// Delete at the pointer, giving a pin or wire end first claim even when it is disconnected.
/// Removing the selection takes one component lookup. Disconnecting uses the wiring planner.
pub fn delete(circuit: &Circuit, target: Target, selected: Option<CompId>) -> Planned {
    if matches!(target, Target::Pin { .. } | Target::WireEnd { .. }) {
        return plan(circuit, &Gesture::Delete(target));
    }
    let Some(comp) = selected.and_then(|id| circuit.comps.get(id.ix())) else { return nothing("") };
    edit(circuit, [CircuitOp::Remove(comp.id)], format!("Delete {}", comp.refdes))
}

/// Delete all selected parts in one source edit. Duplicate ids count once and a missing part
/// cancels the whole edit. Sorting the selection costs O(k log k) for k supplied ids.
pub fn delete_parts(circuit: &Circuit, selected: &[CompId]) -> Planned {
    let mut ids = selected.to_vec();
    ids.sort_unstable();
    ids.dedup();
    match ids.as_slice() {
        [] => nothing(""),
        [id] => delete(circuit, Target::None, Some(*id)),
        [.., last] if last.ix() >= circuit.comps.len() => nothing(""),
        _ => {
            let label = format!("Delete {} parts", ids.len());
            edit(circuit, ids.into_iter().map(CircuitOp::Remove), label)
        }
    }
}

/// The net a label would name, or the hint for this target. A wire is a direct lookup.
/// A pin or wire end checks only that component's connections.
pub fn label_net(circuit: &Circuit, target: Target) -> Result<&Net, &'static str> {
    let net = match target {
        Target::Wire { net, .. } => net,
        Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. } => {
            circuit.comps.get(comp.ix()).and_then(|c| c.part.pins.get(pin.ix())).ok_or("")?;
            circuit.net_of(comp, pin).ok_or("wire the pin first: a label names a net")?
        }
        _ => return Err(""),
    };
    circuit.nets.get(net.ix()).ok_or("")
}

type PinId = (CompId, PinIx);

#[derive(Clone, Copy)]
pub(crate) enum State<'a> { Free, Loose(&'a Net), On(&'a Net) }

impl<'a> State<'a> {
    pub(crate) fn net(self) -> Option<&'a Net> {
        match self {
            Self::Free => None,
            Self::Loose(net) | Self::On(net) => Some(net),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Pin<'a> {
    pub(crate) id: PinId,
    pub(crate) refdes: &'a str,
    name: &'a str,
    pub(crate) state: State<'a>,
    marked: bool,
}

impl fmt::Display for Pin<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.refdes, self.name)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Aim<'a> { Empty, Pin(Pin<'a>), Net(&'a Net) }

pub(crate) struct Targets<'a> {
    pub(crate) circuit: &'a Circuit,
    // A gesture names at most two nets; aliases of a pin reuse its lone check.
    pub(crate) lone: [Option<(NetId, Option<PinId>)>; 2],
}

impl<'a> Targets<'a> {
    fn wire_from(&mut self, from: Target, a: Pin<'a>, to: Target) -> Planned {
        let Some(to) = self.aim(to) else { return nothing("") };
        if matches!(to, Aim::Pin(b) if a.id == b.id) { return nothing(""); }
        if matches!(from, Target::WireEnd { .. }) { move_end(self.circuit, a, to) } else { wire(self.circuit, a, to) }
    }

    pub(crate) fn pin(&mut self, comp: CompId, pin: PinIx) -> Option<Pin<'a>> {
        let circuit = self.circuit;
        let c = circuit.comps.get(comp.ix())?;
        let name = &c.part.pins.get(pin.ix())?.name;
        let state = match circuit.net_of(comp, pin) {
            None => State::Free,
            Some(id) => {
                let net = circuit.nets.get(id.ix())?;
                if self.lone(net) == Some((comp, pin)) { State::Loose(net) } else { State::On(net) }
            }
        };
        Some(Pin { id: (comp, pin), refdes: &c.refdes, name, state, marked: c.nc.contains(&pin) })
    }

    pub(crate) fn aim(&mut self, target: Target) -> Option<Aim<'a>> {
        match target {
            Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. } => {
                self.pin(comp, pin).map(Aim::Pin)
            }
            Target::Wire { net, .. } | Target::Tag { net, .. } => {
                let net = self.circuit.nets.get(net.ix())?;
                match self.lone(net) {
                    Some((comp, pin)) => self.pin(comp, pin).map(Aim::Pin),
                    None => Some(Aim::Net(net)),
                }
            }
            Target::Canvas { .. } | Target::None => Some(Aim::Empty),
        }
    }

    fn lone(&mut self, net: &Net) -> Option<PinId> {
        if net.scope.is_none() { return None; }
        if let Some((_, pin)) = self.lone.iter().flatten().find(|(id, _)| *id == net.id) {
            return *pin;
        }
        let pin = lone_pin(self.circuit, net.id);
        if let Some(slot) = self.lone.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some((net.id, pin));
        }
        pin
    }
}

fn lone_pin(circuit: &Circuit, net: NetId) -> Option<PinId> {
    if circuit.tree.iter().any(|block| block.ports.iter().any(|(_, nets)| nets.contains(&net))) {
        return None;
    }
    let mut lone = None;
    for comp in &circuit.comps {
        for &(pin, n) in &comp.conns {
            if n != net { continue; }
            let id = (comp.id, pin);
            if lone.is_some_and(|p| p != id) { return None; }
            lone = Some(id);
        }
    }
    lone
}

fn rename(circuit: &Circuit, net: &Net, to: &str) -> Planned {
    if net.name == to { return nothing(""); }
    let what = Sel::Net(NetRef { board: circuit.unit, net: net.id });
    edit(circuit, [CircuitOp::Rename { what, to: to.into() }], format!("Name the net {to}"))
}

fn global(circuit: &Circuit, p: Pin<'_>, name: &str) -> Planned {
    if name.is_empty() { return nothing("enter a net name"); }
    let rail = circuit.nets.iter().any(|net| net.name == name && net.kind != NetKind::Signal);
    let flagged = rail || circuit.hints.nets.iter().any(|(n, style)| n == name && *style == NetStyle::Label);
    let tie = CircuitOp::ConnectNamed { pin: p.id, name: name.into() };
    let flag = CircuitOp::NetStyle { name: name.into(), style: Some(NetStyle::Label) };
    match p.state {
        State::On(net) | State::Loose(net) if net.name == name => match flagged {
            true => nothing(""),
            false => edit(circuit, [flag], format!("Show {name} as labels")),
        },
        State::On(net) => Planned::Nothing {
            chip: format!("{p} is on {}: a global label ties a free pin (L renames a net)", net.name),
        },
        State::Free | State::Loose(_) => match flagged {
            true => edit(circuit, [tie], format!("Tie {p} to {name}")),
            false => edit(circuit, [tie, flag], format!("Tie {p} to {name}")),
        },
    }
}

fn wire(circuit: &Circuit, a: Pin<'_>, to: Aim<'_>) -> Planned {
    match to {
        Aim::Empty => nothing("release on a pin, wire or tag"),
        Aim::Pin(Pin { state: State::On(net), .. }) | Aim::Net(net) => wire_to_net(circuit, a, net),
        Aim::Pin(b) => {
            let label = match a.state {
                State::On(net) => format!("Wire {b} to {}", net.name),
                _ => format!("Wire {a} to {b}"),
            };
            match (a.state.net(), b.state.net()) {
                (Some(net), _) => connect(circuit, b, net, label),
                (None, Some(net)) => connect(circuit, a, net, label),
                (None, None) => edit(circuit, [CircuitOp::ConnectPins { a: a.id, b: b.id }], label),
            }
        }
    }
}

fn wire_to_net(circuit: &Circuit, a: Pin<'_>, nb: &Net) -> Planned {
    match a.state {
        State::On(na) if na.id == nb.id => already(a, na),
        State::On(na) => Planned::Refused {
            chip: format!("{} and {} are two nets: join them in the text", na.name, nb.name),
            sites: vec![
                Sel::Net(NetRef { board: circuit.unit, net: na.id }),
                Sel::Net(NetRef { board: circuit.unit, net: nb.id }),
            ],
        },
        _ => connect(circuit, a, nb, format!("Wire {a} to {}", nb.name)),
    }
}

fn move_end(circuit: &Circuit, p: Pin<'_>, to: Aim<'_>) -> Planned {
    let Some(net) = p.state.net() else { return nothing("") };
    let other = match to {
        Aim::Empty => return disconnect(circuit, p, net),
        Aim::Pin(Pin { state: State::On(net), .. }) | Aim::Net(net) => net,
        Aim::Pin(q) => {
            let label = format!("Move {} from {p} to {q}", net.name);
            let disconnect = CircuitOp::Disconnect { pin: p.id };
            let connect = CircuitOp::Connect { pin: q.id, net: net.id };
            return if matches!(q.state, State::Loose(_)) {
                edit(circuit, [disconnect, CircuitOp::Disconnect { pin: q.id }, connect], label)
            } else {
                edit(circuit, [disconnect, connect], label)
            };
        }
    };
    if net.id == other.id { return already(p, net); }
    edit(circuit, [CircuitOp::Disconnect { pin: p.id }, CircuitOp::Connect { pin: p.id, net: other.id }],
        format!("Move {p} from {} to {}", net.name, other.name))
}

fn connect(circuit: &Circuit, pin: Pin<'_>, net: &Net, label: String) -> Planned {
    let op = CircuitOp::Connect { pin: pin.id, net: net.id };
    if matches!(pin.state, State::Loose(_)) {
        edit(circuit, [CircuitOp::Disconnect { pin: pin.id }, op], label)
    } else {
        edit(circuit, [op], label)
    }
}

fn disconnect(circuit: &Circuit, pin: Pin<'_>, net: &Net) -> Planned {
    edit(circuit, [CircuitOp::Disconnect { pin: pin.id }], format!("Disconnect {pin} from {}", net.name))
}

fn already(pin: Pin<'_>, net: &Net) -> Planned {
    Planned::Nothing { chip: format!("{pin} is already on {}", net.name) }
}

pub(crate) fn nothing(chip: &str) -> Planned {
    Planned::Nothing { chip: chip.into() }
}

pub(crate) fn edit(circuit: &Circuit, ops: impl IntoIterator<Item = CircuitOp>, label: String) -> Planned {
    Planned::Edit(Resolved {
        ops: ops.into_iter().map(|op| DesignOp::Circuit { unit: circuit.unit, op }).collect(),
        place: vec![],
        label,
    })
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
