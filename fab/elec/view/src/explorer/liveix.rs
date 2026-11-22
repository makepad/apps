//! Worker-built sheet wiring and static indices into the simulation host's row textures.

use std::collections::{BTreeMap, HashMap};

use fab_elec_bench::ValueIndex;
use fab_elec_circuit::Circuit;
use fab_elec_core::{CompId, CompRef, NetId, NetRef, PinIx, PinRef};
use fab_elec_design::Design;
use fab_elec_explore::{graph::Pt, sheet::{Owner, SheetLayout, TagKind}};
use fab_elec_parts::PartKind;

use crate::live::segs::{cut_keys, TreeIn};
use super::glyphs::Xf;

/// Sheet index for a reveal's own drawing of a unit: `TreeOwner::Sheet(unit, REVEAL_SHEET)`.
/// The same nets have another geometry, so their segments carry rows of their own.
/// One reveal is shown at a time, so one owner per unit suffices.
pub const REVEAL_SHEET: u32 = u32::MAX;

/// Strip nets in label/member order. `adapt::graph_of` preserves circuit bus indices
/// and member order, so no layout graph needs rebuilding on the UI thread.
pub(super) fn mini_nets(circuit: &Circuit, owner: Owner) -> impl Iterator<Item = u32> + '_ {
    let trace = match owner { Owner::Trace(net) => Some(net), _ => None };
    let bus = match owner { Owner::Bundle(bus) => circuit.buses.get(bus as usize), _ => None };
    trace.into_iter().chain(bus.into_iter().flat_map(|bus| bus.members.iter().map(|(_, net)| net.0)))
}

/// A drawn wire piece, oriented from the tree segment's first node to its second.
#[derive(Clone, Debug)]
pub struct Piece {
    /// Start and end in sheet world-grid coordinates, following the original run.
    pub a: Pt,
    pub b: Pt,
    /// Tree and segment offsets in `SheetTrees::trees`.
    pub tree: u32,
    pub seg: u32,
}

/// Pieces in each sheet run's order, and the tree/segment of each bus entry.
#[derive(Clone, Debug, Default)]
pub struct SheetPieces {
    /// Drawn pieces parallel to `SheetLayout::wires`.
    pub runs: Vec<Vec<Piece>>,
    /// Tree/segment offsets parallel to `SheetLayout::entries`; zero-length entries have none.
    pub entries: Vec<Option<(u32, u32)>>,
}

/// Wiring to register with the host, plus the sheet geometry that uses its returned rows.
#[derive(Clone, Debug, Default)]
pub struct SheetTrees {
    pub trees: Vec<TreeIn>,
    pub pieces: SheetPieces,
}

/// One net's grid nodes and unsplit wiring, used only while building on the worker.
#[derive(Default)]
struct NetBuild {
    index: u32,
    nodes: HashMap<Pt, u32>,
    points: Vec<Pt>,
    wiring: Vec<(Pt, Pt)>,
    tree: TreeIn,
}

impl NetBuild {
    fn node(&mut self, at: Pt) -> u32 {
        let next = self.points.len() as u32;
        *self.nodes.entry(at).or_insert_with(|| {
            self.points.push(at);
            next
        })
    }

    fn segment(&mut self, a: Pt, b: Pt) -> Option<u32> {
        if a == b { return None; }
        let ends = (self.node(a), self.node(b));
        let seg = self.tree.segs.len() as u32;
        self.tree.segs.push(ends);
        self.tree.conductance.push((1.0 / length(a, b)) as f32);
        Some(seg)
    }
}

/// Projection along a closed segment, only for grid points on that segment.
fn along(a: Pt, b: Pt, at: Pt) -> Option<i128> {
    let d = [b[0] as i128 - a[0] as i128, b[1] as i128 - a[1] as i128];
    let p = [at[0] as i128 - a[0] as i128, at[1] as i128 - a[1] as i128];
    let dot = p[0] * d[0] + p[1] * d[1];
    let len2 = d[0] * d[0] + d[1] * d[1];
    (len2 > 0 && p[0] * d[1] == p[1] * d[0] && dot >= 0 && dot <= len2).then_some(dot)
}

/// Length in sheet world units, without overflowing integer coordinate differences.
fn length(a: Pt, b: Pt) -> f64 {
    (b[0] as f64 - a[0] as f64).hypot(b[1] as f64 - a[1] as f64)
}

