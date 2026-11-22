//! A deterministic spanning forest through the settled start form's pour fills.
//!
//! Each path joins two hard copper groups of one net within one start-form cluster. Its segments
//! pass `PourTest::reach`, including track keepouts and the drills its net's tracks must avoid. Paths cannot join
//! groups the fill did not already join; the cluster identity is also debug-asserted before acceptance.
//! The caller supplies the board, rules, fills and connectivity of the same settled form, and the
//! fixed-only form: the same board and fills without the copper that moves in the relax.
//!
//! The search core follows the fill's own connectivity, its minimum-width islands (the conductors
//! `connect` counts). Islands touch only at necks narrower than the zone's minimum width `m`, and
//! the fill is cut there (`cut_at_necks`), so the cut is outside for the core and the test alike.
//! With `e = ARC_ERR`, `r0 = m / 2 - 2 e - 2 * MARGIN_NM` is just under the fill's inscribed half
//! width, so the core `F.offset(-r0, e)` minus the forbidden set offset by `r0` is connected
//! wherever an island is, except a pinch no disc of `r0` passes, and ends `r0` short of every cut:
//! no core joins two islands. A passage inside one island that is narrower than `m` but at least
//! `2 * r0` wide stays in its core, since the island conducts around it: `r0` is the search's
//! radius, not a definition of connection. Offset round joins have inscribed chords, so the core's
//! boundary stays at least `r0 - e` from both; `reach` tests that radius. The path's width
//! `w` and margin do not enter the search. They decide what moves: with `r = w / 2 + 2 * MARGIN_NM`,
//! a piece (at most `PIECE_NM`) is free when it passes `legal` (clear by `r`) on the fixed-only
//! fill and its exclusions, else pinned: fixed copper or a mask bounds it and no path of width `w`
//! fits there, now or after the relax. Shape polygonization is circumscribed; growing exclusions also
//! adds `e` to cover the offset's inscribed chords, as the zone filler does. Every graph edge is
//! tested after construction, and any failed edge is dropped and counted. Horizontal bridge hits
//! are computed as exact rationals, then rounded toward the source (into the core) by less than 1 nm;
//! the split edges and bridge all undergo that same test. All graph decisions and costs are integer.
//!
//! Cost: per zone layer, union, offset and difference of the fill and exclusions, O(g log g) geometry
//! work in their vertices/intersections, plus the two PolyGrid indices. A component with n ring
//! vertices and h holes has O(n + h) graph size. A y sweep indexes hole bridges in O(n log n + h log h
//! + sum(k)) time and O(n + h) memory, where k counts edges crossing each hole's row. A nearest-first
//! vertex grid costs O(V) time and memory per component. Each terminal costs the cells and vertices
//! searched, a heap push per vertex found (and a pop per vertex tried), and one `reach` per vertex
//! tried. Usually it searches a few cells; the worst case is still O(T * V * (log V + test)) for
//! T terminals, without a cap.
//! Graph-edge validation takes O(E * test). Dijkstra takes O(E log V) time and O(V + E) memory;
//! the free test is built once per zone layer that has a path (one more fill complement and
//! exclusion index), and each accepted path costs O(length / PIECE_NM * test) to classify;
//! K candidates take O(K log K) sorting and
//! O(K alpha(G)) Kruskal time for G groups. Taut pulling takes O(path points^2) probes per accepted
//! path, most testing one segment-to-segment distance against a cached witness. Without a witness
//! hit, it visits the cells near the segment and stops at the first blocking edge.
//! Total memory is the geometry and indices plus O(G + V + E + K + output points), over the
//! zone layers. This runs once per relax job, off the UI thread; there are no budgets or count caps.
//! Hard-group construction additionally takes O((pieces + joins) log(pieces)) time and O(pieces)
//! memory for ordered maps and unions. Zone clearance and minimum width already enter through F;
//! the remaining exclusion clearances are 1 nm + adapter margin for track keepouts, and the hole
//! rule + adapter margin for drills other than the pour net's own plated ones (the relaxer lets a
//! net's tracks over those). A via drill is present only within its span.

use fab_elec_board::zone::{FillLayer, TileKey};
use fab_elec_board::{Board, ConnItem, Connectivity, Copper, RuleMatrix, Zone, ZoneFill, ARC_ERR};
use fab_elec_core::{point_clearance, BBox, Hit, ItemId, LayerId, NetId, Nm, PolyGrid, PolySet, Shape, P};
use fab_elec_parts::Forbid;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};

/// The adapter's clearance margin (1,020 nm), shared by name so the two guards cannot drift.
pub const MARGIN_NM: Nm = super::adapt::MARGIN_NM;

/// Inputs besides the settled start form.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PourParams {
    /// Margin added to each side of a pour path's width; initially 20 µm.
    pub side_margin: Nm,
}

impl Default for PourParams {
    fn default() -> Self {
        Self { side_margin: 4 * ARC_ERR }
    }
}

/// A polyline of the pour's net inside its start-form fill, joining two hard groups.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PourPath {
    pub zone: ItemId,
    pub net: NetId,
    pub layer: LayerId,
    /// Zone minimum width plus twice the side margin: the width the relax keeps clear.
    pub width: Nm,
    /// The route through the search core, pulled taut: at least two points, from a piece of the
    /// first group to a piece of the second. It does not depend on what is fixed.
    pub route: Vec<P>,
    /// `route` split where its classification changes (see `pinned`).
    pub points: Vec<P>,
    /// Per segment (`points.len() - 1`): pinned, where no path of `width` fits against the
    /// fixed-only fill (fixed copper or a mask bounds it); else free to move.
    pub pinned: Vec<bool>,
    /// Indices into `PourForest::groups`, smaller first.
    pub joins: (u32, u32),
}

