//! The Bench controls and indicators derived from one unit of the design.

use crate::board::nodes::natural_cmp;
use fab_elec_board::Placed;
use fab_elec_circuit::{Circuit, Component, NetKind};
use fab_elec_core::{mm, BBox, BlockId, CompId, Index, NetId, Nm, P};
use fab_elec_design::Unit;
use fab_elec_lang::BenchPanel;
use fab_elec_parts::{Footprint, PartKind};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::ControlFlow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    Key,
    Switch,
    Knob,
    Slider,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Control {
    pub comp: CompId,
    pub kind: ControlKind,
    pub label: String,
    pub row: u16,
    pub col: u16,
    pub glow: Option<CompId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub controls: Vec<Control>,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PanelModel {
    pub groups: Vec<Group>,
    pub indicators: Vec<CompId>,
}

/// Every populated switch and potentiometer contributes one control.
/// Switches are keys unless marked as latching or sliding. Pots are knobs unless over three times as long as wide.
/// Labels come from the label field, then the value text, then the reference.
/// Blocks form groups in circuit order. Indexed blocks share their definition name or unindexed path. Root controls group by kind.
/// Board positions snap within half the smaller footprint dimension (2 mm if unknown), top to bottom and left to right. Unplaced controls share a row in natural reference order.
/// A key or switch pairs with a mutually nearest LED below its group's smallest placed control spacing. A lone control uses twice its larger footprint side (2 mm for an unknown side). Next try a shared signal net or one resistor. Spare LEDs follow reference order.
pub fn panel_model(unit: &Unit) -> PanelModel {
    let circuit = &unit.circuit;
    let mut placed = vec![None; circuit.comps.len()];
    if let Some(board) = &unit.board {
        for part in &board.placed {
            if let Some(slot) = placed.get_mut(part.comp.ix()) {
                *slot = Some(part);
            }
        }
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut group_ids = BTreeMap::new();
    for comp in circuit.comps.iter().filter(|c| !c.dnp) {
        let footprint = placed[comp.id.ix()].map(|p| p.footprint.as_ref()).or(comp.part.footprint.as_deref());
        let Some(kind) = control_kind(comp, footprint) else { continue };
        let (path, name) = group_name(circuit, comp.block, kind);
        let index = group_ids.entry((path, name)).or_insert_with(|| {
            groups.push(Group { name: name.to_owned(), controls: Vec::new(), rows: 0, cols: 0 });
            groups.len() - 1
        });
        let label = comp.fields.get("label").filter(|s| !s.trim().is_empty())
            .or_else(|| comp.value.as_ref().map(|v| v.text.as_str()).filter(|s| !s.trim().is_empty()))
            .unwrap_or(&comp.refdes);
        groups[*index].controls.push(Control { comp: comp.id, kind, label: label.to_owned(), row: 0, col: 0, glow: None });
    }

    let indicators = pair_leds(circuit, &placed, &mut groups);
    let groups = groups.into_iter().flat_map(|group| arrange(group, circuit, &placed)).collect();
    PanelModel { groups, indicators }
}

/// `model` as the bench's `panel` option arranges it ([`BenchPanel`]): the panel's groups first,
/// in its order, each control keeping its generated kind and glow, under the panel's label when it
/// gives one, row by row `cols` wide (one row without); then every control the panel does not
/// name, in its generated group and arrangement, packed so a moved control leaves no empty row or
/// column. It never hides one: a named part the board leaves unpopulated has no control to show.
/// O(controls), once per lowering.
pub fn arranged(model: PanelModel, panel: &BenchPanel) -> PanelModel {
    let named: HashSet<CompId> = panel.groups.iter().flat_map(|g| g.controls.iter().map(|e| e.comp)).collect();
    let mut generated: HashMap<CompId, Control> = HashMap::new();
    let mut rest = Vec::new();
    for group in model.groups {
        let (moved, kept): (Vec<_>, Vec<_>) = group.controls.into_iter().partition(|c| named.contains(&c.comp));
        generated.extend(moved.into_iter().map(|c| (c.comp, c)));
        if !kept.is_empty() {
            rest.push((group.name, kept));
        }
    }
    let mut groups = Vec::new();
    for group in &panel.groups {
        let cols = group.cols.map_or(usize::MAX, |n| usize::from(n.max(1)));
        let controls = group.controls.iter().filter_map(|entry| {
            let mut control = generated.remove(&entry.comp)?;
            if let Some(label) = &entry.label {
                control.label = label.clone();
            }
            Some(control)
        });
        groups.extend(tiled(&group.name, controls.enumerate().map(|(i, c)| (c, (i / cols, i % cols)))));
    }
    for (name, kept) in rest {
        let dense = |of: fn(&Control) -> u16| -> HashMap<u16, usize> {
            kept.iter().map(of).collect::<BTreeSet<_>>().into_iter().enumerate().map(|(i, at)| (at, i)).collect()
        };
        let (rows, cols) = (dense(|c| c.row), dense(|c| c.col));
        let cells: Vec<_> = kept.iter().map(|c| (rows[&c.row], cols[&c.col])).collect();
        groups.extend(tiled(&name, kept.into_iter().zip(cells)));
    }
    PanelModel { groups, indicators: model.indicators }
}

fn maintained(text: &str) -> bool {
    let mut negated = false;
    for word in text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|word| !word.is_empty()) {
        let indicates_switch = ["latch", "latched", "latching", "slide", "slider", "sliding", "maintained", "toggle"]
            .iter().any(|kind| word.eq_ignore_ascii_case(kind));
        if indicates_switch && !negated { return true; }
        negated = ["non", "not", "no"].iter().any(|no| word.eq_ignore_ascii_case(no));
    }
    false
}

fn control_kind(comp: &Component, footprint: Option<&Footprint>) -> Option<ControlKind> {
    match comp.part.kind {
        PartKind::Switch => {
            let switch = footprint.is_some_and(|fp| maintained(&fp.name))
                || comp.fields.0.iter().chain(&comp.part.fields.0).any(|(name, value)| {
                    (maintained(name) && ["true", "yes", "1"].iter().any(|yes| value.eq_ignore_ascii_case(yes)))
                        || (!name.eq_ignore_ascii_case("label") && maintained(value))
                });
            Some(if switch { ControlKind::Switch } else { ControlKind::Key })
        }
        PartKind::Potentiometer => {
            let slider = footprint.is_some_and(|fp| {
                let bounds = fp.bbox();
                let (w, h) = (bounds.width(), bounds.height());
                w.min(h) > 0 && w.max(h) > 3 * w.min(h)
            });
            Some(if slider { ControlKind::Slider } else { ControlKind::Knob })
        }
        _ => None,
    }
}

fn group_name(circuit: &Circuit, id: BlockId, kind: ControlKind) -> (Option<&str>, &str) {
    if id == BlockId(0) {
        let name = match kind {
            ControlKind::Key => "Keys",
            ControlKind::Switch => "Switches",
            ControlKind::Knob | ControlKind::Slider => "Knobs",
        };
        return (None, name);
    }
    let block = &circuit.tree[id.ix()];
    let mut path = block.path.as_str();
    while let Some((prefix, index)) = path.strip_suffix(']').and_then(|s| s.rsplit_once('[')) {
        if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) { break; }
        path = prefix;
    }
    let name = if path.is_empty() || path != block.path {
        block.def.map(|id| circuit.block_defs[id.0 as usize].name.as_str()).filter(|s| !s.is_empty()).unwrap_or(path)
    } else {
        path
    };
    (Some(path), name)
}

