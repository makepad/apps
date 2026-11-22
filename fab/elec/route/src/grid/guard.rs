//! Guards for pads connected only through a pour. An escape is a shortest path of free
//! eight-neighbour track steps ([`Grid::step_pass`]) on the pad's layer, from an access cell
//! whose centre is free, to a cell with via room on every stack layer. Its band marks track
//! clearance along the path and via clearance on every layer for later soft costs.
//! The flood tests complete levels: a cell joins the next level when a free step reaches it
//! from any cell of the current one. It chooses the least (y, x) target and, walking back, the
//! least (y, x) predecessor one level down whose step to the cell is free.
//! These rules depend only on occupancy, so a kept table equals a rebuild after stale entries
//! are refreshed. The watch box includes blocked seeds and blocked boundary neighbours as
//! well as visited cells, inflated by the probes' reach and one pitch. A step's owners lie
//! within that reach of one of its ends, so it covers every probed step, and removals that
//! open a shorter escape even when the removed copper misses the old band.
//! This guard is our design, not a port of the reference router.
//!
//! Escape costs O(probed cells x via test layers + 8 step tests per reached cell + path cells x
//! (track-disc cells + 8 step tests) + via-disc cells x layers), apart from the grid's lazy
//! rasterisation and step memo, plus sorting the band.
//! Stale costs O(entries x touched boxes), plus a linear merge of the entry and pour-only keys.
//! Set costs O((old + new band cells) x owners per cell). Guard lookup costs O(1). There are no
//! flood or band caps.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use fab_elec_board::{Board, ConnItem, Connectivity};
use fab_elec_core::{BBox, ItemId, LayerId, NetId, Nm, PadIx};

use super::{grow, Cell, CellBox, Grid, GridSpec, ObstacleSource, Pass, PlaneId};

/// A pad on one layer: (placed part item, pad index, layer).
pub(crate) type PadKey = (ItemId, PadIx, LayerId);

/// What the router knows about one pour-only pad when it asks for its escape.
pub(crate) struct EscapeIn {
    pub net: NetId,
    /// The pad layer's index in `grid.spec.layers`.
    pub li: usize,
    /// The net's track plane (its class and width).
    pub track: PlaneId,
    /// The net's via plane.
    pub via: PlaneId,
    /// Access cells on `li`, sorted by (y, x), deduplicated.
    pub access: Vec<Cell>,
    /// The net's track width.
    pub w: Nm,
    /// The net's via pad diameter.
    pub via_pad: Nm,
}

/// One pad's escape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Escape {
    pub net: NetId,
    /// From an access cell to the target, on the pad's layer; empty when the pad is pour-bound.
    pub path: Vec<Cell>,
    /// The first cell with room for a via of the net; None: pour-bound (the flood found none).
    pub target: Option<Cell>,
    /// The guarded cells (layer index, cell), sorted, deduplicated.
    pub band: Vec<(usize, Cell)>,
    /// Any obstacle change whose box meets this box can change the escape.
    pub watch: BBox,
}