/// Connections protected by paths and the start-form joins still without protection.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PourForest {
    /// Sorted `ConnItem` pieces (pad indices, track segments, vias and plated-hole copper), with
    /// no zone polygons. Groups are ordered by net then members. A track's segments and a piece's
    /// layers are one conductor, as in `connect`; same-net non-zone joins unite those conductors.
    pub groups: Vec<Vec<ConnItem>>,
    pub paths: Vec<PourPath>,
    /// Sum of (hard groups - 1) in each pour net's filled start cluster, minus accepted joins.
    /// A path cannot join different start clusters; `forest` debug-asserts this invariant.
    pub unprotected: u32,
    /// Graph edges rejected by the exact shared test after construction; normally zero.
    pub dropped_edges: u32,
    /// Length of the pinned segments, in nm.
    pub pinned_nm: u64,
    /// The search test of each zone layer with a path, for `classify`.
    search: BTreeMap<(ItemId, LayerId), PourTest>,
}

impl PourForest {
    /// Classify every path's route against the fixed-only form `fixed` (a board and its fills:
    /// the start form without the copper that moves): `points`, `pinned` and `pinned_nm` follow.
    /// Without a fixed-only form every stretch is pinned. The routes stay; a caller that fixes
    /// more copper (a hold round) classifies again.
    pub fn classify(&mut self, rules: &RuleMatrix, fixed: Option<(&Board, &[ZoneFill])>, params: &PourParams) {
        let mut free_tests = BTreeMap::new();
        self.pinned_nm = 0;
        for path in &mut self.paths {
            let search = &self.search[&(path.zone, path.layer)];
            let free = free_tests.entry((path.zone, path.layer)).or_insert_with(|| {
                let (fixed_board, fixed_fills) = fixed?;
                let zone = fixed_board.zones.iter().find(|z| z.id == path.zone)?;
                let set = fixed_fills.iter().find(|f| f.zone == path.zone).and_then(|f| f.layer(path.layer))
                    .map_or_else(PolySet::default, |l| l.set());
                make_test(fixed_board, rules, &set, zone, path.layer, params)
            });
            (path.points, path.pinned) = stretches(&path.route, search, free.as_ref());
            self.pinned_nm += path.points.windows(2).zip(&path.pinned).filter(|(_, &p)| p).map(|(w, _)| length(w[0], w[1])).sum::<u64>();
        }
    }
}

/// The two exact polygon-grid tests for one zone layer and one path width.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PourTest {
    layer: LayerId,
    width: Nm,
    radius: Nm,
    /// The search core's erosion `r0`, just under the fill's own half width.
    erosion: Nm,
    bounds: BBox,
    outside: PolyGrid,
    forbidden: PolyGrid,
}

/// Blocking edges reused only while taut scans from one start point.
#[derive(Default)]
struct Witness {
    outside: Option<u32>,
    forbidden: Option<u32>,
}

impl PourTest {
    /// True only on this test's layer, within its enclosing box, and clear of both exclusions.
    /// This is shared by graph edges, terminal attachments, taut shortcuts and final checks.
    pub fn legal(&self, layer: LayerId, a: P, b: P) -> bool {
        self.clear(layer, a, b, self.radius)
    }

    /// The search's test: clear by `r0 - ARC_ERR`, which every edge of the search core passes.
    /// It picks a path's route only; whether a stretch moves is decided by `legal` on the
    /// fixed-only fill.
    pub fn reach(&self, layer: LayerId, a: P, b: P) -> bool {
        self.clear(layer, a, b, self.erosion - ARC_ERR)
    }

    fn clear(&self, layer: LayerId, a: P, b: P, radius: Nm) -> bool {
        let segment = Shape::Segment { a, b, w: 0 };
        layer == self.layer && self.bounds.contains(a) && self.bounds.contains(b)
            && !self.outside.collides(&segment, radius)
            && !self.forbidden.collides(&segment, radius)
    }

    /// Reach for taut's graph-path endpoints, already known to be outside both exclusion sets.
    fn reach_from(&self, witness: &mut Witness, layer: LayerId, a: P, b: P) -> bool {
        if layer != self.layer || !self.bounds.contains(a) || !self.bounds.contains(b) {
            return false;
        }
        let segment = Shape::Segment { a, b, w: 0 };
        let radius = self.erosion - ARC_ERR;
        if witness.outside.is_some_and(|id| self.outside.edge_hit(id, &segment, radius)) {
            return false;
        }
        if witness.forbidden.is_some_and(|id| self.forbidden.edge_hit(id, &segment, radius)) {
            return false;
        }
        if let Some(hit) = self.outside.first_hit_clear_ends(&segment, radius) {
            if let Hit::Edge(id) = hit {
                witness.outside = Some(id);
            }
            return false;
        }
        if let Some(hit) = self.forbidden.first_hit_clear_ends(&segment, radius) {
            if let Hit::Edge(id) = hit {
                witness.forbidden = Some(id);
            }
            return false;
        }
        true
    }

    /// The search core `C0`: the fill eroded by `r0` less the exclusions grown by `r0`. Offset
    /// chords are inscribed, so its boundary stays at least `r0 - ARC_ERR` from both.
    fn core(&self, fill: &PolySet) -> PolySet {
        fill.offset(-self.erosion, ARC_ERR).difference(&self.forbidden.set().offset(self.erosion, ARC_ERR))
    }
}

/// Build the shared test from a settled fill. Missing/netless zones, absent layers or fills,
/// and zero, negative or unrepresentable width parameters return `None`; an empty fill remains a test.
pub fn pour_test(
    board: &Board,
    rules: &RuleMatrix,
    fills: &[ZoneFill],
    zone: ItemId,
    layer: LayerId,
    params: &PourParams,
) -> Option<PourTest> {
    let zone = board.zones.iter().find(|z| z.id == zone)?;
    let fill = fills.iter().find(|f| f.zone == zone.id)?.layer(layer)?.set();
    make_test(board, rules, &fill, zone, layer, params)
}

