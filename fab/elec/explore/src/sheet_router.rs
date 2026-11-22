//! Orthogonal sheet wires after placement, computed on a worker when the sheet changes.
//!
//! Each route rasterises the window once in O(window cells + total obstacle area) time.
//! Wall, permitted exits and frame interior share one byte per cell (448,000 bytes for a
//! 560 by 800 window). Every terrain lookup during search is O(1). Wire slots use four more
//! bytes per window cell. An in-window wire lookup indexes the slot directly and scans only
//! that cell's owners. Owner lists and the map for out-of-window wires use O(cell-owner pairs) space.
//!
//! Each connection searches its reachable region once. A table with one slot per 16 by 16 tile
//! locates tiles allocated on first touch. Each tile holds four costs and predecessor directions
//! per cell. Starting a connection advances the visit epoch without clearing the tiles. Visits
//! are reset on first touch in that epoch. A separate net epoch covers pin directions and tree
//! membership, stored in one byte per cell as pins and branches are added.
//! A search touching T tiles and reaching V direction states against S tree segments, with up
//! to K wire owners per cell, costs O(256 * T + V * (1 + S + K + log V)) time. Beyond the slot
//! table, scratch space is O(256 * distinct tiles touched across the route + peak V).
//! Choosing the next of P remaining pins costs O(P * (1 + S)). Path costs use u64.

use crate::graph::Pt;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

/// A pin end in sheet coordinates (y up) and the unit direction its wire leaves in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RTerm {
    pub at: Pt,
    pub dir: [i32; 2],
}

/// Pins in input order and the orthogonal polylines already drawn for their net.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RNet {
    pub net: u32,
    /// Bundle id and place (0 first). Members are drawn one after another in the caller's order.
    /// A lone net uses `None`.
    pub bundle: Option<(u32, u32)>,
    pub terms: Vec<RTerm>,
    pub tree: Vec<Vec<Pt>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink { Wall, Frame }

/// A closed wall or a frame with a cost for steps into its open interior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Obstacle {
    pub bx: [i32; 4],
    pub ink: Ink,
}

/// Costs and bounds for routing sheet wires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouterOpts {
    /// Cost of each unit move.
    pub step: u32,
    /// Additional cost of turning a corner.
    pub bend: u32,
    /// Additional cost of crossing a committed wire.
    pub cross: u32,
    /// Additional cost of stepping into a frame's interior.
    pub frame: u32,
    /// Border around the outermost sheet contents, in world steps.
    pub margin: i32,
    /// Maximum Manhattan distance from a pin to its tree. The search stays inside the box of
    /// the pin and its nearest tree point, grown on each side by the greater of reach and the
    /// grid border. The default reaches across any sheet.
    pub reach: i32,
}

impl Default for RouterOpts {
    fn default() -> Self {
        Self { step: 1, bend: 4, cross: 6, frame: 8, margin: 6, reach: 1 << 24 }
    }
}

/// New runs of complete trees in routing order, their junctions, and failed net ids in input order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SheetRoute {
    pub runs: Vec<(u32, Vec<Pt>)>,
    pub junctions: Vec<(u32, Pt)>,
    pub unrouted: Vec<u32>,
}

// Direction order breaks heap ties. Opposites differ by two, axes by parity.
const DIRS: [Pt; 4] = [[1, 0], [0, 1], [-1, 0], [0, -1]];
const START: usize = 4;
const ALL: u8 = 0b1111;
const WALL: u8 = 1 << 4;
const FRAME: u8 = 1 << 5;

fn direction(d: Pt) -> Option<usize> {
    DIRS.iter().position(|&unit| unit == d)
}

// The window is the sheet's bounds plus a margin, far inside i32, so a step never overflows.
fn moved([x, y]: Pt, dir: usize) -> Option<Pt> {
    let [dx, dy] = DIRS[dir];
    Some([x + dx, y + dy])
}

fn include(b: &mut [i32; 4], [x, y]: Pt) {
    b[0] = b[0].min(x);
    b[1] = b[1].min(y);
    b[2] = b[2].max(x);
    b[3] = b[3].max(y);
}

fn bounds(terms: &[RTerm]) -> [i32; 4] {
    let [x, y] = terms[0].at;
    let mut b = [x, y, x, y];
    for term in &terms[1..] { include(&mut b, term.at); }
    b
}