// Positions increase along the axis. A new centre must fit every earlier footprint's
// half-size, and its own half-size must reach the first centre in the band.
fn snap(mut axis: Vec<(i64, i64, usize)>, mut assign: impl FnMut(usize, usize)) -> usize {
    axis.sort_unstable();
    let (mut count, mut first, mut end) = (0, 0, 0);
    for (pos, size, index) in axis {
        let size = if size > 0 { size } else { i64::from(mm(2.0)) };
        let pos = 2 * pos;
        if count == 0 || pos >= end || pos - first >= size {
            count += 1;
            first = pos;
            end = pos + size;
        } else {
            end = end.min(pos + size);
        }
        assign(index, count - 1);
    }
    count
}

fn arrange(group: Group, circuit: &Circuit, placed: &[Option<&Placed>]) -> Vec<Group> {
    let (mut xs, mut ys, mut unplaced) = (Vec::new(), Vec::new(), Vec::new());
    let mut cells = vec![(0, 0); group.controls.len()];
    for (i, control) in group.controls.iter().enumerate() {
        if let Some(p) = placed[control.comp.ix()] {
            let bounds = p.bbox();
            xs.push((i64::from(p.at.x), bounds.width(), i));
            ys.push((-i64::from(p.at.y), bounds.height(), i));
        } else {
            unplaced.push((&circuit.comps[control.comp.ix()].refdes, i));
        }
    }
    snap(xs, |i, col| cells[i].1 = col);
    let rows = snap(ys, |i, row| cells[i].0 = row);
    unplaced.sort_by(|a, b| natural_cmp(a.0, b.0));
    for (col, (_, i)) in unplaced.into_iter().enumerate() {
        cells[i] = (rows, col);
    }

    tiled(&group.name, group.controls.into_iter().zip(cells))
}