fn make_test(
    board: &Board,
    rules: &RuleMatrix,
    fill: &PolySet,
    zone: &Zone,
    layer: LayerId,
    params: &PourParams,
) -> Option<PourTest> {
    if zone.net.is_none() || !zone.layers.has(layer) || !board.copper_set().has(layer)
        || params.side_margin < 0 || zone.settings.min_width < 0
    {
        return None;
    }
    let width = zone.settings.min_width.checked_add(params.side_margin.checked_mul(2)?)?;
    let radius = (width / 2).checked_add(2 * MARGIN_NM)?;
    // just under the fill's inscribed half width: its offset loses at most ARC_ERR a side
    let erosion = zone.settings.min_width / 2 - 2 * ARC_ERR - 2 * MARGIN_NM;
    if width == 0 || erosion <= ARC_ERR {
        return None;
    }
    // The enclosing board-sized box has a halo, so even a fill at a zero-gap board edge has
    // a complement. Endpoints beyond that finite box are rejected by `legal` itself.
    let bounds = board.outline_bbox().union(fill.bbox()).inflate(radius + ARC_ERR);
    let outside = PolyGrid::new(PolySet::rect(bounds).difference(fill));
    let forbidden = PolyGrid::new(exclusions(board, rules, layer, zone.net));
    Some(PourTest { layer, width, radius, erosion, bounds, outside, forbidden })
}

/// A zone layer's kept fill, cut where two of its minimum-width islands (`TileLabels::island`)
/// touch. Islands meet only at necks narrower than the minimum width; there the fill loses the
/// strip of the second polygon within `ARC_ERR` of the first, so the core ends `r0` short of the
/// cut, the test treats it as outside, and no path crosses a neck. A layer whose islands do not
/// touch keeps its fill as it is. Only polygons of one tile or of side-adjacent tiles can touch:
/// label compares over those pairs, one offset per polygon with such a neighbour, and one
/// intersection per pair of different islands whose boxes meet.
fn cut_at_necks(layer: &FillLayer) -> PolySet {
    let set = layer.set();
    let by_key: HashMap<TileKey, usize> = layer.tiles.iter().enumerate().map(|(i, tile)| (tile.key, i)).collect();
    let mut walls = Vec::new();
    for (i, (tile, labels)) in layer.tiles.iter().zip(&layer.labels).enumerate() {
        let right = by_key.get(&TileKey { x: tile.key.x + 1, y: tile.key.y }).copied();
        let up = by_key.get(&TileKey { x: tile.key.x, y: tile.key.y + 1 }).copied();
        for (a, poly) in tile.polys.iter().enumerate().filter(|&(a, _)| labels.kept[a]) {
            let reach = poly.bbox().inflate(ARC_ERR);
            let mut grown = None;
            for j in [Some(i), right, up].into_iter().flatten() {
                let (other, other_labels) = (&layer.tiles[j], &layer.labels[j]);
                for (b, near) in other.polys.iter().enumerate() {
                    if (j == i && b <= a) || !other_labels.kept[b] || other_labels.island[b] == labels.island[a]
                        || !reach.intersects(&near.bbox())
                    {
                        continue;
                    }
                    let strip = grown.get_or_insert_with(|| poly.offset(ARC_ERR, ARC_ERR)).intersection(near);
                    if !strip.is_empty() {
                        walls.push(strip);
                    }
                }
            }
        }
    }
    if walls.is_empty() { set } else { set.difference(&PolySet::union_all(&walls)) }
}

fn exclusions(board: &Board, rules: &RuleMatrix, layer: LayerId, net: Option<NetId>) -> PolySet {
    // Same containing growth as zone::grown: shape polygons circumscribe arcs; offset chords
    // are inscribed. Keepout holes are discarded, as in the adapter's outer-ring keepouts.
    let grow = |shape: &Shape, gap: Nm| shape.to_polyset(ARC_ERR).offset(gap + ARC_ERR, ARC_ERR);
    let mut sets = Vec::new();
    let mut keepout = |shape: Shape| {
        if let Shape::Poly(set) = shape {
            for polygon in set.polygons() {
                let outer = PolySet::from_ring_nonzero(polygon.outer.clone());
                sets.push(grow(&Shape::Poly(outer), 1 + MARGIN_NM));
            }
        }
    };
    let mut keepouts: Vec<_> = board.keepouts.iter().collect();
    keepouts.sort_by_key(|k| k.id);
    for k in keepouts {
        if k.layers.has(layer) && k.forbid.has(Forbid::TRACKS) && fab_elec_board::drc::reaches_copper(k.z) {
            keepout(Shape::Poly(k.area.clone()));
        }
    }
    let mut placed: Vec<_> = board.placed.iter().collect();
    placed.sort_by_key(|p| p.id);
    for p in placed {
        for k in &p.footprint.keepouts {
            if k.forbid.has(Forbid::TRACKS) && k.layers.has(p.fp_layer(layer, board.copper_count())) {
                keepout(p.shape_to_board(&Shape::Poly(k.area.clone())));
            }
        }
    }
    let vias: BTreeMap<_, _> = board.vias.iter().map(|v| (v.id, v)).collect();
    let mut drills = board.drills();
    drills.sort_by_key(|d| d.item);
    for drill in drills {
        // The relaxer lets a net's tracks over its own plated drills (a same-net pad or via is no
        // obstacle to a track), and the pour's own pads and vias are where its paths end.
        if drill.plated && drill.net == net {
            continue;
        }
        if let ConnItem::Via(id) = drill.item {
            if !vias.get(&id).is_some_and(|v| v.span_layers(board.copper_set()).has(layer)) {
                continue;
            }
        }
        sets.push(grow(&drill.shape(), rules.hole_clearance() + MARGIN_NM));
    }
    PolySet::union_all(&sets)
}

struct UnionFind {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self { parent: (0..n).collect(), size: vec![1; n] }
    }

    fn root(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn join(&mut self, a: usize, b: usize) -> bool {
        let (mut a, mut b) = (self.root(a), self.root(b));
        if a == b {
            return false;
        }
        if self.size[a] < self.size[b] || (self.size[a] == self.size[b] && a > b) {
            std::mem::swap(&mut a, &mut b);
        }
        self.parent[b] = a;
        self.size[a] += self.size[b];
        true
    }
}

struct Groups {
    items: Vec<Vec<ConnItem>>,
    nets: Vec<NetId>,
    of: BTreeMap<ConnItem, u32>,
    cluster: Vec<Option<usize>>,
    needed: u32,
}