// Visit each unit cell with its incident edges, including both edges at each bend.
fn wire_cells(run: &[Pt], mut visit: impl FnMut(Pt, u8)) {
    let Some((&first, rest)) = run.split_first() else { return };
    let mut at = first;
    let mut back = 0;
    for &end in rest {
        if at == end { continue; }
        let unit = [(end[0] - at[0]).signum(), (end[1] - at[1]).signum()];
        let dir = direction(unit).expect("sheet wire segments are orthogonal");
        while at != end {
            visit(at, back | (1 << dir));
            at = [at[0] + unit[0], at[1] + unit[1]];
            back = 1 << (dir ^ 2);
        }
    }
    visit(at, back);
}

#[derive(Clone, Copy)]
struct Window {
    bounds: [i32; 4],
    width: usize,
}

impl Window {
    fn new(bounds: [i32; 4]) -> Self {
        Self { bounds, width: (bounds[2] - bounds[0]) as usize + 1 }
    }

    fn cells(&self) -> usize {
        self.width * ((self.bounds[3] - self.bounds[1]) as usize + 1)
    }

    fn index(&self, [x, y]: Pt) -> usize {
        (y - self.bounds[1]) as usize * self.width + (x - self.bounds[0]) as usize
    }

    fn contains(&self, [x, y]: Pt) -> bool {
        let [l, b, r, t] = self.bounds;
        l <= x && x <= r && b <= y && y <= t
    }
}

type WireOwners = Vec<(u32, u8)>;

// Zero denotes an empty cell, leaving this many addressable owner lists per group.
const WIRE_GROUP_CELLS: usize = u32::MAX as usize;

struct Grid {
    window: Window,
    terrain: Vec<u8>,
    /// Each window cell uses four bytes. Zero means empty, otherwise the slot is a 1-based
    /// index into its group's owner lists. The lists take O(cell-owner pairs) space.
    wire_slots: Vec<u32>,
    wire_owners: Vec<Vec<WireOwners>>,
    outside_wires: HashMap<Pt, WireOwners>,
}

impl Grid {
    fn new(bounds: [i32; 4], obstacles: &[Obstacle]) -> Self {
        let window = Window::new(bounds);
        let cells = window.cells();
        let mut grid = Self {
            window,
            terrain: vec![ALL; cells],
            wire_slots: vec![0; cells],
            wire_owners: vec![Vec::new(); cells.div_ceil(WIRE_GROUP_CELLS)],
            outside_wires: HashMap::new(),
        };
        for obs in obstacles {
            let [l, b, r, t] = obs.bx;
            match obs.ink {
                Ink::Frame => {
                    if l >= r || b >= t { continue; }
                    for y in b + 1..t {
                        let start = window.index([l + 1, y]);
                        let end = window.index([r, y]);
                        for cell in &mut grid.terrain[start..end] { *cell |= FRAME; }
                    }
                }
                Ink::Wall => {
                    for y in b..=t {
                        let start = window.index([l, y]);
                        let vertical = (((y == t) as u8) << 1) | (((y == b) as u8) << 3);
                        for x in l..=r {
                            let outward = vertical | (x == r) as u8 | (((x == l) as u8) << 2);
                            let cell = &mut grid.terrain[start + (x - l) as usize];
                            // A pin may leave only through exits allowed by every touching wall.
                            *cell = WALL | (*cell & (FRAME | outward));
                        }
                    }
                }
            }
        }
        grid
    }

    fn terrain(&self, at: Pt) -> u8 {
        self.terrain[self.window.index(at)]
    }

    fn commit(&mut self, at: Pt, net: u32, edges: u8) {
        if edges == 0 { return; }
        let owners = if self.window.contains(at) {
            let cell = self.window.index(at);
            let slot = &mut self.wire_slots[cell];
            let lists = &mut self.wire_owners[cell / WIRE_GROUP_CELLS];
            if *slot == 0 {
                lists.push(Vec::new());
                *slot = lists.len() as u32;
            }
            &mut lists[*slot as usize - 1]
        } else {
            self.outside_wires.entry(at).or_default()
        };
        if let Some((_, mask)) = owners.iter_mut().find(|(id, _)| *id == net) {
            *mask |= edges;
        } else {
            owners.push((net, edges));
        }
    }

    fn foreign_edges(&self, at: Pt, net: u32) -> u8 {
        let owners = if self.window.contains(at) {
            let cell = self.window.index(at);
            let slot = self.wire_slots[cell];
            if slot == 0 { return 0; }
            &self.wire_owners[cell / WIRE_GROUP_CELLS][slot as usize - 1]
        } else {
            let Some(owners) = self.outside_wires.get(&at) else { return 0 };
            owners
        };
        owners.iter().filter(|(id, _)| *id != net).fold(0, |mask, (_, edges)| mask | edges)
    }
}

