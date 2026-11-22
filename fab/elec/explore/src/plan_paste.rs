//! Copy and paste on the Circuit sheet. The destination block owns the new statements and hints.

use crate::plan::{edit, nothing, Planned};
use crate::plan_drop::{last_part, shared_instances};
use crate::sheet::{to_local, SheetLayout};
use fab_elec_circuit::{Circuit, CircuitOp, Component, End, NetKind, NetStyle, SheetHint};
use fab_elec_core::{CompRef, NetId, PinIx, SchPt, Sel};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// A selected part's source template and its displayed grid position at copy time.
pub struct CopyTemplate<'a> {
    pub part: &'a Component,
    pub stmt: String,
    pub detached: Result<String, String>,
    pub pins: Vec<PinIx>,
    pub at: [i32; 2],
    pub source_start: u32,
}

/// The clipboard text in pick order and the source identities needed to paste it after reevaluation.
pub struct CopiedPart {
    pub stmt: String,
    parts: Vec<PastePart>,
    after: usize,
}

struct PastePart {
    stmt: Range<usize>,
    detached: Result<String, String>,
    source: CompRef,
    path: String,
    refdes: String,
    ends: Vec<CopiedEnd>,
    offset: SchPt,
}

enum CopiedEnd {
    Open,
    Shared(u32),
    Named(NetId, String),
}

impl CopiedPart {
    /// Copy a single part without consulting sheet geometry.
    pub fn new(circuit: &Circuit, part: &Component, stmt: String, detached: Result<String, String>, pins: &[PinIx]) -> Self {
        Self::group(circuit, vec![CopyTemplate { part, stmt, detached, pins: pins.to_vec(), at: [0; 2], source_start: 0 }])
            .expect("a single part belongs to one block")
    }

    /// Count distinct copied pins on each written net, then capture the ends and group offsets.
    /// Runs only on copy, in two passes over the selected slots.
    pub fn group(circuit: &Circuit, templates: Vec<CopyTemplate<'_>>) -> Result<Self, &'static str> {
        let first = templates.first().ok_or("select a part to copy")?;
        if templates.iter().any(|t| t.part.block != first.part.block) { return Err("copy parts of one block"); }
        let origin = first.at;
        let after = templates.iter().enumerate().max_by_key(|(_, t)| t.source_start)
            .expect("the selection has a first part").0;
        let mut shared = HashSet::new();
        if templates.len() > 1 {
            let mut first_pin = HashMap::new();
            for t in &templates {
                for &pin in &t.pins {
                    if let Some(net) = circuit.net_of(t.part.id, pin) {
                        let here = (t.part.id, pin);
                        if *first_pin.entry(net).or_insert(here) != here { shared.insert(net); }
                    }
                }
            }
        }
        let mut stmt = String::new();
        let mut parts = Vec::with_capacity(templates.len());
        for t in templates {
            if !parts.is_empty() { stmt.push('\n'); }
            let start = stmt.len();
            stmt.push_str(&t.stmt);
            let ends = t.pins.iter().map(|&pin| {
                let Some(net) = circuit.net_of(t.part.id, pin).and_then(|id| circuit.nets.get(id.ix()))
                    else { return CopiedEnd::Open };
                let keep = matches!(net.kind, NetKind::Power | NetKind::Ground)
                    || circuit.hints.nets.iter().rev().find(|(name, _)| name == &net.name)
                        .is_some_and(|(_, style)| *style == NetStyle::Label);
                if keep { CopiedEnd::Named(net.id, net.name.clone()) }
                else if shared.contains(&net.id) { CopiedEnd::Shared(net.id.0) }
                else { CopiedEnd::Open }
            }).collect();
            parts.push(PastePart {
                stmt: start..stmt.len(), detached: t.detached, source: CompRef { board: circuit.unit, comp: t.part.id },
                path: t.part.path.clone(), refdes: t.part.refdes.clone(), ends,
                offset: SchPt::new(t.at[0] - origin[0], origin[1] - t.at[1]),
            });
        }
        Ok(Self { stmt, parts, after })
    }
}

impl PastePart {
    fn source<'a>(&self, circuit: &'a Circuit) -> Option<&'a Component> {
        if self.source.board != circuit.unit { return None; }
        circuit.comps.get(self.source.comp.ix()).filter(|part| part.path == self.path)
            .or_else(|| circuit.comp(&self.path))
    }
}

