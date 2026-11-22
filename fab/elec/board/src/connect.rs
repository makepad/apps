//! Board connectivity (C4, E1 4.1): which copper touches which, clusters, shorts, opens and the
//! ratsnest. First version by ELEC at M0; EROUTE owns it from here.
//!
//! Copper that touches on a shared layer (`clearance == 0`) is one conductor; a pad, via or
//! plated hole joins the layers it is on; a zone's fill conducts through its kept tile polygons
//! (`ZoneFill`, E4 delta note: the fills are an input). The polygons of one fill island are one
//! conductor by the fill's island labels, never by geometry, so polygons of one zone and layer are
//! not touch-tested against each other. A cluster with two nets is a short; a net in two clusters
//! is open, and the ratsnest joins its clusters by a minimum spanning tree over their anchor
//! points (pad and via centres, track ends, the first vertex of an island's representative
//! polygon: its smallest member).
//!
//! `connect_update` must equal `connect` (the test property, E4). The host passes the ids of
//! zones it refilled in the delta (`changed`), since fills are not board items. This version of
//! the update recomputes; EROUTE replaces it with the dirty-net form.
//!
//! Cost: O(n log n) in copper pieces for the touch pass (one R-tree per layer, exact tests on
//! candidates), O(p^2) per net in its anchor points for the ratsnest (EROUTE: nearest candidates
//! from the index, fine to 10 k pads).

use crate::geom::Copper;
use crate::index::{piece_clearance, IndexChange, PKey, Piece, PieceIndex};
use crate::ops::BoardDelta;
use crate::zone::{PolyRef, TileKey, ZoneFill};
use crate::{Board, ItemKind};
use fab_elec_core::{clearance, BBox, Index, ItemId, LayerId, NetId, Nm, PadIx, Shape, P};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::ControlFlow;

/// A piece of copper connectivity speaks of.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum ConnItem {
    Pad { placed: ItemId, pad: PadIx },
    Track { item: ItemId, seg: u32 },
    Via(ItemId),
    Hole(ItemId),
    /// Kept polygon `ord` of fill tile `tile` of a zone on `layer`.
    Zone { item: ItemId, layer: LayerId, tile: TileKey, ord: u32 },
}
fab_elec_core::canon_enum!(ConnItem {
    0 => Pad { placed, pad },
    1 => Track { item, seg },
    2 => Via(i),
    3 => Hole(i),
    4 => Zone { item, layer, tile, ord },
});


impl ConnItem {
    /// The board item the piece belongs to.
    pub fn item(self) -> ItemId {
        match self {
            ConnItem::Pad { placed, .. } => placed,
            ConnItem::Track { item, .. } | ConnItem::Zone { item, .. } => item,
            ConnItem::Via(i) | ConnItem::Hole(i) => i,
        }
    }
}

/// One conductor: every item in it touches the others through copper.
#[derive(Clone, PartialEq, Debug)]
pub struct Cluster {
    /// The distinct nets of its items, sorted (two or more: a short).
    pub nets: Vec<NetId>,
    /// Its pieces, sorted.
    pub items: Vec<ConnItem>,
}
fab_elec_core::canon_struct!(Cluster { nets, items });


/// A missing connection: two clusters of one net, at their nearest anchor points.
#[derive(Clone, PartialEq, Debug)]
pub struct RatLine {
    pub net: NetId,
    pub a: ConnItem,
    pub b: ConnItem,
    pub pa: P,
    pub pb: P,
}
fab_elec_core::canon_struct!(RatLine { net, a, b, pa, pb });


/// Copper of two or more nets in one cluster.
#[derive(Clone, PartialEq, Debug)]
pub struct Short {
    pub nets: Vec<NetId>,
    /// Index into `Connectivity::clusters`.
    pub cluster: u32,
    /// Where two of the nets meet. Of the cluster's touches it is, first, one that joins nets no short
    /// of the previous connectivity held together (what a change made); then the witness a short of
    /// the previous connectivity had (a short keeps it while it stands); then a touch of two nets
    /// before a netless piece that bridges them; then the smaller pieces and point. Whole-board
    /// `connect` has no previous connectivity. `None` when no touch could be named, which a cluster
    /// built from touches does not give.
    pub witness: Option<Witness>,
}
fab_elec_core::canon_struct!(Short { nets, cluster, witness });

/// Two touching pieces where a short's nets meet: pieces of two nets, or a piece of one net and a
/// netless piece that bridges it to another.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Witness {
    /// The two pieces, the smaller first.
    pub items: (ConnItem, ConnItem),
    /// Where they touch.
    pub at: P,
}
fab_elec_core::canon_struct!(Witness { items, at });


#[derive(Clone, PartialEq, Debug, Default)]
pub struct Connectivity {
    /// The board generation this was computed from (E4 A14: generation, not a hash).
    pub generation: u64,
    pub clusters: Vec<Cluster>,
    pub ratsnest: Vec<RatLine>,
    pub shorts: Vec<Short>,
    /// Nets in more than one cluster, sorted.
    pub unrouted: Vec<NetId>,
    /// Every join the clusters were made from, sorted: two pieces touching on a layer, and the joins a
    /// stale zone's polygons keep from before (`connect_held`). An update re-tests only copper that
    /// changed; the joins between unchanged pieces stand.
    pub joins: Vec<Join>,
    /// The zones whose polygons joined by their recorded joins (stale fills), sorted. Their joins say
    /// nothing about geometry: once current again their polygons are tested anew.
    pub held: Vec<ItemId>,
}
// Content only: `generation` names the board the result was computed from and is left out.
fab_elec_core::canon_struct!(Connectivity { clusters, ratsnest, shorts, unrouted, joins, held });

