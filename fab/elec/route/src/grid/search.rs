//! Deterministic A* on `(layer, cell, incoming direction)` states.
//! Directions 0..8 run counter-clockwise from +X; 8 means none, at sources and after vias.
//! Eight-neighbour moves cost 5 orthogonally or 7 diagonally, plus each 45-degree bend,
//! orthogonal motion across a layer preference, and the destination's soft cost. Turns
//! greater than 90 degrees are forbidden. Through-vias cost `via`, reset direction, and
//! require the via plane to be free on every stack layer, even disallowed routing layers.
//!
//! Targets are grouped by `(x / GROUP, y / GROUP)` into cell bounding boxes, ignoring layers.
//! The heuristic is the minimum octile distance to these boxes. Each distance is a lower
//! bound on reaching a target in its box and is 1-Lipschitz in the 5/7 octile metric, so
//! each spatial move reduces it by at most its base cost; vias leave it unchanged. The
//! minimum is admissible and consistent, and all added costs are nonnegative. `GROUP` sets only the
//! heuristic's resolution (more groups: tighter h, more work per state), never what can be found.
//!
//! The heap orders `(f, h, row, column, layer, direction)` lexicographically (row then column is
//! the linear cell index's order; the four name the state, so no two entries of distinct states tie).
//! Strict improvements alone replace parents; stale heap entries are discarded. State
//! storage uses dense sub-tiles allocated on first touch, with O(1) lookup and no hashing: a
//! cell's nine direction states sit together (best cost, a parent code, a popped bit each).
//!
//! A pop skips what an earlier pop of the same (layer, cell) already offered at no more cost:
//! a step in direction `e` when a popped direction `d` that may turn to `e` has
//! `g_d + bend(d, e) <= g + bend(dir, e)`, and the via moves when a popped direction has
//! `g_d <= g`. Every other term of such an offer depends only on the target state, so the
//! offer would be refused by the `best <= g` test; skipping it changes no result or count and
//! never reads the neighbour's occupancy, guard or cost. A popped bit clears when its state
//! improves, so it always means "offered at the state's current cost".
//! Layer/target lists are sorted and deduplicated, neighbours have fixed order, and all
//! costs, indices and decisions are integers. The first target popped ends the search.
//!
//! Plain search runs an exact reachability flood in lockstep with A*: one node per A* state
//! popped, alternating breadth-first floods from both endpoint sets, one byte per node in
//! touched sub-tiles. The flood stops when the sides meet, so it at most doubles a found
//! route's work; when one side empties first, the search fails after about twice that
//! side's nodes instead of exhausting the other side's states.
//!
//! With `V = 9 * window cells * allowed layers` states and `E = O(V * (8 + layers))`
//! edges and `G` target boxes, search costs O((V + E) * (log(V + E) + G)) time with lazy
//! deletion, plus rasterisation and O(V * stack layers) via checks. State entries use
//! O(touched sub-tiles x 24 KB) memory, plus O(layers * ceil(cols / SUB) * ceil(rows / SUB))
//! slots and O(V + E) heap space. Target boxes use O(G) memory.
//! Sorting inputs costs O((sources + targets + layers) log(sources + targets + layers))
//! as an upper bound. The cache and all search work belong off the UI thread.
//!
//! Blockers search also enters rippable cells, never hard or fixed copper of another net.
//! Each entry pays every rippable owner's cost, including sources, track landings and every
//! stack layer of a via. Shared owners come from the caller's world using the grid's exact step
//! rule, cached once per (plane, layer, step) in lookup-only storage with a fixed multiplicative hasher.
//! Nonnegative rip costs preserve the heuristic and tie order. Beyond ordinary search, this
//! costs the owner queries, O(owner counts summed over entries considered) time, storage for
//! cached owner lists, and O(N log N) time to sort N owner occurrences on the returned path.
//!
//! Plain search with a guard table (`Query::guard`) and a nonzero `Costs::guard` adds that cost
//! for each entered cell another net's pour-only pad guards, and once per guarded stack layer of
//! a via: one hash lookup per considered move or via layer. The cost is nonnegative, so the
//! heuristic stays admissible and consistent. Without a table or with a zero cost nothing is
//! looked up and the search is unchanged; the blockers search never pays it.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};

use fab_elec_core::NetId;

use super::guard::{Guard, GuardTable};
use super::{Cell, CellBox, Grid, ObstacleSource, Pass, PlaneId};

/// A layer's preferred direction for orthogonal track steps.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Pref {
    /// No direction preference.
    None,
    /// Horizontal is preferred; vertical orthogonal steps pay `against`.
    Horizontal,
    /// Vertical is preferred; horizontal orthogonal steps pay `against`.
    Vertical,
}

/// Nonnegative routing penalties, in the same units as the 5/7 base step costs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Costs {
    /// Penalty per 45 degrees of turn; 90 degrees costs twice this amount.
    pub bend45: u32,
    /// Cost of a through-via to any other allowed layer.
    pub via: u32,
    /// Penalty for an orthogonal step across the layer's preferred direction.
    pub against: u32,
    /// Preferences by layer index; missing entries mean [`Pref::None`].
    pub prefs: Vec<Pref>,
    /// Penalty for entering a cell another net's pour-only pad needs for its escape
    /// ([`Query::guard`]); a via pays it once per guarded layer. 0 turns the guard off.
    pub guard: u32,
}

impl Default for Costs {
    /// Bend 2, via 60, against 3, guard 60 (a via's cost), and no layer preferences.
    fn default() -> Costs {
        Costs { bend45: 2, via: 60, against: 3, prefs: Vec::new(), guard: 60 }
    }
}

/// Multi-source, multi-target routing request. Endpoints outside the grid, window or allowed
/// layers are ignored. Invalid allowed layer indices are ignored; plane IDs must be valid.
pub struct Query<'a> {
    /// Plane rasterised for the track width and rule class.
    pub track: PlaneId,
    /// Plane rasterised for the via pad diameter; `None` forbids layer changes.
    pub via: Option<PlaneId>,
    /// The routed net: its own copper is exempt from clearance and rip costs.
    pub net: NetId,
    /// Allowed layer indices; a layer absent from this list is never entered.
    pub layers: &'a [usize],
    /// Candidate source cells, each with its layer index.
    pub sources: &'a [(usize, Cell)],
    /// Candidate target cells, each with its layer index.
    pub targets: &'a [(usize, Cell)],
    /// Inclusive search bounds, intersected with the grid.
    pub window: CellBox,
    /// Guarded cells: entering one another net guards pays `Costs::guard` (plain search only;
    /// the blockers search ignores it). `None`: no guard.
    pub guard: Option<&'a GuardTable>,
}

/// An ordered route, including both endpoints and repeated cells at layer changes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GridPath {
    /// Consecutive entries are eight-neighbours on one layer or the same cell on two layers.
    pub cells: Vec<(usize, Cell)>,
    /// Total integer step, turn, preference, soft, guard, via and (in blockers search) rip cost.
    pub cost: u64,
}

impl GridPath {
    /// Number of layer changes (through-vias) in this path.
    pub fn vias(&self) -> usize {
        self.cells.windows(2).filter(|pair| pair[0].0 != pair[1].0).count()
    }
}

/// What crossing rippable copper costs, and who owns a shared step.
pub struct Rip<'a> {
    /// Cost of crossing one step for each rippable net in its reach (for example the net's priority
    /// plus its rip history), added on top of the step's normal cost. Must be nonnegative.
    pub cost: &'a dyn Fn(NetId) -> u32,
    /// The rippable nets of a shared step (`Pass::Rip(None)`) from the first cell to the second
    /// on layer index `li` for plane `plane`. The same cell twice denotes its centre. A via plane
    /// is always asked with the same cell twice. The caller computes these nets from its world
    /// with the grid's exact step rule, sorted and deduplicated, never including the routed net.
    pub owners: &'a mut dyn FnMut(PlaneId, usize, Cell, Cell) -> Vec<NetId>,
}

/// A blockers-search result: the route, the sorted, deduplicated nets it crosses, and where.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Blocked {
    /// Route whose cost includes every rip penalty.
    pub path: GridPath,
    /// Rippable nets encountered by track cells and through-vias.
    pub nets: Vec<NetId>,
    /// The (layer index, cell) of every entry that paid a rip cost, in path order, each once: a
    /// track step's far cell, a source or via landing centre, and a via's cell on each stack layer
    /// it paid on. Every net in `nets` was paid for at one of them, so `nets` is non-empty only
    /// when this is.
    pub contested: Vec<(usize, Cell)>,
}

const DIRECTIONS: [(i64, i64); 8] = [
    (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1),
];
const NONE: u8 = 8;
const SUB: u32 = 16;
const GROUP: u32 = 32;

/// One (layer, cell)'s nine direction states.
#[derive(Clone, Copy)]
struct CellStates {
    /// Best known cost per direction state; `u64::MAX` when unseen.
    g: [u64; 9],
    /// Per state, how to find its parent: `ORIGIN` for a source; for a step (direction below
    /// `NONE`) the parent's direction, in the cell one step back on this layer; for a via
    /// arrival (`NONE`) `layer * 9 + direction` of the parent in this cell.
    parent: [u16; 9],
    /// Bit `d`: state `d` was popped at its current cost and has made its offers.
    popped: u16,
}

const ORIGIN: u16 = u16::MAX;
const UNSEEN: CellStates = CellStates { g: [u64::MAX; 9], parent: [ORIGIN; 9], popped: 0 };

struct States {
    slots: Vec<Option<Box<[CellStates]>>>,
    sub_cols: usize,
    sub_count: usize,
}

impl States {
    fn new(cols: u32, rows: u32, layers: usize) -> States {
        let sub_cols = u64::from((cols - 1) / SUB + 1);
        let sub_rows = u64::from((rows - 1) / SUB + 1);
        let sub_count = usize::try_from(sub_cols * sub_rows).expect("search sub-tile slots must fit usize");
        let count = sub_count.checked_mul(layers).expect("search sub-tile slots must fit usize");
        States { slots: vec![None; count], sub_cols: sub_cols as usize, sub_count }
    }

