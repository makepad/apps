//! Board centreline graphs for the live current solver. Copper contacts are ideal nodes;
//! tracks retain resistance, including when connectivity puts an entire track in one cluster.
//! Sorting and index construction are O(pieces log pieces); spatial searches visit candidates
//! only, with the core index's overlap-dependent cost and `PolySet`'s containment cost.

use crate::live::net::NetTree;
use fab_elec_board::zone::PolyRef;
use fab_elec_board::{Anchor, Board, ConnItem, Track, ZoneFill};
use fab_elec_core::{BBox, Index, ItemId, LayerId, LayerSet, NetId, Nm, PadIx, PolySet, P};
use std::collections::BTreeMap;
use std::ops::ControlFlow;

/// A stable id of one wire piece: the track, its path segment, and the piece of that segment
/// when a T-junction splits it (0 when unsplit). The three integer fields key the host's rows;
/// neither board-vector positions nor graph-node numbers enter the key.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct WireId {
    pub track: ItemId,
    pub seg: u32,
    pub piece: u32,
}

/// One net's wire graph and what its parts are on the board.
#[derive(Clone, Debug)]
pub struct NetWires {
    pub net: NetId,
    pub tree: NetTree,
    /// `tree.segs[i]` is wire `wires[i]` (same length as `tree.segs`).
    pub wires: Vec<WireId>,
    /// `tree.pins[j]` is pad `pads[j]`; its pin-current index is `j`.
    pub pads: Vec<(ItemId, PadIx)>,
    /// Where node `k` is on the board, in nm. A merged zone uses a representative point.
    pub at: Vec<P>,
}

#[derive(Default)]
struct Input<'a> {
    tracks: BTreeMap<ItemId, &'a Track>,
    contacts: BTreeMap<ConnItem, (P, LayerSet)>,
    fills: BTreeMap<ItemId, Vec<(LayerId, PolyRef, &'a PolySet)>>,
}

/// Every net's wire graph, in net order; nets with no tracks are left out.
///
/// Ports are empty: there is no board evidence for an external current source at an unrouted
/// end. `NetCurrents` solves each connected island alone and removes its current imbalance
/// evenly from its pins. A zone island is an ideal conductor, not a solver port.
/// Exact contacts require a shared copper layer; a pad or via has one node across its layers.
/// Arcs use their chords, including T projections. Taper widths interpolate at each split,
/// then each piece uses the mean of its two vertex widths divided by its chord length.
pub fn net_wires(board: &Board, fills: &[ZoneFill]) -> Vec<NetWires> {
    let mut inputs: BTreeMap<NetId, Input<'_>> = BTreeMap::new();
    let holes: BTreeMap<_, _> = board.holes.iter().map(|h| (h.id, h.at)).collect();
    for track in &board.tracks {
        let input = inputs.entry(track.net).or_default();
        input.tracks.insert(track.id, track);
        for anchor in track.ends {
            if let Anchor::Hole(id) = anchor {
                if let Some(&at) = holes.get(&id) {
                    input.contacts.insert(ConnItem::Hole(id), (at, LayerSet::EMPTY));
                }
            }
        }
    }
    let copper = board.copper_set();
    let copper_count = board.copper_count();
    // Index the same positions as Board::anchor_pos once, avoiding its per-anchor item scans.
    for placed in &board.placed {
        for (i, net) in placed.pad_nets.iter().enumerate() {
            let Some(input) = net.and_then(|net| inputs.get_mut(&net)) else { continue };
            let pad = PadIx(i as u16);
            if let Some(at) = placed.pad_at(pad) {
                input.contacts.insert(
                    ConnItem::Pad { placed: placed.id, pad },
                    (at, placed.pad_layers(pad, copper, copper_count)),
                );
            }
        }
    }
    for via in &board.vias {
        if let Some(input) = inputs.get_mut(&via.net) {
            input.contacts.insert(ConnItem::Via(via.id), (via.at, via.span_layers(copper)));
        }
    }
    let zones: BTreeMap<_, _> = board.zones.iter().map(|zone| (zone.id, zone.net)).collect();
    for fill in fills {
        let Some(input) = zones.get(&fill.zone).copied().flatten().and_then(|net| inputs.get_mut(&net)) else { continue };
        let polys = input.fills.entry(fill.zone).or_default();
        for layer in &fill.layers {
            let islands = layer.labels.iter().flat_map(|labels| {
                labels.island.iter().zip(&labels.kept)
                    .filter_map(|(&island, &kept)| kept.then_some(island))
            });
            polys.extend(layer.kept().zip(islands).map(|((_, poly), island)| (layer.layer, island, poly)));
        }
    }
    inputs.into_iter().map(|(net, input)| build(net, input)).collect()
}