/// Paste at the snapped pointer in the displayed target frame. Resolve identities by path or
/// name if their ids moved. Crossing blocks scans components for the anchor, and a nested
/// destination collects its port and pin nets once for the whole group.
pub fn plan_paste(circuit: &Circuit, shown: &Circuit, sheet: &SheetLayout, copy: &CopiedPart, block: u32, at: [f64; 2]) -> Planned {
    let anchor = &copy.parts[copy.after];
    let Some(part) = anchor.source(circuit) else { return nothing("copy again: the part was removed") };
    let Some(displayed) = shown.tree.get(block as usize) else { return nothing("") };
    let Some(target) = circuit.tree.get(block as usize).filter(|b| b.path == displayed.path)
        .or_else(|| circuit.tree.iter().find(|b| b.path == displayed.path)) else { return nothing("the paste target was removed") };
    let cross_block = target.id != part.block;
    let after = if cross_block {
        let Some(last) = last_part(circuit, target.id) else { return nothing("this block has no part to paste after") };
        last
    } else {
        if anchor.source(shown).is_none() { return nothing(""); }
        part.id
    };
    let Some([x, y]) = to_local(sheet, block, at.map(|v| v.round() as i32)) else { return nothing("") };
    // Another block reaches a named net through a port, through a pin of one of its own parts,
    // or as its own: inside a block a net's name carries the instance path (`kick.LINE` is
    // kick's), and a net without one is the root's.
    let reachable = cross_block.then(|| {
        let mut nets: HashSet<_> = target.ports.iter().flat_map(|(_, nets)| nets.iter().copied()).collect();
        for comp in circuit.comps.iter().filter(|c| c.block == target.id) {
            nets.extend(comp.conns.iter().map(|&(_, net)| net));
        }
        nets
    });
    let own = |name: &str| circuit.tree.iter()
        .filter(|b| !b.path.is_empty() && name.strip_prefix(b.path.as_str()).is_some_and(|rest| rest.starts_with('.')))
        .max_by_key(|b| b.path.len()).map_or(target.path.is_empty(), |owner| owner.id == target.id);
    let mut opened = HashSet::new();
    let mut open_names = Vec::new();
    let mut ops = Vec::with_capacity(copy.parts.len());
    for member in &copy.parts {
        let Some(current) = member.source(circuit) else { return nothing("copy again: the part was removed") };
        if current.block != part.block { return nothing("copy parts of one block"); }
        let stmt = if cross_block {
            match &member.detached {
                Ok(stmt) => stmt.as_str(),
                Err(message) => return Planned::Refused { chip: message.clone(), sites: vec![Sel::Comp(member.source)] },
            }
        } else { &copy.stmt[member.stmt.clone()] };
        let mut ends = Vec::with_capacity(member.ends.len());
        for end in &member.ends {
            ends.push(match end {
                CopiedEnd::Open => End::Open,
                CopiedEnd::Shared(key) => End::Shared(*key),
                CopiedEnd::Named(id, name) => {
                    let Some(net) = circuit.nets.get(id.ix()).filter(|net| &net.name == name).or_else(|| circuit.net(name))
                        else { return nothing(&format!("copy again: net {name} was removed")) };
                    if reachable.as_ref().is_some_and(|nets| !nets.contains(&net.id) && !own(&net.name)) {
                        if opened.insert(name.as_str()) { open_names.push(name.as_str()); }
                        End::Open
                    } else { End::Net(net.id) }
                }
            });
        }
        // AddPart renames a let binding for the new sheet hint, including on a bare or mutable call.
        let first = stmt.split(|c: char| c != '_' && !c.is_alphanumeric()).next();
        let stmt = match first {
            Some("let") => stmt.to_string(),
            Some("var") => stmt.replacen("var", "let", 1),
            _ => format!("let part = {stmt}"),
        };
        let hint = SheetHint { at: Some(SchPt::new(x, y).add(member.offset)), ..SheetHint::default() };
        ops.push(CircuitOp::AddPart { stmt, after: Some(after), ends, hint: Some(hint) });
    }
    let mut label = if ops.len() == 1 { format!("Paste {}", anchor.refdes) } else { format!("Paste {} parts", ops.len()) };
    if cross_block {
        let name = if target.path.is_empty() { circuit.name.as_str() } else { target.path.rsplit('.').next().unwrap_or(&target.path) };
        label.push_str(&format!(" into {name}"));
        if let Some(count) = shared_instances(circuit, target) { label.push_str(&format!(" ({count} instances)")); }
        if !open_names.is_empty() { label.push_str(&format!(" ({} open)", open_names.join(", "))); }
    }
    edit(circuit, ops, label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Frame;
    use fab_elec_design::{fixture, DesignOp};

    #[test]
    fn paste_group_keeps_shared_and_named_nets_and_the_picked_offsets() {
        let mut c = (*fixture::probe::probe().circuit).clone();
        let j = c.comp("j").expect("the fixture has a header").id;
        let r = c.comp("led.r").expect("the fixture has a resistor").id;
        let header = "let j = e.part(hdr, {pins: [_n0 _n1 _n2 _n3 _n4]})";
        let resistor = "let r = e.r(1kohm, _n0, _n1)";
        // Pick the resistor first. Its statement is last in source.
        let copy = |c: &Circuit| CopiedPart::group(c, vec![
            CopyTemplate { part: &c.comps[r.ix()], stmt: resistor.into(), detached: Ok(resistor.into()), pins: vec![PinIx(0), PinIx(1)],
                at: [10, 20], source_start: 80 },
            CopyTemplate { part: &c.comps[j.ix()], stmt: header.into(), detached: Ok(header.into()), pins: (0..5).map(PinIx).collect(),
                at: [15, 17], source_start: 30 },
        ]);
        assert_eq!(copy(&c).err(), Some("copy parts of one block"), "a multi-block selection gives the copy refusal");
        let block = c.comps[r.ix()].block;
        c.comps[j.ix()].block = block;
        let shared = c.net("SWDIO").expect("header pin two is a signal").id;
        c.comps[r.ix()].conns[1].1 = shared;
        c.hints.nets.push(("NRST".into(), NetStyle::Label));
        let copied = copy(&c).expect("the two parts now share a block");
        assert_eq!(copied.stmt, format!("{resistor}\n{header}"), "clipboard text contains both complete statements in pick order");
        let mut sheet = SheetLayout::default();
        sheet.frames.resize(c.tree.len(), Frame::default());
        sheet.frames[block.ix()] = Frame { origin: [20, -5], placed: true, ..Frame::default() };
        let Planned::Edit(plan) = plan_paste(&c, &c, &sheet, &copied, block.0, [23.6, -8.2]) else { panic!("group paste plans one edit") };
        assert_eq!(plan.label, "Paste 2 parts", "both additions share one undo label");
        let named = |name| End::Net(c.net(name).expect("the named net exists").id);
        let add = |stmt: &str, ends, x, y| DesignOp::Circuit { unit: c.unit, op: CircuitOp::AddPart {
            stmt: stmt.into(), after: Some(r), ends,
            hint: Some(SheetHint { at: Some(SchPt::new(x, y)), ..SheetHint::default() }),
        } };
        assert_eq!(plan.ops, [
            add(resistor, vec![named("3V3"), End::Shared(shared.0)], 4, 3),
            add(header, vec![named("3V3"), End::Shared(shared.0), End::Open, named("NRST"), named("GND")], 9, 6),
        ], "both statements follow the last source part, share a fresh signal, retain named nets and preserve their grid offset");
    }

    #[test]
    fn paste_preserves_only_rails_and_labels_in_the_source_scope() {
        let mut c = (*fixture::probe::probe().circuit).clone();
        let j = c.comp("j").expect("the fixture has a header").id;
        let block = c.comp("led.r").expect("the fixture has a nested block").block;
        c.comps[j.ix()].block = block;
        c.comps[j.ix()].conns.retain(|(pin, _)| *pin != PinIx(3));
        c.hints.nets.push(("SWDIO".into(), NetStyle::Label));
        let stmt = "let j = e.part(hdr, {pins: [_n4 _n1 _n2 _n3 _n0]})";
        let copy = CopiedPart::new(&c, &c.comps[j.ix()], stmt.into(), Ok(stmt.into()), &[PinIx(4), PinIx(1), PinIx(2), PinIx(3), PinIx(0)]);
        let mut sheet = SheetLayout::default();
        sheet.frames.resize(c.tree.len(), Frame::default());
        sheet.frames[block.ix()] = Frame { origin: [20, -5], placed: true, ..Frame::default() };
        let shown = c.clone();
        // Evaluation renumbers both parts and nets when an earlier statement changes.
        c.comps.swap(0, 1);
        for (i, part) in c.comps.iter_mut().enumerate() { part.id = fab_elec_core::CompId(i as u32); }
        c.nets.swap(0, 2);
        for (i, net) in c.nets.iter_mut().enumerate() { net.id = NetId(i as u32); }
        let source = c.comp("j").expect("the original header is still present").id;
        let net = |name| End::Net(c.net(name).expect("the retained net exists").id);
        for (at, cell) in [([23.6, -8.2], SchPt::new(4, 3)), ([17.1, -2.4], SchPt::new(-3, -3))] {
            let Planned::Edit(plan) = plan_paste(&c, &shown, &sheet, &copy, block.0, at) else { panic!("each paste plans an edit") };
            assert_eq!(plan.label, "Paste J1", "each paste names its own undo step");
            assert_eq!(plan.ops, [DesignOp::Circuit { unit: c.unit, op: CircuitOp::AddPart {
                stmt: stmt.into(), after: Some(source), ends: vec![net("GND"), net("SWDIO"), End::Open, End::Open, net("3V3")],
                hint: Some(SheetHint { at: Some(cell), ..SheetHint::default() }),
            } }], "one statement keeps ground, label and power in slot order at the snapped block-local cell");
        }
        c.comps.remove(source.ix());
        assert_eq!(plan_paste(&c, &shown, &sheet, &copy, block.0, [0.0; 2]), nothing("copy again: the part was removed"),
            "an id reused by a different path cannot anchor the paste");
    }

    #[test]
    fn paste_into_block_uses_its_last_part_frame_and_reachable_nets() {
        use fab_elec_circuit::{Block, BlockDef, BlockDefId, PortDef, PortKind};
        use fab_elec_core::BlockId;

        let mut c = (*fixture::probe::probe().circuit).clone();
        let [j, r, d] = ["j", "led.r", "led.d"].map(|path| c.comp(path).expect("the probe has this part").id);
        let [rail, debug, clock, reset, gnd, anode] = ["3V3", "SWDIO", "SWCLK", "NRST", "GND", "led.A"]
            .map(|name| c.net(name).expect("the probe has this net").id);
        let led = c.comps[r.ix()].block;
        let def = BlockDefId(0);
        c.block_defs.push(BlockDef {
            id: def, name: "indicator".into(), params: vec![], sheet: Default::default(), template: None,
            ports: vec![PortDef { name: "debug".into(), kind: PortKind::In, volts: None, current: None, default: None }],
        });
        c.tree[led.ix()].def = Some(def);
        c.tree[led.ix()].ports.push(("debug".into(), vec![debug]));
        let empty = BlockId(c.tree.len() as u32);
        c.tree.push(Block { id: empty, path: "spare".into(), ..c.tree[led.ix()].clone() });
        c.hints.nets.extend(["SWDIO", "SWCLK", "NRST"].map(|name| (name.into(), NetStyle::Label)));
        // Two header pins on reset also leave ground on the diode alone.
        c.comps[j.ix()].conns[4].1 = reset;
        let header = "let j = e.part(hdr, {pins: [_n0 _n1 _n2 _n3 _n4]})";
        let copy = CopiedPart::new(&c, &c.comps[j.ix()], header.into(), Ok(header.into()), &[0, 1, 2, 3, 4].map(PinIx));
        let mut sheet = SheetLayout::default();
        sheet.frames.resize(c.tree.len(), Frame { placed: true, ..Frame::default() });
        sheet.frames[0].origin = [-4, 8];
        sheet.frames[led.ix()].origin = [20, -5];
        let at = [23.6, -8.2];
        let add = |stmt: &str, after, ends, x, y| DesignOp::Circuit { unit: c.unit, op: CircuitOp::AddPart {
            stmt: stmt.into(), after: Some(after), ends,
            hint: Some(SheetHint { at: Some(SchPt::new(x, y)), ..SheetHint::default() }),
        } };
        let Planned::Edit(same) = plan_paste(&c, &c, &sheet, &copy, 0, at) else { panic!("the header pastes in its source block") };
        assert_eq!(same.label, "Paste J1", "a same-block paste retains its label");
        assert_eq!(same.ops, [add(header, j, [rail, debug, clock, reset, reset].map(End::Net).to_vec(), 28, 16)],
            "a same-block paste keeps the source anchor, local cell and every named connection");

        let Planned::Edit(nested) = plan_paste(&c, &c, &sheet, &copy, led.0, at) else { panic!("the header pastes into led") };
        assert_eq!(nested.label, "Paste J1 into led (2 instances) (SWCLK, NRST open)",
            "the label counts affected instances and lists unreachable names once in slot order");
        assert_eq!(nested.ops, [add(header, d, vec![End::Net(rail), End::Net(debug), End::Open, End::Open, End::Open], 4, 3)],
            "the header follows the last led part at its local cell, keeping the rail by pin and debug by port");

        let resistor = "let r = e.r(1kohm, _n0, _n1)";
        let written = "let r = e.r(o.resistance, _n0, _n1)";
        let diode = "let d = e.part(led, {pins: [_n0 _n1]})";
        let copy_group = |detached| CopiedPart::group(&c, vec![
            CopyTemplate { part: &c.comps[r.ix()], stmt: written.into(), detached: Ok(resistor.into()),
                pins: vec![PinIx(0), PinIx(1)], at: [3, 5], source_start: 0 },
            CopyTemplate { part: &c.comps[d.ix()], stmt: diode.into(), detached,
                pins: vec![PinIx(0), PinIx(1)], at: [6, 3], source_start: 40 },
        ]).expect("both led parts belong to the same block");
        let group = copy_group(Ok(diode.into()));
        assert_eq!(group.stmt, format!("{written}\n{diode}"), "the clipboard keeps the parameter expression");
        let Planned::Edit(root) = plan_paste(&c, &c, &sheet, &group, 0, at) else { panic!("the led group pastes into the root") };
        assert_eq!(root.label, "Paste 2 parts into probe", "a group pasted into the root names the circuit");
        assert_eq!(root.ops, [
            add(resistor, j, vec![End::Net(rail), End::Shared(anode.0)], 28, 16),
            add(diode, j, vec![End::Shared(anode.0), End::Net(gnd)], 31, 18),
        ], "both members follow the root anchor with their offset, shared anode and ground even without a root ground pin");
        let message = "D1 uses led, which exists only inside indicator: paste it there";
        let local = copy_group(Err(message.into()));
        assert_eq!(plan_paste(&c, &c, &sheet, &local, 0, at),
            Planned::Refused { chip: message.into(), sites: vec![Sel::Comp(CompRef { board: c.unit, comp: d })] },
            "a block-local name in the second part refuses the whole cross-block edit");
        let Planned::Edit(same) = plan_paste(&c, &c, &sheet, &local, led.0, at) else { panic!("the group still pastes in its source block") };
        assert_eq!(same.ops, [
            add(written, d, vec![End::Net(rail), End::Shared(anode.0)], 4, 3),
            add(diode, d, vec![End::Shared(anode.0), End::Net(gnd)], 7, 5),
        ], "the source block keeps the parameter and local reads in the written statements");
        c.hints.nets.push(("led.A".into(), NetStyle::Label));
        let lone = CopiedPart::new(&c, &c.comps[r.ix()], written.into(), Ok(resistor.into()), &[PinIx(0), PinIx(1)]);
        let Planned::Edit(lone) = plan_paste(&c, &c, &sheet, &lone, 0, at) else { panic!("the led resistor pastes into the root") };
        assert_eq!((lone.label.as_str(), &lone.ops[..]), (format!("Paste {} into probe (led.A open)", c.comps[r.ix()].refdes).as_str(),
            &[add(resistor, j, vec![End::Net(rail), End::Open], 28, 16)][..]),
            "a label made inside led is led's own: the root cannot name it, so that end goes open");

        assert_eq!(plan_paste(&c, &c, &sheet, &copy, empty.0, at), nothing("this block has no part to paste after"),
            "an empty destination refuses before emitting an edit");
    }
}