    fn at(&self, li: usize, c: Cell) -> (usize, usize) {
        let slot = li * self.sub_count + (c.y / SUB) as usize * self.sub_cols + (c.x / SUB) as usize;
        (slot, ((c.y % SUB) * SUB + c.x % SUB) as usize)
    }

    fn cell(&self, li: usize, c: Cell) -> &CellStates {
        let (slot, entry) = self.at(li, c);
        self.slots[slot].as_ref().map_or(&UNSEEN, |cells| &cells[entry])
    }

    fn cell_mut(&mut self, li: usize, c: Cell) -> &mut CellStates {
        let (slot, entry) = self.at(li, c);
        let cells = self.slots[slot]
            .get_or_insert_with(|| vec![UNSEEN; (SUB * SUB) as usize].into_boxed_slice());
        &mut cells[entry]
    }
}

struct ReachMarks {
    slots: Vec<Option<Box<[u8]>>>,
    sub_cols: usize,
    sub_count: usize,
}

impl ReachMarks {
    fn new(cols: u32, rows: u32, layers: usize) -> ReachMarks {
        let sub_cols = u64::from((cols - 1) / SUB + 1);
        let sub_rows = u64::from((rows - 1) / SUB + 1);
        let sub_count = usize::try_from(sub_cols * sub_rows).expect("search sub-tile slots must fit usize");
        let count = sub_count.checked_mul(layers).expect("search sub-tile slots must fit usize");
        ReachMarks { slots: vec![None; count], sub_cols: sub_cols as usize, sub_count }
    }

    /// Mark before pushing; return true if the other side already reached this node.
    fn push(&mut self, li: usize, c: Cell, side: u8, queue: &mut VecDeque<(usize, Cell)>) -> bool {
        let slot = li * self.sub_count + (c.y / SUB) as usize * self.sub_cols + (c.x / SUB) as usize;
        let entry = ((c.y % SUB) * SUB + c.x % SUB) as usize;
        let nodes = self.slots[slot].get_or_insert_with(|| vec![0; (SUB * SUB) as usize].into_boxed_slice());
        if nodes[entry] != 0 {
            return nodes[entry] != side;
        }
        nodes[entry] = side;
        queue.push_back((li, c));
        false
    }
}

/// What the reachability flood has learned so far.
enum Reached {
    /// Both sides still have nodes to pop.
    Pending,
    /// The sides met: a route exists in the window.
    Met,
    /// One side ran out of nodes: no route exists in the window. `clipped` says whether that
    /// side tried an eight-neighbour step out of the window but inside the grid.
    Cut { clipped: bool },
}

/// Reachability over (layer, cell) nodes joined by passable track steps ([`Grid::step_pass`])
/// and through-vias that clear every stack layer onto a passable landing centre. A step or via
/// is passable when it is free, or with ripping (`ripping`) when it is not blocked: a rip cost is
/// finite, so it changes which route is cheapest, never whether one exists.
/// A* adds direction states and forbids turns over 90 degrees; the flood takes every step, so
/// its edges are a superset of A*'s and a cut means A* would fail. Meeting is no promise (a
/// sharp turn's shortcut step need not be passable): it only ends the flood, which changes no
/// A* input.
struct Reach {
    marks: ReachMarks,
    ripping: bool,
    /// Side 0 floods from the sources, side 1 from the targets.
    queues: [VecDeque<(usize, Cell)>; 2],
    clipped: [bool; 2],
    side: usize,
}

fn passable(pass: Pass, ripping: bool) -> bool {
    if ripping { pass != Pass::Blocked } else { pass == Pass::Free }
}

impl Reach {
    /// Mark both endpoint sets before either side pops. Returns the flood, or what the
    /// seeds alone decide (an endpoint on both sides, or a side with no passable node).
    fn seed(
        grid: &mut Grid,
        src: &dyn ObstacleSource,
        q: &Query<'_>,
        layers: &[usize],
        window: CellBox,
        targets: &[(usize, Cell)],
        ripping: bool,
    ) -> Result<Reach, Reached> {
        let mut reach = Reach {
            marks: ReachMarks::new(grid.spec.cols, grid.spec.rows, grid.spec.layers.len()),
            ripping,
            queues: [VecDeque::new(), VecDeque::new()],
            clipped: [false; 2],
            side: 0,
        };
        for (side, endpoints) in [q.sources, targets].into_iter().enumerate() {
            for &(li, c) in endpoints {
                if window.contains(c) && layers.binary_search(&li).is_ok()
                    && passable(grid.step_pass(src, q.track, li, c, c, q.net), ripping)
                    && reach.marks.push(li, c, 1 << side, &mut reach.queues[side])
                {
                    return Err(Reached::Met);
                }
            }
        }
        if reach.queues.iter().any(VecDeque::is_empty) {
            return Err(Reached::Cut { clipped: false });
        }
        Ok(reach)
    }

    /// Pop one node of the current side, then hand over to the other side.
    fn step(
        &mut self,
        grid: &mut Grid,
        src: &dyn ObstacleSource,
        q: &Query<'_>,
        layers: &[usize],
        window: CellBox,
        work: &mut u64,
    ) -> Reached {
        let side = self.side;
        let (li, c) = self.queues[side].pop_front().expect("both reachability sides have pending nodes");
        *work = work.checked_add(1).expect("search work counter must fit u64");
        for &(dx, dy) in &DIRECTIONS {
            let (x, y) = (i64::from(c.x) + dx, i64::from(c.y) + dy);
            if x < i64::from(window.min.x) || x > i64::from(window.max.x)
                || y < i64::from(window.min.y) || y > i64::from(window.max.y)
            {
                self.clipped[side] |= x >= 0 && x < i64::from(grid.spec.cols)
                    && y >= 0 && y < i64::from(grid.spec.rows);
                continue;
            }
            let next = Cell { x: x as u32, y: y as u32 };
            if passable(grid.step_pass(src, q.track, li, c, next, q.net), self.ripping)
                && self.marks.push(li, next, 1 << side, &mut self.queues[side])
            {
                return Reached::Met;
            }
        }
        if let Some(via) = q.via {
            if layers.len() > 1 && (0..grid.spec.layers.len()).all(|layer| {
                passable(grid.state(src, via, layer, c).pass(q.net), self.ripping)
            }) {
                for &next_li in layers {
                    if next_li != li && passable(grid.step_pass(src, q.track, next_li, c, c, q.net), self.ripping)
                        && self.marks.push(next_li, c, 1 << side, &mut self.queues[side])
                    {
                        return Reached::Met;
                    }
                }
            }
        }
        if self.queues[side].is_empty() {
            return Reached::Cut { clipped: self.clipped[side] };
        }
        self.side ^= 1;
        Reached::Pending
    }
}

#[derive(Default)]
struct StateHasher(u64);

impl Hasher for StateHasher {
    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 32)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

/// `(f, h, row, column, layer, direction)`; `Grid::new` keeps layer indices below 256.
type Open = BinaryHeap<Reverse<(u64, u64, u32, u32, u8, u8)>>;
type Owners = HashMap<(PlaneId, usize, Cell, Cell), Vec<NetId>, BuildHasherDefault<StateHasher>>;

fn entry_cost(
    pass: Pass,
    plane: PlaneId,
    li: usize,
    a: Cell,
    b: Cell,
    rip: Option<&mut Rip<'_>>,
    owners: &mut Owners,
) -> Option<u64> {
    match pass {
        Pass::Free => Some(0),
        Pass::Blocked => None,
        Pass::Rip(Some(net)) => Some(u64::from((rip?.cost)(net))),
        Pass::Rip(None) => {
            let rip = rip?;
            let key = (plane, li, a.min(b), a.max(b));
            let nets = owners.entry(key).or_insert_with(|| (rip.owners)(plane, li, a, b));
            Some(nets.iter().fold(0u64, |total, &net| {
                total.checked_add(u64::from((rip.cost)(net))).expect("rip cost must fit u64")
            }))
        }
    }
}

struct Frontier {
    states: States,
    open: Open,
    target_boxes: Option<Vec<CellBox>>,
}

impl Frontier {
    fn offer(&mut self, li: usize, c: Cell, dir: u8, g: u64, parent: u16) {
        let cell = self.states.cell_mut(li, c);
        let d = usize::from(dir);
        if cell.g[d] <= g {
            return;
        }
        let h = self.target_boxes.as_ref().map_or(0, |boxes| {
            boxes.iter().map(|&bounds| octile(c, bounds)).min().unwrap_or(0)
        });
        let f = g.checked_add(h).expect("search priority must fit u64");
        cell.g[d] = g;
        cell.parent[d] = parent;
        cell.popped &= !(1 << d);
        self.open.push(Reverse((f, h, c.y, c.x, li as u8, dir)));
    }

    fn path(&self, mut li: usize, mut c: Cell, mut dir: u8, cost: u64) -> GridPath {
        let mut cells = Vec::new();
        loop {
            cells.push((li, c));
            let code = self.states.cell(li, c).parent[usize::from(dir)];
            if code == ORIGIN {
                break;
            }
            if dir == NONE {
                (li, dir) = (usize::from(code / 9), (code % 9) as u8);
            } else {
                let (dx, dy) = DIRECTIONS[usize::from(dir)];
                c = Cell { x: (i64::from(c.x) - dx) as u32, y: (i64::from(c.y) - dy) as u32 };
                dir = code as u8;
            }
        }
        cells.reverse();
        GridPath { cells, cost }
    }
}

/// Turns of 45 degrees from arriving direction `from` to step direction `to`; none from `NONE`.
fn turns(from: u8, to: u8) -> u8 {
    if from == NONE {
        0
    } else {
        let difference = from.abs_diff(to);
        difference.min(8 - difference)
    }
}

fn octile(c: Cell, bounds: CellBox) -> u64 {
    let dx = u64::from(bounds.min.x.saturating_sub(c.x).max(c.x.saturating_sub(bounds.max.x)));
    let dy = u64::from(bounds.min.y.saturating_sub(c.y).max(c.y.saturating_sub(bounds.max.y)));
    7 * dx.min(dy) + 5 * (dx.max(dy) - dx.min(dy))
}