#[derive(Default)]
struct Points {
    at: Vec<P>,
    parent: Vec<usize>,
    size: Vec<usize>,
    used: Vec<bool>,
}

impl Points {
    fn add(&mut self, at: P, used: bool) -> usize {
        let node = self.at.len();
        self.at.push(at);
        self.parent.push(node);
        self.size.push(1);
        self.used.push(used);
        node
    }

    fn root(&mut self, mut node: usize) -> usize {
        while self.parent[node] != node {
            self.parent[node] = self.parent[self.parent[node]];
            node = self.parent[node];
        }
        node
    }

    fn join(&mut self, a: usize, b: usize) {
        let (mut a, mut b) = (self.root(a), self.root(b));
        if a == b {
            return;
        }
        if self.size[a] < self.size[b] {
            std::mem::swap(&mut a, &mut b);
        }
        let size = self.size[a] + self.size[b];
        let used = self.used[a] || self.used[b];
        self.parent[b] = a;
        self.size[a] = size;
        self.used[a] = used;
    }
}

#[derive(Clone, Copy)]
struct Cut {
    t: f64,
    node: usize,
}

struct TrackPath<'a> {
    track: &'a Track,
    nodes: Vec<usize>,
    chords: Vec<(P, P)>,
    index: Index<usize>,
    cuts: Vec<Vec<Cut>>,
}

fn anchor_item(anchor: Anchor) -> Option<ConnItem> {
    match anchor {
        Anchor::Pad { placed, pad } => Some(ConnItem::Pad { placed, pad }),
        Anchor::Via(id) => Some(ConnItem::Via(id)),
        Anchor::Hole(id) => Some(ConnItem::Hole(id)),
        Anchor::Free | Anchor::Track { .. } => None,
    }
}

/// Fraction, rounded board point and unrounded squared distance to a chord.
fn project(p: P, a: P, b: P) -> (f64, P, f64) {
    let (dx, dy) = (b.x as f64 - a.x as f64, b.y as f64 - a.y as f64);
    let (px, py) = (p.x as f64 - a.x as f64, p.y as f64 - a.y as f64);
    let length2 = dx * dx + dy * dy;
    let t = if length2 == 0.0 { 0.0 } else { ((px * dx + py * dy) / length2).clamp(0.0, 1.0) };
    let (ex, ey) = (px - t * dx, py - t * dy);
    (t, P::from_f(a.x as f64 + t * dx, a.y as f64 + t * dy), ex * ex + ey * ey)
}

fn nearest(path: &TrackPath<'_>, p: P) -> Option<(usize, f64, P)> {
    if path.chords.is_empty() {
        return None;
    }
    let initial = path.index.nearest(p, Nm::MAX, |_, seg| {
        let (a, b) = path.chords[seg];
        Some(project(p, a, b).2.sqrt().ceil() as Nm)
    }).map(|(seg, _)| seg).unwrap_or(0);
    let mut seg = initial;
    let mut best = project(p, path.chords[seg].0, path.chords[seg].1);
    // Refine the index's integer distance and break ties by this track's segment index.
    // The fallback also covers the full i32 coordinate range, beyond Nm::MAX distance.
    let r = best.2.sqrt().ceil();
    let area = BBox::new(P::from_f(p.x as f64 - r, p.y as f64 - r), P::from_f(p.x as f64 + r, p.y as f64 + r));
    path.index.query(area, |_, candidate| {
        let q = project(p, path.chords[candidate].0, path.chords[candidate].1);
        if q.2 < best.2 || (q.2 == best.2 && candidate < seg) {
            seg = candidate;
            best = q;
        }
        ControlFlow::Continue(())
    });
    Some((seg, best.0, best.1))
}

fn exact_contact(exact: &mut BTreeMap<(LayerId, P), usize>, points: &mut Points, layer: LayerId, node: usize) {
    if let Some(&other) = exact.get(&(layer, points.at[node])) {
        points.join(other, node);
    } else {
        exact.insert((layer, points.at[node]), node);
    }
}

