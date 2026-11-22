//! Routed wire topology and persistent identities for the host's current textures.
//!
//! Geometry only breaks ties where connectivity cannot name a piece. No texture rows or
//! simulation state live here; ordered collections keep every output independent of hash order.

use crate::graph::Pt;
use crate::{LayoutGraph, SheetLayout};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// A circuit pin: (component index, pin index).
pub type PinKey = (u32, u32);

/// The sorted pins on the side of the cut that holds the net's smallest pin, which is the
/// lexicographically smaller side whenever both sides have pins (the host's segment-row key form,
/// `simhost::segs::SegKey::pins`, under `TreeOwner::Sheet`). An island without that pin uses its
/// lexicographically smaller nonempty side. Loops and pinless islands use sorted endpoint keys.
/// The ordinal distinguishes equal keys in first-stroke order.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SegKey {
    Cut(Vec<PinKey>, u32),
    Ends(NodeKey, NodeKey, u32),
}

/// A pin, or a non-pin junction, frame port or bus entry at its grid position.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKey {
    Pin(PinKey),
    Point(i32, i32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetWires {
    pub net: u32,
    /// Pins first, sorted by PinKey, then other nodes sorted by NodeKey.
    /// Coincident pins share a node named by their smallest pin; `pins` includes them all.
    pub nodes: Vec<NodeKey>,
    /// Unbranched pieces, sorted by their unique keys.
    pub segs: Vec<Seg>,
    /// (Node index, circuit pin), sorted by circuit pin.
    pub pins: Vec<(u32, PinKey)>,
    /// Degree-one endpoints without pins, where an island exchanges current with the net.
    pub ports: Vec<u32>,
    /// Run order, then stroke order; a split stroke appears for each piece it crosses.
    pub strokes: Vec<Stroke>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    /// Current is positive from `a` to `b`; both index `NetWires::nodes`.
    pub a: u32,
    /// The end toward the net's smallest pin for a cut, otherwise the smaller NodeKey.
    pub b: u32,
    pub key: SegKey,
    /// Arc length in grid units.
    pub len: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    /// Index into `SheetLayout::wires`.
    pub run: u32,
    /// The stroke from `pts[index]` to `pts[index + 1]`.
    pub index: u32,
    /// Index into `NetWires::segs`.
    pub seg: u32,
    /// Arc coordinate of the original stroke's start, measured from the segment's `a` end.
    /// For a split stroke this can lie outside `[0, seg.len]`: clip its arc interval to that
    /// range to draw just this piece. Keeping the original start preserves the draw phase.
    pub along0: f64,
    /// The draw direction is from `b` toward `a`, opposite positive current.
    pub reversed: bool,
}

/// Every net with at least one wire run, sorted by net index. Pure and deterministic.
pub fn wire_trees(sheet: &SheetLayout, g: &LayoutGraph) -> Vec<NetWires> {
    let mut runs = BTreeMap::<u32, Vec<usize>>::new();
    for (i, run) in sheet.wires.iter().enumerate() {
        runs.entry(run.net).or_default().push(i);
    }
    let mut pins = BTreeMap::<u32, Vec<(PinKey, Pt)>>::new();
    let port_nets = g.port_nets();
    for (n, (part, node)) in sheet.parts.iter().zip(&g.nodes).enumerate() {
        if !part.placed {
            continue;
        }
        for (p, port) in node.ports.iter().enumerate() {
            let Some(net) = port_nets[n][p] else { continue };
            let at = rot(port.at, part.turn, part.mirror);
            pins.entry(net).or_default().push((
                (n as u32, p as u32),
                [part.at[0] + at[0], part.at[1] + at[1]],
            ));
        }
    }
    runs.into_iter().map(|(net, runs)| {
        let smallest = g.nets.get(net as usize).and_then(|n| n.pins.iter().min().copied());
        net_tree(sheet, net, &runs, pins.get(&net).map(Vec::as_slice).unwrap_or(&[]), smallest)
    }).collect()
}

// Mirror x first, then quarter turns counter-clockwise, exactly as in sheet::rot.
pub(crate) fn rot(p: Pt, turn: u8, mirror: bool) -> Pt {
    let p = if mirror { [-p[0], p[1]] } else { p };
    match turn & 3 {
        0 => p,
        1 => [-p[1], p[0]],
        2 => [-p[0], -p[1]],
        _ => [p[1], -p[0]],
    }
}

fn distance(a: Pt, b: Pt) -> i64 {
    (i64::from(a[0]) - i64::from(b[0])).abs()
        + (i64::from(a[1]) - i64::from(b[1])).abs()
}

pub(crate) fn on_stroke(p: Pt, a: Pt, b: Pt) -> bool {
    (a[0] == b[0] && p[0] == a[0] && a[1].min(b[1]) <= p[1] && p[1] <= a[1].max(b[1]))
        || (a[1] == b[1] && p[1] == a[1] && a[0].min(b[0]) <= p[0] && p[0] <= a[0].max(b[0]))
}

struct EdgeUse {
    run: u32,
    index: u32,
    forward: bool,
    offset: i64,
}

struct Edge {
    a: usize,
    b: usize,
    len: i64,
    usage: EdgeUse,
}

struct Piece {
    a: usize,
    b: usize,
    len: i64,
    // Edge index and whether the walk follows its canonical a-to-b direction.
    steps: Vec<(usize, bool)>,
}

fn walk(
    start: usize,
    first: usize,
    edges: &[Edge],
    adjacent: &[Vec<usize>],
    nodes: &[bool],
    used: &mut [bool],
) -> Piece {
    let mut piece = Piece { a: start, b: start, len: 0, steps: Vec::new() };
    let mut edge_id = first;
    loop {
        used[edge_id] = true;
        let edge = &edges[edge_id];
        let forward = piece.b == edge.a;
        piece.b = if forward { edge.b } else { edge.a };
        piece.len += edge.len;
        piece.steps.push((edge_id, forward));
        if nodes[piece.b] {
            return piece;
        }
        let links = &adjacent[piece.b];
        edge_id = if links[0] == edge_id { links[1] } else { links[0] };
    }
}

fn reachable(start: usize, excluded: usize, adjacent: &[Vec<(usize, usize)>]) -> Vec<bool> {
    let mut seen = vec![false; adjacent.len()];
    let mut queue = VecDeque::from([start]);
    seen[start] = true;
    while let Some(node) = queue.pop_front() {
        for &(next, seg) in &adjacent[node] {
            if seg != excluded && !seen[next] {
                seen[next] = true;
                queue.push_back(next);
            }
        }
    }
    seen
}

fn net_tree(
    sheet: &SheetLayout,
    net: u32,
    runs: &[usize],
    pins: &[(PinKey, Pt)],
    smallest: Option<PinKey>,
) -> NetWires {
    let mut at_pins = BTreeMap::<Pt, Vec<PinKey>>::new();
    let mut points = BTreeSet::new();
    for &(pin, at) in pins {
        at_pins.entry(at).or_default().push(pin);
        points.insert(at);
    }
    for &run in runs {
        points.extend(sheet.wires[run].pts.iter().copied());
    }
    let points: Vec<_> = points.into_iter().collect();
    let point_ids: BTreeMap<_, _> = points.iter().enumerate().map(|(i, &p)| (p, i)).collect();
    let mut edge_uses = BTreeMap::<(usize, usize), Vec<EdgeUse>>::new();
    for &run in runs {
        for (index, stroke) in sheet.wires[run].pts.windows(2).enumerate() {
            let (a, b) = (stroke[0], stroke[1]);
            // Repeated vertices have no length and draw no straight stroke.
            if a == b {
                continue;
            }
            let mut split: Vec<_> = points.iter().enumerate()
                .filter_map(|(i, &p)| on_stroke(p, a, b).then_some(i)).collect();
            if a > b {
                split.reverse();
            }
            for pair in split.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                edge_uses.entry((a.min(b), a.max(b))).or_default().push(EdgeUse {
                    run: run as u32,
                    index: index as u32,
                    forward: a < b,
                    offset: distance(stroke[0], points[a]),
                });
            }
        }
    }
    // Keep parallel drawn edges: every original stroke contributes its length exactly once.
    let edges: Vec<_> = edge_uses.into_iter().flat_map(|((a, b), uses)| {
        let len = distance(points[a], points[b]);
        uses.into_iter().map(move |usage| Edge { a, b, len, usage })
    }).collect();
    let mut adjacent = vec![Vec::new(); points.len()];
    for (i, edge) in edges.iter().enumerate() {
        adjacent[edge.a].push(i);
        adjacent[edge.b].push(i);
    }
    let mut is_node: Vec<_> = points.iter().enumerate().map(|(i, p)| {
        at_pins.contains_key(p) || adjacent[i].len() == 1 || adjacent[i].len() >= 3
    }).collect();
    let mut used = vec![false; edges.len()];
    let mut pieces = Vec::new();
    for node in 0..points.len() {
        if is_node[node] {
            for &edge in &adjacent[node] {
                if !used[edge] {
                    pieces.push(walk(node, edge, &edges, &adjacent, &is_node, &mut used));
                }
            }
        }
    }
    // Only isolated degree-two loops remain. Sorted edges select each loop's smallest point.
    for (i, edge) in edges.iter().enumerate() {
        if !used[i] {
            is_node[edge.a] = true;
            pieces.push(walk(edge.a, i, &edges, &adjacent, &is_node, &mut used));
        }
    }
    let mut named: Vec<_> = points.iter().enumerate().filter_map(|(i, &p)| {
        is_node[i].then(|| {
            let key = match at_pins.get(&p) {
                Some(pins) => NodeKey::Pin(*pins.iter().min().expect("pin point has pins")),
                None => NodeKey::Point(p[0], p[1]),
            };
            (key, i)
        })
    }).collect();
    named.sort();
    let mut node_ids = vec![0; points.len()];
    for (i, &(_, point)) in named.iter().enumerate() {
        node_ids[point] = i;
    }
    let ports = named.iter().enumerate().filter_map(|(i, (_, p))| {
        (adjacent[*p].len() == 1 && !at_pins.contains_key(&points[*p])).then_some(i as u32)
    }).collect();
    let nodes: Vec<_> = named.into_iter().map(|(key, _)| key).collect();
    let pins: Vec<_> = pins.iter().map(|&(pin, p)| (node_ids[point_ids[&p]] as u32, pin)).collect();
    let mut links = vec![Vec::new(); nodes.len()];
    for (i, piece) in pieces.iter().enumerate() {
        let (a, b) = (node_ids[piece.a], node_ids[piece.b]);
        links[a].push((b, i));
        links[b].push((a, i));
    }
    let mut segs = Vec::new();
    let mut strokes = Vec::new();
    let mut first_strokes = Vec::new();
    for (i, piece) in pieces.iter().enumerate() {
        let (mut a, mut b) = (node_ids[piece.a], node_ids[piece.b]);
        let from_a = reachable(a, i, &links);
        let mut cut = Vec::new();
        let mut flip = nodes[a] < nodes[b]; // The smaller key is the b end of a loop.
        if !from_a[b] {
            let from_b = reachable(b, i, &links);
            let a_pins: Vec<_> = pins.iter().filter_map(|&(n, p)| from_a[n as usize].then_some(p)).collect();
            let b_pins: Vec<_> = pins.iter().filter_map(|&(n, p)| from_b[n as usize].then_some(p)).collect();
            if smallest.is_some_and(|p| a_pins.contains(&p)) {
                flip = true;
                cut = a_pins;
            } else if smallest.is_some_and(|p| b_pins.contains(&p)) {
                flip = false;
                cut = b_pins;
            } else {
                // A labelled/bus island may not contain the net's smallest pin. Both sides
                // qualify; prefer the lexicographically first nonempty pin set, independent
                // of where this island moved. With no pins, retain the endpoint-key order.
                if !a_pins.is_empty() && (b_pins.is_empty() || a_pins < b_pins) {
                    flip = false;
                    cut = a_pins;
                } else if !b_pins.is_empty() {
                    flip = true;
                    cut = b_pins;
                }
            }
        }
        let key = if cut.is_empty() {
            SegKey::Ends(nodes[a.min(b)].clone(), nodes[a.max(b)].clone(), 0)
        } else {
            SegKey::Cut(cut, 0)
        };
        if flip {
            std::mem::swap(&mut a, &mut b);
        }
        segs.push(Seg { a: a as u32, b: b as u32, key, len: piece.len as f64 });
        // Collapse successive elementary edges belonging to the same original stroke.
        // The signed arc coordinate also distinguishes two visits across a loop's seam.
        let mut mapped = BTreeMap::<(u32, u32, i64, bool), i64>::new();
        let mut along = 0;
        for &(edge_id, forward) in &piece.steps {
            let edge = &edges[edge_id];
            let usage = &edge.usage;
            let mut reversed = forward != usage.forward;
            let mut along0 = if reversed { along + edge.len + usage.offset } else { along - usage.offset };
            if flip {
                along0 = piece.len - along0;
                reversed = !reversed;
            }
            mapped.entry((usage.run, usage.index, along0, reversed))
                .and_modify(|offset| *offset = (*offset).min(usage.offset))
                .or_insert(usage.offset);
            along += edge.len;
        }
        let first = mapped.iter().map(|(&(run, index, _, _), &offset)| (run, index, offset))
            .min().expect("a piece contains a drawn edge");
        first_strokes.push((first, i));
        for ((run, index, along0, reversed), offset) in mapped {
            strokes.push((Stroke { run, index, seg: i as u32, along0: along0 as f64, reversed }, offset));
        }
    }
    first_strokes.sort();
    let mut ordinals = BTreeMap::<SegKey, u32>::new();
    for (_, i) in first_strokes {
        let ordinal = ordinals.entry(segs[i].key.clone()).or_default();
        match &mut segs[i].key {
            SegKey::Cut(_, n) | SegKey::Ends(_, _, n) => *n = *ordinal,
        }
        *ordinal += 1;
    }
    let mut order: Vec<_> = segs.into_iter().enumerate().collect();
    order.sort_by(|(_, a), (_, b)| a.key.cmp(&b.key));
    let mut remap = vec![0; order.len()];
    for (i, (old, _)) in order.iter().enumerate() {
        remap[*old] = i as u32;
    }
    for (stroke, _) in &mut strokes {
        stroke.seg = remap[stroke.seg as usize];
    }
    strokes.sort_by_key(|(s, offset)| (s.run, s.index, *offset, s.seg));
    NetWires {
        net,
        nodes,
        segs: order.into_iter().map(|(_, seg)| seg).collect(),
        pins,
        ports,
        strokes: strokes.into_iter().map(|(stroke, _)| stroke).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Opts;
    use fab_elec_circuit::{Circuit, Component, QtyUnit, Quantity, SheetHints, Value};
    use fab_elec_core::{BlockId, CompId, NetId};
    use fab_elec_design::{fixture, Design};
    use fab_elec_parts::{Fields, PartDef};
    use std::sync::Arc;

    // The same component construction and add_part edit as explore's layout tests.
    fn component(
        c: &mut Circuit,
        path: &str,
        refdes: &str,
        part: Arc<PartDef>,
        value: Option<Value>,
        connections: &[(&str, NetId)],
        nc: &[&str],
        block: BlockId,
    ) -> CompId {
        let id = CompId(c.comps.len() as u32);
        let mut conns: Vec<_> = connections.iter()
            .map(|&(name, net)| (part.pin(name).expect("fixture connection names a part pin"), net))
            .collect();
        conns.sort_by_key(|&(pin, _)| pin);
        let nc = nc.iter().map(|name| part.pin(name).expect("fixture NC names a part pin")).collect();
        c.comps.push(Component {
            id, path: path.into(), refdes: refdes.into(), ref_kind: Default::default(), part, value, conns, nc, block, dnp: false,
            fields: Fields::default(),
        });
        id
    }

    fn add_part(d: &Design) -> (Design, BlockId) {
        let mut edited = d.clone();
        let mut c = (*edited.units[0].circuit).clone();
        let block = c.comp("mcu.u").expect("fixture has MCU").block;
        let v33 = c.net("3V3").expect("fixture has 3V3").id;
        let gnd = c.net("GND").expect("fixture has GND").id;
        component(
            &mut c,
            "mcu.dec[3]",
            "C7",
            fixture::parts::capacitor("0402"),
            Some(Value {
                text: "100nF".into(),
                qty: Some(Quantity { si: 100e-9, unit: QtyUnit::Farad }),
                tol: None,
            }),
            &[("1", v33), ("2", gnd)],
            &[],
            block,
        );
        edited.units[0].circuit = Arc::new(c);
        (edited, block)
    }

    #[test]
    fn trees_cover_the_wires_and_name_their_pins() {
        let design = fab_elec_fixtures::ledctl();
        let hints = SheetHints::default();
        let sheet = crate::layout(&design, 0, &hints, &Opts::default());
        let graph = crate::adapt::graph_of(&design.units[0].circuit, &hints);
        let trees = wire_trees(&sheet, &graph);
        assert_eq!(trees, wire_trees(&sheet, &graph), "purity: identical inputs yield identical trees");
        let nets: BTreeSet<_> = sheet.wires.iter().map(|r| r.net).collect();
        assert!(!nets.is_empty(), "coverage: the fixture has drawn nets");
        assert_eq!(trees.iter().map(|t| t.net).collect::<Vec<_>>(), nets.into_iter().collect::<Vec<_>>(),
            "coverage: every drawn net occurs once in net order");
        for tree in &trees {
            let ends: BTreeMap<_, _> = graph.nets[tree.net as usize].pins.iter().filter_map(|&(n, p)| {
                let part = sheet.parts[n as usize];
                if !part.placed {
                    return None;
                }
                let at = rot(graph.nodes[n as usize].ports[p as usize].at, part.turn, part.mirror);
                Some(((n, p), [part.at[0] + at[0], part.at[1] + at[1]]))
            }).collect();
            let node_point = |node: u32| match &tree.nodes[node as usize] {
                NodeKey::Pin(pin) => *ends.get(pin).expect("pin identity: a pin node belongs to this net"),
                NodeKey::Point(x, y) => [*x, *y],
            };
            assert!(tree.nodes.windows(2).all(|w| w[0] < w[1]), "node order: pins precede sorted point nodes");
            for (i, node) in tree.nodes.iter().enumerate() {
                if let NodeKey::Pin(pin) = node {
                    assert!(ends.contains_key(pin), "pin identity: every pin node is an endpoint of this net");
                    assert!(tree.pins.contains(&(i as u32, *pin)), "pin mapping: every pin node has a current input");
                }
            }
            assert_eq!(tree.pins.iter().map(|&(_, pin)| pin).collect::<Vec<_>>(), ends.keys().copied().collect::<Vec<_>>(),
                "pin coverage: every placed pin of this net is mapped once");
            for &(node, pin) in &tree.pins {
                assert_eq!(node_point(node), ends[&pin], "pin geometry: coincident pin mappings use their actual endpoint");
            }
            let mut covered = vec![0.0; sheet.wires.len()];
            let mut segment_covered = vec![0.0; tree.segs.len()];
            for stroke in &tree.strokes {
                let run = &sheet.wires[stroke.run as usize];
                assert_eq!(run.net, tree.net, "stroke ownership: mapped runs belong to their net");
                let (a, b) = (run.pts[stroke.index as usize], run.pts[stroke.index as usize + 1]);
                let len = distance(a, b) as f64;
                let seg = &tree.segs[stroke.seg as usize];
                let along1 = stroke.along0 + if stroke.reversed { -len } else { len };
                let lo = stroke.along0.min(along1).max(0.0);
                let hi = stroke.along0.max(along1).min(seg.len);
                assert!(lo < hi, "stroke coverage: each mapping covers a positive interval");
                covered[stroke.run as usize] += hi - lo;
                segment_covered[stroke.seg as usize] += hi - lo;
                for (along, node) in [(0.0, seg.a), (seg.len, seg.b)] {
                    if along < lo || along > hi {
                        continue;
                    }
                    let offset = if stroke.reversed { stroke.along0 - along } else { along - stroke.along0 };
                    let at = [
                        a[0] as f64 + (i64::from(b[0]) - i64::from(a[0])).signum() as f64 * offset,
                        a[1] as f64 + (i64::from(b[1]) - i64::from(a[1])).signum() as f64 * offset,
                    ];
                    assert_eq!(at, node_point(node).map(f64::from), "arc geometry: stroke direction and phase reach the segment endpoint");
                }
            }
            let mut total = 0.0;
            for (i, run) in sheet.wires.iter().enumerate().filter(|(_, r)| r.net == tree.net) {
                let len: f64 = run.pts.windows(2).map(|w| distance(w[0], w[1]) as f64).sum();
                assert_eq!(covered[i], len, "run coverage: net {} run {i} is covered exactly once", tree.net);
                total += len;
            }
            assert_eq!(tree.segs.iter().map(|s| s.len).sum::<f64>(), total, "length conservation: pieces have the total wire length");
            for (i, seg) in tree.segs.iter().enumerate() {
                assert_eq!(segment_covered[i], seg.len, "segment coverage: strokes cover piece {i} exactly once");
            }
            assert_eq!(tree.segs.iter().map(|s| &s.key).collect::<BTreeSet<_>>().len(), tree.segs.len(),
                "key uniqueness: each segment has a distinct key within its net");
            assert!(tree.segs.windows(2).all(|w| w[0].key < w[1].key), "segment order: keys are sorted");
            assert!(tree.strokes.windows(2).all(|w| (w[0].run, w[0].index) <= (w[1].run, w[1].index)),
                "stroke order: runs precede their point order");
            for &port in &tree.ports {
                assert!(matches!(tree.nodes[port as usize], NodeKey::Point(_, _)), "port identity: island ports have no pin");
                let degree: usize = tree.segs.iter().map(|s| usize::from(s.a == port) + usize::from(s.b == port)).sum();
                assert_eq!(degree, 1, "port degree: interior points are never island ports");
            }
        }
    }

    #[test]
    fn keys_survive_a_relayout_elsewhere() {
        let design = fab_elec_fixtures::ledctl();
        let (edited, touched) = add_part(&design);
        let hints = SheetHints::default();
        let before = crate::layout(&design, 0, &hints, &Opts::default());
        let after = crate::layout(&edited, 0, &hints, &Opts::default());
        let before_graph = crate::adapt::graph_of(&design.units[0].circuit, &hints);
        let after_graph = crate::adapt::graph_of(&edited.units[0].circuit, &hints);
        let keys = |sheet: &SheetLayout, graph: &LayoutGraph| -> BTreeMap<u32, BTreeSet<SegKey>> {
            wire_trees(sheet, graph).into_iter().map(|tree| {
                (tree.net, tree.segs.into_iter().map(|seg| seg.key).collect())
            }).collect()
        };
        let before_keys = keys(&before, &before_graph);
        let after_keys = keys(&after, &after_graph);
        assert_eq!(after.parts.len(), before.parts.len() + 1, "edit: the fourth decoupling capacitor was added");
        let mut compared = 0;
        for (i, net) in before_graph.nets.iter().enumerate() {
            if net.pins.iter().any(|&(n, _)| before_graph.within(before_graph.nodes[n as usize].block, touched.0)) {
                continue;
            }
            assert_eq!(net.pins, after_graph.nets[i].pins, "edit isolation: outside net {i} keeps its pins");
            assert_eq!(before_keys.get(&(i as u32)), after_keys.get(&(i as u32)),
                "key stability: relayout inside mcu preserves outside net {i} ({})", net.name);
            if before_keys.get(&(i as u32)).is_some_and(|keys| !keys.is_empty()) {
                compared += 1;
            }
        }
        assert!(compared > 0, "key stability: at least one outside net has segments to compare");
    }
}