fn target_boxes(targets: &[(usize, Cell)], group: u32) -> Vec<CellBox> {
    let mut cells: Vec<_> = targets.iter().map(|&(_, c)| c).collect();
    cells.sort_unstable_by_key(|c| (c.x / group, c.y / group));
    let mut boxes: Vec<CellBox> = Vec::new();
    let mut previous = None;
    for c in cells {
        let bucket = (c.x / group, c.y / group);
        let bounds = CellBox { min: c, max: c };
        if previous == Some(bucket) {
            let last = boxes.last_mut().expect("a previous bucket has a box");
            *last = last.union(bounds);
        } else {
            boxes.push(bounds);
            previous = Some(bucket);
        }
    }
    boxes.sort_unstable_by_key(|b| (b.min.y, b.min.x, b.max.y, b.max.x));
    boxes.dedup();
    boxes
}

/// A* from any usable source to any target inside `q.window`, with an exact reachability
/// flood in lockstep. `work` gains one per non-stale A* state popped, including the goal, and
/// one per flood node popped. Returns `None` when no route exists in the window.
/// Cost/priority and work-counter overflow panic rather than wrap.
pub fn search(
    grid: &mut Grid,
    src: &dyn ObstacleSource,
    q: &Query<'_>,
    costs: &Costs,
    work: &mut u64,
) -> Option<GridPath> {
    search_inner(grid, src, q, costs, None, true, work).map(|result| result.path)
}

/// A* allowing rippable copper at its per-step cost, returning the owners crossed by the route
/// and the cells where it paid for them.
/// Sources pay their track-plane rip cost; vias pay every stack layer plus the track landing.
/// Shared owners are queried at most once per (plane, layer, step) during this search.
/// The same exact reachability flood runs in lockstep on every node that is not blocked, so a
/// net that no ripping can connect fails without A* exhausting the window; `work` counts as in
/// [`search`]. Window and overflow behaviour are the same as [`search`].
pub fn search_blockers(
    grid: &mut Grid,
    src: &dyn ObstacleSource,
    q: &Query<'_>,
    costs: &Costs,
    rip: &mut Rip<'_>,
    work: &mut u64,
) -> Option<Blocked> {
    search_inner(grid, src, q, costs, Some(rip), true, work)
}

fn search_inner(
    grid: &mut Grid,
    src: &dyn ObstacleSource,
    q: &Query<'_>,
    costs: &Costs,
    rip: Option<&mut Rip<'_>>,
    heuristic: bool,
    work: &mut u64,
) -> Option<Blocked> {
    search_with_group_size(grid, src, q, costs, rip, heuristic.then_some(GROUP), true, work, &mut false)
}

fn search_with_group_size(
    grid: &mut Grid,
    src: &dyn ObstacleSource,
    q: &Query<'_>,
    costs: &Costs,
    mut rip: Option<&mut Rip<'_>>,
    group: Option<u32>,
    check_reachability: bool,
    work: &mut u64,
    clipped: &mut bool,
) -> Option<Blocked> {
    *clipped = false;
    let all = grid.spec.all();
    let window = q.window.intersection(all)?;
    let mut layers: Vec<_> = q.layers.iter().copied().filter(|&li| li < grid.spec.layers.len()).collect();
    layers.sort_unstable();
    layers.dedup();
    // Check both endpoint sets before an empty target set can end the search.
    *clipped = q.sources.iter().chain(q.targets).any(|&(li, c)| {
        !window.contains(c) && all.contains(c) && layers.binary_search(&li).is_ok()
    });
    let valid = |&(li, c): &(usize, Cell)| window.contains(c) && layers.binary_search(&li).is_ok();
    let mut targets: Vec<_> = q.targets.iter().copied().filter(valid).collect();
    targets.sort_unstable();
    targets.dedup();
    targets.first()?;
    let mut reach = None;
    if check_reachability {
        match Reach::seed(grid, src, q, &layers, window, &targets, rip.is_some()) {
            Ok(flood) => reach = Some(flood),
            Err(Reached::Cut { clipped: cut }) => {
                *clipped |= cut;
                return None;
            }
            Err(_) => {}
        }
    }
    // A*'s own steps out of the window count only when A* itself runs out of states.
    let mut stepped_out = false;
    let mut frontier = Frontier {
        states: States::new(grid.spec.cols, grid.spec.rows, grid.spec.layers.len()),
        open: Open::new(),
        target_boxes: group.map(|size| target_boxes(&targets, size)),
    };
    let bend = u64::from(costs.bend45);
    let mut owners = Owners::default();
    let guarded = guard_rule(q, costs, rip.is_none());
    for &(li, c) in q.sources.iter().filter(|entry| valid(entry)) {
        let pass = grid.step_pass(src, q.track, li, c, c, q.net);
        if let Some(extra) = entry_cost(pass, q.track, li, c, c, rip.as_deref_mut(), &mut owners) {
            frontier.offer(li, c, NONE, extra, ORIGIN);
        }
    }
    while let Some(Reverse((f, h, y, x, li, dir))) = frontier.open.pop() {
        let (li, c) = (usize::from(li), Cell { x, y });
        let g = f - h;
        let cell = frontier.states.cell_mut(li, c);
        if g > cell.g[usize::from(dir)] {
            continue;
        }
        // What the earlier pops of this cell offered: per step direction, the least of
        // `g_d + bend(d, e)` over popped directions `d` that may turn to `e`; for vias, any `g_d <= g`.
        let mut offered = [u64::MAX; 8];
        let mut via_offered = false;
        let mut earlier = cell.popped;
        while earlier != 0 {
            let d = earlier.trailing_zeros() as u8;
            earlier &= earlier - 1;
            let gd = cell.g[usize::from(d)];
            via_offered |= gd <= g;
            for (e, least) in offered.iter_mut().enumerate() {
                let turn = turns(d, e as u8);
                if turn <= 2 {
                    *least = (*least).min(gd + u64::from(turn) * bend);
                }
            }
        }
        cell.popped |= 1 << dir;
        *work = work.checked_add(1).expect("search work counter must fit u64");
        if targets.binary_search(&(li, c)).is_ok() {
            let path = frontier.path(li, c, dir, g);
            let (mut nets, mut contested) = (Vec::new(), Vec::new());
            if rip.is_some() {
                let stack_layers = grid.spec.layers.len();
                let mut seen = std::collections::HashSet::new();
                let mut collect = |plane, layer, a, b| {
                    let pass = if plane == q.track {
                        grid.step_pass(src, plane, layer, a, b, q.net)
                    } else {
                        grid.state(src, plane, layer, a).pass(q.net)
                    };
                    match pass {
                        Pass::Rip(Some(net)) => nets.push(net),
                        Pass::Rip(None) => nets.extend_from_slice(&owners[&(plane, layer, a.min(b), a.max(b))]),
                        Pass::Free | Pass::Blocked => return,
                    }
                    if seen.insert((layer, b)) {
                        contested.push((layer, b));
                    }
                };
                let (layer, c) = path.cells[0];
                collect(q.track, layer, c, c);
                for pair in path.cells.windows(2) {
                    let ((li, a), (next_li, b)) = (pair[0], pair[1]);
                    if li == next_li {
                        collect(q.track, li, a, b);
                    } else {
                        let via = q.via.expect("a via move requires a via plane");
                        for layer in 0..stack_layers {
                            collect(via, layer, a, a);
                        }
                        collect(q.track, next_li, b, b);
                    }
                }
                nets.sort_unstable();
                nets.dedup();
            }
            return Some(Blocked { path, nets, contested });
        }
        if let Some(flood) = reach.as_mut() {
            match flood.step(grid, src, q, &layers, window, work) {
                Reached::Pending => {}
                Reached::Met => reach = None,
                Reached::Cut { clipped: cut } => {
                    *clipped |= cut;
                    return None;
                }
            }
        }
        for (next_dir, &(dx, dy)) in DIRECTIONS.iter().enumerate() {
            let next_dir = next_dir as u8;
            let turn = turns(dir, next_dir);
            if turn > 2 {
                continue;
            }
            let (x, y) = (i64::from(c.x) + dx, i64::from(c.y) + dy);
            if x < i64::from(window.min.x) || x > i64::from(window.max.x)
                || y < i64::from(window.min.y) || y > i64::from(window.max.y)
            {
                stepped_out |= x >= 0 && x < i64::from(grid.spec.cols)
                    && y >= 0 && y < i64::from(grid.spec.rows);
                continue;
            }
            if offered[usize::from(next_dir)] <= g + u64::from(turn) * bend {
                continue;
            }
            let next = Cell { x: x as u32, y: y as u32 };
            let pass = grid.step_pass(src, q.track, li, c, next, q.net);
            let Some(extra) = entry_cost(pass, q.track, li, c, next, rip.as_deref_mut(), &mut owners) else {
                continue;
            };
            let diagonal = dx != 0 && dy != 0;
            let against = match costs.prefs.get(li).copied().unwrap_or(Pref::None) {
                Pref::None => false,
                Pref::Horizontal => dx == 0,
                Pref::Vertical => dy == 0,
            };
            let step = if diagonal { 7 } else { 5 }
                + u64::from(turn) * u64::from(costs.bend45)
                + if against { u64::from(costs.against) } else { 0 }
                + u64::from(grid.extra(li, next))
                + if guarded(li, next) { u64::from(costs.guard) } else { 0 };
            let next_g = g.checked_add(step).and_then(|cost| cost.checked_add(extra))
                .expect("route cost must fit u64");
            frontier.offer(li, next, next_dir, next_g, u16::from(dir));
        }
        if let Some(via) = q.via {
            if layers.len() <= 1 || via_offered {
                continue;
            }
            let via_cost = (0..grid.spec.layers.len()).try_fold(u64::from(costs.via), |total, layer| {
                let pass = grid.state(src, via, layer, c).pass(q.net);
                let extra = entry_cost(pass, via, layer, c, c, rip.as_deref_mut(), &mut owners)?;
                let guard = if guarded(layer, c) { u64::from(costs.guard) } else { 0 };
                Some(total.checked_add(extra + guard).expect("via cost must fit u64"))
            });
            if let Some(via_cost) = via_cost {
                let next_g = g.checked_add(via_cost).expect("route cost must fit u64");
                for &next_li in &layers {
                    if next_li == li {
                        continue;
                    }
                    let pass = grid.step_pass(src, q.track, next_li, c, c, q.net);
                    if let Some(extra) = entry_cost(pass, q.track, next_li, c, c, rip.as_deref_mut(), &mut owners) {
                        let next_g = next_g.checked_add(extra).expect("route cost must fit u64");
                        frontier.offer(next_li, c, NONE, next_g, li as u16 * 9 + u16::from(dir));
                    }
                }
            }
        }
    }
    *clipped |= stepped_out;
    None
}