/// Controls at their (row, col) cells as groups named `name`: one, or a very large grid continued
/// in groups of the same name when a dimension no longer fits a cell index.
fn tiled(name: &str, cells: impl Iterator<Item = (Control, (usize, usize))>) -> Vec<Group> {
    let span = usize::from(u16::MAX);
    let mut tiles = BTreeMap::new();
    for (mut control, (row, col)) in cells {
        let tile = tiles.entry((row / span, col / span)).or_insert_with(|| Group {
            name: name.to_owned(), controls: Vec::new(), rows: 0, cols: 0,
        });
        control.row = (row % span) as u16;
        control.col = (col % span) as u16;
        tile.rows = tile.rows.max(control.row + 1);
        tile.cols = tile.cols.max(control.col + 1);
        tile.controls.push(control);
    }
    tiles.into_values().map(|mut tile| {
        tile.controls.sort_by_key(|c| (c.row, c.col));
        tile
    }).collect()
}

fn center(p: &Placed) -> P {
    p.to_board(p.footprint.anchor)
}

fn center_index<'a>(parts: impl Iterator<Item = &'a Placed>) -> Index<CompId> {
    Index::build(parts.map(|p| (BBox::point(center(p)), p.comp)).collect())
}

fn search_box(at: P, radius2: i128) -> BBox {
    match Nm::try_from(radius2.isqrt()) {
        Ok(radius) => BBox::point(at).inflate(radius),
        Err(_) => BBox::new(P::new(Nm::MIN, Nm::MIN), P::new(Nm::MAX, Nm::MAX)),
    }
}

// Tighten an upper bound using exact centre distances. Skip the centre itself when measuring pitch.
fn closest2(index: &Index<CompId>, at: P, mut limit: i128, skip: Option<CompId>) -> i128 {
    index.query(search_box(at, limit), |bounds, id| {
        if Some(id) != skip {
            limit = limit.min(at.dist2(bounds.min));
        }
        ControlFlow::Continue(())
    });
    limit
}

fn group_pitch2(parts: &[&Placed]) -> i128 {
    let Some((first, rest)) = parts.split_first() else { return 0 };
    let Some(second) = rest.first() else {
        let bounds = first.footprint.bbox();
        let side = bounds.width().max(bounds.height());
        let side = if side > 0 { side } else { i64::from(mm(2.0)) };
        return i128::from(2 * side).pow(2);
    };
    let index = center_index(parts.iter().copied());
    let mut pitch2 = center(first).dist2(center(second));
    for (bounds, id) in index.iter() {
        if pitch2 == 0 { break; }
        pitch2 = closest2(&index, bounds.min, pitch2, Some(id));
    }
    pitch2
}