impl Connectivity {
    /// The joins made through a fill polygon: either end is a zone's polygon.
    pub fn fill_joins(&self) -> impl Iterator<Item = &Join> + '_ {
        self.joins.iter().filter(|j| matches!(j.a, ConnItem::Zone { .. }) || matches!(j.b, ConnItem::Zone { .. }))
    }
}

/// Two pieces that conduct into each other on `layer`, `a < b`: by touch, or a stale zone's recorded join.
/// A piece is its item and layer (one shape per item and copper layer).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Join {
    pub a: ConnItem,
    pub b: ConnItem,
    pub layer: LayerId,
}
fab_elec_core::canon_struct!(Join { a, b, layer });

impl Join {
    fn new(x: &Copper, y: &Copper) -> Join {
        Join { a: x.item.min(y.item), b: x.item.max(y.item), layer: x.layer }
    }
}


/// The copper pieces of the board and its fills.
pub fn pieces(board: &Board, fills: &[ZoneFill]) -> Vec<Copper> {
    let mut v = board.all_copper();
    v.extend(board.reached_copper(fills));
    for f in fills {
        let net = zone_net(board, f.zone).flatten();
        append_fill(&mut v, board, f, net);
    }
    v
}

// `None` when the zone is not on the board, else its net.
fn zone_net(board: &Board, zone: ItemId) -> Option<Option<NetId>> {
    match board.locate(zone)? {
        (ItemKind::Zone, i) => Some(board.zones[i].net),
        _ => None,
    }
}

fn append_fill(pcs: &mut Vec<Copper>, board: &Board, fill: &ZoneFill, net: Option<NetId>) {
    let class = crate::zone::class_of(board, net);
    for layer in &fill.layers {
        for (r, poly) in layer.kept() {
            pcs.push(Copper {
                item: ConnItem::Zone { item: fill.zone, layer: layer.layer, tile: r.tile, ord: r.ord },
                net,
                class,
                layer: layer.layer,
                shape: Shape::Poly(poly.clone()),
            });
        }
    }
}

// The island of a fill polygon, from the fills themselves.
fn fill_island(fills: &[ZoneFill], c: ConnItem) -> Option<PolyRef> {
    let ConnItem::Zone { item, layer, tile, ord } = c else { return None };
    let layer = fills.iter().find(|f| f.zone == item)?.layer(layer)?;
    let i = layer.tiles.binary_search_by_key(&tile, |t| t.key).ok()?;
    layer.labels[i].island.get(ord as usize).copied()
}

/// The conductor a piece belongs to without geometry: a track's segments are one, and so are
/// the polygons of one fill island (keyed by its representative). Every other piece is its own.
fn conductor(c: ConnItem, island: Option<PolyRef>) -> ConnItem {
    match (c, island) {
        (ConnItem::Track { item, .. }, _) => ConnItem::Track { item, seg: 0 },
        (ConnItem::Zone { item, layer, .. }, Some(r)) => ConnItem::Zone { item, layer, tile: r.tile, ord: r.ord },
        (c, _) => c,
    }
}

fn conductors(pcs: &[&Copper], island: impl Fn(ConnItem) -> Option<PolyRef>) -> Vec<ConnItem> {
    pcs.iter().map(|c| conductor(c.item, island(c.item))).collect()
}

fn find(parent: &mut [u32], mut i: u32) -> u32 {
    while parent[i as usize] != i {
        parent[i as usize] = parent[parent[i as usize] as usize];
        i = parent[i as usize];
    }
    i
}

fn union(parent: &mut [u32], a: u32, b: u32) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
        parent[hi as usize] = lo;
    }
}

/// An anchor point of a piece for the ratsnest.
fn anchor(board: &Board, c: &Copper) -> Vec<P> {
    match c.item {
        ConnItem::Pad { placed, pad } => board.placed_item(placed).and_then(|p| p.pad_at(pad)).into_iter().collect(),
        ConnItem::Via(_) | ConnItem::Hole(_) => match &c.shape {
            Shape::Circle { c, .. } => vec![*c],
            s => vec![s.bbox().center()],
        },
        ConnItem::Track { .. } => match &c.shape {
            Shape::Segment { a, b, .. } | Shape::Arc { a, b, .. } => vec![*a, *b],
            s => vec![s.bbox().center()],
        },
        ConnItem::Zone { .. } => match &c.shape {
            Shape::Poly(set) => set.polygons().first().and_then(|p| p.outer.first().copied()).into_iter().collect(),
            s => vec![s.bbox().center()],
        },
    }
}

/// Full connectivity of a board with its zone fills.
pub fn connect(board: &Board, fills: &[ZoneFill]) -> Connectivity {
    let pcs = pieces(board, fills);
    let pcs: Vec<&Copper> = pcs.iter().collect();
    let keys = conductors(&pcs, |c| fill_island(fills, c));
    let t = touch_pairs(&pcs);
    assemble(board, &pcs, &keys, t, Vec::new(), None)
}

// Copper that joins by geometry: on one layer, touching, and not two polygons of one zone's fill
// on that layer (those join by island labels alone).
fn touch(a: &Copper, b: &Copper) -> bool {
    a.layer == b.layer && !same_fill(a.item, b.item) && clearance(&a.shape, &b.shape) == 0
}

fn touch_pieces(pa: &Piece, a: &Copper, pb: &Piece, b: &Copper) -> bool {
    a.layer == b.layer && !same_fill(a.item, b.item) && piece_clearance(pa, a, pb, b) == 0
}

fn same_fill(a: ConnItem, b: ConnItem) -> bool {
    matches!((a, b), (ConnItem::Zone { item: x, layer: l, .. }, ConnItem::Zone { item: y, layer: m, .. }) if x == y && l == m)
}