#[derive(Default)]
struct Tree {
    edges: HashMap<Pt, u8>,
    segments: Vec<[Pt; 2]>,
    runs: Vec<Vec<Pt>>,
    junctions: Vec<Pt>,
}

impl Tree {
    fn insert(&mut self, run: &[Pt], search: &mut Search) {
        wire_cells(run, |at, mask| {
            let edges = self.edges.entry(at).or_insert_with(|| {
                search.mark(at, ON_TREE);
                0
            });
            *edges |= mask;
        });
        if let [p] = run { self.segments.push([*p, *p]); }
        self.segments.extend(run.windows(2).map(|s| [s[0], s[1]]));
    }

    // Clamping to a segment picks its nearest grid point, including hidden collinear steps.
    fn distance(&self, [x, y]: Pt) -> u64 {
        self.segments.iter().map(|&[[ax, ay], [bx, by]]| {
            let dx = x.abs_diff(x.clamp(ax.min(bx), ax.max(bx)));
            let dy = y.abs_diff(y.clamp(ay.min(by), ay.max(by)));
            u64::from(dx) + u64::from(dy)
        }).min().expect("a routing tree has a seed")
    }

    fn add(&mut self, path: &[Pt], pin_first: bool, search: &mut Search) {
        let mut run = vec![path[0]];
        let mut prev = START;
        for edge in path.windows(2) {
            let [x, y] = edge[0];
            let [nx, ny] = edge[1];
            let dir = direction([nx - x, ny - y]).expect("a route advances by one grid step");
            if prev != START && prev != dir { run.push(edge[0]); }
            prev = dir;
        }
        run.push(path[path.len() - 1]);
        self.insert(&run, search);
        if self.edges[&path[0]].count_ones() >= 3 { self.junctions.push(path[0]); }
        if pin_first { run.reverse(); }
        self.runs.push(run);
    }
}

#[derive(Clone, Copy)]
struct CellVisits {
    cost: [u64; 4],
    previous_dir: [u8; 4],
}

const UNVISITED: CellVisits = CellVisits { cost: [u64::MAX; 4], previous_dir: [START as u8; 4] };
const TILE_SIDE: usize = 16;
const TILE_CELLS: usize = TILE_SIDE * TILE_SIDE;
// The low four bits of a net's cell flags are its pin directions.
const ON_TREE: u8 = 1 << 4;

struct Tile {
    net_epoch: usize,
    flags: [u8; TILE_CELLS],
    visit_epoch: usize,
    visits: [CellVisits; TILE_CELLS],
}

/// Scratch shared by all connections in a route. Slots are one-based so zero is an untouched
/// tile. Boxing each tile keeps its cells in place when the pool grows.
struct Search {
    origin: Pt,
    columns: usize,
    slots: Vec<usize>,
    tiles: Vec<Box<Tile>>,
    // Each net epoch uses a distinct input net, each visit epoch a distinct input term.
    net_epoch: usize,
    visit_epoch: usize,
    queue: BinaryHeap<Reverse<(u64, u64, i32, i32, usize)>>,
}

impl Search {
    fn new(window: Window) -> Self {
        let [l, b, _, t] = window.bounds;
        let columns = window.width.div_ceil(TILE_SIDE);
        let rows = ((t - b) as usize + 1).div_ceil(TILE_SIDE);
        Self {
            origin: [l, b], columns, slots: vec![0; rows * columns], tiles: Vec::new(),
            net_epoch: 0, visit_epoch: 0, queue: BinaryHeap::new(),
        }
    }

    fn tile(&mut self, [x, y]: Pt) -> (&mut Tile, usize) {
        let x = (x - self.origin[0]) as usize;
        let y = (y - self.origin[1]) as usize;
        let slot = &mut self.slots[(y / TILE_SIDE) * self.columns + x / TILE_SIDE];
        if *slot == 0 {
            self.tiles.push(Box::new(Tile {
                net_epoch: self.net_epoch, flags: [0; TILE_CELLS],
                visit_epoch: self.visit_epoch, visits: [UNVISITED; TILE_CELLS],
            }));
            *slot = self.tiles.len();
        }
        (&mut self.tiles[*slot - 1], (y % TILE_SIDE) * TILE_SIDE + x % TILE_SIDE)
    }

    fn mark(&mut self, at: Pt, bits: u8) {
        let epoch = self.net_epoch;
        let (tile, cell) = self.tile(at);
        if tile.net_epoch != epoch {
            tile.flags.fill(0);
            tile.net_epoch = epoch;
        }
        tile.flags[cell] |= bits;
    }