/// Build one tree per wired circuit net, splitting runs at every node of their own net.
/// Pure worker work: pin geometry, grid searches, sorting and cut keys never run on the UI thread.
pub fn sheet_trees(design: &Design, unit: usize, sheet: &SheetLayout) -> SheetTrees {
    let mut out = SheetTrees {
        pieces: SheetPieces { runs: vec![Vec::new(); sheet.wires.len()], entries: vec![None; sheet.entries.len()] },
        ..SheetTrees::default()
    };
    let Some(unit) = design.units.get(unit) else { return out };
    let mut nets: BTreeMap<u32, NetBuild> = BTreeMap::new();
    for run in &sheet.wires {
        if run.net as usize >= unit.circuit.nets.len() { continue; }
        let net = nets.entry(run.net).or_default();
        for &at in &run.pts { net.node(at); }
        net.wiring.extend(run.pts.windows(2).map(|pair| (pair[0], pair[1])));
    }
    for entry in &sheet.entries {
        if entry.net as usize >= unit.circuit.nets.len() { continue; }
        let net = nets.entry(entry.net).or_default();
        net.node(entry.at);
        let port = net.node(entry.to);
        net.tree.ports.push(port);
        net.wiring.push((entry.at, entry.to));
    }
    for &(net, at) in &sheet.junctions {
        if let Some(net) = nets.get_mut(&net) { net.node(at); }
    }
    for (n, p) in sheet.parts.iter().enumerate().filter(|(_, p)| p.placed) {
        let Some(comp) = unit.circuit.comps.get(n) else { continue };
        let (_, ports) = fab_elec_explore::adapt::symbol_geometry(&comp.part, p.quarter);
        let xf = Xf { at: p.at.map(|v| v as f32), turn: p.turn, mirror: p.mirror };
        for (pin, port) in ports.iter().enumerate() {
            let Some((_, net)) = comp.conns.iter().find(|(index, _)| index.ix() == pin) else { continue };
            let Some(net) = nets.get_mut(&net.0) else { continue };
            let at = xf.point(port.at.map(|v| v as f32)).map(|v| v.round() as i32);
            // An interior pin splits its wire too; an unconnected drawing gets no stray pin node.
            if !net.nodes.contains_key(&at) && !net.wiring.iter().any(|&(a, b)| along(a, b, at).is_some()) {
                continue;
            }
            let node = net.node(at);
            net.tree.pins.push((node, PinRef { board: unit.id, comp: CompId(n as u32), pin: PinIx(pin as u16) }));
        }
    }
    for tag in &sheet.tags {
        if tag.kind == TagKind::NoConnect { continue; }
        let Some(net) = tag.net.and_then(|net| nets.get_mut(&net)) else { continue };
        let port = net.node(tag.at);
        net.tree.ports.push(port);
    }
    for (index, net) in nets.values_mut().enumerate() { net.index = index as u32; }
    for (i, run) in sheet.wires.iter().enumerate() {
        let Some(net) = nets.get_mut(&run.net) else { continue };
        for pair in run.pts.windows(2) {
            let mut points: Vec<_> = net.points.iter().filter_map(|&at| along(pair[0], pair[1], at).map(|t| (t, at))).collect();
            points.sort_unstable_by_key(|&(t, _)| t);
            for piece in points.windows(2) {
                let (a, b) = (piece[0].1, piece[1].1);
                if let Some(seg) = net.segment(a, b) {
                    out.pieces.runs[i].push(Piece { a, b, tree: net.index, seg });
                }
            }
        }
    }
    for (i, entry) in sheet.entries.iter().enumerate() {
        let Some(net) = nets.get_mut(&entry.net) else { continue };
        out.pieces.entries[i] = net.segment(entry.at, entry.to).map(|seg| (net.index, seg));
    }
    out.trees = nets.into_iter().map(|(net, mut built)| {
        built.tree.net = NetRef { board: unit.id, net: NetId(net) };
        built.tree.nodes = built.points.len() as u32;
        built.tree.ports.sort_unstable();
        built.tree.ports.dedup();
        built.tree.keys = cut_keys(built.tree.nodes, &built.tree.segs, &built.tree.pins);
        built.tree
    }).collect();
    out
}