fn pair_leds(circuit: &Circuit, placed: &[Option<&Placed>], groups: &mut [Group]) -> Vec<CompId> {
    let mut leds: Vec<_> = circuit.comps.iter().filter(|c| c.part.kind == PartKind::Led).map(|c| c.id).collect();
    if leds.is_empty() { return leds; }
    let lights = center_index(leds.iter().filter_map(|id| placed[id.ix()]));
    let switches = center_index(circuit.comps.iter().filter(|c| !c.dnp && c.part.kind == PartKind::Switch)
        .filter_map(|c| placed[c.id.ix()]));
    let mut nearby = Vec::new();
    for group in groups.iter() {
        let parts: Vec<_> = group.controls.iter().filter_map(|c| placed[c.comp.ix()]).collect();
        let mut keys = parts.iter().copied().filter(|p| circuit.comps[p.comp.ix()].part.kind == PartKind::Switch).peekable();
        if keys.peek().is_none() { continue; }
        let pitch2 = group_pitch2(&parts);
        for key in keys {
            let at = center(key);
            let distance2 = closest2(&lights, at, pitch2, None);
            if distance2 >= pitch2 { continue; }
            lights.query(search_box(at, distance2), |bounds, led| {
                if at.dist2(bounds.min) == distance2 && closest2(&switches, bounds.min, distance2, None) == distance2 {
                    nearby.push((distance2, led, key.comp));
                }
                ControlFlow::Continue(())
            });
        }
    }
    // Equal-distance pairs take free LEDs and keys in circuit order.
    nearby.sort_unstable();
    let mut glows = BTreeMap::new();
    let mut used = BTreeSet::new();
    for (_, led, key) in nearby {
        if !glows.contains_key(&key) && used.insert(led) {
            glows.insert(key, led);
        }
    }

    let mut on_net = vec![BTreeSet::new(); circuit.nets.len()];
    // only signal nets link a key to its LED: a shared supply or ground says nothing
    let signal = |n: NetId| circuit.nets[n.ix()].kind == NetKind::Signal;
    for &led in leds.iter().filter(|id| !used.contains(*id)) {
        for &(_, net) in circuit.comps[led.ix()].conns.iter().filter(|(_, n)| signal(*n)) {
            on_net[net.ix()].insert(led);
        }
    }
    let mut across = vec![Vec::new(); circuit.nets.len()];
    for r in circuit.comps.iter().filter(|c| c.part.kind == PartKind::Resistor && !c.dnp) {
        if let [(_, a), (_, b)] = r.conns[..] {
            across[a.ix()].push(b);
            across[b.ix()].push(a);
        }
    }
    // Reserve all direct matches before trying any one-resistor paths.
    for through_resistor in [false, true] {
        for key in circuit.comps.iter().filter(|c| c.part.kind == PartKind::Switch && !c.dnp) {
            if glows.contains_key(&key.id) { continue; }
            let available = |n: NetId| on_net[n.ix()].first().copied();
            let led = key.conns.iter().find_map(|&(_, n)| {
                if through_resistor { across[n.ix()].iter().copied().find_map(available) } else { available(n) }
            });
            if let Some(led) = led {
                glows.insert(key.id, led);
                used.insert(led);
                for &(_, net) in &circuit.comps[led.ix()].conns {
                    on_net[net.ix()].remove(&led);
                }
            }
        }
    }
    for c in groups.iter_mut().flat_map(|g| &mut g.controls) {
        c.glow = glows.get(&c.comp).copied();
    }
    leds.retain(|id| !used.contains(id));
    leds.sort_by(|a, b| natural_cmp(&circuit.comps[a.ix()].refdes, &circuit.comps[b.ix()].refdes));
    leds
}