    fn cell(&mut self, at: Pt) -> (&mut CellVisits, u8) {
        let net_epoch = self.net_epoch;
        let visit_epoch = self.visit_epoch;
        let (tile, cell) = self.tile(at);
        if tile.visit_epoch != visit_epoch {
            tile.visits.fill(UNVISITED);
            tile.visit_epoch = visit_epoch;
        }
        let flags = if tile.net_epoch == net_epoch { tile.flags[cell] } else { 0 };
        (&mut tile.visits[cell], flags)
    }
}

// Recover the branch from the tree out to the pin. Equal-cost states (zero-cost options)
// can visit a cell in two directions. Keep its last exit to erase any such loop.
fn path_to_pin(mut at: Pt, mut dir: usize, search: &mut Search) -> Vec<Pt> {
    let join = at;
    let mut exits = HashMap::new();
    while dir != START {
        let (visits, _) = search.cell(at);
        let prev = visits.previous_dir[dir] as usize;
        let next = moved(at, dir ^ 2).expect("a predecessor is on the grid");
        exits.insert(at, next);
        at = next;
        dir = prev;
    }
    let mut path = vec![join];
    while let Some(&next) = exits.get(path.last().unwrap()) { path.push(next); }
    path
}

/// Find the nearest tree point in O(segments), then search the reachable box for a join.
fn connect(grid: &Grid, tree: &Tree, net: u32, term: RTerm, o: &RouterOpts, search: &mut Search) -> Option<Vec<Pt>> {
    let [x, y] = term.at;
    let nearest = tree.segments.iter().map(|&[[ax, ay], [bx, by]]| [x.clamp(ax.min(bx), ax.max(bx)), y.clamp(ay.min(by), ay.max(by))])
        .min_by_key(|p| ((p[0] - x).abs() + (p[1] - y).abs(), p[1], p[0]))?;
    if (nearest[0] - x).abs() + (nearest[1] - y).abs() > o.reach { return None; }
    let mut bounds = [x, y, x, y];
    include(&mut bounds, nearest);
    let [l, b, r, t] = bounds;
    let [left, bottom, right, top] = grid.window.bounds;
    let reach = o.reach.max(o.margin.max(0));
    let most = Window::new([(l - reach).max(left), (b - reach).max(bottom), (r + reach).min(right), (t + reach).min(top)]);

    let first = direction(term.dir)?;
    let h = |at| u64::from(tree.distance(at)) * u64::from(o.step);
    search.visit_epoch += 1;
    search.queue.clear();
    let initial = h(term.at);
    search.queue.push(Reverse((initial, initial, y, x, START)));
    while let Some(Reverse((f, estimate, y, x, incoming))) = search.queue.pop() {
        let at = [x, y];
        let g = f - estimate;
        let (visits, flags) = search.cell(at);
        if incoming != START {
            if visits.cost[incoming] != g { continue; }
            if flags & ON_TREE != 0 { return Some(path_to_pin(at, incoming, search)); }
        }
        let here = grid.terrain(at);
        let wire = grid.foreign_edges(at, net);
        let pin = flags & ALL;
        for dir in 0..4 {
            if incoming == START {
                if dir != first { continue; }
            } else {
                if dir == (incoming ^ 2) { continue; }
                // A branch passing through a future pin must include its outward edge.
                let incident = (1 << (incoming ^ 2)) | (1 << dir);
                if pin & !incident != 0 { continue; }
            }
            let Some(to) = moved(at, dir) else { continue };
            if to == term.at { continue; }
            if !most.contains(to) { continue; }
            let next = grid.terrain(to);
            let (entry, next_flags) = search.cell(to);
            let next_pin = next_flags & ALL;
            let back = 1 << (dir ^ 2);
            if here & (1 << dir) == 0 || next & back == 0 || (next & WALL != 0 && next_pin == 0) {
                continue;
            }
            let on_tree = next_flags & ON_TREE != 0;
            if on_tree && next_pin != 0 && next_pin & back == 0 { continue; }
            let next_wire = grid.foreign_edges(to, net);
            let axis = (1 << dir) | back;
            if (wire | next_wire) & axis != 0 { continue; }
            // A foreign straight wire can be crossed. A bend, junction or existing crossing
            // has edges on both axes and therefore admits neither move.
            let mut cost = g + u64::from(o.step);
            if incoming != START && incoming != dir { cost += u64::from(o.bend); }
            if next & FRAME != 0 { cost += u64::from(o.frame); }
            if next_wire != 0 { cost += u64::from(o.cross); }
            if cost >= entry.cost[dir] { continue; }
            entry.cost[dir] = cost;
            entry.previous_dir[dir] = incoming as u8;
            let estimate = h(to);
            search.queue.push(Reverse((cost + estimate, estimate, to[1], to[0], dir)));
        }
    }
    None
}