/// Static live indices carried by scene instances; -1 means absent. Default has no live rows.
#[derive(Clone, Debug, Default)]
pub struct SceneLive {
    /// Net texture row indexed by circuit NetId.
    pub nets: Vec<f32>,
    /// Mini trace slot indexed by graph net (the circuit NetId); -1 means no strip.
    pub mini: Vec<f32>,
    /// Part texture row indexed by circuit component.
    pub parts: Vec<f32>,
    /// Pin texture rows by circuit component then pin index; non-MCU components stay empty.
    pub pins: Vec<Vec<f32>>,
    /// Each run's oriented pieces and their segment texture rows, in drawing order.
    pub runs: Vec<Vec<(Pt, Pt, f32)>>,
    /// Segment texture row of each sheet entry.
    pub entries: Vec<f32>,
}

impl SceneLive {
    /// Resolve one lowering and the rows returned by the host's tree registration.
    /// Without pieces, only net/part/pin rows and the supplied mini slots are present.
    pub fn new(index: &ValueIndex, design: &Design, unit: usize, segs: Option<(&SheetPieces, &[Vec<u32>])>, mini: &[f32]) -> SceneLive {
        let Some(unit) = design.units.get(unit) else { return Self::default() };
        let mut live = Self {
            mini: mini.to_vec(),
            nets: unit.circuit.nets.iter().enumerate().map(|(k, _)| {
                index.net(NetRef { board: unit.id, net: NetId(k as u32) }).map_or(-1.0, |row| row as f32)
            }).collect(),
            parts: unit.circuit.comps.iter().enumerate().map(|(n, _)| {
                index.comp(CompRef { board: unit.id, comp: CompId(n as u32) }).map_or(-1.0, |row| row as f32)
            }).collect(),
            pins: unit.circuit.comps.iter().enumerate().map(|(n, comp)| {
                if comp.part.kind != PartKind::Mcu { return Vec::new(); }
                comp.part.pins.iter().enumerate().map(|(pin, _)| {
                    index.pin(PinRef { board: unit.id, comp: CompId(n as u32), pin: PinIx(pin as u16) })
                        .map_or(-1.0, |row| row as f32)
                }).collect()
            }).collect(),
            ..Self::default()
        };
        if let Some((pieces, rows)) = segs {
            let row = |tree: u32, seg: u32| rows.get(tree as usize).and_then(|rows| rows.get(seg as usize)).map_or(-1.0, |&row| row as f32);
            live.runs = pieces.runs.iter().map(|run| run.iter().map(|p| (p.a, p.b, row(p.tree, p.seg))).collect()).collect();
            live.entries = pieces.entries.iter().map(|entry| entry.map_or(-1.0, |(tree, seg)| row(tree, seg))).collect();
        }
        live
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Tokens;
    use crate::explorer::scene::{Scene, SceneState};
    use crate::live::segs::{SegRows, TreeOwner};
    use fab_elec_bench::{fixture::ledctl_bench, lower, ModelLibs};
    use fab_elec_circuit::SheetHints;
    use fab_elec_explore::{layout, rows::SheetRows, Opts};
    use fab_elec_parts::PinKind;

    #[test]
    fn ledctl_pin_marks_follow_bench_rows_and_glyph_poses() {
        let design = fab_elec_fixtures::ledctl();
        let unit = &design.units[0];
        let circuit = &unit.circuit;
        let lowered = lower(&design, unit.id, &ledctl_bench(10e3, [0.3; 8]), &ModelLibs::generic());
        assert!(lowered.diags.is_empty(), "pin marks: the fixture bench lowers without diagnostics");
        let live = SceneLive::new(&lowered.index, &design, 0, None, &[]);
        let sheet = layout(&design, 0, &SheetHints::default(), &Opts::default());
        let rows = SheetRows::new().sync(circuit);
        let state = SceneState { assemble_at: Some(1.0), ..SceneState::default() };
        let scene = Scene::build(&design, 0, &sheet, &rows, Tokens::for_theme(true), &state, &live);
        let mut signals = 0;
        let mut excluded = 0;
        let mut others = 0;
        for comp in &circuit.comps {
            let item = rows.comp(comp.id.0).expect("pin marks: every fixture component has a sheet row") as f32;
            if comp.part.kind != PartKind::Mcu {
                assert!(live.pins[comp.id.ix()].is_empty(), "pin marks: non-MCU components allocate no pin rows");
                assert!(scene.pins.iter().all(|pin| pin.c.item != item), "pin marks: non-MCU components have no marks");
                others += 1;
                continue;
            }
            assert_eq!(live.pins[comp.id.ix()].len(), comp.part.pins.len(), "pin marks: MCU rows are indexed by part pin");
            for (pin, def) in comp.part.pins.iter().enumerate() {
                let row = lowered.index.pin(PinRef { board: unit.id, comp: comp.id, pin: PinIx(pin as u16) })
                    .expect("pin marks: every fixture MCU pin has a bench row") as f32;
                assert_eq!(live.pins[comp.id.ix()][pin], row, "pin marks: scene indices use the bench's ValueIndex::pin");
                let marks: Vec<_> = scene.pins.iter().filter(|mark| mark.live_pin == row).collect();
                if matches!(def.kind, PinKind::PowerIn | PinKind::PowerOut | PinKind::NoConnect) {
                    assert!(marks.is_empty(), "pin marks: power and NC pins have no mark");
                    excluded += 1;
                    continue;
                }
                assert_eq!(marks.len(), 1, "pin marks: every MCU signal pin has exactly one mark at its bench row");
                let mark = marks[0];
                let hit = scene.hits.pins.iter().find(|hit| hit.node == comp.id.0 && hit.pin == pin as u32)
                    .expect("pin marks: a signal pin has an outer-end hit");
                assert_eq!(mark.at, hit.outer.map(|v| v as f32), "pin marks: anchor is the pin's outer end");
                assert_eq!(mark.dir, std::array::from_fn(|i| (hit.outer[i] - hit.inner[i]) as f32), "pin marks: direction points away from the body");
                assert_eq!(mark.dir[0] * mark.dir[0] + mark.dir[1] * mark.dir[1], 1.0, "pin marks: outward direction is a unit vector");
                assert_eq!(mark.size, 0.22, "pin marks: half size is in world units");
                let stub = scene.lines.iter().find(|line| line.c.item == item && line.seg[..2] == mark.at)
                    .expect("pin marks: the outer end has a part pin stub");
                assert_eq!(mark.c, stub.c, "pin marks: identity, ink, pose and grouped assembly flight match the pin stub");
                signals += 1;
            }
        }
        assert!(signals > 0 && excluded > 0 && others > 0, "pin marks: the fixture exercises signals, excluded pins and non-MCU parts");
        assert_eq!(scene.pins.len(), signals, "pin marks: there are no extra instances");
        assert!(SceneLive::default().pins.is_empty(), "pin marks: default live indices allocate no pins");
        let absent = SceneLive::new(&ValueIndex::new(0), &design, 0, None, &[]);
        assert!(absent.pins.iter().flatten().all(|&row| row == -1.0), "pin marks: pins outside the lowering have absent row indices");
        let off = Scene::build(&design, 0, &sheet, &rows, Tokens::for_theme(true), &SceneState::default(), &absent);
        assert!(off.pins.is_empty(), "pin marks: absent rows produce no instances");
    }

    #[test]
    fn ledctl_sheet_trees_cover_wiring_and_keep_rows() {
        let design = fab_elec_fixtures::ledctl();
        let unit = &design.units[0];
        let circuit = &unit.circuit;
        let sheet = layout(&design, 0, &SheetHints::default(), &Opts::default());
        let SheetTrees { trees, pieces } = sheet_trees(&design, 0, &sheet);
        for (run, pieces) in sheet.wires.iter().zip(&pieces.runs) {
            let whole: f64 = run.pts.windows(2).map(|p| length(p[0], p[1])).sum();
            let split: f64 = pieces.iter().map(|p| length(p.a, p.b)).sum();
            assert!((whole - split).abs() < 1e-6, "trees: every wire is covered");
        }
        for tree in &trees {
            assert_eq!(tree.keys.len(), tree.segs.len(), "trees: keys parallel");
            assert_eq!(tree.conductance.len(), tree.segs.len(), "trees: keys parallel");
            assert!(tree.segs.iter().all(|&(a, b)| a < tree.nodes && b < tree.nodes), "trees: keys parallel");
            for &(node, pin) in &tree.pins {
                assert!(node < tree.nodes && pin.board == unit.id
                    && circuit.net_of(pin.comp, pin.pin) == Some(tree.net.net), "trees: pins are the net's");
            }
        }
        let nrst = circuit.nets.iter().find(|net| net.name.ends_with("NRST"));
        let u2 = circuit.comps.iter().find(|comp| comp.refdes == "U2");
        assert!(nrst.zip(u2).is_some_and(|(net, comp)| {
            comp.part.pin("NRST").is_some_and(|pin| trees.iter().any(|tree| tree.net.net == net.id
                && tree.pins.iter().any(|&(_, p)| p.comp == comp.id && p.pin == pin)))
        }), "trees: NRST has U2's pin");
        assert!(circuit.net("3V3").is_some_and(|net| trees.iter().any(|tree| tree.net.net == net.id && !tree.ports.is_empty())),
            "trees: supplies have ports");
        for &(net, at) in &sheet.junctions {
            let end = pieces.runs.iter().flatten().find_map(|p| {
                let tree = &trees[p.tree as usize];
                let &(a, b) = tree.segs.get(p.seg as usize)?;
                if tree.net.net.0 != net { return None; }
                if p.a == at { Some((tree, a)) } else if p.b == at { Some((tree, b)) } else { None }
            }).or_else(|| sheet.entries.iter().zip(&pieces.entries).find_map(|(entry, piece)| {
                if entry.net != net { return None; }
                let (tree, seg) = (*piece)?;
                let tree = &trees[tree as usize];
                let &(a, b) = tree.segs.get(seg as usize)?;
                if entry.at == at { Some((tree, a)) } else if entry.to == at { Some((tree, b)) } else { None }
            }));
            let ends = end.map(|(tree, node)| (
                tree.segs.iter().map(|&(a, b)| usize::from(a == node) + usize::from(b == node)).sum::<usize>(),
                tree.pins.iter().filter(|&&(at, _)| at == node).count(),
                tree.ports.iter().filter(|&&port| port == node).count(),
            ));
            assert!(ends.is_some_and(|(segs, pins, ports)| segs + pins + ports >= 3),
                "trees: junctions split ({} at {at:?}: segs, pins, ports {ends:?})", circuit.nets[net as usize].name);
        }
        let mut allocator = SegRows::new();
        let owner = TreeOwner::Sheet(unit.id, 0);
        let rows = allocator.set(owner, trees.clone());
        assert_eq!(rows.len(), trees.len(), "trees: rows");
        assert!(rows.iter().zip(&trees).all(|(rows, tree)| rows.len() == tree.segs.len()), "trees: rows");
        let again = sheet_trees(&design, 0, &sheet);
        assert_eq!(allocator.set(owner, again.trees), rows, "trees: rows are stable");

        let graph = fab_elec_explore::adapt::graph_of(circuit, &SheetHints::default());
        let focus = circuit.comps.iter().position(|comp| comp.refdes == "U2").expect("reveal trees: U2 exists") as u32;
        let reveal = fab_elec_explore::reveal::reveal_sheet(&design, 0, &graph, &sheet, focus);
        let SheetTrees { trees, pieces } = sheet_trees(&design, 0, &reveal.sheet);
        for (run, pieces) in reveal.sheet.wires.iter().zip(&pieces.runs) {
            let whole: f64 = run.pts.windows(2).map(|p| length(p[0], p[1])).sum();
            let split: f64 = pieces.iter().map(|p| length(p.a, p.b)).sum();
            assert!((whole - split).abs() < 1e-6, "reveal trees: every wire is covered");
        }
        let reveal_owner = TreeOwner::Sheet(unit.id, REVEAL_SHEET);
        allocator.set_prepared(reveal_owner, SegRows::prepare(trees));
        let sheet_rows = allocator.rows_of(owner);
        let reveal_rows = allocator.rows_of(reveal_owner);
        assert!(reveal_rows.iter().all(|row| sheet_rows.binary_search(row).is_err()), "reveal trees: rows of its own");
        allocator.remove(reveal_owner);
        assert!(allocator.rows_of(reveal_owner).is_empty(), "reveal trees: removed with the reveal");
        let mut freed = allocator.take_freed();
        freed.sort_unstable();
        assert_eq!(freed, reveal_rows, "reveal trees: removed with the reveal");
    }
}