fn touch_pairs(pcs: &[&Copper]) -> Vec<(u32, u32)> {
    let n = pcs.len();
    // touching pieces on each layer
    let mut layers: Vec<LayerId> = pcs.iter().map(|c| c.layer).collect();
    layers.sort();
    layers.dedup();
    let mut touch_pairs: Vec<(u32, u32)> = Vec::new();
    for l in layers {
        let on: Vec<(u32, BBox)> =
            (0..n as u32).filter(|i| pcs[*i as usize].layer == l).map(|i| (i, pcs[i as usize].shape.bbox())).collect();
        let index = Index::build(on.iter().map(|(i, b)| (*b, *i)).collect());
        for &(i, bbox) in &on {
            let a = &pcs[i as usize];
            index.query(bbox, |_, j| {
                if j > i && touch(a, &pcs[j as usize]) {
                    touch_pairs.push((i, j));
                }
                ControlFlow::Continue(())
            });
        }
    }
    // Keep pair lists in a consistent order for every caller.
    touch_pairs.sort();
    touch_pairs
}

// The joins of piece pairs, sorted.
fn joins_of(pcs: &[&Copper], pairs: &[(u32, u32)]) -> Vec<Join> {
    let mut joins: Vec<Join> = pairs.iter().map(|&(i, j)| Join::new(pcs[i as usize], pcs[j as usize])).collect();
    joins.sort_unstable();
    joins.dedup();
    joins
}

fn cluster_pieces(pcs: &[&Copper], keys: &[ConnItem], pairs: &[(u32, u32)]) -> (Vec<Cluster>, Vec<u32>) {
    let n = pcs.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    // one conductor per key (a pad on several layers, a via, a track's segments, a fill island)
    let mut by_item: Vec<(ConnItem, u32)> = keys.iter().enumerate().map(|(i, k)| (*k, i as u32)).collect();
    by_item.sort();
    for w in by_item.windows(2) {
        if w[0].0 == w[1].0 {
            union(&mut parent, w[0].1, w[1].1);
        }
    }
    for (i, j) in pairs {
        union(&mut parent, *i, *j);
    }
    // clusters, sorted by their smallest item
    let mut groups: Vec<(u32, Vec<u32>)> = Vec::new();
    let mut root_group: Vec<Option<usize>> = vec![None; n];
    for i in 0..n as u32 {
        let r = find(&mut parent, i) as usize;
        let g = *root_group[r].get_or_insert_with(|| {
            groups.push((r as u32, Vec::new()));
            groups.len() - 1
        });
        groups[g].1.push(i);
    }
    let mut clusters: Vec<(Cluster, Vec<u32>)> = groups
        .into_iter()
        .map(|(_, members)| {
            let mut items: Vec<ConnItem> = members.iter().map(|i| pcs[*i as usize].item).collect();
            items.sort();
            items.dedup();
            let mut nets: Vec<NetId> = members.iter().filter_map(|i| pcs[*i as usize].net).collect();
            nets.sort();
            nets.dedup();
            (Cluster { nets, items }, members)
        })
        .collect();
    clusters.sort_by(|a, b| a.0.items.cmp(&b.0.items));
    let mut cluster_of = vec![0u32; n];
    for (ci, (_, members)) in clusters.iter().enumerate() {
        for m in members {
            cluster_of[*m as usize] = ci as u32;
        }
    }
    (clusters.into_iter().map(|(c, _)| c).collect(), cluster_of)
}