fn hard_groups(board: &Board, copper: &[Copper], conn: &Connectivity) -> Groups {
    let zones: BTreeMap<_, _> = board.zones.iter().filter_map(|z| z.net.map(|n| (z.id, n))).collect();
    let poured: BTreeSet<_> = zones.values().copied().collect();
    let items: BTreeMap<_, _> = copper.iter().filter_map(|c| {
        c.net.filter(|n| poured.contains(n)).map(|n| (c.item, n))
    }).collect();
    let keys: Vec<_> = items.keys().copied().collect();
    let index: BTreeMap<_, _> = keys.iter().enumerate().map(|(i, &c)| (c, i)).collect();
    let mut union = UnionFind::new(keys.len());
    let mut tracks = BTreeMap::new();
    for (i, item) in keys.iter().enumerate() {
        if let ConnItem::Track { item, .. } = item {
            let first = *tracks.entry(*item).or_insert(i);
            union.join(first, i);
        }
    }
    for join in &conn.joins {
        if let (Some(&a), Some(&b)) = (index.get(&join.a), index.get(&join.b)) {
            if items[&join.a] == items[&join.b] {
                union.join(a, b);
            }
        }
    }
    let mut members: BTreeMap<(NetId, usize), Vec<ConnItem>> = BTreeMap::new();
    for (i, &item) in keys.iter().enumerate() {
        members.entry((items[&item], union.root(i))).or_default().push(item);
    }
    let mut members: Vec<_> = members.into_iter().map(|((net, _), items)| (net, items)).collect();
    members.sort();
    let mut groups = Groups {
        items: Vec::new(), nets: Vec::new(), of: BTreeMap::new(), cluster: Vec::new(), needed: 0,
    };
    for (net, items) in members {
        let group = u32::try_from(groups.items.len()).expect("connectivity group indices fit u32");
        for &item in &items {
            groups.of.insert(item, group);
        }
        groups.items.push(items);
        groups.nets.push(net);
        groups.cluster.push(None);
    }
    for (ci, cluster) in conn.clusters.iter().enumerate() {
        let zone_nets: BTreeSet<_> = cluster.items.iter().filter_map(|c| match c {
            ConnItem::Zone { item, .. } => zones.get(item).copied(),
            _ => None,
        }).collect();
        let mut in_cluster: BTreeMap<NetId, BTreeSet<u32>> = BTreeMap::new();
        for item in &cluster.items {
            if let Some(&g) = groups.of.get(item) {
                let slot = &mut groups.cluster[g as usize];
                debug_assert!(slot.is_none() || *slot == Some(ci), "a hard group is in one start cluster");
                *slot = Some(ci);
                let net = groups.nets[g as usize];
                if zone_nets.contains(&net) {
                    in_cluster.entry(net).or_default().insert(g);
                }
            }
        }
        for set in in_cluster.values() {
            groups.needed += u32::try_from(set.len().saturating_sub(1)).expect("group counts fit u32");
        }
    }
    groups
}

fn terminals(board: &Board, copper: &[Copper], groups: &Groups) -> BTreeMap<(NetId, LayerId), Vec<(u32, P)>> {
    let mut out: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for c in copper {
        let Some(&group) = groups.of.get(&c.item) else { continue };
        let points = match c.item {
            ConnItem::Pad { placed, pad } => {
                board.placed_item(placed).and_then(|p| p.pad_at(pad)).into_iter().collect()
            }
            ConnItem::Track { .. } => match &c.shape {
                Shape::Segment { a, b, .. } | Shape::Arc { a, b, .. } => vec![*a, *b],
                _ => Vec::new(),
            },
            ConnItem::Via(_) | ConnItem::Hole(_) => vec![c.shape.bbox().center()],
            ConnItem::Zone { .. } => Vec::new(),
        };
        for point in points {
            if point_clearance(point, &c.shape) == 0 {
                out.entry((groups.nets[group as usize], c.layer)).or_default().push((group, point));
            }
        }
    }
    for points in out.values_mut() {
        points.sort();
        points.dedup();
    }
    out
}

#[derive(Clone, Copy)]
struct RingEdge {
    a: usize,
    b: usize,
    ring: usize,
}

/// A component's vertices, counting-sorted into half-open square cells. This owns indices only:
/// attachments may append source points without adding them to the component's search range.
struct NearestFirst {
    origin: P,
    size: i128,
    cols: usize,
    rows: usize,
    starts: Vec<usize>,
    vertices: Vec<usize>,
}

impl NearestFirst {
    fn new(points: &[P], range: (usize, usize)) -> Self {
        let component = &points[range.0..range.1];
        let first = component.first().copied().unwrap_or(P::ZERO);
        let (mut lo, mut hi) = (first, first);
        for &p in component {
            lo.x = lo.x.min(p.x);
            lo.y = lo.y.min(p.y);
            hi.x = hi.x.max(p.x);
            hi.y = hi.y.max(p.y);
        }
        // Include the last integer coordinate, so even a line or a point has positive area.
        // Aim for four vertices per cell, rounded up to a power-of-two nm side. Also bound the
        // actual cell count by 4V: area alone would allocate too many cells for a thin box.
        // Nm is i32, so this sizing loop has a constant number of steps; no vertices are sorted.
        let width = hi.x as i128 - lo.x as i128 + 1;
        let height = hi.y as i128 - lo.y as i128 + 1;
        let count = component.len().max(1) as i128;
        let area = (4 * width * height + count - 1) / count;
        let mut size = 1_i128;
        let (cols, rows) = loop {
            let cols = (width - 1) / size + 1;
            let rows = (height - 1) / size + 1;
            if size * size >= area && cols * rows <= 4 * count {
                break (cols as usize, rows as usize);
            }
            size *= 2;
        };
        let mut grid = Self {
            origin: lo, size, cols, rows,
            starts: vec![0; cols * rows + 1], vertices: vec![0; component.len()],
        };
        for &p in component {
            let (x, y) = grid.cell(p);
            grid.starts[y * cols + x + 1] += 1;
        }
        for i in 1..grid.starts.len() {
            let previous = grid.starts[i - 1];
            grid.starts[i] += previous;
        }
        let mut next = grid.starts[..cols * rows].to_vec();
        for v in range.0..range.1 {
            let (x, y) = grid.cell(points[v]);
            let cell = y * cols + x;
            grid.vertices[next[cell]] = v;
            next[cell] += 1;
        }
        grid
    }