/// Whether entering layer index `li`, cell `c` pays `costs.guard` for `q.net`: plain search
/// (`plain`) with a table and a nonzero cost, on a cell another net guards.
fn guard_rule<'q>(q: &'q Query<'_>, costs: &Costs, plain: bool) -> impl Fn(usize, Cell) -> bool + 'q {
    let table = q.guard.filter(|_| plain && costs.guard > 0);
    let net = q.net;
    move |li, c| match table.and_then(|table| table.guard_at(li, c)) {
        Some(Guard::One(owner)) => owner != net,
        Some(Guard::Several) => true,
        None => false,
    }
}

/// Search `q.window`, then double each dimension about its centre and clamp to the grid,
/// retrying up to the full grid. Odd extra cells go to the upper side unless already at
/// that grid edge, when they go to the lower side to ensure progress. Returns the first
/// successful window's route; this need not be the cheapest route in a larger window.
/// Work accumulates across all attempts; an empty intersection with the grid returns `None`.
/// A failure stops widening if the side that ran out (a flood side, or A* itself) had no
/// eight-neighbour step leave the window while staying in the grid, and no in-grid source or
/// target on an existing allowed layer was excluded by the window. That side then reaches the
/// same nodes and endpoints in any larger window: vias stay in-cell and grid states are
/// window-independent.
/// An empty endpoint side also stays empty. Thus no larger window can succeed.
pub fn search_widening(
    grid: &mut Grid,
    src: &dyn ObstacleSource,
    q: &Query<'_>,
    costs: &Costs,
    work: &mut u64,
) -> Option<GridPath> {
    let all = grid.spec.all();
    let mut window = q.window.intersection(all)?;
    loop {
        let attempt = Query { window, ..*q };
        let mut clipped = false;
        if let Some(result) = search_with_group_size(grid, src, &attempt, costs, None,
            Some(GROUP), true, work, &mut clipped)
        {
            return Some(result.path);
        }
        if !clipped || window == all {
            return None;
        }
        let grow_axis = |lo: u32, hi: u32, end: u32| {
            let width = u64::from(hi) - u64::from(lo) + 1;
            let lower = if hi == end { width - width / 2 } else { width / 2 };
            let low = u64::from(lo).saturating_sub(lower);
            let high = (u64::from(hi) + width - lower).min(u64::from(end));
            (low as u32, high as u32)
        };
        let (x0, x1) = grow_axis(window.min.x, window.max.x, all.max.x);
        let (y0, y1) = grow_axis(window.min.y, window.max.y, all.max.y);
        window = CellBox { min: Cell { x: x0, y: y0 }, max: Cell { x: x1, y: y1 } };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::guard::Escape;
    use crate::grid::tests::{fixture, planes, random, VecSource};
    use crate::grid::{GridSpec, Owner, Probe};
    use fab_elec_core::{clearance, point_clearance, Angle, BBox, ClassId, ItemId, LayerId, Nm, PadIx, P, Shape};

    fn step_owners(spec: &GridSpec, src: &VecSource, net: NetId, w: Nm, li: usize, a: Cell, b: Cell) -> Vec<NetId> {
        let threshold = i64::from(src.need) + (i64::from(w) + 1) / 2;
        let (a, b) = (spec.centre(a), spec.centre(b));
        let mut nets = Vec::new();
        for (layer, shape, owner) in &src.items {
            if let Owner::Net(owner) = *owner {
                if owner != net && *layer == spec.layers[li] {
                    let gap = if a == b {
                        point_clearance(a, shape)
                    } else {
                        clearance(&Shape::Segment { a, b, w: 0 }, shape)
                    };
                    if i64::from(gap) < threshold {
                        nets.push(owner);
                    }
                }
            }
        }
        nets.sort_unstable();
        nets.dedup();
        nets
    }

    fn verify_blockers(grid: &mut Grid, src: &VecSource, q: &Query<'_>, result: &Blocked) {
        let path = &result.path;
        assert!(result.nets.is_empty() || !result.contested.is_empty(), "named nets have a contested cell");
        assert!(result.contested.iter().all(|&(_, cell)| path.cells.iter().any(|&(_, c)| c == cell)),
            "contested cells lie on the path's cells (a via's on any stack layer)");
        assert!(path.cells.first().is_some_and(|c| q.sources.contains(c)), "path starts at a source");
        assert!(path.cells.last().is_some_and(|c| q.targets.contains(c)), "path ends at a target");
        for &(li, c) in &path.cells {
            assert!(q.layers.contains(&li) && q.window.contains(c), "path stays on allowed layers in window");
        }
        let (li, c) = path.cells[0];
        assert_ne!(grid.step_pass(src, q.track, li, c, c, q.net), Pass::Blocked,
            "each path step is free or rippable");
        let mut dir = NONE;
        for pair in path.cells.windows(2) {
            let ((li, a), (next_li, b)) = (pair[0], pair[1]);
            let start = if li == next_li { a } else { b };
            assert_ne!(grid.step_pass(src, q.track, next_li, start, b, q.net), Pass::Blocked,
                "each path step is free or rippable");
            if li != next_li {
                assert_eq!(a, b, "a via does not change cell");
                let via = q.via.expect("a via move requires a via plane");
                for layer in 0..grid.spec.layers.len() {
                    assert_ne!(grid.state(src, via, layer, a).pass(q.net), Pass::Blocked,
                        "via cells on every stack layer are free or rippable");
                }
                dir = NONE;
            } else {
                let delta = (i64::from(b.x) - i64::from(a.x), i64::from(b.y) - i64::from(a.y));
                let next = DIRECTIONS.iter().position(|&d| d == delta)
                    .expect("a track step must be an eight-neighbour move") as u8;
                let difference = dir.abs_diff(next);
                let turn = if dir == NONE { 0 } else { difference.min(8 - difference) };
                assert!(turn <= 2, "track turns never exceed ninety degrees");
                dir = next;
            }
        }
    }

    #[test]
    fn bus_blockers() {
        let spec = GridSpec::covering(
            BBox::new(P::ZERO, P::new(2_000_000, 2_000_000)),
            100_000,
            vec![LayerId::F_CU],
        );
        let src = VecSource {
            items: (1..=3).map(|net| {
                let y = 700_000 + (net as Nm - 1) * 300_000;
                let shape = Shape::Segment {
                    a: P::new(-100_000, y),
                    b: P::new(2_100_000, y),
                    w: 150_000,
                };
                (LayerId::F_CU, shape, Owner::Net(NetId(net)))
            }).collect(),
            need: 150_000,
        };
        let mut grid = Grid::new(spec.clone());
        let track = grid.plane(Probe { class: ClassId(0), w: 150_000, drill: None });
        let sources = [(0, Cell { x: 10, y: 2 })];
        let targets = [(0, Cell { x: 10, y: 18 })];
        let q = Query {
            track,
            via: None,
            net: NetId(0),
            layers: &[0],
            sources: &sources,
            targets: &targets,
            window: spec.all(),
            guard: None,
        };
        let costs = Costs::default();
        let mut work = 0;
        assert!(search(&mut grid, &src, &q, &costs, &mut work).is_none(), "the bus blocks plain search");
        assert_eq!(grid.state(&src, track, 0, Cell { x: 10, y: 8 }).pass(q.net), Pass::Rip(None),
            "cells between tracks at minimum pitch are shared, not hard");
        let mut queried = std::collections::BTreeSet::new();
        let mut owners = |plane, li, a, b| {
            assert_eq!(plane, track, "the bus uses only the track plane");
            assert!(queried.insert((plane, li, a, b)), "shared owners are read at most once per search key");
            step_owners(&spec, &src, q.net, 150_000, li, a, b)
        };
        let mut rip = Rip { cost: &|_| 10, owners: &mut owners };
        let result = search_blockers(&mut grid, &src, &q, &costs, &mut rip, &mut work)
            .expect("rippable bus can be crossed");
        assert_eq!(result.nets, [NetId(1), NetId(2), NetId(3)], "a net across the bus names all three");
        assert!(!queried.is_empty(), "the route exercises shared owners");
        // Sixteen vertical steps, and per bus track the six steps that come within its need plus
        // half the width of it (under 300 um centre to centre), each costing ten.
        assert_eq!(result.path.cost, 16 * 5 + 3 * 6 * 10, "every step pays every rippable owner in reach");
        verify_blockers(&mut grid, &src, &q, &result);
    }

    #[test]
    fn blockers_prefer_cheap_nets() {
        let spec = GridSpec::covering(
            BBox::new(P::ZERO, P::new(3_000_000, 3_000_000)),
            100_000,
            vec![LayerId::F_CU],
        );
        let segment = |from, to, owner| {
            (LayerId::F_CU, Shape::Segment {
                a: P::new(1_500_000, from),
                b: P::new(1_500_000, to),
                w: 150_000,
            }, owner)
        };
        let sources = [(0, Cell { x: 3, y: 15 })];
        let targets = [(0, Cell { x: 27, y: 15 })];
        let costs = Costs::default();
        for (first, second, fixed, expected) in [(5, 50, false, 1), (50, 5, false, 2),
            (0, 50, true, 2), (50, 0, true, 2)]
        {
            let first_owner = if fixed { Owner::Fixed(NetId(1)) } else { Owner::Net(NetId(1)) };
            // The two gaps mirror each other about the endpoints' row; hard copper closes every bypass.
            let src = VecSource {
                items: vec![
                    segment(-300_000, 300_000, Owner::Hard),
                    segment(300_000, 1_200_000, first_owner),
                    segment(1_200_000, 1_800_000, Owner::Hard),
                    segment(1_800_000, 2_700_000, Owner::Net(NetId(2))),
                    segment(2_700_000, 3_300_000, Owner::Hard),
                ],
                need: 0,
            };
            let mut grid = Grid::new(spec.clone());
            let track = grid.plane(Probe { class: ClassId(0), w: 150_000, drill: None });
            let q = Query {
                track,
                via: None,
                net: NetId(0),
                layers: &[0],
                sources: &sources,
                targets: &targets,
                window: spec.all(),
                guard: None,
            };
            let cost = |net| if net == NetId(1) { first } else { second };
            let mut owners = |plane, li, a, b| {
                assert_eq!(plane, track, "the gaps use only the track plane");
                step_owners(&spec, &src, q.net, 150_000, li, a, b)
            };
            let mut rip = Rip { cost: &cost, owners: &mut owners };
            let mut work = 0;
            let result = search_blockers(&mut grid, &src, &q, &costs, &mut rip, &mut work)
                .expect("a rippable gap remains");
            if fixed {
                assert_eq!(result.nets, [NetId(2)], "fixed copper is never ripped");
            } else {
                assert_eq!(result.nets, [NetId(expected)], "equal-length gaps prefer the cheaper net");
            }
            verify_blockers(&mut grid, &src, &q, &result);
        }
    }

    #[test]
    fn free_ripping_is_never_worse() {
        let spec = GridSpec::covering(
            BBox::new(P::ZERO, P::new(2_000_000, 2_000_000)),
            100_000,
            vec![LayerId::F_CU, LayerId::inner(1), LayerId::B_CU],
        );
        let src = VecSource {
            items: [1, 2].into_iter().map(|net| {
                (LayerId::inner(1), Shape::Segment {
                    a: P::new(900_000, 900_000),
                    b: P::new(1_100_000, 1_100_000),
                    w: 150_000,
                }, Owner::Net(NetId(net)))
            }).collect(),
            need: 150_000,
        };
        let mut grid = Grid::new(spec.clone());
        let (track, via) = planes(&mut grid);
        let sources = [(0, Cell { x: 10, y: 10 })];
        let targets = [(2, Cell { x: 10, y: 10 })];
        let q = Query {
            track,
            via: Some(via),
            net: NetId(0),
            layers: &[0, 2],
            sources: &sources,
            targets: &targets,
            window: spec.all(),
            guard: None,
        };
        let costs = Costs::default();
        let mut work = 0;
        let plain = search(&mut grid, &src, &q, &costs, &mut work).expect("plain search can go around copper");
        verify_path(&mut grid, &src, &q, &costs, &plain);
        let mut owners = |plane, li, a, b| {
            let w = if plane == track { 150_000 } else { 450_000 };
            step_owners(&spec, &src, q.net, w, li, a, b)
        };
        let mut rip = Rip { cost: &|_| 0, owners: &mut owners };
        let result = search_blockers(&mut grid, &src, &q, &costs, &mut rip, &mut work)
            .expect("ripping retains every plain route");
        assert!(result.path.cost <= plain.cost, "ripping for free is never worse");
        assert_eq!(result.path.cost, u64::from(costs.via), "free ripping allows a direct through-via");
        assert_eq!(result.nets, [NetId(1), NetId(2)], "a via names owners even on excluded routing layers");
        assert_eq!(result.contested, [(1, Cell { x: 10, y: 10 })], "the via contests its cell on the layer it paid on");
        verify_blockers(&mut grid, &src, &q, &result);
    }

    fn verify_path(grid: &mut Grid, src: &VecSource, q: &Query<'_>, costs: &Costs, path: &GridPath) {
        assert!(path.cells.first().is_some_and(|c| q.sources.contains(c)), "path starts at a source");
        assert!(path.cells.last().is_some_and(|c| q.targets.contains(c)), "path ends at a target");
        for &(li, c) in &path.cells {
            assert!(q.layers.contains(&li) && q.window.contains(c), "path stays on allowed layers in window");
        }
        let (li, c) = path.cells[0];
        assert_eq!(grid.step_pass(src, q.track, li, c, c, q.net), Pass::Free, "each path step is usable");
        let guarded = guard_rule(q, costs, true);
        let mut dir = NONE;
        let mut cost = 0;
        for pair in path.cells.windows(2) {
            let ((li, a), (next_li, b)) = (pair[0], pair[1]);
            let start = if li == next_li { a } else { b };
            assert_eq!(grid.step_pass(src, q.track, next_li, start, b, q.net), Pass::Free, "each path step is usable");
            if li != next_li {
                assert_eq!(a, b, "a via does not change cell");
                let via = q.via.expect("a via move requires a via plane");
                for layer in 0..grid.spec.layers.len() {
                    assert!(grid.state(src, via, layer, a).free_for(q.net), "via clears every stack layer");
                    cost += if guarded(layer, a) { u64::from(costs.guard) } else { 0 };
                }
                cost += u64::from(costs.via);
                dir = NONE;
            } else {
                let delta = (i64::from(b.x) - i64::from(a.x), i64::from(b.y) - i64::from(a.y));
                let next = DIRECTIONS.iter().position(|&d| d == delta)
                    .expect("a track step must be an eight-neighbour move") as u8;
                let difference = dir.abs_diff(next);
                let turn = if dir == NONE { 0 } else { difference.min(8 - difference) };
                assert!(turn <= 2, "track turns never exceed ninety degrees");
                let against = match costs.prefs.get(li).copied().unwrap_or(Pref::None) {
                    Pref::None => false,
                    Pref::Horizontal => delta.0 == 0,
                    Pref::Vertical => delta.1 == 0,
                };
                cost += if delta.0 != 0 && delta.1 != 0 { 7 } else { 5 };
                cost += u64::from(turn) * u64::from(costs.bend45);
                cost += if against { u64::from(costs.against) } else { 0 };
                cost += u64::from(grid.extra(li, b));
                cost += if guarded(li, b) { u64::from(costs.guard) } else { 0 };
                dir = next;
            }
        }
        assert_eq!(cost, path.cost, "path cost includes every movement and penalty");
    }

    #[test]
    fn astar_matches_dijkstra() {
        let costs = Costs { prefs: vec![Pref::Horizontal, Pref::Vertical], ..Costs::default() };
        let mut seed = 0x1a2b_3c4d_5e6f_7788;
        let mut guard_seed = 0x9b7c_6d5e_4f32_1081;
        let mut found = 0;
        for _ in 0..3 {
            let obstacle_seed = u64::from(random(&mut seed)) + 1;
            let (spec, src) = fixture(obstacle_seed, 40, 40);
            let mut grid = Grid::new(spec.clone());
            let (track, via) = planes(&mut grid);
            let mut extras = Vec::new();
            for _ in 0..12 {
                let li = (random(&mut seed) % 2) as usize;
                let c = Cell { x: random(&mut seed) % spec.cols, y: random(&mut seed) % spec.rows };
                let value = (random(&mut seed) % 20) as u16;
                grid.add_extra(li, c, value);
                extras.push((li, c, value));
            }
            for _ in 0..10 {
                // Random local queries bound debug-time work while the cache covers a 40x40 board.
                let min = Cell { x: random(&mut seed) % 30, y: random(&mut seed) % 30 };
                let window = CellBox { min, max: Cell { x: min.x + 10, y: min.y + 10 } };
                let source = Cell { x: min.x + random(&mut seed) % 11, y: min.y + random(&mut seed) % 11 };
                let target = Cell { x: min.x + random(&mut seed) % 11, y: min.y + random(&mut seed) % 11 };
                let sources = [((random(&mut seed) % 2) as usize, source)];
                let targets = [((random(&mut seed) % 2) as usize, target)];
                let q = Query {
                    track,
                    via: Some(via),
                    net: NetId(random(&mut seed) % 3),
                    layers: &[0, 1],
                    sources: &sources,
                    targets: &targets,
                    window,
                    guard: None,
                };
                let (mut aw, mut dw) = (0, 0);
                let a = search(&mut grid, &src, &q, &costs, &mut aw);
                let d = search_inner(&mut grid, &src, &q, &costs, None, false, &mut dw).map(|result| result.path);
                assert_eq!(a.as_ref().map(|p| p.cost), d.as_ref().map(|p| p.cost),
                    "A* and Dijkstra agree on reachability and cost");
                assert!(aw <= dw, "A* expands no more states than Dijkstra: {aw} <= {dw}");
                let mut repeat_work = 0;
                assert_eq!(a, search(&mut grid, &src, &q, &costs, &mut repeat_work),
                    "repeating a query gives the identical path");
                assert_eq!(aw, repeat_work, "warm-cache searches expand identical states");
                let mut fresh = Grid::new(spec.clone());
                let (fresh_track, fresh_via) = planes(&mut fresh);
                for &(li, c, value) in &extras {
                    fresh.add_extra(li, c, value);
                }
                let fresh_q = Query { track: fresh_track, via: Some(fresh_via), ..q };
                let mut fresh_work = 0;
                assert_eq!(a, search(&mut fresh, &src, &fresh_q, &costs, &mut fresh_work),
                    "fresh geometry cache gives the identical path");
                if let Some(path) = &a {
                    verify_path(&mut grid, &src, &q, &costs, path);
                    found += 1;
                }

                // Reserve three random neighbours per layer for exclusive-own,
                // exclusive-other and shared ownership; scatter the rest of each band.
                let reserved: Vec<_> = (0..2).map(|_| Cell {
                    x: min.x + random(&mut guard_seed) % 9,
                    y: min.y + random(&mut guard_seed) % 11,
                }).collect();
                let mut table = GuardTable::default();
                for owner in 0..3 {
                    let mut band = Vec::new();
                    for (li, &c) in reserved.iter().enumerate() {
                        band.push((li, Cell { x: c.x + 2, ..c }));
                        if owner < 2 {
                            band.push((li, Cell { x: c.x + owner, ..c }));
                        }
                        for _ in 0..12 {
                            let cell = Cell {
                                x: min.x + random(&mut guard_seed) % 11,
                                y: min.y + random(&mut guard_seed) % 11,
                            };
                            if cell != c && cell != (Cell { x: c.x + 1, ..c }) {
                                band.push((li, cell));
                            }
                        }
                    }
                    band.sort_unstable();
                    band.dedup();
                    let target = band[0].1;
                    table.set((ItemId(owner), PadIx(0), LayerId::F_CU), Some(Escape {
                        net: NetId((q.net.0 + owner) % 3),
                        path: vec![target], target: Some(target), band, watch: BBox::EMPTY,
                    }));
                }
                for (li, &c) in reserved.iter().enumerate() {
                    assert!(matches!(table.guard_at(li, c), Some(Guard::One(net)) if net == q.net),
                        "random guard includes an exclusive owner cell on layer {li}");
                    assert!(matches!(table.guard_at(li, Cell { x: c.x + 1, ..c }),
                        Some(Guard::One(net)) if net != q.net),
                        "random guard includes an exclusive foreign cell on layer {li}");
                    assert!(matches!(table.guard_at(li, Cell { x: c.x + 2, ..c }), Some(Guard::Several)),
                        "random guard includes a shared cell on layer {li}");
                }
                let guarded_q = Query { guard: Some(&table), ..q };
                let guarded_costs = Costs { guard: 1 + random(&mut guard_seed) % 80, ..costs.clone() };
                let guarded_a = search(&mut grid, &src, &guarded_q, &guarded_costs, &mut 0);
                let guarded_d = search_inner(&mut grid, &src, &guarded_q, &guarded_costs,
                    None, false, &mut 0).map(|result| result.path);
                assert_eq!(guarded_a.as_ref().map(|p| p.cost), guarded_d.as_ref().map(|p| p.cost),
                    "guarded A* and Dijkstra agree on reachability and cost");
                assert_eq!(guarded_a.is_some(), a.is_some(), "a soft guard preserves reachability");
                for path in guarded_a.iter().chain(guarded_d.iter()) {
                    verify_path(&mut grid, &src, &guarded_q, &guarded_costs, path);
                    assert!(path.cost >= a.as_ref().expect("the unguarded route also exists").cost,
                        "guard penalties cannot make a route cheaper than its unguarded optimum");
                }
            }
        }
        assert!(found > 0, "random path checks are nonvacuous");
    }

    #[test]
    fn guard_cost_is_soft_and_net_specific() {
        let spec = GridSpec {
            origin: P::ZERO, pitch: 100_000, cols: 5, rows: 1,
            layers: vec![LayerId::F_CU, LayerId::B_CU],
        };
        let mut grid = Grid::new(spec.clone());
        let src = VecSource { items: Vec::new(), need: 0 };
        let (track, via) = planes(&mut grid);
        let sources = [(0, Cell { x: 0, y: 0 })];
        let targets = [(0, Cell { x: 4, y: 0 })];
        let q = Query {
            track, via: Some(via), net: NetId(0), layers: &[0, 1],
            sources: &sources, targets: &targets, window: spec.all(), guard: None,
        };
        let costs = Costs { guard: 17, ..Costs::default() };
        let unguarded = search(&mut grid, &src, &q, &costs, &mut 0)
            .expect("the empty corridor has a route");
        assert_eq!(unguarded.cost, 4 * 5, "the straight corridor needs four orthogonal steps");
        let band: Vec<_> = (0..2).flat_map(|li| {
            (0..spec.cols).map(move |x| (li, Cell { x, y: 0 }))
        }).collect();
        let escaped = Escape {
            net: q.net, path: vec![sources[0].1], target: Some(sources[0].1),
            band, watch: BBox::EMPTY,
        };
        let mut table = GuardTable::default();
        table.set((ItemId(0), PadIx(0), LayerId::F_CU), Some(escaped.clone()));
        let own_q = Query { guard: Some(&table), ..q };
        let own = search(&mut grid, &src, &own_q, &costs, &mut 0)
            .expect("the owner can traverse its guarded corridor");
        assert_eq!(own, unguarded, "a sole owner pays no guard cost and keeps the unguarded path");
        verify_path(&mut grid, &src, &own_q, &costs, &own);
        let other_q = Query { net: NetId(1), ..own_q };
        let other = search(&mut grid, &src, &other_q, &costs, &mut 0)
            .expect("a guard on every cell is soft and cannot close the corridor");
        assert_eq!(other.cells, unguarded.cells, "the foreign net still follows the straight corridor");
        assert_eq!(other.cost, unguarded.cost + 4 * u64::from(costs.guard),
            "a foreign net pays once per entered cell, including the target but excluding the source");
        verify_path(&mut grid, &src, &other_q, &costs, &other);

        let mut shared = table.clone();
        shared.set((ItemId(1), PadIx(0), LayerId::B_CU), Some(Escape { net: NetId(1), ..escaped }));
        let shared_q = Query { guard: Some(&shared), ..q };
        let several = search(&mut grid, &src, &shared_q, &costs, &mut 0)
            .expect("a corridor shared by several guards is still traversable");
        assert_eq!(several.cells, unguarded.cells, "shared guards keep the corridor traversable end to end");
        assert_eq!(several.cost, other.cost, "a shared guard charges even one of its owners");
        verify_path(&mut grid, &src, &shared_q, &costs, &several);

        let via_sources = [(0, Cell { x: 2, y: 0 })];
        let via_targets = [(1, Cell { x: 2, y: 0 })];
        let via_q = Query { sources: &via_sources, targets: &via_targets, ..shared_q };
        let via_plain_q = Query { guard: None, ..via_q };
        let via_plain = search(&mut grid, &src, &via_plain_q, &costs, &mut 0)
            .expect("the empty stack permits a direct via");
        let guarded_via = search(&mut grid, &src, &via_q, &costs, &mut 0)
            .expect("guarding both layers still permits a via");
        assert_eq!(via_plain.cost, u64::from(costs.via), "the unguarded via pays only the via cost");
        assert_eq!(guarded_via.cells, vec![via_sources[0], via_targets[0]],
            "the guarded via changes layers without moving along the corridor");
        assert_eq!(guarded_via.cost, via_plain.cost + 2 * u64::from(costs.guard),
            "a via pays once on each guarded stack layer, with no extra source or landing charge");
        verify_path(&mut grid, &src, &via_q, &costs, &guarded_via);

        for guarded_q in [shared_q, via_q] {
            let plain_q = Query { guard: None, ..guarded_q };
            let mut owners = |_, _, _, _| Vec::new();
            let mut rip = Rip { cost: &|_| 10, owners: &mut owners };
            let (mut plain_work, mut guard_work) = (0, 0);
            let plain = search_blockers(&mut grid, &src, &plain_q, &costs, &mut rip, &mut plain_work)
                .expect("blockers search traverses the empty corridor or via");
            let guarded = search_blockers(&mut grid, &src, &guarded_q, &costs, &mut rip, &mut guard_work);
            assert_eq!(guarded, Some(plain), "blockers search ignores guard costs for tracks and vias");
            assert_eq!(guard_work, plain_work, "a guard table does not alter blockers-search work");

            let off = Costs { guard: 0, ..costs.clone() };
            let (mut plain_work, mut guard_work) = (0, 0);
            let plain = search(&mut grid, &src, &plain_q, &off, &mut plain_work)
                .expect("plain search traverses the corridor or via");
            let guarded = search(&mut grid, &src, &guarded_q, &off, &mut guard_work);
            assert_eq!(guarded, Some(plain), "zero guard cost with a table keeps the path and cost without a table");
            assert_eq!(guard_work, plain_work, "zero guard cost with a table keeps the work without a table");
        }

        // Rip-up's history cost and the guard add up on the same cell; neither replaces the other.
        grid.add_extra(0, Cell { x: 2, y: 0 }, 9);
        let both = search(&mut grid, &src, &other_q, &costs, &mut 0)
            .expect("history and the guard are costs, not blocks");
        assert_eq!(both.cells, unguarded.cells, "the corridor stays the cheapest route");
        assert_eq!(both.cost, other.cost + 9, "a guarded cell with history pays its guard and its history");
        verify_path(&mut grid, &src, &other_q, &costs, &both);
    }

    /// Runs the reachability flood alone until it decides.
    fn flood_reaches(grid: &mut Grid, src: &dyn ObstacleSource, q: &Query<'_>, ripping: bool) -> bool {
        let Some(window) = q.window.intersection(grid.spec.all()) else {
            return false;
        };
        let mut layers: Vec<_> = q.layers.iter().copied().filter(|&li| li < grid.spec.layers.len()).collect();
        layers.sort_unstable();
        layers.dedup();
        let mut flood = match Reach::seed(grid, src, q, &layers, window, q.targets, ripping) {
            Ok(flood) => flood,
            Err(found) => return matches!(found, Reached::Met),
        };
        loop {
            match flood.step(grid, src, q, &layers, window, &mut 0) {
                Reached::Pending => {}
                found => return matches!(found, Reached::Met),
            }
        }
    }

    #[test]
    fn reachability_check_is_exact() {
        let costs = Costs { prefs: vec![Pref::Horizontal, Pref::Vertical], ..Costs::default() };
        let mut seed = 0x1a2b_3c4d_5e6f_7788;
        let (spec, src) = fixture(u64::from(random(&mut seed)) + 1, 40, 40);
        let mut grid = Grid::new(spec.clone());
        let (track, via) = planes(&mut grid);
        for _ in 0..12 {
            let li = (random(&mut seed) % 2) as usize;
            let c = Cell { x: random(&mut seed) % spec.cols, y: random(&mut seed) % spec.rows };
            grid.add_extra(li, c, (random(&mut seed) % 20) as u16);
        }
        let (mut found, mut failed) = ([0; 4], [0; 4]);
        for case in 0..60 {
            let min = Cell { x: random(&mut seed) % 30, y: random(&mut seed) % 30 };
            let window = CellBox { min, max: Cell { x: min.x + 10, y: min.y + 10 } };
            let mut endpoint = || {
                let li = (random(&mut seed) % 2) as usize;
                (li, Cell { x: min.x + random(&mut seed) % 11, y: min.y + random(&mut seed) % 11 })
            };
            let mut sources = [endpoint(), endpoint(), (2, min), (0, Cell { x: spec.cols, y: 0 })];
            let mut targets = [endpoint(), endpoint(), (2, min), (0, Cell { x: 0, y: spec.rows })];
            if case % 3 == 0 {
                sources[1] = sources[0];
                targets[1] = targets[0];
            }
            if case % 10 == 0 {
                targets[0] = sources[0];
            }
            let net = NetId(random(&mut seed) % 3);
            for (mode, via) in [None, Some(via)].into_iter().enumerate() {
                let q = Query {
                    track,
                    via,
                    net,
                    layers: if case % 4 == 0 { &[2, 0, 0] } else { &[1, 0, 1, 2] },
                    sources: &sources,
                    targets: &targets,
                    window,
                    guard: None,
                };
                let checked = search(&mut grid, &src, &q, &costs, &mut 0);
                let unchecked = search_with_group_size(&mut grid, &src, &q, &costs, None,
                    Some(GROUP), false, &mut 0, &mut false).map(|result| result.path);
                assert_eq!(checked, unchecked,
                    "reachability check preserves failure, exact cells and cost: case {case}, via {via:?}");
                // The flood's edges are a superset of A*'s: a cut flood means no route.
                assert!(checked.is_none() || flood_reaches(&mut grid, &src, &q, false),
                    "a route implies the flood meets: case {case}, via {via:?}");
                if checked.is_some() {
                    found[mode] += 1;
                } else {
                    failed[mode] += 1;
                }
                // with ripping: the flood passes every node that is not blocked
                let mut owners = |plane, li, a, b| {
                    let w = if Some(plane) == q.via { 450_000 } else { 150_000 };
                    step_owners(&spec, &src, q.net, w, li, a, b)
                };
                let cost = |n: NetId| n.0 * 7 + 3;
                let mut rip = Rip { cost: &cost, owners: &mut owners };
                let checked = search_blockers(&mut grid, &src, &q, &costs, &mut rip, &mut 0);
                let unchecked = search_with_group_size(&mut grid, &src, &q, &costs, Some(&mut rip),
                    Some(GROUP), false, &mut 0, &mut false);
                assert_eq!(checked.as_ref().map(|b| (&b.path, &b.nets)), unchecked.as_ref().map(|b| (&b.path, &b.nets)),
                    "with ripping the check preserves failure, exact cells, cost and owners: case {case}, via {via:?}");
                assert!(checked.is_none() || flood_reaches(&mut grid, &src, &q, true),
                    "with ripping a route implies the flood meets: case {case}, via {via:?}");
                if checked.is_some() {
                    found[2 + mode] += 1;
                } else {
                    failed[2 + mode] += 1;
                }
            }
        }
        assert!(found.iter().all(|&n| n > 0),
            "exact path comparisons exercise successes with and without vias, with and without ripping");
        assert!(failed.iter().all(|&n| n > 0),
            "exact failure comparisons exercise failures with and without vias, with and without ripping");
    }

    const RIP_TEST: u32 = 10;

    #[test]
    fn cut_off_island_fails_without_expanding() {
        let area = BBox::new(P::ZERO, P::new(8_000_000, 8_000_000));
        let spec = GridSpec::covering(area, 100_000, vec![LayerId::F_CU]);
        let window = CellBox { min: Cell { x: 4, y: 4 }, max: Cell { x: 76, y: 76 } };
        let ring = CellBox { min: Cell { x: 36, y: 36 }, max: Cell { x: 44, y: 44 } };
        let interior = CellBox {
            min: Cell { x: ring.min.x + 1, y: ring.min.y + 1 },
            max: Cell { x: ring.max.x - 1, y: ring.max.y - 1 },
        };
        let island_nodes = u64::from(interior.width()) * u64::from(interior.height());
        let segment = |a, b| {
            (LayerId::F_CU, Shape::Segment {
                a: spec.centre(a), b: spec.centre(b), w: 10_000,
            }, Owner::Hard)
        };
        // Track, copper and slack together reach less than one pitch: the ring is one cell thick.
        let mut src = VecSource {
            items: vec![
                segment(ring.min, Cell { x: ring.max.x, y: ring.min.y }),
                segment(ring.min, Cell { x: ring.min.x, y: ring.max.y }),
                segment(Cell { x: ring.max.x, y: ring.min.y }, ring.max),
                segment(Cell { x: ring.min.x, y: ring.max.y }, ring.max),
            ],
            need: 0,
        };
        let mut grid = Grid::new(spec.clone());
        let track = grid.plane(Probe { class: ClassId(0), w: 10_000, drill: None });
        let mut sources = Vec::new();
        for y in window.min.y..=window.max.y {
            for x in window.min.x..=window.max.x {
                let c = Cell { x, y };
                if !ring.contains(c) {
                    sources.push((0, c));
                }
            }
        }
        let targets = [(0, Cell { x: 40, y: 40 })];
        let q = Query {
            track,
            via: None,
            net: NetId(0),
            layers: &[0],
            sources: &sources,
            targets: &targets,
            window,
            guard: None,
        };
        let costs = Costs::default();
        assert!(sources.len() > 2_000, "source pour contains thousands of cells outside the target island");
        let mut work = 0;
        assert!(search(&mut grid, &src, &q, &costs, &mut work).is_none(),
            "closed hard ring blocks the single-window route");
        let single_work = work;
        let before = work;
        assert!(search_widening(&mut grid, &src, &q, &costs, &mut work).is_none(),
            "closed hard ring blocks the widening route");
        let added = work - before;
        assert_eq!(added, single_work,
            "exhausted target island prevents widening despite source-side clipping");
        // The flood alternates sides, so the island side runs out after its nodes and as many
        // source-side pops; A* pops one state per flood node.
        assert!((4 * island_nodes - 2..=4 * island_nodes).contains(&added),
            "failure work is the island's flood on both sides plus as many A* states in lockstep: {added}");

        // From a corner source A* steps out of the window at once; the island's cut alone decides.
        let corner = [(0, window.min)];
        let corner_q = Query { sources: &corner, ..q };
        let mut single = 0;
        assert!(search(&mut grid, &src, &corner_q, &costs, &mut single).is_none(),
            "the closed ring blocks the corner source");
        let before = work;
        assert!(search_widening(&mut grid, &src, &corner_q, &costs, &mut work).is_none(),
            "the closed ring blocks the corner source at every width");
        assert_eq!(work - before, single, "A*'s own steps out of the window do not widen a flood cut");

        // No ripping opens a hard ring: the blockers search fails after the same flood, with
        // rippable copper of another net all over the source side.
        for k in 0..8 {
            let at = Cell { x: 8 + 8 * k, y: 12 };
            src.items.push((LayerId::F_CU, Shape::Segment { a: spec.centre(at), b: spec.centre(Cell { x: at.x, y: 28 }), w: 10_000 },
                Owner::Net(NetId(1))));
        }
        grid.invalidate(&src, area, &[0]);
        let mut owners = |_, li, a, b| step_owners(&spec, &src, q.net, 10_000, li, a, b);
        let mut rip = Rip { cost: &|_| RIP_TEST, owners: &mut owners };
        let mut blockers = 0;
        assert!(search_blockers(&mut grid, &src, &q, &costs, &mut rip, &mut blockers).is_none(),
            "no ripping opens the hard ring");
        assert!((4 * island_nodes - 2..=4 * island_nodes).contains(&blockers),
            "a blockers search that no ripping can complete stops with the island's flood: {blockers}");
        src.items.truncate(4);
        grid.invalidate(&src, area, &[0]);

        let gap = Cell { x: ring.min.x, y: targets[0].1.y };
        src.items[1] = segment(ring.min, Cell { x: gap.x, y: gap.y - 1 });
        src.items.push(segment(Cell { x: gap.x, y: gap.y + 1 }, Cell { x: gap.x, y: ring.max.y }));
        grid.invalidate(&src, area, &[0]);
        assert_eq!(grid.state(&src, track, 0, gap).pass(q.net), Pass::Free,
            "one removed ring cell opens a passable gap");
        assert!(flood_reaches(&mut grid, &src, &q, false), "the flood meets the source pour through the one-cell gap");
        let checked = search_widening(&mut grid, &src, &q, &costs, &mut work)
            .expect("one-cell gap connects the source pour to the target island");
        let unchecked = search_with_group_size(&mut grid, &src, &q, &costs, None,
            Some(GROUP), false, &mut 0, &mut false).expect("A* reaches the island through the same gap");
        assert_eq!(checked, unchecked.path, "gap route has identical cells and cost with and without the check");
        verify_path(&mut grid, &src, &q, &costs, &checked);
    }

    #[test]
    fn grouped_heuristic_is_exact() {
        let costs = Costs { prefs: vec![Pref::Horizontal, Pref::Vertical], ..Costs::default() };
        let sources = [(0, Cell { x: 0, y: 0 }), (1, Cell { x: 0, y: 0 })];
        let targets = [
            (0, Cell { x: 59, y: 0 }), (1, Cell { x: 59, y: 0 }),
            (0, Cell { x: 57, y: 2 }), (1, Cell { x: 57, y: 2 }),
            (0, Cell { x: 0, y: 59 }), (1, Cell { x: 0, y: 59 }),
            (0, Cell { x: 2, y: 57 }), (1, Cell { x: 2, y: 57 }),
        ];
        let mut seed = 0x1a2b_3c4d_5e6f_7788;
        let (mut grouped_work, mut merged_work) = (0, 0);
        for _ in 0..20 {
            let obstacle_seed = u64::from(random(&mut seed)) + 1;
            let (spec, mut src) = fixture(obstacle_seed, 60, 60);
            src.items.truncate(12);
            // Keep the outer row and column reachable; obstacles can still obstruct inset targets.
            src.need = 0;
            let mut grid = Grid::new(spec.clone());
            let (track, via) = planes(&mut grid);
            let q = Query {
                track,
                via: Some(via),
                net: NetId(0),
                layers: &[0, 1],
                sources: &sources,
                targets: &targets,
                window: spec.all(),
                guard: None,
            };
            let grouped = search(&mut grid, &src, &q, &costs, &mut grouped_work)
                .expect("corner targets are reachable");
            let mut dijkstra_work = 0;
            let dijkstra = search_inner(&mut grid, &src, &q, &costs, None, false, &mut dijkstra_work)
                .expect("corner targets are reachable without a heuristic");
            assert_eq!(grouped.cost, dijkstra.path.cost, "grouped heuristic keeps optimal costs");
            // One bucket merges all targets into the former single bounding box.
            let merged = search_with_group_size(&mut grid, &src, &q, &costs, None,
                Some(u32::MAX), true, &mut merged_work, &mut false)
                .expect("corner targets are reachable with one box");
            assert_eq!(grouped.cost, merged.path.cost, "grouped heuristic keeps optimal costs");
            verify_path(&mut grid, &src, &q, &costs, &grouped);
        }
        assert!(grouped_work < merged_work, "grouped heuristic expands less");
    }

    fn wall(half_gap: Option<Nm>) -> VecSource {
        let rect = |y, half_y| Shape::Rect {
            c: P::new(2_000_000, y),
            half: P::new(100_000, half_y),
            rot: Angle::ZERO,
            corner: 0,
        };
        let shapes = match half_gap {
            None => vec![rect(2_000_000, 4_000_000)],
            Some(gap) => vec![rect(-gap, 2_000_000), rect(4_000_000 + gap, 2_000_000)],
        };
        VecSource {
            items: shapes.into_iter().map(|shape| (LayerId::F_CU, shape, Owner::Hard)).collect(),
            need: 150_000,
        }
    }

    #[test]
    fn widening_stops_only_when_exhausted_inside_window() {
        let area = BBox::new(P::ZERO, P::new(3_100_000, 3_100_000));
        let spec = GridSpec::covering(area, 100_000, vec![LayerId::F_CU, LayerId::B_CU]);
        let ring = CellBox { min: Cell { x: 10, y: 10 }, max: Cell { x: 20, y: 20 } };
        let window = ring.inflate(4, &spec);
        let segment = |x0, y0, x1, y1| {
            (LayerId::F_CU, Shape::Segment {
                a: spec.centre(Cell { x: x0, y: y0 }),
                b: spec.centre(Cell { x: x1, y: y1 }),
                w: 100_000,
            }, Owner::Hard)
        };
        let mut src = VecSource {
            items: vec![
                segment(10, 10, 20, 10), segment(10, 10, 10, 20),
                segment(20, 10, 20, 20), segment(10, 20, 20, 20),
            ],
            need: 0,
        };
        let mut grid = Grid::new(spec.clone());
        let (track, _) = planes(&mut grid);
        let sources = [(0, Cell { x: 15, y: 15 })];
        let targets = [(0, Cell { x: 23, y: 15 })];
        let q = Query {
            track,
            via: None,
            net: NetId(0),
            layers: &[0],
            sources: &sources,
            targets: &targets,
            window,
            guard: None,
        };
        let costs = Costs::default();
        assert!(window.min.x > 0 && window.min.y > 0
            && window.max.x < spec.all().max.x && window.max.y < spec.all().max.y,
            "initial window is strictly inside the grid on every side");
        let mut work = 0;
        assert!(search(&mut grid, &src, &q, &costs, &mut work).is_none(),
            "closed hard ring blocks the single-window search");
        let single_work = work;
        assert!(single_work > 0, "closed ring failure explores reachable source states");
        let before = work;
        assert!(search_widening(&mut grid, &src, &q, &costs, &mut work).is_none(),
            "closed hard ring has no route after widening");
        assert_eq!(work - before, single_work, "exhausted closed ring does not widen");

        let excluded_sources = [sources[0], (1, Cell { x: 2, y: 2 })];
        let excluded = Query { sources: &excluded_sources, ..q };
        let before = work;
        assert!(search_widening(&mut grid, &src, &excluded, &costs, &mut work).is_none(),
            "source on an excluded layer cannot bypass the closed ring");
        assert_eq!(work - before, single_work,
            "out-of-window source on an excluded layer does not cause widening");

        let outside_sources = [sources[0], (0, excluded_sources[1].1)];
        let outside = Query { sources: &outside_sources, ..q };
        let before = work;
        let path = search_widening(&mut grid, &src, &outside, &costs, &mut work)
            .expect("out-of-window source on an allowed layer causes widening");
        assert!(work - before > single_work, "window-dropped source adds a search attempt");
        assert_eq!(path.cells.first(), Some(&outside_sources[1]),
            "widening admits the source outside the closed ring");
        verify_path(&mut grid, &src, &Query { window: spec.all(), ..outside }, &costs, &path);

        // Remove the top and extend both sides beyond the window: escape needs y > 27.
        src.items.truncate(3);
        src.items[1] = segment(10, 10, 10, 27);
        src.items[2] = segment(20, 10, 20, 27);
        grid.invalidate(&src, area, &[0]);
        let before = work;
        assert!(search(&mut grid, &src, &q, &costs, &mut work).is_none(),
            "U and initial window boundary enclose the source");
        let single_work = work - before;
        let before = work;
        let path = search_widening(&mut grid, &src, &q, &costs, &mut work)
            .expect("clipped failure widens to reach the U opening");
        assert!(work - before > single_work, "U opening requires another search attempt");
        assert!(path.cells.iter().any(|&(_, c)| c.y > 27 && !window.contains(c)),
            "widened route passes through the U opening outside the initial window");
        verify_path(&mut grid, &src, &Query { window: spec.all(), ..q }, &costs, &path);

        let outside_targets = [(0, Cell { x: 15, y: 29 })];
        let outside = Query { targets: &outside_targets, ..q };
        let before = work;
        assert!(search(&mut grid, &src, &outside, &costs, &mut work).is_none(),
            "window-dropped only target prevents an initial search");
        assert_eq!(work, before, "empty filtered target set expands no states");
        let path = search_widening(&mut grid, &src, &outside, &costs, &mut work)
            .expect("window-dropped target causes widening even before state expansion");
        verify_path(&mut grid, &src, &Query { window: spec.all(), ..outside }, &costs, &path);
    }

    #[test]
    fn wall_needs_two_vias() {
        let spec = GridSpec::covering(
            BBox::new(P::ZERO, P::new(3_900_000, 3_900_000)),
            100_000,
            vec![LayerId::F_CU, LayerId::B_CU],
        );
        let mut grid = Grid::new(spec.clone());
        let (track, via) = planes(&mut grid);
        let sources = [(0, Cell { x: 5, y: 20 })];
        let targets = [(0, Cell { x: 35, y: 20 })];
        let mut q = Query {
            track,
            via: Some(via),
            net: NetId(0),
            layers: &[0, 1],
            sources: &sources,
            targets: &targets,
            window: spec.all(),
            guard: None,
        };
        let costs = Costs::default();
        let mut work = 0;
        let solid = wall(None);
        assert!(flood_reaches(&mut grid, &solid, &q, false),
            "reachability connects the front-layer sides through vias to the back layer");
        let path = search(&mut grid, &solid, &q, &costs, &mut work).expect("back layer bypass exists");
        assert_eq!(path.vias(), 2, "a full front-layer wall requires two vias");
        verify_path(&mut grid, &solid, &q, &costs, &path);
        q.via = None;
        assert!(!flood_reaches(&mut grid, &solid, &q, false),
            "reachability rejects the full front-layer wall without vias");
        assert!(search(&mut grid, &solid, &q, &costs, &mut work).is_none(), "solid wall blocks without vias");
        q.layers = &[0];
        // A centre-aligned gap needs 2 * (150000 + 75000) = 450000 nm: steps along its centre row
        // are tested exactly, with no slack. Equality is legal: the step rule blocks only strictly
        // smaller clearances.
        let half_gap = solid.need + 75_000;
        let narrow = wall(Some(half_gap - spec.pitch / 2));
        grid.invalidate(&narrow, BBox::new(P::new(-1_000_000, -4_000_000), P::new(5_000_000, 8_000_000)), &[0]);
        assert!(search(&mut grid, &narrow, &q, &costs, &mut work).is_none(),
            "a gap one cell narrower than a track needs blocks it");
        let wide = wall(Some(half_gap));
        grid.invalidate(&wide, BBox::new(P::new(-1_000_000, -4_000_000), P::new(5_000_000, 8_000_000)), &[0]);
        let path = search(&mut grid, &wide, &q, &costs, &mut work).expect("a gap of exactly a track's need is passable");
        assert_eq!(path.vias(), 0, "a sufficiently wide gap needs no vias");
        verify_path(&mut grid, &wide, &q, &costs, &path);

        let lower_sources = [(0, Cell { x: 5, y: 10 })];
        let lower_targets = [(0, Cell { x: 35, y: 10 })];
        let bounded = Query {
            sources: &lower_sources,
            targets: &lower_targets,
            window: CellBox { min: lower_sources[0].1, max: lower_targets[0].1 },
            ..q
        };
        assert!(search(&mut grid, &wide, &bounded, &costs, &mut work).is_none(),
            "initial one-row window cannot reach the gap");
        let widened = search_widening(&mut grid, &wide, &bounded, &costs, &mut work)
            .expect("doubling windows eventually reaches the gap");
        assert_eq!(widened.vias(), 0, "widening preserves the no-via constraint");

        let mut three_layers = spec.clone();
        three_layers.layers.insert(1, LayerId::inner(1));
        let mut blocked = solid.clone();
        blocked.items.push((LayerId::inner(1), Shape::Rect {
            c: P::new(2_000_000, 2_000_000),
            half: P::new(4_000_000, 4_000_000),
            rot: Angle::ZERO,
            corner: 0,
        }, Owner::Hard));
        let mut stack = Grid::new(three_layers);
        let (stack_track, stack_via) = planes(&mut stack);
        let stack_q = Query { track: stack_track, via: Some(stack_via), layers: &[0, 2], ..q };
        assert!(!flood_reaches(&mut stack, &blocked, &stack_q, false),
            "reachability vias must clear disallowed stack layers too");
        assert!(search(&mut stack, &blocked, &stack_q, &costs, &mut work).is_none(),
            "through-vias cannot cross an obstructed layer excluded from track routing");

        let empty = VecSource { items: Vec::new(), need: 0 };
        let mut corner_grid = Grid::new(spec.clone());
        let (corner_track, _) = planes(&mut corner_grid);
        let corner_sources = [(0, spec.all().max)];
        let corner_targets = [(0, Cell { x: 38, y: 39 })];
        let corner = Query {
            track: corner_track,
            sources: &corner_sources,
            targets: &corner_targets,
            window: CellBox { min: spec.all().max, max: spec.all().max },
            ..q
        };
        let path = search_widening(&mut corner_grid, &empty, &corner, &costs, &mut work)
            .expect("a corner window widens toward the interior");
        assert_eq!(path.cost, 5, "one-cell corner widening reaches the adjacent target");
    }
}