// The shorts of `clusters`, each with its witness (`Short::witness`, ranked by `prev`). A cluster
// joins its nets through `pairs` and same-key pieces, and a key's pieces share a net, so a path between
// two of its nets has two touching pieces of different nets or a netless group touching both: every
// short has a witness. O(pairs + prev's short nets), plus a sort of the netless pieces that touch a
// shorted cluster's copper.
fn short_witnesses(
    clusters: &[Cluster],
    pcs: &[&Copper],
    keys: &[ConnItem],
    pairs: &[(u32, u32)],
    cluster_of: &[u32],
    prev: Option<&Connectivity>,
) -> Vec<Short> {
    let shorted = |i: u32| clusters[cluster_of[i as usize] as usize].nets.len() >= 2;
    // The shorts of `prev` that hold each net, and their witnesses.
    let mut held_by: HashMap<NetId, Vec<u32>> = HashMap::new();
    let mut was: Vec<(ConnItem, ConnItem)> = Vec::new();
    for (k, s) in prev.map_or(&[][..], |p| &p.shorts[..]).iter().enumerate() {
        for &net in &s.nets {
            held_by.entry(net).or_default().push(k as u32);
        }
        was.extend(s.witness.map(|w| w.items));
    }
    was.sort_unstable();
    let apart = |a: NetId, b: NetId| {
        prev.is_some()
            && match (held_by.get(&a), held_by.get(&b)) {
                (Some(x), Some(y)) => !x.iter().any(|k| y.binary_search(k).is_ok()),
                _ => true,
            }
    };
    type Rank = (bool, bool, bool, ConnItem, ConnItem, Nm, Nm);
    let mut best: Vec<Option<(Rank, P)>> = vec![None; clusters.len()];
    let mut offer = |ci: u32, new: bool, bridge: bool, a: &Copper, b: &Copper| {
        // Canonical ends also fix the fallback point for a held join whose boxes are now apart.
        let (a, b) = if a.item <= b.item { (a, b) } else { (b, a) };
        let at = a.shape.bbox().intersection_center(&b.shape.bbox());
        let kept = was.binary_search(&(a.item, b.item)).is_ok();
        let rank = (!new, !kept, bridge, a.item, b.item, at.x, at.y);
        let slot = &mut best[ci as usize];
        if slot.as_ref().map_or(true, |(r, _)| rank < *r) {
            *slot = Some((rank, at));
        }
    };
    // (netless piece, piece of a net) touches, and netless touches, in shorted clusters
    let mut edges: Vec<(u32, u32)> = Vec::new();
    let mut netless: Vec<(u32, u32)> = Vec::new();
    for &(i, j) in pairs {
        if !shorted(i) {
            continue;
        }
        let (a, b) = (pcs[i as usize], pcs[j as usize]);
        match (a.net, b.net) {
            (Some(x), Some(y)) if x != y => offer(cluster_of[i as usize], apart(x, y), false, a, b),
            (Some(_), None) => edges.push((j, i)),
            (None, Some(_)) => edges.push((i, j)),
            (None, None) => netless.push((i, j)),
            _ => {}
        }
    }
    if !edges.is_empty() {
        // Netless pieces in groups, joined by touch and by conductor; a group touching two nets
        // bridges them.
        let mut nodes: Vec<u32> =
            edges.iter().map(|e| e.0).chain(netless.iter().flat_map(|&(i, j)| [i, j])).collect();
        nodes.sort_unstable();
        nodes.dedup();
        let pos = |i: u32| nodes.binary_search(&i).unwrap_or_default() as u32;
        let mut parent: Vec<u32> = (0..nodes.len() as u32).collect();
        for &(i, j) in &netless {
            union(&mut parent, pos(i), pos(j));
        }
        let mut by_key: Vec<(ConnItem, u32)> = nodes.iter().enumerate().map(|(k, &i)| (keys[i as usize], k as u32)).collect();
        by_key.sort_unstable();
        for w in by_key.windows(2) {
            if w[0].0 == w[1].0 {
                union(&mut parent, w[0].1, w[1].1);
            }
        }
        let mut group_nets: Vec<(u32, NetId)> =
            edges.iter().filter_map(|&(n, a)| Some((find(&mut parent, pos(n)), pcs[a as usize].net?))).collect();
        group_nets.sort_unstable();
        group_nets.dedup();
        for &(n, a) in &edges {
            let (group, Some(net)) = (find(&mut parent, pos(n)), pcs[a as usize].net) else { continue };
            let from = group_nets.partition_point(|g| g.0 < group);
            let run = &group_nets[from..group_nets.partition_point(|g| g.0 <= group)];
            if run.len() >= 2 {
                let new = run.iter().any(|&(_, other)| other != net && apart(net, other));
                offer(cluster_of[n as usize], new, true, pcs[a as usize], pcs[n as usize]);
            }
        }
    }
    let mut shorts = Vec::new();
    for (ci, c) in clusters.iter().enumerate() {
        if c.nets.len() < 2 {
            continue;
        }
        let witness = best[ci].map(|((.., x, y, _, _), at)| Witness { items: (x, y), at });
        shorts.push(Short { nets: c.nets.clone(), cluster: ci as u32, witness });
    }
    shorts
}

// Prim over one net's clusters by nearest anchor points, with deterministic tie breaking.
/// One net's ratsnest: the spanning tree over its clusters, each joining the tree by its nearest
/// anchor pair (`pts`: cluster, anchor, piece). Prim over anchors: `near[b]` is the nearest tree
/// anchor of anchor `b` outside the tree, so a joining cluster's anchors are scanned against the
/// rest once and the whole tree costs O(A^2) for A anchors (O(A) per join to pick the next).
/// Ties go to the smallest (squared distance, tree anchor, outside anchor) in sorted order.
fn net_ratsnest(net: NetId, pts: &mut Vec<(u32, P, ConnItem)>) -> Vec<RatLine> {
    pts.sort_by(|a, b| (a.0, a.2, a.1).cmp(&(b.0, b.2, b.1)));
    pts.dedup();
    let mut ids: Vec<u32> = pts.iter().map(|p| p.0).collect();
    ids.dedup();
    let mut ratsnest = Vec::new();
    if ids.len() < 2 {
        return ratsnest;
    }
    let mut in_tree = vec![false; pts.len()];
    let mut near: Vec<Option<(i128, usize)>> = vec![None; pts.len()];
    let mut joined = ids[0];
    for _ in 1..ids.len() {
        // `pts` is sorted by cluster: the joining cluster's anchors are one run.
        let run = pts.partition_point(|p| p.0 < joined)..pts.partition_point(|p| p.0 <= joined);
        for a in run.clone() {
            in_tree[a] = true;
        }
        for a in run {
            let pa = pts[a].1;
            for (b, pb) in pts.iter().enumerate() {
                if in_tree[b] {
                    continue;
                }
                let d = pa.dist2(pb.1);
                if near[b].map_or(true, |best| (d, a) < best) {
                    near[b] = Some((d, a));
                }
            }
        }
        let mut pick: Option<(i128, usize, usize)> = None;
        for (b, n) in near.iter().enumerate() {
            if let (false, Some((d, a))) = (in_tree[b], *n) {
                if pick.map_or(true, |best| (d, a, b) < best) {
                    pick = Some((d, a, b));
                }
            }
        }
        let Some((_, a, b)) = pick else { break };
        ratsnest.push(RatLine { net, a: pts[a].2, b: pts[b].2, pa: pts[a].1, pb: pts[b].1 });
        joined = pts[b].0;
    }
    ratsnest
}

// A fill polygon anchors the ratsnest only as its island's representative.
fn anchors_ratsnest(c: ConnItem, key: ConnItem) -> bool {
    !matches!(c, ConnItem::Zone { .. }) || c == key
}