/// Find an escape with valid in-grid access cells, layer and plane indices. `clearance` is
/// the rules' maximum clearance plus the grid's slack. Count every cell removed from a level.
pub(crate) fn escape(
    grid: &mut Grid,
    src: &dyn ObstacleSource,
    e: &EscapeIn,
    clearance: Nm,
    work: &mut u64,
) -> Escape {
    let bounds = grid.spec.all();
    // Each reached cell's level; a probed cell left out may still be reached by another step.
    let mut depths: HashMap<Cell, u64> = HashMap::new();
    let mut level = Vec::new();
    let mut watch = BBox::EMPTY;
    for &c in &e.access {
        watch = watch.with(grid.spec.centre(c));
        if !depths.contains_key(&c) && free(grid, src, e, c, c) {
            depths.insert(c, 0);
            level.push(c);
        }
    }

    let mut depth = 0;
    let mut target = None;
    while !level.is_empty() {
        // Do not expand or stop partway through a level: its entire cell set determines
        // both the winning target and the watch box, independently of discovery order.
        for &c in &level {
            *work = work.checked_add(1).expect("escape work counter must fit u64");
            watch = watch.with(grid.spec.centre(c));
            if (0..grid.spec.layers.len()).all(|li| grid.state(src, e.via, li, c).free_for(e.net))
                && target.is_none_or(|old: Cell| (c.y, c.x) < (old.y, old.x))
            {
                target = Some(c);
            }
        }
        if target.is_some() {
            break;
        }
        let mut next = Vec::new();
        for c in level {
            for n in neighbours(bounds, c) {
                if !depths.contains_key(&n) {
                    watch = watch.with(grid.spec.centre(n));
                    if free(grid, src, e, c, n) {
                        depths.insert(n, depth + 1);
                        next.push(n);
                    }
                }
            }
        }
        level = next;
        depth += 1;
    }

    let mut path = Vec::new();
    let mut band = Vec::new();
    if let Some(target_cell) = target {
        let mut c = target_cell;
        path.push(c);
        while depth > 0 {
            let mut previous = None;
            for n in neighbours(bounds, c) {
                if depths.get(&n) == Some(&(depth - 1)) && free(grid, src, e, n, c)
                    && previous.is_none_or(|p: Cell| (n.y, n.x) < (p.y, p.x))
                {
                    previous = Some(n);
                }
            }
            c = previous.expect("every reached non-seed has a predecessor");
            path.push(c);
            depth -= 1;
        }
        path.reverse();
        let track_radius = i64::from(e.w) / 2 + i64::from(clearance);
        for &c in &path {
            disc(&grid.spec, c, track_radius, |c| band.push((e.li, c)));
        }
        let via_radius = i64::from(e.via_pad) / 2 + i64::from(clearance);
        disc(&grid.spec, target_cell, via_radius, |c| {
            for li in 0..grid.spec.layers.len() {
                band.push((li, c));
            }
        });
        band.sort_unstable();
        band.dedup();
    }
    let reach = i64::from(e.w.max(e.via_pad)) / 2 + i64::from(clearance)
        + i64::from(grid.spec.pitch);
    Escape { net: e.net, path, target, band, watch: grow(watch, reach) }
}

/// Whether the track step from `a` to `b` (`a` itself: its centre) is free for the escape's net.
fn free(grid: &mut Grid, src: &dyn ObstacleSource, e: &EscapeIn, a: Cell, b: Cell) -> bool {
    grid.step_pass(src, e.track, e.li, a, b, e.net) == Pass::Free
}

fn neighbours(bounds: CellBox, c: Cell) -> impl Iterator<Item = Cell> {
    (-1..=1).flat_map(move |dy| {
        (-1..=1).filter_map(move |dx| {
            let x = i64::from(c.x) + dx;
            let y = i64::from(c.y) + dy;
            (dx != 0 || dy != 0).then_some(())?;
            (x >= i64::from(bounds.min.x) && x <= i64::from(bounds.max.x)
                && y >= i64::from(bounds.min.y) && y <= i64::from(bounds.max.y))
                .then_some(Cell { x: x as u32, y: y as u32 })
        })
    })
}

fn disc(spec: &GridSpec, c: Cell, radius: i64, mut visit: impl FnMut(Cell)) {
    let centre = spec.centre(c);
    let Some(cells) = spec.cells_in(grow(BBox::point(centre), radius)) else { return };
    let radius2 = i128::from(radius) * i128::from(radius);
    for y in cells.min.y..=cells.max.y {
        for x in cells.min.x..=cells.max.x {
            let c = Cell { x, y };
            let p = spec.centre(c);
            let dx = i128::from(p.x) - i128::from(centre.x);
            let dy = i128::from(p.y) - i128::from(centre.y);
            if dx * dx + dy * dy <= radius2 {
                visit(c);
            }
        }
    }
}