    fn cell(&self, p: P) -> (usize, usize) {
        let x = (p.x as i128 - self.origin.x as i128).div_euclid(self.size);
        let y = (p.y as i128 - self.origin.y as i128).div_euclid(self.size);
        (x.clamp(0, self.cols as i128 - 1) as usize, y.clamp(0, self.rows as i128 - 1) as usize)
    }

    /// Try exactly a prefix of ascending (distance squared, vertex index), stopping on acceptance.
    fn first(&self, points: &[P], p: P, mut accept: impl FnMut(usize) -> bool) -> Option<usize> {
        if self.vertices.is_empty() {
            return None;
        }
        let (cx, cy) = self.cell(p);
        let mut heap = BinaryHeap::new();
        let push_cell = |x: usize, y: usize, heap: &mut BinaryHeap<Reverse<(i128, usize)>>| {
            let cell = y * self.cols + x;
            for &v in &self.vertices[self.starts[cell]..self.starts[cell + 1]] {
                heap.push(Reverse((p.dist2(points[v]), v)));
            }
        };
        let mut ring = 0;
        loop {
            let (x0, x1) = (cx.saturating_sub(ring), cx.saturating_add(ring).min(self.cols - 1));
            let (y0, y1) = (cy.saturating_sub(ring), cy.saturating_add(ring).min(self.rows - 1));
            if ring == 0 {
                push_cell(cx, cy, &mut heap);
            } else {
                // Only new rows/columns, even when a ring is clipped at a grid edge. The side
                // columns exclude the new rows, so every cell and vertex is pushed once.
                if ring <= cy {
                    for x in x0..=x1 { push_cell(x, y0, &mut heap); }
                }
                if ring < self.rows - cy {
                    for x in x0..=x1 { push_cell(x, y1, &mut heap); }
                }
                let old_y0 = cy.saturating_sub(ring - 1);
                let old_y1 = cy.saturating_add(ring - 1).min(self.rows - 1);
                if ring <= cx {
                    for y in old_y0..=old_y1 { push_cell(x0, y, &mut heap); }
                }
                if ring < self.cols - cx {
                    for y in old_y0..=old_y1 { push_cell(x1, y, &mut heap); }
                }
            }
            let all = x0 == 0 && x1 + 1 == self.cols && y0 == 0 && y1 + 1 == self.rows;
            // Only sides with unsearched cells constrain the covered radius. At a low side L,
            // outside integer coordinates end at L - 1; at a high side H they start at H.
            // Clamping the centre cell makes these gaps positive even for an outside query.
            let mut radius = i128::MAX;
            if x0 > 0 {
                radius = radius.min(p.x as i128 - (self.origin.x as i128 + x0 as i128 * self.size) + 1);
            }
            if x1 + 1 < self.cols {
                radius = radius.min(self.origin.x as i128 + (x1 + 1) as i128 * self.size - p.x as i128);
            }
            if y0 > 0 {
                radius = radius.min(p.y as i128 - (self.origin.y as i128 + y0 as i128 * self.size) + 1);
            }
            if y1 + 1 < self.rows {
                radius = radius.min(self.origin.y as i128 + (y1 + 1) as i128 * self.size - p.y as i128);
            }
            while let Some(&Reverse((distance, v))) = heap.peek() {
                // Strict: an unseen vertex at exactly radius may tie with a smaller index.
                // Short-circuit when all cells are covered, without squaring the sentinel.
                if !all && distance >= radius * radius {
                    break;
                }
                heap.pop();
                if accept(v) {
                    return Some(v);
                }
            }
            if all {
                return None;
            }
            ring += 1;
        }
    }
}

#[derive(Default)]
struct Graph {
    points: Vec<P>,
    edges: Vec<(usize, usize)>,
    sources: Vec<Option<u32>>,
}

impl Graph {
    fn point(&mut self, p: P, source: Option<u32>) -> usize {
        let i = self.points.len();
        self.points.push(p);
        self.sources.push(source);
        i
    }

    fn component(&mut self, core: &PolySet) -> (usize, usize) {
        let begin = self.points.len();
        let mut edges = Vec::new();
        let mut holes = Vec::new();
        for (ring, points) in core.rings().enumerate() {
            let base = self.points.len();
            for &p in points {
                self.point(p, None);
            }
            for i in 0..points.len() {
                edges.push(RingEdge { a: base + i, b: base + (i + 1) % points.len(), ring });
            }
            if ring != 0 {
                if let Some(i) = (0..points.len()).min_by_key(|&i| points[i]) {
                    holes.push((points[i].y, points[i].x, ring, base + i));
                }
            }
        }
        self.bridges(&edges, &mut holes);
        (begin, self.points.len())
    }