fn assemble(
    board: &Board,
    pcs: &[&Copper],
    keys: &[ConnItem],
    touch_pairs: Vec<(u32, u32)>,
    held: Vec<ItemId>,
    prev: Option<&Connectivity>,
) -> Connectivity {
    let joins = joins_of(pcs, &touch_pairs);
    let (clusters, cluster_of) = cluster_pieces(pcs, keys, &touch_pairs);
    let shorts = short_witnesses(&clusters, pcs, keys, &touch_pairs, &cluster_of, prev);
    let mut ratsnest = Vec::new();
    let mut unrouted = Vec::new();
    let mut per_net: Vec<Vec<(u32, P, ConnItem)>> = vec![Vec::new(); board.nets.len()];
    for (i, c) in pcs.iter().enumerate() {
        if !anchors_ratsnest(c.item, keys[i]) {
            continue;
        }
        if let Some(net) = c.net {
            if let Some(list) = per_net.get_mut(net.ix()) {
                for p in anchor(board, c) {
                    list.push((cluster_of[i], p, c.item));
                }
            }
        }
    }
    for (ni, pts) in per_net.iter_mut().enumerate() {
        let net = NetId(ni as u32);
        let lines = net_ratsnest(net, pts);
        if !lines.is_empty() {
            unrouted.push(net);
        }
        ratsnest.extend(lines);
    }
    Connectivity { generation: board.generation, clusters, ratsnest, shorts, unrouted, joins, held }
}

/// Connectivity after `delta` from `prev` (must equal `connect(board, fills)`).
pub fn connect_update(prev: &Connectivity, board: &Board, fills: &[ZoneFill], delta: &BoardDelta) -> Connectivity {
    if delta.is_empty() && prev.generation == board.generation {
        return prev.clone();
    }
    connect(board, fills)
}

/// Connectivity while the fills of `stale` zones are stale (gate 3: no join or split through a stale
/// fill): copper joins follow `board`; current zones' polygons join by geometry; each polygon of a
/// stale zone joins exactly the pieces it joined in `prev` (its `joins`) that still exist: a changed
/// item keeps its join, an added item joins no polygon, a removed one drops out. Islands of every
/// fill, stale or not, stay one conductor by their labels. `fills` holds the stale zones' previous
/// fills. With no stale zone it equals `connect` but for the shorts' witnesses, which `prev` ranks
/// (`Short::witness`). The full form (O(n)) the indexed update is proven equal to.
pub fn connect_held(board: &Board, fills: &[ZoneFill], prev: &Connectivity, stale: &[ItemId]) -> Connectivity {
    // A fill whose zone is no longer on the board has no islands, so none of its joins come back.
    let mut pcs = board.all_copper();
    pcs.extend(board.reached_copper(fills));
    for f in fills {
        if let Some(net) = zone_net(board, f.zone) {
            append_fill(&mut pcs, board, f, net);
        }
    }
    let pcs: Vec<&Copper> = pcs.iter().collect();
    let keys = conductors(&pcs, |c| fill_island(fills, c));
    held(board, &pcs, &keys, prev, stale)
}

fn is_stale(c: ConnItem, stale: &BTreeSet<ItemId>) -> bool {
    matches!(c, ConnItem::Zone { item, .. } if stale.contains(&item))
}

fn sorted_ids(ids: &[ItemId]) -> Vec<ItemId> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn held(board: &Board, pcs: &[&Copper], keys: &[ConnItem], prev: &Connectivity, stale: &[ItemId]) -> Connectivity {
    let mut pairs = touch_pairs(pcs);
    let held = sorted_ids(stale);
    if held.is_empty() {
        return assemble(board, pcs, keys, pairs, held, Some(prev));
    }
    let stale = held.iter().copied().collect();
    pairs.retain(|&(i, j)| !is_stale(pcs[i as usize].item, &stale) && !is_stale(pcs[j as usize].item, &stale));
    pairs.extend(recorded_pairs(pcs, prev, &stale));
    pairs.sort();
    pairs.dedup();
    assemble(board, pcs, keys, pairs, held, Some(prev))
}

fn recorded_pairs(pcs: &[&Copper], prev: &Connectivity, stale: &BTreeSet<ItemId>) -> Vec<(u32, u32)> {
    let mut pairs = Vec::new();
    if stale.is_empty() {
        return pairs;
    }
    // A recorded piece maps to the same piece (item and layer), else (a track whose segments changed) to
    // the first piece of its item; a piece of a removed item, or a fill polygon no longer present, maps to
    // nothing. Both board copper and indexed copper list each item's pieces in rank order.
    let mut by_piece: HashMap<(ConnItem, LayerId), u32> = HashMap::new();
    let mut by_item: HashMap<ItemId, u32> = HashMap::new();
    for (i, c) in pcs.iter().enumerate() {
        by_piece.entry((c.item, c.layer)).or_insert(i as u32);
        if !matches!(c.item, ConnItem::Zone { .. }) {
            by_item.entry(c.item.item()).or_insert(i as u32);
        }
    }
    let find = |c: ConnItem, layer: LayerId| match c {
        ConnItem::Zone { .. } => by_piece.get(&(c, layer)).copied(),
        _ => by_piece.get(&(c, layer)).or_else(|| by_item.get(&c.item())).copied(),
    };
    for join in &prev.joins {
        if !is_stale(join.a, stale) && !is_stale(join.b, stale) {
            continue;
        }
        if let (Some(i), Some(j)) = (find(join.a, join.layer), find(join.b, join.layer)) {
            if i != j {
                pairs.push((i.min(j), i.max(j)));
            }
        }
    }
    pairs
}

// Two sorted lists as one sorted, repeat-free list.
fn sorted_union<T: Ord + Copy>(a: &[T], b: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    out.sort_unstable();
    out.dedup();
    out
}

