//! What an outside edit changed in the design, for its cue and flash (V9.3): a label in a history
//! entry's words ("R1 to 22k", "R5 added", "14 parts and 6 tracks") and the sites in the new design.
//! An edit from disk or another writer restarts the engine, so ids do not carry over: parts are
//! keyed by instance path, nets by name, board copper by what it is (net name, layer, width, shape).
//! An AI commit needs none of this: it carries its own label and sites.
//!
//! Cost: O(parts + connections + nets + tracks x segments + vias) hash-map work, once per outside
//! edit; nothing per frame.

use fab_elec_board::Board;
use fab_elec_circuit::Circuit;
use fab_elec_core::ids::{CompRef, NetRef};
use fab_elec_core::{BoardId, LayerId, Nm, Sel, P};
use fab_elec_core::Path;
use fab_elec_design::Design;
use super::views::ElecEditor;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

/// The label and sites of an outside edit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesignChanges {
    /// Empty when the design shows no change (a comment, a document no view reads).
    pub label: String,
    /// Each changed part, net and new piece of copper in `after`; removed things have none.
    pub sites: Vec<Sel>,
    /// The editors that show a change: a part or net the schematic (and the board when its unit
    /// has one), copper the board, a line the firmware. An edit of a document no editor opens (a
    /// block's or a part's definition) still shows where its effect lands.
    pub editors: Vec<ElecEditor>,
}

/// One change, counted under its kind for a burst's label.
struct Change {
    kind: Kind,
    /// Its label when it is the only change.
    words: String,
    /// How many of its kind it is (a net's routed copper counts each track and via).
    count: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Part,
    Net,
    Track,
    Via,
    FirmwareLine,
}

impl Kind {
    fn noun(self, count: usize) -> &'static str {
        match (self, count == 1) {
            (Kind::Part, true) => "part",
            (Kind::Part, false) => "parts",
            (Kind::Net, true) => "net",
            (Kind::Net, false) => "nets",
            (Kind::Track, true) => "track",
            (Kind::Track, false) => "tracks",
            (Kind::Via, true) => "via",
            (Kind::Via, false) => "vias",
            (Kind::FirmwareLine, true) => "firmware line",
            (Kind::FirmwareLine, false) => "firmware lines",
        }
    }
}

/// What `after` changed against `before`, with `lines` (each changed firmware document's changed
/// lines) counted in. Every change is a site; only the words summarise a burst.
pub fn design_changes(before: &Design, after: &Design, lines: &[(String, Range<usize>)]) -> DesignChanges {
    let mut changes = Vec::new();
    let mut sites = Vec::new();
    let mut editors = Vec::new();
    for unit in &after.units {
        let from = changes.len();
        match before.units.iter().find(|u| u.name == unit.name) {
            None => {
                for comp in &unit.circuit.comps {
                    changes.push(Change { kind: Kind::Part, words: format!("{} added", comp.refdes), count: 1 });
                    sites.push(Sel::Comp(CompRef { board: unit.id, comp: comp.id }));
                }
            }
            Some(was) => {
                let renamed = circuit_changes(&was.circuit, &unit.circuit, unit.id, unit.board.as_deref(), was.board.as_deref(), &mut changes, &mut sites);
                if let (Some(board), Some(old)) = (unit.board.as_deref(), was.board.as_deref()) {
                    copper_changes(old, board, unit.id, &renamed, &mut changes, &mut sites);
                }
            }
        }
        shown_in(&changes[from..], unit.board.is_some(), &mut editors);
    }
    for unit in &before.units {
        if !after.units.iter().any(|u| u.name == unit.name) {
            let from = changes.len();
            for comp in &unit.circuit.comps {
                changes.push(Change { kind: Kind::Part, words: format!("{} removed", comp.refdes), count: 1 });
            }
            shown_in(&changes[from..], unit.board.is_some(), &mut editors);
        }
    }
    if !lines.is_empty() && !editors.contains(&ElecEditor::Firmware) {
        editors.push(ElecEditor::Firmware);
    }
    for (doc, range) in lines {
        let count = range.len().max(1);
        let words = if count == 1 { format!("firmware line {}", range.start + 1) } else { format!("firmware lines {}–{}", range.start + 1, range.end) };
        let words = if lines.len() == 1 { words } else { format!("{words} of {doc}") };
        changes.push(Change { kind: Kind::FirmwareLine, words, count });
    }
    DesignChanges { label: label(&changes), sites, editors }
}