    fn bridges(&mut self, edges: &[RingEdge], holes: &mut [(Nm, Nm, usize, usize)]) {
        let mut starts: Vec<_> = edges.iter().enumerate().map(|(i, e)| {
            (self.points[e.a].y.min(self.points[e.b].y), i)
        }).collect();
        let mut ends: Vec<_> = edges.iter().enumerate().map(|(i, e)| {
            (self.points[e.a].y.max(self.points[e.b].y), i)
        }).collect();
        starts.sort();
        ends.sort();
        holes.sort();
        let mut active = BTreeSet::new();
        let (mut start, mut end) = (0, 0);
        let mut splits: Vec<Vec<usize>> = vec![Vec::new(); edges.len()];
        let mut split_ids = BTreeMap::new();
        for &(y, x, ring, source) in holes.iter() {
            while start < starts.len() && starts[start].0 <= y {
                active.insert(starts[start].1);
                start += 1;
            }
            while end < ends.len() && ends[end].0 < y {
                active.remove(&ends[end].1);
                end += 1;
            }
            let mut hit: Option<(i128, i128, usize)> = None;
            for &id in &active {
                let edge = edges[id];
                if edge.ring == ring {
                    continue;
                }
                let Some((num, den)) = ray_hit(self.points[edge.a], self.points[edge.b], P::new(x, y)) else {
                    continue;
                };
                if hit.map_or(true, |(n, d, _)| num * d > n * den) {
                    hit = Some((num, den, id));
                }
            }
            let Some((num, den, id)) = hit else {
                debug_assert!(false, "a bounded component has a boundary left of every hole");
                continue;
            };
            let q = P::new((num.div_euclid(den) + i128::from(num.rem_euclid(den) != 0)) as Nm, y);
            let edge = edges[id];
            let target = if q == self.points[edge.a] {
                edge.a
            } else if q == self.points[edge.b] {
                edge.b
            } else if let Some(&v) = split_ids.get(&(id, q)) {
                v
            } else {
                let v = self.point(q, None);
                split_ids.insert((id, q), v);
                splits[id].push(v);
                v
            };
            self.edges.push((source, target));
        }
        for (id, edge) in edges.iter().enumerate() {
            let (a, b) = (self.points[edge.a], self.points[edge.b]);
            splits[id].sort_by_key(|&v| {
                let p = self.points[v];
                ((p.x as i128 - a.x as i128) * (b.x as i128 - a.x as i128)
                    + (p.y as i128 - a.y as i128) * (b.y as i128 - a.y as i128), v)
            });
            let mut prev = edge.a;
            for next in splits[id].iter().copied().chain(std::iter::once(edge.b)) {
                self.edges.push((prev, next));
                prev = next;
            }
        }
    }

    fn attach(&mut self, core: &PolySet, range: (usize, usize), terminals: &[(u32, P)], test: &PourTest) {
        let nearest = NearestFirst::new(&self.points, range);
        for &(group, p) in terminals {
            if !core.contains(p) {
                continue;
            }
            if let Some(v) = nearest.first(&self.points, p, |v| test.reach(test.layer, p, self.points[v])) {
                // Separate source vertices retain every group even when two terminals attach to
                // the same boundary vertex, including a zero-length attachment.
                let source = self.point(p, Some(group));
                self.edges.push((source, v));
            }
        }
    }
}

// Exact x coordinate of the first hit on this closed edge, restricted to the leftward ray.
fn ray_hit(mut a: P, mut b: P, p: P) -> Option<(i128, i128)> {
    if a.y > b.y {
        std::mem::swap(&mut a, &mut b);
    }
    if p.y < a.y || p.y > b.y {
        return None;
    }
    if a.y == b.y {
        return (a.x.min(b.x) <= p.x).then_some((i128::from(a.x.max(b.x).min(p.x)), 1));
    }
    let den = b.y as i128 - a.y as i128;
    let num = a.x as i128 * den + (p.y as i128 - a.y as i128) * (b.x as i128 - a.x as i128);
    (num <= p.x as i128 * den).then_some((num, den))
}

// Euclidean length rounded to whole nm, with an integer square root. Widen costs so summing
// paths never truncates to the coordinate range; i32 endpoint distances fit u64.
fn length(a: P, b: P) -> u64 {
    let square = a.dist2(b) as u128;
    if square == 0 {
        return 0;
    }
    let mut root = 1_u128 << ((128 - square.leading_zeros() + 1) / 2);
    loop {
        let next = (root + square / root) / 2;
        if next >= root {
            return (root + u128::from(square - root * root > root)) as u64;
        }
        root = next;
    }
}

#[derive(Clone, Copy)]
struct Visit {
    distance: u128,
    group: u32,
    prev: Option<usize>,
}

fn dijkstra(graph: &Graph) -> Vec<Option<Visit>> {
    let mut adjacent = vec![Vec::new(); graph.points.len()];
    for &(a, b) in &graph.edges {
        let len = length(graph.points[a], graph.points[b]);
        adjacent[a].push((b, len));
        adjacent[b].push((a, len));
    }
    for edges in &mut adjacent {
        edges.sort();
    }
    let mut visits = vec![None; graph.points.len()];
    let mut done = vec![false; graph.points.len()];
    let mut queue = BinaryHeap::new();
    for (v, group) in graph.sources.iter().enumerate() {
        if let Some(group) = *group {
            visits[v] = Some(Visit { distance: 0, group, prev: None });
            queue.push(Reverse((0_u128, group, v)));
        }
    }
    while let Some(Reverse((distance, group, v))) = queue.pop() {
        if done[v] || visits[v].map_or(true, |s| (s.distance, s.group) != (distance, group)) {
            continue;
        }
        done[v] = true;
        for &(next, len) in &adjacent[v] {
            if done[next] || graph.sources[next].is_some() {
                continue;
            }
            let cost = distance + u128::from(len);
            if visits[next].map_or(true, |s| (cost, group) < (s.distance, s.group)) {
                visits[next] = Some(Visit { distance: cost, group, prev: Some(v) });
                queue.push(Reverse((cost, group, next)));
            }
        }
    }
    visits
}

struct LayerGraph {
    zone: ItemId,
    net: NetId,
    test: PourTest,
    graph: Graph,
    visits: Vec<Option<Visit>>,
    base: usize,
}

#[derive(Clone, Copy)]
struct Candidate {
    cost: u128,
    joins: (u32, u32),
    graph: usize,
    u: usize,
    v: usize,
}

fn backtrace(graph: &LayerGraph, mut v: usize) -> Vec<P> {
    let mut points = Vec::new();
    loop {
        points.push(graph.graph.points[v]);
        let visit = graph.visits[v].expect("candidate endpoints and their predecessors were reached");
        match visit.prev {
            Some(prev) => v = prev,
            None => return points,
        }
    }
}