// Two sorted, repeat-free join lists as one.
fn merge_joins(a: Vec<Join>, b: Vec<Join>) -> Vec<Join> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut a, mut b) = (a.into_iter().peekable(), b.into_iter().peekable());
    loop {
        let next = match (a.peek(), b.peek()) {
            (Some(x), Some(y)) if x < y => a.next(),
            (Some(x), Some(y)) if y < x => b.next(),
            (Some(_), Some(_)) => {
                b.next();
                a.next()
            }
            (Some(_), None) => a.next(),
            (None, Some(_)) => b.next(),
            (None, None) => break,
        };
        out.extend(next);
    }
    out
}

/// Connectivity after `delta`, from `prev` (the connectivity of the board before it, under the
/// same held rule), `index` (already updated to `board` and the current fills), `change` (what that
/// update re-derived) and `since` (what the index re-derived while the zones held in `prev` were held:
/// the absorbed changes of the updates since those zones were last current, `change` among them or
/// not; `IndexChange::all()` when unknown). Equals `connect_held(board, fills, prev, stale)`: stale
/// islands join only by their recorded joins, while all other copper joins by touch on a shared layer.
///
/// Only fresh copper is tested: the pieces of changed items and the polygons of changed tiles. A zone
/// held in `prev` and current now (released) joined by its recorded joins, which are geometry for
/// copper that did not change while it was held: its own polygons kept their tiles unless the refill
/// replaced them (`change`). So the copper `since` names is fresh too, and the released zones' other
/// polygons keep their joins; after a rebuild while held, released zones are fresh whole. A cluster is
/// affected when it held a fresh piece, when a fresh piece touches it, or when a changed or relabeled
/// tile's polygon names one of its pieces as island representative (so every polygon that shares the
/// island comes along). The affected clusters are joined again by union-find over their pieces' island
/// labels, the joins of `prev` between unchanged pieces, the touches of the fresh pieces and the stale
/// zones' recorded joins.
///
/// Costs: O(n log n) cheap passes over prev's lists, without geometry; indexed geometry for the fresh
/// pieces only; O(k) for the k pieces of affected clusters. Only nets in two or more clusters repeat the
/// per-net Prim ratsnest pass over their anchors. A global delta runs the full held form over the index.
pub fn connect_indexed(
    prev: &Connectivity,
    index: &PieceIndex,
    board: &Board,
    delta: &BoardDelta,
    change: &IndexChange,
    stale: &[ItemId],
    since: &IndexChange,
) -> Connectivity {
    if delta.global || change.rebuilt {
        let pcs: Vec<&Copper> = index.iter().filter_map(|p| p.copper()).collect();
        let keys = conductors(&pcs, |c| index.island(c));
        return held(board, &pcs, &keys, prev, stale);
    }
    let held = sorted_ids(stale);
    let stale: BTreeSet<ItemId> = held.iter().copied().collect();
    // A zone held in `prev` and current now. Its recorded joins are geometry except with copper that
    // changed while it was held, which is fresh with this update's changes.
    let released: Vec<ItemId> = prev.held.iter().copied().filter(|zone| !stale.contains(zone)).collect();
    let late = !released.is_empty() && !since.rebuilt;
    let whole: &[ItemId] = if since.rebuilt { &released } else { &[] };
    let items: Cow<[ItemId]> =
        if late { Cow::Owned(sorted_union(&change.items, &since.items)) } else { Cow::Borrowed(&change.items) };
    let tiles: Cow<[(ItemId, LayerId, TileKey)]> =
        if late { Cow::Owned(sorted_union(&change.tiles, &since.tiles)) } else { Cow::Borrowed(&change.tiles) };
    let fresh = |c: ConnItem| match c {
        ConnItem::Zone { item, layer, tile, .. } => {
            whole.binary_search(&item).is_ok() || tiles.binary_search(&(item, layer, tile)).is_ok()
        }
        c => items.binary_search(&c.item()).is_ok(),
    };

    let mut prev_cluster = BTreeMap::new();
    let mut item_clusters: BTreeMap<ItemId, Vec<usize>> = BTreeMap::new();
    for (ci, cluster) in prev.clusters.iter().enumerate() {
        for &c in &cluster.items {
            prev_cluster.insert(c, ci);
            let ids = item_clusters.entry(c.item()).or_default();
            if ids.last().copied() != Some(ci) {
                ids.push(ci);
            }
        }
    }

    // Old clusters of the changed pieces.
    let mut affected = vec![false; prev.clusters.len()];
    for id in items.iter().chain(whole) {
        for &ci in item_clusters.get(id).into_iter().flatten() {
            affected[ci] = true;
        }
    }
    for &(item, layer, tile) in tiles.iter() {
        let lo = ConnItem::Zone { item, layer, tile, ord: 0 };
        let hi = ConnItem::Zone { item, layer, tile, ord: u32::MAX };
        for (_, &ci) in prev_cluster.range(lo..=hi) {
            affected[ci] = true;
        }
    }
    // A polygon of a changed or relabeled tile may have changed island. Its new island's other members
    // either moved label too (their tiles are listed) or kept it, and then they were in the old island of
    // the same representative: its old cluster has them all.
    for &(item, layer, tile) in tiles.iter().chain(&change.relabeled) {
        for c in index.tile_pieces(item, layer, tile).iter().filter_map(Piece::copper) {
            for key in [c.item, conductor(c.item, index.island(c.item))] {
                if let Some(&ci) = prev_cluster.get(&key) {
                    affected[ci] = true;
                }
            }
        }
    }

    // The fresh pieces, and the old clusters they touch.
    let mut fresh_pieces: Vec<(&Piece, &Copper)> = Vec::new();
    for id in items.iter() {
        fresh_pieces.extend(index.pieces(*id).filter_map(|p| Some((p, p.copper()?))));
    }
    for &(item, layer, tile) in tiles.iter() {
        fresh_pieces.extend(index.tile_pieces(item, layer, tile).iter().filter_map(|p| Some((p, p.copper()?))));
    }
    for zone in whole {
        fresh_pieces.extend(index.pieces(*zone).filter_map(|p| Some((p, p.copper()?))));
    }
    for &(p, a) in &fresh_pieces {
        if is_stale(a.item, &stale) {
            continue;
        }
        index.query(a.layer, p.bbox, |q| {
            let Some(b) = q.copper() else { return ControlFlow::Continue(()) };
            if !is_stale(b.item, &stale) {
                if let Some(&ci) = prev_cluster.get(&b.item) {
                    if !affected[ci] && touch_pieces(p, a, q, b) {
                        affected[ci] = true;
                    }
                }
            }
            ControlFlow::Continue(())
        });
    }

    // Closure: a piece outside the selection touches only unchanged pieces of its own old cluster (a
    // touch of a fresh piece would have affected that cluster) and shares an island only with polygons
    // of that cluster. Recorded stale joins stay within their previous clusters, which come in full.
    let mut affected_nets: BTreeSet<_> = delta.nets.iter().copied().collect();
    let mut selected: BTreeMap<PKey, (&Piece, &Copper)> =
        fresh_pieces.iter().map(|&(p, c)| (p.key(), (p, c))).collect();
    let mut gone: Vec<ConnItem> = Vec::new();
    for (ci, cluster) in prev.clusters.iter().enumerate() {
        if !affected[ci] {
            continue;
        }
        affected_nets.extend(cluster.nets.iter().copied());
        gone.extend_from_slice(&cluster.items);
        for &c in cluster.items.iter().filter(|c| !fresh(**c)) {
            for p in index.copper_of(c) {
                if let Some(copper) = p.copper() {
                    selected.insert(p.key(), (p, copper));
                }
            }
        }
    }
    let selected: Vec<(&Piece, &Copper)> = selected.into_values().collect();
    let pcs: Vec<&Copper> = selected.iter().map(|&(_, c)| c).collect();
    let at: HashMap<(ConnItem, LayerId), u32> =
        pcs.iter().enumerate().map(|(i, c)| ((c.item, c.layer), i as u32)).collect();
    let is_fresh: Vec<bool> = pcs.iter().map(|c| fresh(c.item)).collect();
    let live: Vec<bool> = pcs.iter().map(|c| !is_stale(c.item, &stale)).collect();
    let mut pairs = Vec::new();
    // Joins between unchanged current pieces stand: neither side's copper moved, and neither was held.
    for (i, a) in pcs.iter().enumerate() {
        if is_fresh[i] || !live[i] {
            continue;
        }
        let from = prev.joins.partition_point(|j| j.a < a.item);
        for join in prev.joins[from..].iter().take_while(|j| j.a == a.item) {
            if join.layer != a.layer {
                continue;
            }
            if let Some(&k) = at.get(&(join.b, join.layer)) {
                if !is_fresh[k as usize] && live[k as usize] {
                    pairs.push(((i as u32).min(k), (i as u32).max(k)));
                }
            }
        }
    }
    // Touches of the fresh pieces, each found once from a fresh end.
    for (i, &(p, a)) in selected.iter().enumerate() {
        if !is_fresh[i] || !live[i] {
            continue;
        }
        index.query(a.layer, p.bbox, |q| {
            let Some(b) = q.copper() else { return ControlFlow::Continue(()) };
            if let Some(&k) = at.get(&(b.item, b.layer)) {
                let k = k as usize;
                if k != i && live[k] && (!is_fresh[k] || i < k) && touch_pieces(p, a, q, b) {
                    pairs.push((i.min(k) as u32, i.max(k) as u32));
                }
            }
            ControlFlow::Continue(())
        });
    }
    pairs.extend(recorded_pairs(&pcs, prev, &stale));
    pairs.sort_unstable();
    pairs.dedup();

    let keys = conductors(&pcs, |c| index.island(c));
    let (new_clusters, cluster_of) = cluster_pieces(&pcs, &keys, &pairs);
    let new_shorts = short_witnesses(&new_clusters, &pcs, &keys, &pairs, &cluster_of, Some(prev));
    for cluster in &new_clusters {
        affected_nets.extend(cluster.nets.iter().copied());
    }
    let old_count = prev.clusters.len();
    let mut renumbered = vec![0u32; old_count + new_clusters.len()];
    let mut clusters: Vec<_> = prev
        .clusters
        .iter()
        .enumerate()
        .filter(|(ci, _)| !affected[*ci])
        .map(|(ci, c)| (c.clone(), ci))
        .collect();
    clusters.extend(new_clusters.into_iter().enumerate().map(|(ci, c)| (c, old_count + ci)));
    clusters.sort_by(|a, b| a.0.items.cmp(&b.0.items));
    for (ci, (_, source)) in clusters.iter().enumerate() {
        renumbered[*source] = ci as u32;
    }
    let mut shorts: Vec<_> = prev
        .shorts
        .iter()
        .filter(|s| !affected[s.cluster as usize])
        .cloned()
        .map(|mut s| {
            s.cluster = renumbered[s.cluster as usize];
            s
        })
        .collect();
    shorts.extend(new_shorts.into_iter().map(|mut s| {
        s.cluster = renumbered[old_count + s.cluster as usize];
        s
    }));
    shorts.sort_by_key(|s| s.cluster);
    let clusters: Vec<_> = clusters.into_iter().map(|(c, _)| c).collect();

    // The joins of unaffected clusters stand; a join's ends share a cluster, so its first end decides.
    gone.sort_unstable();
    let mut g = 0;
    let mut kept = Vec::with_capacity(prev.joins.len());
    for join in &prev.joins {
        while g < gone.len() && gone[g] < join.a {
            g += 1;
        }
        if gone.get(g) != Some(&join.a) {
            kept.push(*join);
        }
    }
    let joins = merge_joins(kept, joins_of(&pcs, &pairs));

    // A net in fewer than two clusters has no ratsnest lines: only the others gather anchors.
    let mut seen: BTreeMap<NetId, usize> = BTreeMap::new();
    for cluster in &clusters {
        for net in cluster.nets.iter().filter(|net| affected_nets.contains(net)) {
            *seen.entry(*net).or_default() += 1;
        }
    }
    let mut per_net: BTreeMap<NetId, Vec<(u32, P, ConnItem)>> = seen
        .into_iter()
        .filter(|&(net, count)| count >= 2 && net.ix() < board.nets.len())
        .map(|(net, _)| (net, Vec::new()))
        .collect();
    for (ci, cluster) in clusters.iter().enumerate() {
        if !cluster.nets.iter().any(|net| per_net.contains_key(net)) {
            continue;
        }
        for &c in &cluster.items {
            if !anchors_ratsnest(c, conductor(c, index.island(c))) {
                continue;
            }
            for copper in index.copper_of(c).filter_map(|p| p.copper()) {
                if let Some(pts) = copper.net.and_then(|net| per_net.get_mut(&net)) {
                    pts.extend(anchor(board, copper).into_iter().map(|p| (ci as u32, p, c)));
                }
            }
        }
    }
    let mut ratsnest: Vec<_> = prev
        .ratsnest
        .iter()
        .filter(|line| !affected_nets.contains(&line.net))
        .cloned()
        .collect();
    let mut unrouted: Vec<_> = prev.unrouted.iter().copied().filter(|net| !affected_nets.contains(net)).collect();
    for (net, mut pts) in per_net {
        let lines = net_ratsnest(net, &mut pts);
        if !lines.is_empty() {
            unrouted.push(net);
        }
        ratsnest.extend(lines);
    }
    ratsnest.sort_by_key(|line| line.net);
    unrouted.sort();
    Connectivity { generation: board.generation, clusters, ratsnest, shorts, unrouted, joins, held }
}