fn route_tree(grid: &Grid, net: &RNet, o: &RouterOpts, search: &mut Search) -> Option<Tree> {
    search.net_epoch += 1;
    let mut tree = Tree::default();
    for run in &net.tree { tree.insert(run, search); }
    let seeded = !tree.edges.is_empty();
    if !seeded { tree.insert(&[net.terms[0].at], search); }
    let mut pending = Vec::new();
    for (i, &term) in net.terms.iter().enumerate() {
        if seeded && tree.edges.contains_key(&term.at) { continue; }
        let dir = direction(term.dir)?;
        if grid.terrain(term.at) & (1 << dir) == 0 { return None; }
        search.mark(term.at, 1 << dir);
        if seeded || i != 0 { pending.push(term); }
    }
    while !pending.is_empty() {
        let (i, _) = pending.iter().enumerate().min_by_key(|(_, t)| tree.distance(t.at))?;
        let term = pending.remove(i);
        if let Some(&edges) = tree.edges.get(&term.at) {
            if edges == 0 || edges & (1 << direction(term.dir)?) != 0 { continue; }
            return None;
        }
        let path = connect(grid, &tree, net.net, term, o, search)?;
        tree.add(&path, seeded, search);
    }
    Some(tree)
}

/// Routes short nets first. Each bundle goes as a block at its shortest member's place, in the caller's order.
/// Drawn wires reserve space from the start, new branches on net completion.
pub fn route(nets: &[RNet], drawn: &[(u32, Vec<Pt>)], obstacles: &[Obstacle], o: &RouterOpts) -> SheetRoute {
    let mut result = SheetRoute::default();
    let mut order: Vec<_> = nets.iter().enumerate().filter(|(_, n)| {
        n.terms.len() >= 2 || (n.terms.len() == 1 && n.tree.iter().any(|run| !run.is_empty()))
    }).collect();
    if order.is_empty() { return result; }
    let net_key = |n: &RNet| {
        let [l, b, r, t] = bounds(&n.terms);
        (u64::from(l.abs_diff(r)) + u64::from(b.abs_diff(t)), n.net)
    };
    let mut leaders = std::collections::BTreeMap::new();
    for (_, n) in &order {
        if let Some((id, _)) = n.bundle {
            let key = net_key(n);
            let leader = leaders.entry(id).or_insert(key);
            *leader = key.min(*leader);
        }
    }
    order.sort_by_cached_key(|(_, n)| {
        let leader = match n.bundle {
            Some((id, _)) => leaders[&id],
            None => net_key(n),
        };
        (leader, n.bundle, n.net)
    });
    let mut window = bounds(&order[0].1.terms);
    for term in nets.iter().flat_map(|n| &n.terms) { include(&mut window, term.at); }
    for run in nets.iter().flat_map(|n| &n.tree).chain(drawn.iter().map(|(_, run)| run)) {
        for &at in run { include(&mut window, at); }
    }
    for obs in obstacles {
        include(&mut window, [obs.bx[0], obs.bx[1]]);
        include(&mut window, [obs.bx[2], obs.bx[3]]);
    }
    let m = o.margin.max(0);
    let [l, b, r, t] = window;
    let mut grid = Grid::new([l - m, b - m, r + m, t + m], obstacles);
    let mut search = Search::new(grid.window);
    let mut failed = vec![false; nets.len()];
    for (net, run) in drawn { wire_cells(run, |at, mask| grid.commit(at, *net, mask)); }
    for (i, net) in order {
        let Some(mut tree) = route_tree(&grid, net, o, &mut search) else { failed[i] = true; continue };
        for (at, edges) in tree.edges { grid.commit(at, net.net, edges); }
        tree.junctions.sort_unstable();
        tree.junctions.dedup();
        result.junctions.extend(tree.junctions.into_iter().map(|p| (net.net, p)));
        result.runs.extend(tree.runs.into_iter().map(|run| (net.net, run)));
    }
    result.unrouted.extend(nets.iter().zip(failed).filter(|(_, fail)| *fail).map(|(n, _)| n.net));
    result
}

#[cfg(test)]
#[path = "sheet_router_tests.rs"]
mod tests;