/// The pads of a net with a zone whose joins (on every layer) are all with zone polygons, each
/// on every layer it joins a zone. A pad that also joins a track, via or pad on any layer is
/// served by that copper and is not pour-only. A pad cut off behind a pour neck (its fill part holds
/// no other copper) stays listed: it is an open terminal of its net, and its own route pays its guard
/// only on cells another net's guard shares. Visit the joins once; resolve each candidate pad's net
/// from the placed item's pad map.
pub(crate) fn pour_only_pads(board: &Board, conn: &Connectivity) -> BTreeSet<PadKey> {
    let zone_nets: HashSet<_> = board.zones.iter().filter_map(|zone| zone.net).collect();
    let mut pads: BTreeMap<(ItemId, PadIx), (bool, BTreeSet<LayerId>)> = BTreeMap::new();
    for join in &conn.joins {
        for (item, other) in [(join.a, join.b), (join.b, join.a)] {
            if let ConnItem::Pad { placed, pad } = item {
                let (only, layers) = pads.entry((placed, pad)).or_insert_with(|| (true, BTreeSet::new()));
                *only &= matches!(other, ConnItem::Zone { .. });
                layers.insert(join.layer);
            }
        }
    }
    let mut out = BTreeSet::new();
    for ((placed, pad), (only, layers)) in pads {
        let net = board.placed_item(placed).and_then(|p| p.pad_nets.get(pad.ix()).copied().flatten());
        if only && net.is_some_and(|net| zone_nets.contains(&net)) {
            out.extend(layers.into_iter().map(|layer| (placed, pad, layer)));
        }
    }
    out
}

/// The escapes of a board's pour-only pads and the cells they guard (`Query::guard`).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct GuardTable {
    entries: BTreeMap<PadKey, Escape>,
    /// Per guarded cell: the (net, pad) pairs whose band holds it, sorted by (net, pad).
    field: HashMap<(usize, Cell), Vec<(NetId, PadKey)>>,
}

/// Who guards a cell: one net's pads, or the pads of several nets.
pub(crate) enum Guard {
    One(NetId),
    Several,
}

impl GuardTable {
    /// Sorted missing, departed or touched entries. Compare each touched box separately.
    pub(crate) fn stale(&self, pour_only: &BTreeSet<PadKey>, touched: &[BBox]) -> Vec<PadKey> {
        let mut stale = Vec::new();
        let mut entries = self.entries.iter().peekable();
        for &key in pour_only {
            while entries.peek().is_some_and(|entry| *entry.0 < key) {
                stale.push(*entries.next().expect("entry exists").0);
            }
            if entries.peek().is_some_and(|entry| *entry.0 == key) {
                let (_, escape) = entries.next().expect("entry exists");
                if touched.iter().any(|b| escape.watch.intersects(b)) {
                    stale.push(key);
                }
            } else {
                stale.push(key);
            }
        }
        stale.extend(entries.map(|(&key, _)| key));
        stale
    }

    /// Replace or remove one escape, updating only its old and new band cells.
    pub(crate) fn set(&mut self, key: PadKey, escape: Option<Escape>) {
        if let Some(old) = self.entries.remove(&key) {
            for c in old.band {
                if let Some(owners) = self.field.get_mut(&c) {
                    if let Ok(index) = owners.binary_search(&(old.net, key)) {
                        owners.remove(index);
                    }
                    if owners.is_empty() {
                        self.field.remove(&c);
                    }
                }
            }
        }
        if let Some(escape) = escape {
            for &c in &escape.band {
                let owners = self.field.entry(c).or_default();
                let owner = (escape.net, key);
                if let Err(index) = owners.binary_search(&owner) {
                    owners.insert(index, owner);
                }
            }
            self.entries.insert(key, escape);
        }
    }