trait BoxCentre {
    fn intersection_center(&self, o: &BBox) -> P;
}

impl BoxCentre for BBox {
    /// The centre of the overlap of two boxes (of the first box when they are apart).
    fn intersection_center(&self, o: &BBox) -> P {
        let b = BBox {
            min: P::new(self.min.x.max(o.min.x), self.min.y.max(o.min.y)),
            max: P::new(self.max.x.min(o.max.x), self.max.y.min(o.max.y)),
        };
        if b.min.x <= b.max.x && b.min.y <= b.max.y { b.center() } else { self.center() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ratsnest before the dense Prim: every join rescans all tree/outside anchor pairs.
    fn all_pairs_prim(net: NetId, pts: &mut Vec<(u32, P, ConnItem)>) -> Vec<RatLine> {
        pts.sort_by(|a, b| (a.0, a.2, a.1).cmp(&(b.0, b.2, b.1)));
        pts.dedup();
        let mut ids: Vec<u32> = pts.iter().map(|p| p.0).collect();
        ids.dedup();
        let mut ratsnest = Vec::new();
        if ids.len() < 2 {
            return ratsnest;
        }
        let mut in_tree = vec![false; pts.len()];
        let mut tree_clusters = vec![ids[0]];
        for (k, p) in pts.iter().enumerate() {
            in_tree[k] = p.0 == ids[0];
        }
        while tree_clusters.len() < ids.len() {
            let mut best: Option<(i128, usize, usize)> = None;
            for (a, pa) in pts.iter().enumerate() {
                if !in_tree[a] {
                    continue;
                }
                for (b, pb) in pts.iter().enumerate() {
                    if in_tree[b] {
                        continue;
                    }
                    let d = pa.1.dist2(pb.1);
                    if best.map_or(true, |(bd, _, _)| d < bd) {
                        best = Some((d, a, b));
                    }
                }
            }
            let Some((_, a, b)) = best else { break };
            let joined = pts[b].0;
            ratsnest.push(RatLine { net, a: pts[a].2, b: pts[b].2, pa: pts[a].1, pb: pts[b].1 });
            tree_clusters.push(joined);
            for (k, p) in pts.iter().enumerate() {
                if p.0 == joined {
                    in_tree[k] = true;
                }
            }
        }
        ratsnest
    }

    /// The dense Prim gives the all-pairs Prim's lines, in its order, ties included: anchors on a
    /// coarse grid make many equal distances, and clusters share anchor positions.
    #[test]
    fn ratsnest_equals_the_all_pairs_prim() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |m: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m as u64) as u32
        };
        for (clusters, anchors, grid) in [(2u32, 4u32, 3u32), (8, 40, 3), (20, 120, 6), (40, 160, 12)] {
            let mut pts: Vec<(u32, P, ConnItem)> = (0..anchors)
                .map(|i| {
                    let cluster = if i < clusters { i } else { next(clusters) };
                    let at = P::new(next(grid) as i32 * 1_000, next(grid) as i32 * 1_000);
                    (cluster, at, ConnItem::Via(ItemId(i)))
                })
                .collect();
            let mut reference = pts.clone();
            let lines = net_ratsnest(NetId(1), &mut pts);
            assert_eq!(lines.len(), clusters as usize - 1, "a spanning tree over {clusters} clusters");
            assert_eq!(
                lines,
                all_pairs_prim(NetId(1), &mut reference),
                "the same lines as the all-pairs Prim for {clusters} clusters and {anchors} anchors"
            );
        }
    }
}