fn build(net: NetId, input: Input<'_>) -> NetWires {
    let mut points = Points::default();
    let mut contacts = BTreeMap::new();
    let mut exact = BTreeMap::new();
    let mut centres: BTreeMap<LayerId, Vec<(BBox, usize)>> = BTreeMap::new();
    for (&item, &(at, layers)) in &input.contacts {
        let node = points.add(at, false);
        contacts.insert(item, node);
        // Holes participate through explicit anchors only; zone membership is pads and vias.
        if matches!(item, ConnItem::Pad { .. } | ConnItem::Via(_)) {
            for layer in layers.iter() {
                exact_contact(&mut exact, &mut points, layer, node);
                centres.entry(layer).or_default().push((BBox::point(at), node));
            }
        }
    }
    let mut paths = BTreeMap::new();
    for (&id, &track) in &input.tracks {
        let mut at: Vec<_> = std::iter::once(track.path.start).chain(track.path.segs.iter().map(|seg| seg.end())).collect();
        let last = at.len() - 1;
        let anchors = track.ends.map(|anchor| anchor_item(anchor).and_then(|item| contacts.get(&item).copied()));
        for (i, anchor) in [(0, anchors[0]), (last, anchors[1])] {
            if let Some(node) = anchor {
                at[i] = points.at[node];
            }
        }
        let nodes: Vec<_> = at.iter().map(|&p| points.add(p, !track.path.segs.is_empty())).collect();
        for (i, anchor) in [(0, anchors[0]), (last, anchors[1])] {
            if let Some(node) = anchor {
                points.join(node, nodes[i]);
            }
        }
        let chords: Vec<_> = at.windows(2).map(|ends| (ends[0], ends[1])).collect();
        let index = Index::build(chords.iter().enumerate().map(|(seg, &(a, b))| (BBox::new(a, b), seg)).collect());
        let cuts = nodes.windows(2).map(|ends| vec![Cut { t: 0.0, node: ends[0] }, Cut { t: 1.0, node: ends[1] }]).collect();
        paths.insert(id, TrackPath { track, nodes, chords, index, cuts });
    }
    // Project against immutable chords so resolving a T cannot depend on board-vector order.
    let mut junctions = Vec::new();
    for path in paths.values() {
        for (end, anchor) in path.track.ends.iter().enumerate() {
            let Anchor::Track { id: target, .. } = anchor else { continue };
            let Some(other) = paths.get(target) else { continue };
            if other.track.layer != path.track.layer {
                continue;
            }
            let node = path.nodes[if end == 0 { 0 } else { path.nodes.len() - 1 }];
            if let Some((seg, t, at)) = nearest(other, points.at[node]) {
                junctions.push((*target, seg, t, at, node));
            }
        }
    }
    for (target, seg, t, at, node) in junctions {
        points.at[node] = at;
        paths.get_mut(&target).expect("indexed T target").cuts[seg].push(Cut { t, node });
    }
    for path in paths.values() {
        for node in [path.nodes[0], path.nodes[path.nodes.len() - 1]] {
            exact_contact(&mut exact, &mut points, path.track.layer, node);
        }
    }
    let centres: BTreeMap<_, _> = centres.into_iter().map(|(layer, entries)| (layer, Index::build(entries))).collect();
    for polys in input.fills.into_values() {
        // Labels are local to a zone. Canonical tile order visits each representative first.
        let mut islands = BTreeMap::new();
        for (layer, island, poly) in polys {
            let node = *islands.entry((layer, island))
                .or_insert_with(|| points.add(poly.polygons()[0].outer[0], true));
            if let Some(index) = centres.get(&layer) {
                index.query(poly.bbox(), |_, contact| {
                    if poly.contains(points.at[contact]) {
                        points.join(node, contact);
                    }
                    ControlFlow::Continue(())
                });
            }
        }
    }
    let mut wires = Vec::new();
    let mut edges = Vec::new();
    let mut conductance = Vec::new();
    for path in paths.values_mut() {
        for (seg, cuts) in path.cuts.iter_mut().enumerate() {
            cuts.sort_by(|a, b| a.t.total_cmp(&b.t));
            let mut unique: Vec<Cut> = Vec::with_capacity(cuts.len());
            for cut in cuts.iter().copied() {
                if let Some(last) = unique.last() {
                    if points.at[last.node] == points.at[cut.node] {
                        points.join(last.node, cut.node);
                        continue;
                    }
                }
                unique.push(cut);
            }
            // Even a zero-length authored segment owns one stable row.
            if unique.len() == 1 {
                unique.push(unique[0]);
            }
            let width = |vertex: usize| path.track.widths.as_ref().and_then(|ws| ws.get(vertex)).copied().unwrap_or(path.track.w) as f64;
            let (wa, wb) = (width(seg), width(seg + 1));
            for (piece, ends) in unique.windows(2).enumerate() {
                let (a, b) = (ends[0], ends[1]);
                let length = (points.at[a.node].dist2(points.at[b.node]) as f64).sqrt();
                let mean_width = wa + (wb - wa) * (a.t + b.t) * 0.5;
                wires.push(WireId { track: path.track.id, seg: seg as u32, piece: piece as u32 });
                edges.push((a.node, b.node));
                conductance.push(if length == 0.0 { 1e6 } else { (mean_width / length) as f32 });
            }
        }
    }
    let mut at = Vec::new();
    let mut numbers = vec![None; points.at.len()];
    let mut nodes = vec![None; points.at.len()];
    for (i, node) in nodes.iter_mut().enumerate() {
        let root = points.root(i);
        if points.used[root] {
            *node = Some(*numbers[root].get_or_insert_with(|| {
                let number = at.len() as u32;
                at.push(points.at[root]);
                number
            }));
        }
    }
    let segs = edges.into_iter().map(|(a, b)| (nodes[a].expect("wire node"), nodes[b].expect("wire node"))).collect();
    let mut pins = Vec::new();
    let mut pads = Vec::new();
    for (item, node) in contacts {
        if let (ConnItem::Pad { placed, pad }, Some(node)) = (item, nodes[node]) {
            pins.push((node, pads.len() as u32));
            pads.push((placed, pad));
        }
    }
    // rows: the host's segment-row allocator assigns them from `wires` (keyed by the board's copper
    // and each WireId); empty until it has.
    NetWires { net, tree: NetTree { nodes: at.len() as u32, segs, rows: Vec::new(), pins, ports: Vec::new(), conductance }, wires, pads, at }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::net::NetCurrents;
    use fab_elec_board::zone::{FillLattice, FillLayer, FillTile, TileKey, TileLabels};
    use fab_elec_board::{Area, BoardNet, Flash, Hole, Origin, Placed, Tent, Via, ViaKind, Zone, ZoneSettings};
    use fab_elec_core::{mm, Angle, BoardId, ClassId, CompId, Path, Seg, Side};
    use fab_elec_parts::{Footprint, Pad, PadKind, PadProp, PadShape, PadStack, PasteRule};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    #[test]
    fn fixture_graphs_keep_ids_across_an_unrelated_edit() {
        let design = fab_elec_fixtures::ledctl();
        let mut board = design.units[0].board.as_deref().expect("fixture board").clone();
        let net = board.tracks.iter().map(|track| track.net).min().expect("fixture tracks");
        let other = board.tracks.iter().position(|track| track.net != net).expect("another routed net");
        let track = &mut board.tracks[other];
        let middle = P::from_f((track.path.start.x as f64 + track.path.end().x as f64) * 0.5,
            (track.path.start.y as f64 + track.path.end().y as f64) * 0.5);
        track.path.segs.insert(0, Seg::Line(middle));
        let before = net_wires(&board, &[]);
        assert!(before.windows(2).all(|nets| nets[0].net < nets[1].net), "graphs are in net order");
        for graph in &before {
            assert_eq!(graph.wires.len(), graph.tree.segs.len(), "one id per wire");
            assert_eq!(graph.tree.conductance.len(), graph.wires.len(), "one conductance per wire");
            assert_eq!(graph.at.len(), graph.tree.nodes as usize, "one position per node");
            assert!(graph.tree.segs.iter().all(|&(a, b)| a < graph.tree.nodes && b < graph.tree.nodes), "wire endpoints are nodes");
            assert_eq!(graph.pads.len(), graph.tree.pins.len(), "one pad per pin");
            for (j, &(node, pin)) in graph.tree.pins.iter().enumerate() {
                assert_eq!(pin as usize, j, "pin-current indices follow pads");
                assert!(graph.tree.segs.iter().any(|&(a, b)| node == a || node == b), "without fills, pins must touch a wire");
            }
        }
        let ids = |graphs: &[NetWires]| graphs.iter().find(|g| g.net == net).expect("routed net").wires.iter().copied().collect::<BTreeSet<_>>();
        let mut edited = board.clone();
        edited.tracks[other].path.segs[0] = Seg::Line(middle.add(P::mm(0.1, 0.2)));
        edited.tracks.reverse();
        assert_eq!(ids(&before), ids(&net_wires(&edited, &[])), "an unrelated edit and track reordering preserve wire ids");
    }

    fn t_board() -> (Board, ItemId, ItemId, ItemId) {
        let mut board = Board::new(BoardId(0), "wire flow");
        board.nets.push(BoardNet { name: "signal".into(), class: ClassId(0) });
        let placed = board.alloc_id();
        let pads = [P::mm(10.0, 0.0), P::mm(5.0, 5.0)].into_iter().enumerate().map(|(i, at)| Pad {
            no: (i + 1).to_string(), kind: PadKind::Smd, stack: PadStack::smd(PadShape::Circle { d: mm(0.5) }),
            at, rot: Angle::ZERO, drill: None, mask_margin: None, paste: PasteRule::default(),
            thermal: None, zone_connect: None, die_len: 0, prop: PadProp::None,
        }).collect();
        board.placed.push(Placed {
            id: placed, comp: CompId(0), path: "part".into(), refdes: "J1".into(),
            footprint: Arc::new(Footprint { pads, ..Footprint::default() }), pad_nets: vec![Some(NetId(0)); 2],
            at: P::ZERO, rot: Angle::ZERO, side: Side::Top, locked: false, origin: Origin::Authored, body_z: None, ref_label: None, value_label: None, legend: None, value: None,
        });
        let t1 = board.alloc_id();
        let t2 = board.alloc_id();
        board.tracks.push(Track {
            id: t1, net: NetId(0), layer: LayerId::F_CU, w: mm(0.25), widths: None,
            path: Path::new(P::ZERO).line(P::mm(10.0, 0.0)),
            ends: [Anchor::Free, Anchor::Pad { placed, pad: PadIx(0) }], locked: false, origin: Origin::Authored,
        });
        board.tracks.push(Track {
            id: t2, net: NetId(0), layer: LayerId::F_CU, w: mm(0.25), widths: None,
            path: Path::new(P::mm(5.0, 0.0)).line(P::mm(5.0, 5.0)),
            ends: [board.track_anchor(t1, P::mm(5.0, 0.0)).expect("junction host"), Anchor::Free], locked: false, origin: Origin::Authored,
        });
        (board, placed, t1, t2)
    }

    #[test]
    fn t_junction_splits_the_trunk_and_routes_pin_current() {
        let (mut board, placed, t1, t2) = t_board();
        let graph = net_wires(&board, &[]).pop().expect("T net");
        let ids = [WireId { track: t1, seg: 0, piece: 0 }, WireId { track: t1, seg: 0, piece: 1 }, WireId { track: t2, seg: 0, piece: 0 }];
        assert_eq!(graph.wires, ids, "T splits produce ordered, track-local ids");
        let junction = graph.tree.segs[0].1;
        assert_eq!(graph.tree.segs[1].0, junction, "trunk pieces share the T node");
        assert_eq!(graph.tree.segs[2].0, junction, "branch starts at the T node");
        assert_eq!(graph.at[junction as usize], P::mm(5.0, 0.0), "junction position");
        assert_eq!(graph.pads, [(placed, PadIx(0)), (placed, PadIx(1))], "anchored and free-end pads are both pins");
        assert!(graph.tree.ports.is_empty(), "unrouted ends are not current ports");
        let mut currents = Vec::new();
        NetCurrents::new().currents(&graph.tree, &[1.0, -1.0], &mut currents);
        for (i, expected) in [0.0, 1.0, 1.0].into_iter().enumerate() {
            assert!((currents[i].abs() - expected).abs() < 1e-6, "wire {:?}: expected magnitude {expected}, got {}", ids[i], currents[i]);
            assert!((graph.tree.conductance[i] - 0.05).abs() < 1e-7, "width / piece length");
        }

        // Several T ends, including a repeated split and a target endpoint, must not create
        // extra zero-length pieces. Insert out of geometric order and project an off-track end.
        for x in [7.5, 2.5, 5.0, 10.0] {
            let id = board.alloc_id();
            let mut branch = board.tracks[1].clone();
            branch.id = id;
            branch.path = Path::new(P::mm(x, -0.1)).line(P::mm(x, -2.0));
            board.tracks.push(branch);
        }
        board.tracks[0].widths = Some(vec![mm(0.2), mm(0.6)]);
        // One arc is one chord, and an authored zero-length segment still has a row.
        let extra = board.alloc_id();
        let mut arc = board.tracks[0].clone();
        arc.id = extra;
        arc.path = Path::new(P::mm(20.0, 0.0)).arc(P::mm(22.0, 3.0), P::mm(24.0, 0.0)).line(P::mm(24.0, 0.0));
        arc.widths = Some(vec![mm(0.2), mm(0.6), mm(0.6)]);
        arc.ends = [Anchor::Free; 2];
        board.tracks.push(arc);
        let split = net_wires(&board, &[]).pop().expect("split net");
        let trunk: Vec<_> = split.wires.iter().enumerate().filter(|(_, wire)| wire.track == t1).collect();
        assert_eq!(trunk.len(), 4, "four distinct intervals despite repeated and endpoint T anchors");
        for (piece, &(i, wire)) in trunk.iter().enumerate() {
            assert_eq!(wire.piece as usize, piece, "piece ids follow distance along the trunk");
            let (a, b) = split.tree.segs[i];
            assert_eq!(split.at[a as usize], P::mm(2.5 * piece as f64, 0.0), "projected piece start");
            assert_eq!(split.at[b as usize], P::mm(2.5 * (piece + 1) as f64, 0.0), "projected piece end");
            let expected = (0.25 + 0.1 * piece as f32) / 2.5;
            assert!((split.tree.conductance[i] - expected).abs() < 1e-7, "taper interpolates at both split vertices");
        }
        let arc_index = split.wires.iter().position(|wire| wire.track == extra).expect("arc wire");
        assert!((split.tree.conductance[arc_index] - 0.1).abs() < 1e-7, "arc conductance uses its chord and mean width");
        assert_eq!(split.tree.conductance[arc_index + 1], 1e6, "zero-length conductance");
        assert_eq!(split.tree.segs[arc_index + 1].0, split.tree.segs[arc_index + 1].1, "zero-length endpoints merge");
        board.tracks.reverse();
        assert_eq!(split.wires, net_wires(&board, &[])[0].wires, "T ids survive track-vector reordering");
    }

    #[test]
    fn zone_islands_respect_holes_and_layers_and_vias_bridge_layers() {
        let (mut board, placed, _, _) = t_board();
        // Make two isolated one-pin wires and put the branch on the back. Coincident free
        // ends on opposite layers must stay separate until a through via is present.
        board.tracks[0].path.start = P::mm(5.0, 0.0);
        board.tracks[1].ends[0] = Anchor::Free;
        board.tracks[1].layer = LayerId::B_CU;
        Arc::make_mut(&mut board.placed[0].footprint).pads[1].stack.copper = LayerSet::one(LayerId::B_CU);
        let isolated = net_wires(&board, &[]).pop().expect("isolated net");
        assert_ne!(isolated.tree.segs[0].0, isolated.tree.segs[1].0, "opposite-layer endpoints do not touch");
        let mut currents = Vec::new();
        NetCurrents::new().currents(&isolated.tree, &[1.0, -1.0], &mut currents);
        assert_eq!(currents, [0.0, 0.0], "unrouted components solve alone without ports");

        let via = board.alloc_id();
        board.vias.push(Via {
            id: via, net: NetId(0), at: P::mm(5.0, 0.0), drill: mm(0.3), pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU), kind: ViaKind::Through, tent: Tent::Both,
            backdrill: None, locked: false, origin: Origin::Authored,
            flash: Flash::All,
        });
        board.tracks[0].ends[0] = Anchor::Via(via);
        let bridged = net_wires(&board, &[]).pop().expect("bridged net");
        assert_eq!(bridged.tree.segs[0].0, bridged.tree.segs[1].0, "explicit and free ends share the via across layers");
        NetCurrents::new().currents(&bridged.tree, &[1.0, -1.0], &mut currents);
        assert!(currents.iter().all(|i| (i.abs() - 1.0).abs() < 1e-6), "the via carries current between layers");

        let hole = board.alloc_id();
        board.holes.push(Hole {
            id: hole, name: "plated".into(), at: P::mm(5.0, 0.0), drill: mm(0.3), pad: Some(mm(0.6)),
            net: Some(NetId(0)), plated: true, origin: Origin::Authored,
        });
        board.vias.clear();
        for track in &mut board.tracks {
            track.ends[0] = Anchor::Hole(hole);
        }
        let holed = net_wires(&board, &[]).pop().expect("hole net");
        assert_eq!(holed.tree.segs[0].0, holed.tree.segs[1].0, "hole anchors share an item node across layers");

        // Add untracked pads: one in each fill island, one in a hole, one on the wrong layer,
        // and one on another net. Only the first two become additional pins.
        let fp = Arc::make_mut(&mut board.placed[0].footprint);
        for at in [P::mm(9.0, 1.0), P::mm(20.0, 0.0), P::mm(8.0, 0.0), P::mm(9.0, 0.5), P::mm(9.0, -0.5)] {
            let mut pad = fp.pads[0].clone();
            pad.no = (fp.pads.len() + 1).to_string();
            pad.at = at;
            fp.pads.push(pad);
        }
        fp.pads[5].stack.copper = LayerSet::one(LayerId::B_CU);
        board.nets.push(BoardNet { name: "unrouted".into(), class: ClassId(0) });
        board.placed[0].pad_nets.extend([Some(NetId(0)); 4]);
        board.placed[0].pad_nets.push(Some(NetId(1)));
        let zone = board.alloc_id();
        board.zones.push(Zone {
            id: zone, net: Some(NetId(0)), name: "plane".into(), layers: LayerSet::one(LayerId::F_CU),
            area: Area::Board, priority: 0, settings: ZoneSettings::default(), locked: false, origin: Origin::Authored,
        });
        let rect = |x0, y0, x1, y1| PolySet::rect(BBox::new(P::mm(x0, y0), P::mm(x1, y1)));
        let plane = rect(7.0, -2.0, 11.0, 2.0).difference(&rect(7.5, -0.5, 8.5, 0.5))
            .union(&rect(19.0, -1.0, 21.0, 1.0));
        let lattice = FillLattice { level: 14 };
        let mut layer = FillLayer { layer: LayerId::F_CU, tiles: Vec::new(), labels: Vec::new(), reached: Vec::new() };
        for key in lattice.tiles_in(plane.bbox()) {
            let polys = plane.intersection(&PolySet::rect(lattice.tile_box(key))).split();
            // Each island spans two rows, with its representative in the lower row.
            let island = PolyRef { tile: TileKey { x: key.x, y: -1 }, ord: 0 };
            layer.labels.push(TileLabels {
                island: vec![island; polys.len()],
                kept: vec![true; polys.len()],
                view_hash: Default::default(),
            });
            layer.tiles.push(Arc::new(FillTile {
                key,
                own: vec![true; polys.len()],
                node: (0..polys.len()).map(|ord| ord as u32).collect(),
                group: (0..polys.len()).map(|ord| ord as u32).collect(),
                foreign: vec![false; polys.len()],
                core_cuts: Default::default(), lone: Default::default(),
                polys,
                cuts: Default::default(),
                hash: Default::default(),
            }));
        }
        let graphs = net_wires(&board, &[ZoneFill { zone, lattice, layers: vec![layer] }]);
        assert_eq!(graphs.len(), 1, "nets without tracks are omitted");
        let graph = &graphs[0];
        assert_eq!(graph.pads, (0..4).map(|pad| (placed, PadIx(pad))).collect::<Vec<_>>(), "holes, foreign nets and absent layers do not add pins");
        assert_eq!(graph.tree.pins[0].0, graph.tree.pins[2].0, "pads within one island merge");
        let island_node = graph.tree.pins[3].0;
        assert_ne!(graph.tree.pins[0].0, island_node, "separate polygons remain separate island nodes");
        assert!(!graph.tree.segs.iter().any(|&(a, b)| a == island_node || b == island_node), "a zone-only pad is still a pin");
        assert!(rect(19.0, -1.0, 21.0, 1.0).contains(graph.at[island_node as usize]), "the zone node has a representative board position");
    }
}