    /// One lookup and the first/last owners distinguish an unguarded cell or one/several nets.
    pub(crate) fn guard_at(&self, li: usize, c: Cell) -> Option<Guard> {
        let owners = self.field.get(&(li, c))?;
        let first = owners.first()?;
        Some(if first.0 == owners.last().expect("field owners are nonempty").0 {
            Guard::One(first.0)
        } else {
            Guard::Several
        })
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Entries whose flood found no cell with via room.
    #[cfg(test)]
    pub(crate) fn pour_bound(&self) -> usize {
        self.entries.values().filter(|escape| escape.target.is_none()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{fixture, random, VecSource};
    use super::super::{Owner, Probe};
    use fab_elec_core::{ClassId, Shape, P};
    use std::collections::VecDeque;

    fn input(grid: &mut Grid, net: NetId, li: usize, access: Vec<Cell>) -> EscapeIn {
        let w = 20_000;
        // Wider than the pocket's 0.38 mm between wall edges: no via fits inside the U.
        let via_pad = 400_000;
        EscapeIn {
            net,
            li,
            track: grid.plane(Probe { class: ClassId(0), w, drill: None }),
            via: grid.plane(Probe { class: ClassId(0), w: via_pad, drill: Some(100_000) }),
            access,
            w,
            via_pad,
        }
    }

    fn small_grid() -> Grid {
        Grid::new(GridSpec {
            origin: P::ZERO, pitch: 100_000, cols: 9, rows: 10,
            layers: vec![LayerId::F_CU, LayerId::B_CU],
        })
    }

    fn wall(spec: &GridSpec, a: Cell, b: Cell) -> (LayerId, Shape, Owner) {
        (spec.layers[0], Shape::Segment {
            a: spec.centre(a), b: spec.centre(b), w: 20_000,
        }, Owner::Hard)
    }

    fn pocket(spec: &GridSpec) -> VecSource {
        // Board coordinates have y up: the mouth is above the two walls' y=6 ends.
        VecSource {
            items: vec![
                wall(spec, Cell { x: 2, y: 2 }, Cell { x: 2, y: 6 }),
                wall(spec, Cell { x: 6, y: 2 }, Cell { x: 6, y: 6 }),
                wall(spec, Cell { x: 2, y: 2 }, Cell { x: 6, y: 2 }),
            ],
            need: 0,
        }
    }

    // Independent exhaustive BFS: scan all cells for adjacent destinations, finish the
    // entire component, then minimise (distance, y, x) across all via-capable cells.
    fn brute_target(grid: &mut Grid, src: &VecSource, e: &EscapeIn) -> Option<(Cell, u64)> {
        let mut distance = HashMap::new();
        let mut queue = VecDeque::new();
        for &c in &e.access {
            if grid.step_pass(src, e.track, e.li, c, c, e.net) == Pass::Free && !distance.contains_key(&c) {
                distance.insert(c, 0);
                queue.push_back(c);
            }
        }
        while let Some(c) = queue.pop_front() {
            let d = distance[&c];
            for y in 0..grid.spec.rows {
                for x in 0..grid.spec.cols {
                    let n = Cell { x, y };
                    if x.abs_diff(c.x) <= 1 && y.abs_diff(c.y) <= 1
                        && !distance.contains_key(&n)
                        && grid.step_pass(src, e.track, e.li, c, n, e.net) == Pass::Free
                    {
                        distance.insert(n, d + 1);
                        queue.push_back(n);
                    }
                }
            }
        }
        distance.into_iter()
            .filter(|(c, _)| (0..grid.spec.layers.len()).all(|li| {
                grid.state(src, e.via, li, *c).free_for(e.net)
            }))
            .min_by_key(|(c, d)| (*d, c.y, c.x))
    }

    #[test]
    fn escape_leaves_a_pocket_by_its_mouth() {
        let mut grid = small_grid();
        let src = pocket(&grid.spec);
        let e = input(&mut grid, NetId(0), 0, vec![Cell { x: 4, y: 3 }]);
        let clearance = grid.spec.slack();
        let mut work = 0;
        let escaped = escape(&mut grid, &src, &e, clearance, &mut work);
        let (target, depth) = brute_target(&mut grid, &src, &e).expect("pocket has an exit");
        assert_eq!(escaped.target, Some(target), "target matches independent BFS and its tie rule");
        assert_eq!(target, Cell { x: 4, y: 7 }, "first via room is in the mouth, past the wall ends");
        assert_eq!(escaped.path.len() as u64, depth + 1, "path has minimum hop count");
        assert_eq!(escaped.path.first(), e.access.first(), "path starts at pad access");
        assert_eq!(escaped.path.last(), Some(&target), "path ends at the target");
        let mouth = escaped.path.iter().position(|c| c.y > 6).expect("path leaves the U");
        assert!(escaped.path[..mouth].iter().all(|c| (3..=5).contains(&c.x) && c.y > 2),
            "path stays inside the walls until it leaves through the mouth");
        for pair in escaped.path.windows(2) {
            assert!(pair[0].x.abs_diff(pair[1].x) <= 1 && pair[0].y.abs_diff(pair[1].y) <= 1,
                "path steps are eight-neighbours");
        }
        for x in 3..=5 {
            assert!(escaped.band.contains(&(0, Cell { x, y: 7 })), "band covers mouth cell {x}");
        }
        let mut repeated_work = 0;
        assert_eq!(escape(&mut grid, &src, &e, clearance, &mut repeated_work), escaped,
            "a second call gives the identical escape and watch");
        assert_eq!(repeated_work, work, "cached occupancy does not change counted flood work");
    }

    #[test]
    fn escape_in_the_open_is_a_disc() {
        let mut grid = small_grid();
        let src = VecSource { items: Vec::new(), need: 0 };
        let c = Cell { x: 4, y: 4 };
        let mut e = input(&mut grid, NetId(0), 1, vec![c]);
        // A wider track makes the pad-layer union observable as well as the via disc.
        e.w = 500_000;
        e.track = grid.plane(Probe { class: ClassId(0), w: e.w, drill: None });
        let clearance = grid.spec.slack();
        let mut work = 0;
        let escaped = escape(&mut grid, &src, &e, clearance, &mut work);
        assert_eq!(escaped.path, vec![c], "open pad has a one-cell path");
        assert_eq!(escaped.target, Some(c), "open pad is its own via target");
        assert_eq!(work, 1, "one cell is taken off depth zero");
        let mut expected = Vec::new();
        for li in 0..grid.spec.layers.len() {
            for x in 0..grid.spec.cols {
                for y in 0..grid.spec.rows {
                    let cell = Cell { x, y };
                    let p = grid.spec.centre(cell);
                    let centre = grid.spec.centre(c);
                    let dx = i128::from(p.x) - i128::from(centre.x);
                    let dy = i128::from(p.y) - i128::from(centre.y);
                    let radius = if li == e.li { e.w.max(e.via_pad) } else { e.via_pad } / 2 + clearance;
                    if dx * dx + dy * dy <= i128::from(radius).pow(2) {
                        expected.push((li, cell));
                    }
                }
            }
        }
        assert_eq!(escaped.band, expected, "band is exactly the sorted track/via disc union");
        let reach = e.w.max(e.via_pad) / 2 + clearance + grid.spec.pitch;
        assert_eq!(escaped.watch, BBox::point(grid.spec.centre(c)).inflate(reach),
            "depth-zero watch is exactly the probed centre plus the required reach");

        e.access = vec![Cell { x: 5, y: 3 }, Cell { x: 3, y: 4 }];
        let mut work = 0;
        let tied = escape(&mut grid, &src, &e, clearance, &mut work);
        assert_eq!(tied.target, Some(Cell { x: 5, y: 3 }), "target tie uses y before x");
        assert_eq!(work, 2, "all cells in the winning level count as pops");
    }

    #[test]
    fn walled_in_pad_is_pour_bound() {
        let mut grid = small_grid();
        let mut src = pocket(&grid.spec);
        src.items.push(wall(&grid.spec, Cell { x: 2, y: 6 }, Cell { x: 6, y: 6 }));
        let blocked = Cell { x: 0, y: 0 };
        src.items.push((LayerId::F_CU, Shape::Circle {
            c: grid.spec.centre(blocked), r: 10_000,
        }, Owner::Hard));
        let e = input(&mut grid, NetId(0), 0, vec![blocked, Cell { x: 4, y: 3 }]);
        let clearance = grid.spec.slack();
        let mut work = 0;
        let escaped = escape(&mut grid, &src, &e, clearance, &mut work);
        assert_eq!(escaped.target, None, "enclosed component has no via room");
        assert!(escaped.path.is_empty(), "pour-bound pad has no path");
        assert!(escaped.band.is_empty(), "pour-bound pad has no band");
        assert_eq!(work, 9, "only the nine reachable cells count as pops");
        let reach = e.w.max(e.via_pad) / 2 + clearance + grid.spec.pitch;
        let probed = BBox::new(grid.spec.centre(blocked), grid.spec.centre(Cell { x: 6, y: 6 }));
        assert_eq!(escaped.watch, probed.inflate(reach),
            "watch includes the distant blocked seed and all blocked boundary neighbours");
    }

    /// A pad's key with its escape input.
    fn pad(grid: &mut Grid, id: u32, net: NetId, li: usize, access: Vec<Cell>) -> (PadKey, EscapeIn) {
        ((ItemId(id), PadIx(0), grid.spec.layers[li]), input(grid, net, li, access))
    }

    fn rebuild(spec: &GridSpec, src: &VecSource, pads: &[(PadKey, EscapeIn)], only: &BTreeSet<PadKey>) -> GuardTable {
        let mut grid = Grid::new(spec.clone());
        let mut table = GuardTable::default();
        let mut work = 0;
        for (key, pad) in pads.iter().rev().filter(|(key, _)| only.contains(key)) {
            let e = input(&mut grid, pad.net, pad.li, pad.access.clone());
            let escaped = escape(&mut grid, src, &e, src.need + spec.slack(), &mut work);
            table.set(*key, Some(escaped));
        }
        table
    }

    #[test]
    fn kept_table_equals_rebuild() {
        let mut seed = 0x1234_5678_9abc_def0;
        let (spec, mut src) = fixture(seed, 9, 10);
        // Keep mixed fixture copper below the mouth on the other layer, preserving the
        // known pocket's track access and first via target. Random edits can change either.
        src.items.retain(|(layer, shape, _)| *layer == LayerId::B_CU && shape.bbox().max.y <= 400_000);
        src.need = 0;
        src.items.extend(pocket(&spec).items);
        let removed_index = src.items.len();
        src.items.push((LayerId::B_CU, Shape::Circle {
            c: spec.centre(Cell { x: 4, y: 9 }), r: 10_000,
        }, Owner::Hard));
        let mut grid = Grid::new(spec.clone());
        let pads = vec![
            pad(&mut grid, 1, NetId(0), 0, vec![Cell { x: 4, y: 3 }]),
            pad(&mut grid, 2, NetId(1), 1, vec![Cell { x: 0, y: 1 }]),
            pad(&mut grid, 3, NetId(2), 0, vec![Cell { x: 8, y: 8 }]),
        ];
        let mut only: BTreeSet<_> = pads.iter().map(|(key, _)| *key).collect();
        let mut table = GuardTable::default();
        let mut work = 0;
        assert_eq!(table.stale(&only, &[]), only.iter().copied().collect::<Vec<_>>(),
            "every missing entry is stale in key order");
        for (key, pad) in &pads {
            let escaped = escape(&mut grid, &src, pad, spec.slack(), &mut work);
            table.set(*key, Some(escaped));
        }
        assert_eq!(table, rebuild(&spec, &src, &pads, &only), "initial insertion order is immaterial");
        let far = [BBox::point(P::new(-10_000_000, -10_000_000)),
            BBox::point(P::new(10_000_000, 10_000_000))];
        assert!(table.stale(&only, &far).is_empty(),
            "separate distant boxes do not invalidate the area between them");

        let old = table.entries[&pads[0].0].clone();
        let removed_box = src.items[removed_index].1.bbox();
        assert!(old.target.is_some(), "removal regression starts with an existing escape");
        assert!(old.band.iter().all(|&(_, c)| {
            !BBox::point(spec.centre(c)).inflate(spec.pitch / 2).intersects(&removed_box)
        }), "removed obstacle misses even the full cell squares of the old band");
        assert!(old.watch.intersects(&removed_box), "watch covers copper outside the old band");

        for step in 0..9 {
            let touched = if step == 0 {
                src.items.remove(removed_index).1.bbox()
            } else if step % 2 == 1 {
                let c = Cell { x: random(&mut seed) % spec.cols, y: random(&mut seed) % spec.rows };
                let shape = Shape::Circle { c: spec.centre(c), r: 10_000 };
                let bbox = shape.bbox();
                let layer = spec.layers[(random(&mut seed) % 2) as usize];
                let owner = if step % 4 == 1 { Owner::Hard } else { Owner::Net(NetId(1)) };
                src.items.push((layer, shape, owner));
                bbox
            } else {
                let index = random(&mut seed) as usize % src.items.len();
                src.items.remove(index).1.bbox()
            };
            grid.invalidate(&src, touched, &[0, 1]);
            if step == 4 {
                only.remove(&pads[1].0);
            } else if step == 5 {
                only.insert(pads[1].0);
            }
            let stale = table.stale(&only, &[touched]);
            assert!(stale.windows(2).all(|w| w[0] < w[1]), "step {step}: stale keys are sorted and unique");
            if step == 0 {
                assert!(stale.contains(&pads[0].0), "removal outside the band invalidates its watch");
            }
            for key in stale {
                let escaped = if only.contains(&key) {
                    let (_, pad) = pads.iter().find(|(k, _)| *k == key).expect("known pad");
                    Some(escape(&mut grid, &src, pad, spec.slack(), &mut work))
                } else {
                    None
                };
                table.set(key, escaped);
            }
            assert_eq!(table, rebuild(&spec, &src, &pads, &only),
                "step {step}: kept entries and field equal a fresh grid/table after additions or removals");
            if step == 0 {
                let reopened = &table.entries[&pads[0].0];
                assert!(reopened.path.len() < old.path.len(), "removal reopens a strictly shorter escape");
                assert_eq!(reopened.target, Some(Cell { x: 4, y: 7 }),
                    "via obstruction on the other layer was the only blocker of the shorter target");
            }
        }
    }

    #[test]
    fn field_owners() {
        let a = (ItemId(1), PadIx(0), LayerId::F_CU);
        let b = (ItemId(2), PadIx(0), LayerId::F_CU);
        let c = (ItemId(3), PadIx(0), LayerId::F_CU);
        let left = (0, Cell { x: 0, y: 0 });
        let shared = (0, Cell { x: 1, y: 0 });
        let right = (0, Cell { x: 2, y: 0 });
        let first = Escape {
            net: NetId(1), path: vec![left.1], target: Some(left.1),
            band: vec![left, shared], watch: BBox::point(P::ZERO),
        };
        let second = Escape {
            net: NetId(2), path: vec![right.1], target: Some(right.1),
            band: vec![shared, right], watch: BBox::point(P::ZERO),
        };
        let mut table = GuardTable::default();
        table.set(b, Some(second.clone()));
        table.set(a, Some(first.clone()));
        assert_eq!(table.len(), 2, "table counts pads rather than cells");
        assert_eq!(table.pour_bound(), 0, "both initial entries have targets");
        assert!(matches!(table.guard_at(shared.0, shared.1), Some(Guard::Several)),
            "overlapping different nets yield Several");
        assert!(matches!(table.guard_at(left.0, left.1), Some(Guard::One(NetId(1)))),
            "first pad's exclusive cell has its net");
        assert!(matches!(table.guard_at(right.0, right.1), Some(Guard::One(NetId(2)))),
            "second pad's exclusive cell has its net");
        assert!(table.guard_at(1, shared.1).is_none(), "field lookup distinguishes layers");
        table.set(a, None);
        for &(li, cell) in &second.band {
            assert!(matches!(table.guard_at(li, cell), Some(Guard::One(NetId(2)))),
                "removing first entry leaves one owner throughout the second band");
        }
        assert!(!table.field.contains_key(&left), "empty owner vectors are removed");

        table.set(c, Some(second.clone()));
        table.set(b, Some(second.clone()));
        assert!(matches!(table.guard_at(shared.0, shared.1), Some(Guard::One(NetId(2)))),
            "multiple pads of one net still yield One");
        assert_eq!(table.field[&shared].len(), 2, "replacement does not duplicate an owner");
        table.set(b, Some(first.clone()));
        assert!(matches!(table.guard_at(shared.0, shared.1), Some(Guard::Several)),
            "replacement updates a changed net as well as changed band cells");
        let mut fresh = GuardTable::default();
        fresh.set(b, Some(first));
        fresh.set(c, Some(second));
        assert_eq!(table, fresh, "incremental field equals fresh insertion in another order");
        table.set(b, Some(Escape {
            net: NetId(1), path: Vec::new(), target: None, band: Vec::new(), watch: BBox::EMPTY,
        }));
        assert_eq!((table.len(), table.pour_bound()), (2, 1), "pour-bound entries remain in the table");
        assert!(table.guard_at(left.0, left.1).is_none(), "empty replacement removes its old band");
        table.set(c, None);
        table.set(c, None);
        assert!(table.field.is_empty(), "removal is idempotent and drops every last owner");
    }
}