fn taut(points: &[P], test: &PourTest) -> Vec<P> {
    let mut result = vec![points[0]];
    let mut at = 0;
    while at + 1 < points.len() {
        let mut witness = Witness::default();
        // component's ring, split and bridge edges pass forest's reach validation; attach
        // adds sources only through reach. Backtraces therefore have clear endpoints, even
        // for zero-length attachments. make_test ensures the reach radius is positive.
        // Keep far-end-first find: identical probe booleans select identical points and bytes.
        at = (at + 1..points.len()).rev().find(|&next| {
            test.reach_from(&mut witness, test.layer, points[at], points[next])
        })
            .expect("the next edge in an accepted graph path is legal");
        result.push(points[at]);
    }
    result
}

/// Protect start-form pour connectivity with a forest of legal polylines. All layers and zones of
/// a net compete in one Kruskal pass; hard groups already include their cross-layer copper unions.
pub fn forest(
    board: &Board,
    rules: &RuleMatrix,
    fills: &[ZoneFill],
    fixed: (&Board, &[ZoneFill]),
    conn: &Connectivity,
    params: &PourParams,
) -> PourForest {
    let mut copper = board.all_copper();
    copper.extend(board.reached_copper(fills));
    copper.sort_by_key(|c| (c.item, c.layer));
    let groups = hard_groups(board, &copper, conn);
    let terminals = terminals(board, &copper, &groups);
    let mut zones: Vec<_> = board.zones.iter().collect();
    zones.sort_by_key(|z| z.id);
    let mut graphs = Vec::new();
    let mut candidates = Vec::new();
    let mut dropped_edges = 0;
    let mut base = 0;
    for zone in zones {
        let Some(net) = zone.net else { continue };
        let Some(fill) = fills.iter().find(|f| f.zone == zone.id) else { continue };
        let mut layers: Vec<_> = fill.layers.iter().collect();
        layers.sort_by_key(|l| l.layer);
        for layer in layers {
            let set = cut_at_necks(layer);
            let Some(test) = make_test(board, rules, &set, zone, layer.layer, params) else { continue };
            let mut graph = Graph::default();
            let points = terminals.get(&(net, layer.layer)).map_or(&[][..], Vec::as_slice);
            for core in test.core(&set).split() {
                let first_edge = graph.edges.len();
                let range = graph.component(&core);
                let edges = graph.edges.split_off(first_edge);
                for (a, b) in edges {
                    if test.reach(layer.layer, graph.points[a], graph.points[b]) {
                        graph.edges.push((a, b));
                    } else {
                        dropped_edges += 1;
                    }
                }
                graph.attach(&core, range, points, &test);
            }
            for (a, b) in &mut graph.edges {
                if *a > *b {
                    std::mem::swap(a, b);
                }
            }
            graph.edges.sort();
            graph.edges.dedup();
            let visits = dijkstra(&graph);
            for &(u, v) in &graph.edges {
                let (Some(a), Some(b)) = (visits[u], visits[v]) else { continue };
                if a.group != b.group {
                    candidates.push(Candidate {
                        cost: a.distance + u128::from(length(graph.points[u], graph.points[v])) + b.distance,
                        joins: (a.group.min(b.group), a.group.max(b.group)), graph: graphs.len(), u, v,
                    });
                }
            }
            let next_base = base + graph.points.len();
            graphs.push(LayerGraph { zone: zone.id, net, test, graph, visits, base });
            base = next_base;
        }
    }
    candidates.sort_by_key(|c| {
        let g = &graphs[c.graph];
        (c.cost, c.joins, g.test.layer, g.base + c.u, g.base + c.v)
    });
    let mut union = UnionFind::new(groups.items.len());
    let mut paths = Vec::new();
    for c in candidates {
        let (a, b) = (c.joins.0 as usize, c.joins.1 as usize);
        let same_cluster = groups.cluster[a].is_some() && groups.cluster[a] == groups.cluster[b];
        debug_assert!(same_cluster, "a pour path cannot join different start-form clusters");
        if !same_cluster || !union.join(a, b) {
            continue;
        }
        let g = &graphs[c.graph];
        let (u, v) = if g.visits[c.u].expect("candidate endpoint was reached").group == c.joins.0 {
            (c.u, c.v)
        } else {
            (c.v, c.u)
        };
        let mut points = backtrace(g, u);
        points.reverse();
        points.extend(backtrace(g, v));
        paths.push(PourPath {
            zone: g.zone, net: g.net, layer: g.test.layer, width: g.test.width, route: taut(&points, &g.test),
            points: Vec::new(), pinned: Vec::new(), joins: c.joins,
        });
    }
    let accepted = u32::try_from(paths.len()).expect("forest join counts fit u32");
    let unprotected = groups.needed.checked_sub(accepted)
        .expect("a forest accepts at most hard groups minus one per filled start cluster");
    let mut search = BTreeMap::new();
    for g in graphs {
        if paths.iter().any(|p| (p.zone, p.layer) == (g.zone, g.test.layer)) {
            search.insert((g.zone, g.test.layer), g.test);
        }
    }
    let mut out = PourForest { groups: groups.items, paths, unprotected, dropped_edges, pinned_nm: 0, search };
    out.classify(rules, Some(fixed), params);
    out
}

/// The longest piece a path segment is classified in: a pinned piece ends within this of the
/// fixed copper or mask that pins it.
const PIECE_NM: Nm = 250_000;

/// Split `points` into pieces of at most `PIECE_NM`, classify each piece free when `free` (the
/// full-width test on the fixed-only fill) passes it, else pinned (all pinned without a test),
/// and merge consecutive pieces of one segment with the same class. Split points round to whole
/// nm, so a segment whose pieces would not all pass the search test `search` stays whole.
fn stretches(points: &[P], search: &PourTest, free: Option<&PourTest>) -> (Vec<P>, Vec<bool>) {
    let mut out = vec![points[0]];
    let mut pinned = Vec::new();
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let mut n = length(a, b).div_ceil(PIECE_NM as u64).max(1) as i64;
        let at = |n: i64, k: i64| if k == n { b } else {
            P::new(a.x + (i64::from(b.x - a.x) * k / n) as Nm, a.y + (i64::from(b.y - a.y) * k / n) as Nm)
        };
        if (0..n).any(|k| !search.reach(search.layer, at(n, k), at(n, k + 1))) {
            n = 1;
        }
        let mut last: Option<bool> = None;
        for k in 0..n {
            let (p, q) = (at(n, k), at(n, k + 1));
            let pin = free.is_none_or(|t| !t.legal(t.layer, p, q));
            if last == Some(pin) {
                *out.last_mut().expect("a piece has a start point") = q;
            } else {
                out.push(q);
                pinned.push(pin);
            }
            last = Some(pin);
        }
    }
    (out, pinned)
}