/// Add the editors that show `changes` of one unit (`board`: it has a board) to `editors`.
fn shown_in(changes: &[Change], board: bool, editors: &mut Vec<ElecEditor>) {
    for change in changes {
        let shows: &[ElecEditor] = match (change.kind, board) {
            (Kind::Part | Kind::Net, true) => &[ElecEditor::Schematic, ElecEditor::Board],
            (Kind::Part | Kind::Net, false) => &[ElecEditor::Schematic],
            (Kind::Track | Kind::Via, _) => &[ElecEditor::Board],
            (Kind::FirmwareLine, _) => &[ElecEditor::Firmware],
        };
        for editor in shows {
            if !editors.contains(editor) {
                editors.push(*editor);
            }
        }
    }
}

/// The parts and nets that changed; returns the renamed nets (old name to new), so copper on a
/// renamed net is not counted as rerouted.
fn circuit_changes(was: &Circuit, now: &Circuit, unit: BoardId, board: Option<&Board>, old_board: Option<&Board>, changes: &mut Vec<Change>, sites: &mut Vec<Sel>) -> HashMap<String, String> {
    let net_name = |circuit: &Circuit, net: fab_elec_core::NetId| circuit.nets.get(net.ix()).map_or(String::new(), |n| n.name.clone());
    // each net's members, to tell a rename (same members, another name) from a new net
    let members = |circuit: &Circuit| {
        let mut of: BTreeMap<String, Vec<(String, u32)>> = circuit.nets.iter().map(|n| (n.name.clone(), Vec::new())).collect();
        for comp in &circuit.comps {
            for (pin, net) in &comp.conns {
                if let Some(list) = of.get_mut(&net_name(circuit, *net)) {
                    list.push((comp.path.clone(), pin.0 as u32));
                }
            }
        }
        for list in of.values_mut() {
            list.sort_unstable();
        }
        of
    };
    let (old_nets, new_nets) = (members(was), members(now));
    let gone: HashMap<&Vec<(String, u32)>, &String> = old_nets.iter().filter(|(name, _)| !new_nets.contains_key(*name)).map(|(name, m)| (m, name)).collect();
    let mut renamed = HashMap::new();
    for net in &now.nets {
        if old_nets.contains_key(&net.name) {
            continue;
        }
        let site = Sel::Net(NetRef { board: unit, net: net.id });
        match gone.get(&new_nets[&net.name]).filter(|_| !new_nets[&net.name].is_empty()) {
            Some(old) => {
                renamed.insert((*old).clone(), net.name.clone());
                changes.push(Change { kind: Kind::Net, words: format!("net {old} to {}", net.name), count: 1 });
            }
            None => changes.push(Change { kind: Kind::Net, words: format!("net {} added", net.name), count: 1 }),
        }
        sites.push(site);
    }
    for name in old_nets.keys().filter(|name| !new_nets.contains_key(*name) && !renamed.contains_key(*name)) {
        changes.push(Change { kind: Kind::Net, words: format!("net {name} removed"), count: 1 });
    }

    let old_parts: HashMap<&str, &fab_elec_circuit::Component> = was.comps.iter().map(|c| (c.path.as_str(), c)).collect();
    let placed = |board: Option<&Board>| -> HashMap<String, (P, fab_elec_core::Angle, fab_elec_core::Side)> {
        board.map_or_else(HashMap::new, |b| b.placed.iter().map(|p| (p.path.clone(), (p.at, p.rot, p.side))).collect())
    };
    let (old_at, new_at) = (placed(old_board), placed(board));
    for comp in &now.comps {
        let site = Sel::Comp(CompRef { board: unit, comp: comp.id });
        let Some(old) = old_parts.get(comp.path.as_str()) else {
            changes.push(Change { kind: Kind::Part, words: format!("{} added", comp.refdes), count: 1 });
            sites.push(site);
            continue;
        };
        let value = |c: &fab_elec_circuit::Component| c.value.as_ref().map(|v| v.text.clone());
        let conns = |c: &fab_elec_circuit::Component, circuit: &Circuit, rename: &HashMap<String, String>| {
            let mut list: Vec<(u32, String)> = c.conns.iter().map(|(pin, net)| {
                let name = net_name(circuit, *net);
                (pin.0 as u32, rename.get(&name).cloned().unwrap_or(name))
            }).collect();
            list.sort_unstable();
            list
        };
        let words = if value(old) != value(comp) {
            Some(match value(comp) {
                Some(v) => format!("{} to {v}", comp.refdes),
                None => format!("{}'s value cleared", comp.refdes),
            })
        } else if old.part.name != comp.part.name {
            Some(format!("{} to {}", comp.refdes, comp.part.name))
        } else if conns(old, was, &renamed) != conns(comp, now, &HashMap::new()) {
            Some(format!("{} rewired", comp.refdes))
        } else if old.dnp != comp.dnp {
            Some(format!("{} {}", comp.refdes, if comp.dnp { "not fitted" } else { "fitted" }))
        } else if old_at.get(&comp.path).is_some() && old_at.get(&comp.path) != new_at.get(&comp.path) {
            Some(format!("{} moved", comp.refdes))
        } else {
            None
        };
        if let Some(words) = words {
            changes.push(Change { kind: Kind::Part, words, count: 1 });
            sites.push(site);
        }
    }
    let new_paths: std::collections::HashSet<&str> = now.comps.iter().map(|c| c.path.as_str()).collect();
    for comp in &was.comps {
        if !new_paths.contains(comp.path.as_str()) {
            changes.push(Change { kind: Kind::Part, words: format!("{} removed", comp.refdes), count: 1 });
        }
    }
    renamed
}