#[cfg(test)]
mod tests {
    use super::{NearestFirst, Nm, P};

    #[test]
    fn nearest_first_matches_the_sorted_order() {
        fn next(state: &mut u64) -> u32 {
            *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (*state >> 32) as u32
        }

        fn check(vertices: &[P], queries: &[P]) {
            // A nonzero range start and later appends model successive graph components and
            // source points. Neither earlier vertices nor appended sources may be searched.
            let mut points = vec![P::ZERO, P::new(1, 1)];
            points.extend_from_slice(vertices);
            let range = (2, points.len());
            let grid = NearestFirst::new(&points, range);
            points.extend_from_slice(queries);
            assert!(grid.size >= 1);
            assert!(grid.cols * grid.rows <= 4 * vertices.len().max(1));
            for (query, &p) in queries.iter().enumerate() {
                let mut sorted: Vec<_> = (range.0..range.1).map(|v| (p.dist2(points[v]), v)).collect();
                sorted.sort();
                let order: Vec<_> = sorted.into_iter().map(|(_, v)| v).collect();
                for mode in 0..4 {
                    let accepted = |v: usize| match mode {
                        0 => true,
                        1 => false,
                        2 => {
                            let hash = (v as u64).wrapping_mul(0x9e3779b97f4a7c15)
                                ^ (query as u64).wrapping_mul(0xbf58476d1ce4e5b9);
                            hash.rotate_left(23) % 7 < 2
                        }
                        _ => order.last() == Some(&v),
                    };
                    let answer = order.iter().position(|&v| accepted(v));
                    let expected = answer.map(|i| order[i]);
                    let prefix = answer.map_or(order.len(), |i| i + 1);
                    let mut tried = Vec::new();
                    let actual = grid.first(&points, p, |v| {
                        tried.push(v);
                        accepted(v)
                    });
                    assert_eq!(actual, expected, "answer at {p:?}, mode {mode}");
                    assert_eq!(tried.as_slice(), &order[..prefix], "call order at {p:?}, mode {mode}");
                }
            }
        }

        let queries = [
            P::ZERO, P::new(1, -1), P::new(-17, 17), P::new(1_000_000, -1_000_000),
            P::new(Nm::MIN, Nm::MAX), P::new(Nm::MAX, Nm::MIN),
        ];
        check(&[], &queries);
        check(&[P::new(7, -3)], &queries);
        check(&[P::new(-5, 11); 19], &queries);
        let horizontal: Vec<_> = (-24..=24).rev().map(|x| P::new(x * 500_000, 19)).collect();
        let vertical: Vec<_> = horizontal.iter().map(|p| P::new(p.y, p.x)).collect();
        check(&horizontal, &queries);
        check(&vertical, &queries);
        check(&[P::new(Nm::MIN, 17), P::new(Nm::MAX, 17)], &queries);

        // These cells have side 16. At x=15, the unseen point at 16 ties the point at 14
        // already in the heap, but has the smaller index: popping at distance <= r fails.
        let ties = [P::new(16, 0), P::new(14, 0), P::ZERO, P::new(64, 0)];
        assert_eq!(NearestFirst::new(&ties, (0, ties.len())).size, 16);
        check(&ties, &[P::new(15, 0), P::new(16, 0), P::new(-1, 0), P::new(65, 0)]);
        // The low-side counterpart: x=15 is just below the half-open cell starting at 16.
        check(&[P::new(15, 0), P::new(17, 0), P::ZERO, P::new(64, 0)], &[P::new(16, 0)]);

        // Many points lie on cell boundaries; query every boundary and its neighbouring nm.
        let mut lattice = Vec::new();
        for y in (-4..=4).rev() {
            for x in (-4..=4).rev() {
                lattice.push(P::new(x * 8, y * 8));
            }
        }
        let grid = NearestFirst::new(&lattice, (0, lattice.len()));
        let mut boundary_queries = Vec::new();
        for y in 0..=grid.rows {
            for x in 0..=grid.cols {
                for delta in -1..=1 {
                    boundary_queries.push(P::new(
                        (grid.origin.x as i128 + x as i128 * grid.size + delta) as Nm,
                        (grid.origin.y as i128 + y as i128 * grid.size - delta) as Nm,
                    ));
                }
            }
        }
        check(&lattice, &boundary_queries);

        let mut seed = 0x6a09e667f3bcc909;
        for case in 0..64 {
            let count = 1 + (next(&mut seed) % 64) as usize;
            let mut points: Vec<P> = Vec::new();
            for i in 0..count {
                let x = next(&mut seed);
                let y = next(&mut seed);
                let p = match case % 4 {
                    0 => P::new((x % 33) as Nm - 16, (y % 33) as Nm - 16),
                    1 => P::new((x % 2_000_001) as Nm - 1_000_000, (y % 2_000_001) as Nm - 1_000_000),
                    2 => {
                        let centre = if i % 2 == 0 { -900_000 } else { 900_000 };
                        P::new(centre + (x % 17) as Nm, centre + (y % 17) as Nm)
                    }
                    _ => P::new(x as Nm, y as Nm),
                };
                points.push(if i > 0 && i % 7 == 0 { points[x as usize % i] } else { p });
            }
            let mut queries = queries.to_vec();
            queries.extend(points.iter().take(4).copied());
            for _ in 0..12 {
                let x = next(&mut seed);
                let y = next(&mut seed);
                queries.push(if case % 4 == 0 {
                    P::new((x % 65) as Nm - 32, (y % 65) as Nm - 32)
                } else {
                    P::new(x as Nm, y as Nm)
                });
            }
            check(&points, &queries);
        }
    }
}