/// New and removed tracks and vias, per net: a net's new copper is one change ("GND routed").
fn copper_changes(old: &Board, now: &Board, unit: BoardId, renamed: &HashMap<String, String>, changes: &mut Vec<Change>, sites: &mut Vec<Sel>) {
    type TrackKey = (String, LayerId, Nm, Path);
    type ViaKey = (String, P, Nm, Nm, (LayerId, LayerId));
    let name = |board: &Board, net: fab_elec_core::NetId, rename: Option<&HashMap<String, String>>| {
        let name = board.nets.get(net.ix()).map_or(String::new(), |n| n.name.clone());
        rename.and_then(|r| r.get(&name).cloned()).unwrap_or(name)
    };
    let mut tracks: HashMap<TrackKey, usize> = HashMap::new();
    for t in &old.tracks {
        *tracks.entry((name(old, t.net, Some(renamed)), t.layer, t.w, t.path.clone())).or_default() += 1;
    }
    let mut vias: HashMap<ViaKey, usize> = HashMap::new();
    for v in &old.vias {
        *vias.entry((name(old, v.net, Some(renamed)), v.at, v.drill, v.pad, v.span)).or_default() += 1;
    }
    // per net: (tracks added, tracks removed, vias added, vias removed)
    let mut per_net: BTreeMap<String, [usize; 4]> = BTreeMap::new();
    fn take<K: std::hash::Hash + Eq>(map: &mut HashMap<K, usize>, key: K) -> bool {
        match map.get_mut(&key) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        }
    }
    for t in &now.tracks {
        let net = name(now, t.net, None);
        if !take(&mut tracks, (net.clone(), t.layer, t.w, t.path.clone())) {
            per_net.entry(net).or_default()[0] += 1;
            sites.push(Sel::Item(unit, t.id));
        }
    }
    for v in &now.vias {
        let net = name(now, v.net, None);
        if !take(&mut vias, (net.clone(), v.at, v.drill, v.pad, v.span)) {
            per_net.entry(net).or_default()[2] += 1;
            sites.push(Sel::Item(unit, v.id));
        }
    }
    for ((net, ..), n) in tracks {
        per_net.entry(net).or_default()[1] += n;
    }
    for ((net, ..), n) in vias {
        per_net.entry(net).or_default()[3] += n;
    }
    for (net, [ta, tr, va, vr]) in per_net {
        let (added, removed) = (ta + va, tr + vr);
        if added + removed == 0 {
            continue;
        }
        let words = match (added > 0, removed > 0) {
            (true, false) => format!("{net} routed"),
            (false, true) => format!("{net} unrouted"),
            _ => format!("{net} rerouted"),
        };
        // a net's copper is one change; its tracks and vias still count as such in a burst
        let (kind, count) = if ta + tr > 0 { (Kind::Track, ta + tr) } else { (Kind::Via, va + vr) };
        changes.push(Change { kind, words, count });
        if ta + tr > 0 && va + vr > 0 {
            changes.push(Change { kind: Kind::Via, words: String::new(), count: va + vr });
        }
    }
}

/// One change in its own words; more as counts, the two biggest kinds named, the rest counted:
/// "14 parts and 6 tracks", "14 parts, 6 tracks and 3 more".
fn label(changes: &[Change]) -> String {
    // a net's vias beside its tracks have no words of their own: they are that net's change
    let named: Vec<&Change> = changes.iter().filter(|c| !c.words.is_empty()).collect();
    if let [one] = named.as_slice() {
        return one.words.clone();
    }
    let mut counts: BTreeMap<Kind, usize> = BTreeMap::new();
    for c in changes {
        *counts.entry(c.kind).or_default() += c.count;
    }
    let mut kinds: Vec<(Kind, usize)> = counts.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let phrase = |(kind, n): (Kind, usize)| format!("{n} {}", kind.noun(n));
    match kinds.as_slice() {
        [] => String::new(),
        [one] => phrase(*one),
        [a, b] => format!("{} and {}", phrase(*a), phrase(*b)),
        [a, b, rest @ ..] => format!("{}, {} and {} more", phrase(*a), phrase(*b), rest.iter().map(|(_, n)| n).sum::<usize>()),
    }
}
