//! Lazy, tiled routing occupancy: geometry is the truth and this grid is a rebuildable cache.
//! Clearance rules belong entirely to [`ObstacleSource`], and equal probes share a plane.
//!
//! For width `w`, let `half = (w + 1) / 2` and `s = ceil(pitch / sqrt(2))`, computed in integers.
//! An obstacle needing gap `g` blocks a centre when its rounded-down point clearance is less
//! than `g + half + s`. A tile visits obstacles once in its centre box grown by
//! `max_need + half + s`; each obstacle tests only centres in its similarly inflated box.
//! Every point of an eight-neighbour step is within `s` of an end. Both ends are at least
//! `g + half + s` away, so the entire segment is at least `g + half` away and a track of
//! width `w` along it keeps gap `g`. An obstacle owned by the routed net is exempt.
//! A gap may be negative: the probe's copper may then overlap the obstacle by up to `-g`, and an
//! obstacle with `g + half + s <= 0` blocks nothing (a via plane keeps only a via's drill off pads).
//! A via never moves off its centre, so a via plane (a probe with a drill) pads by `half` alone: a
//! centre is blocked exactly when a via there would come within `g` of the obstacle.
//!
//! A track plane's steps are also tested exactly ([`Grid::step`]): an obstacle owns the step
//! between two eight-neighbour centres when the segment's rounded-down clearance to it is less
//! than `g + half`, and a cell's own centre (a track's end) likewise. By the argument above every
//! exact owner of a step is a padded owner of one of its ends, so a step whose ends are padded
//! free for a net is free for it without a test ([`Grid::step_pass`]). The others read a step
//! memo: per built tile, five slots per cell (the steps to its east, north-east, north and
//! north-west neighbours, then its centre), filled from one source visit in the tile's centre
//! box grown by `max_need + half + pitch`; each obstacle tests only the cells within
//! `g + half + pitch` of it. Every point of a step lies within half its length of an end, so the
//! ends' point clearances bound the step's: an end under `g + half` owns the step, and ends past
//! it by half the length leave it free. Only the steps between take the segment test; both bounds
//! keep 2 nm over rounding, so the memo equals the test everywhere. The exact rule is DRC's
//! `check_to` without its 0.5 um allowance, with `half` rounded up: never looser.
//!
//! A cell's state ([`Occ`]) is four-valued for a net (gate 3 change 2): free; owned by its
//! own net; blocked by rippable copper of one other net or of several (shared); or hard (an
//! edge, a hole, a keepout, a pad, fixed or locked copper of another net). Plain search
//! enters only free and own cells; the cheapest-blockers search may cross rippable cells at
//! the cost of their owners, which it reads from the world, never from the grid. Merging is a
//! set operation per kind (fixed, rippable), so visit order cannot change a state.
//! Soft costs are independent, saturating `u16` history/congestion penalties per layer/cell.
//! Search uses integer costs: 5/7 per orthogonal/diagonal step, plus bends, preference and
//! destination soft cost; vias have a separate cost. No routing decision uses floating point.
//! A cached lookup is O(1); a tile build costs one source visit plus its candidate cell tests.
//! Occupancy storage is O(probes * layers * tile slots), with 64 * 64 states per built tile and,
//! per built step memo tile, 5 * 64 * 64 16-bit indices (40 KiB) plus the tile's distinct
//! owner sets.

use fab_elec_core::{clearance, point_clearance, BBox, ClassId, LayerId, NetId, Nm, P, Shape};
use std::collections::HashMap;

/// Escapes of pads that connect only through a pour, and the soft guard on their cells.
pub mod guard;
/// Grid paths to track runs and vias.
pub mod lift;
/// Deterministic A* over the grid's direction-aware routing states.
pub mod search;

/// Tile side length in cells; tiles are built on first touch.
pub const TILE: u32 = 64;

/// The steps a cell owns, one per undirected eight-neighbour pair: to the cells at these
/// `(dx, dy)`. The other four of its steps are owned by the neighbours they lead to.
const OWNED: [(i64, i64); 4] = [(1, 0), (1, 1), (0, 1), (-1, 1)];
/// The step memo slot of a cell's own centre, after its owned steps.
const NODE: usize = 4;
/// A cell's step memo: the exact owners of its owned steps, then of its centre.
type Steps = [Occ; NODE + 1];

/// A built step memo tile: each cell's slots as indices into the tile's distinct owner sets,
/// `values[0]` free. A tile holds at most `5 * TILE * TILE` (20,480) distinct sets, so an index
/// always fits 16 bits.
#[derive(Clone)]
struct StepTile {
    values: Vec<Occ>,
    slots: Box<[[u16; NODE + 1]]>,
}

/// A column and row whose centre is `origin + (x, y) * pitch`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Cell {
    /// Column, starting at zero.
    pub x: u32,
    /// Row, starting at zero.
    pub y: u32,
}

/// An inclusive rectangle of cells; an inverted axis denotes an empty box.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct CellBox {
    /// First column and row.
    pub min: Cell,
    /// Last column and row, inclusive.
    pub max: Cell,
}

impl CellBox {
    /// Whether `c` is inside this inclusive box.
    pub fn contains(&self, c: Cell) -> bool {
        c.x >= self.min.x && c.x <= self.max.x && c.y >= self.min.y && c.y <= self.max.y
    }

    /// Grow each side by `n` cells, clamping coordinates to `spec`; empty boxes stay empty.
    pub fn inflate(self, n: u32, spec: &GridSpec) -> CellBox {
        if self.is_empty() {
            return self;
        }
        CellBox {
            min: Cell {
                x: self.min.x.saturating_sub(n).min(spec.cols - 1),
                y: self.min.y.saturating_sub(n).min(spec.rows - 1),
            },
            max: Cell {
                x: self.max.x.saturating_add(n).min(spec.cols - 1),
                y: self.max.y.saturating_add(n).min(spec.rows - 1),
            },
        }
    }

    /// The smallest box containing both boxes; empty boxes are identities.
    pub fn union(self, other: CellBox) -> CellBox {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        CellBox {
            min: Cell { x: self.min.x.min(other.min.x), y: self.min.y.min(other.min.y) },
            max: Cell { x: self.max.x.max(other.max.x), y: self.max.y.max(other.max.y) },
        }
    }

    /// Number of columns, including both ends (zero for an empty box).
    pub fn width(&self) -> u64 {
        if self.is_empty() { 0 } else { u64::from(self.max.x) - u64::from(self.min.x) + 1 }
    }

    /// Number of rows, including both ends (zero for an empty box).
    pub fn height(&self) -> u64 {
        if self.is_empty() { 0 } else { u64::from(self.max.y) - u64::from(self.min.y) + 1 }
    }

    fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }

    fn intersection(self, other: CellBox) -> Option<CellBox> {
        let result = CellBox {
            min: Cell { x: self.min.x.max(other.min.x), y: self.min.y.max(other.min.y) },
            max: Cell { x: self.max.x.min(other.max.x), y: self.max.y.min(other.max.y) },
        };
        (!result.is_empty()).then_some(result)
    }
}

/// Grid geometry and layers in stack order. Dimensions and pitch must be positive, and all
/// centres must fit `Nm`. Treat a specification installed in a [`Grid`] as immutable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GridSpec {
    /// Centre of cell `(0, 0)`.
    pub origin: P,
    /// Centre spacing in nanometres, strictly positive.
    pub pitch: Nm,
    /// Number of columns, strictly positive.
    pub cols: u32,
    /// Number of rows, strictly positive.
    pub rows: u32,
    /// Layer identities in stack order; index zero is first.
    pub layers: Vec<LayerId>,
}

impl GridSpec {
    /// Cover `area` with centres starting at `area.min` and ending at or beyond `area.max`.
    /// Panics for empty areas, nonpositive pitch, or unrepresentable dimensions/centres.
    pub fn covering(area: BBox, pitch: Nm, layers: Vec<LayerId>) -> GridSpec {
        assert!(!area.is_empty() && pitch > 0, "grid needs a nonempty area and positive pitch");
        let count = |span: i64| {
            u32::try_from((span + i64::from(pitch) - 1) / i64::from(pitch) + 1)
                .expect("grid dimension must fit u32")
        };
        let spec = GridSpec {
            origin: area.min,
            pitch,
            cols: count(area.width()),
            rows: count(area.height()),
            layers,
        };
        spec.centre(spec.all().max);
        spec
    }

    /// The smallest in-scope `(width + clearance) / 3`, rounded down and at least 1 nm.
    pub fn pitch_for(min_w_plus_clearance: Nm) -> Nm {
        (min_w_plus_clearance / 3).max(1)
    }

    /// The centre of an in-grid cell; panics for an invalid cell or unrepresentable centre.
    pub fn centre(&self, c: Cell) -> P {
        assert!(c.x < self.cols && c.y < self.rows, "cell must be in grid");
        let coordinate = |origin: Nm, index: u32| {
            Nm::try_from(i64::from(origin) + i64::from(index) * i64::from(self.pitch))
                .expect("grid centres must fit Nm")
        };
        P::new(coordinate(self.origin.x, c.x), coordinate(self.origin.y, c.y))
    }

    /// Nearest centre to `p`, clamped into the grid; exact halfway ties choose the higher index.
    pub fn cell_at(&self, p: P) -> Cell {
        let index = |value: Nm, origin: Nm, count: u32| {
            let offset = (i64::from(value) - i64::from(origin)).max(0);
            ((offset + i64::from(self.pitch) / 2) / i64::from(self.pitch))
                .min(i64::from(count - 1)) as u32
        };
        Cell { x: index(p.x, self.origin.x, self.cols), y: index(p.y, self.origin.y, self.rows) }
    }

    /// Cells whose centres lie in `b`, clamped to the grid; `None` if there are none.
    pub fn cells_in(&self, b: BBox) -> Option<CellBox> {
        if b.is_empty() {
            return None;
        }
        let axis = |min: Nm, max: Nm, origin: Nm, count: u32| {
            let pitch = i64::from(self.pitch);
            let lo = -(-(i64::from(min) - i64::from(origin))).div_euclid(pitch);
            let hi = (i64::from(max) - i64::from(origin)).div_euclid(pitch);
            let lo = lo.max(0);
            let hi = hi.min(i64::from(count - 1));
            (lo <= hi).then_some((lo as u32, hi as u32))
        };
        let (x0, x1) = axis(b.min.x, b.max.x, self.origin.x, self.cols)?;
        let (y0, y1) = axis(b.min.y, b.max.y, self.origin.y, self.rows)?;
        Some(CellBox { min: Cell { x: x0, y: y0 }, max: Cell { x: x1, y: y1 } })
    }

    /// Smallest integer `s` with `2*s*s >= pitch*pitch`: the maximum distance to the nearer
    /// endpoint of any neighbouring-centre step, rounded up, calculated exactly in integers.
    pub fn slack(&self) -> Nm {
        assert!(self.pitch > 0, "grid pitch must be positive");
        let pitch = i64::from(self.pitch);
        let (mut lo, mut hi) = (0, pitch);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if 2 * mid * mid >= pitch * pitch {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo as Nm
    }

    /// A probe's padding: an obstacle needing gap `g` blocks a centre whose point clearance to
    /// it is below `g + padding` (half the probe's width, rounded up, plus the slack).
    pub fn padding(&self, probe: &Probe) -> i64 {
        let half = (i64::from(probe.w) + 1) / 2;
        if probe.drill.is_some() { half } else { half + i64::from(self.slack()) }
    }

    /// Inclusive bounds of every cell in the grid.
    pub fn all(&self) -> CellBox {
        CellBox { min: Cell { x: 0, y: 0 }, max: Cell { x: self.cols - 1, y: self.rows - 1 } }
    }
}

/// Obstacle ownership, independent of clearance rules. Net IDs must be below `u32::MAX - 1`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Owner {
    /// Rippable copper of a net (solved, unlocked): its own net passes; another net may rip it.
    Net(NetId),
    /// Fixed copper of a net (pads, authored, block-owned or locked copper, a plated drill):
    /// its own net passes; no other net may pass or rip it.
    Fixed(NetId),
    /// Blocks every net: board edges, unplated holes, keepouts and copper without a net.
    Hard,
}

/// Track width or via pad and drill diameters, and rule class for one occupancy plane.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Probe {
    /// The rule class passed to the obstacle source.
    pub class: ClassId,
    /// Nonnegative full width or pad diameter in nanometres.
    pub w: Nm,
    /// `None` for a track, or the nonnegative drill diameter of a via with pad diameter `w`.
    pub drill: Option<Nm>,
}

/// Routing geometry and its rule-derived gaps. A need may be negative (an allowed overlap); the
/// bound on them is nonnegative. A rule change requires invalidating its entire affected area, or
/// constructing a new grid.
pub trait ObstacleSource {
    /// Visit every obstacle on `layer` whose box meets `area`, reporting its required gap
    /// in the probe's width/pad frame. Order and duplicate visits do not affect occupancy.
    fn visit(&self, layer: LayerId, area: BBox, probe: &Probe, f: &mut dyn FnMut(&Shape, Owner, Nm));
    /// A nonnegative upper bound on every gap `visit` can report for `probe`.
    fn max_need(&self, probe: &Probe) -> Nm;
}

/// A cell's owners, summarised per kind: `0` none, `net.0 + 1` that net alone, `MANY` two or
/// more (for `fixed`, `MANY` also stands for a hard owner).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Occ {
    /// Fixed and hard owners.
    pub fixed: u32,
    /// Rippable owners.
    pub rip: u32,
}

const MANY: u32 = u32::MAX;

/// What a cell is to one net.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Pass {
    /// Nothing of another net is in reach.
    Free,
    /// Only rippable copper of other nets is in reach: of this one net, or (`None`) shared by
    /// several, whose owners the caller reads from the world.
    Rip(Option<NetId>),
    /// A hard or fixed owner of another net is in reach.
    Blocked,
}

impl Occ {
    /// No owner.
    pub const FREE: Occ = Occ { fixed: 0, rip: 0 };
    /// Blocked for every net.
    pub const HARD: Occ = Occ { fixed: MANY, rip: 0 };

    /// Add an owner in reach (commutative and idempotent).
    pub fn add(&mut self, owner: Owner) {
        let code = |n: NetId| {
            assert!(n.0 < MANY - 1, "net ID must fit the occupancy encoding");
            n.0 + 1
        };
        let merge = |a: u32, b: u32| if a == 0 || a == b { b } else { MANY };
        match owner {
            Owner::Hard => self.fixed = MANY,
            Owner::Fixed(n) => self.fixed = merge(self.fixed, code(n)),
            Owner::Net(n) => self.rip = merge(self.rip, code(n)),
        }
    }

    /// What this cell is to `net`.
    pub fn pass(self, net: NetId) -> Pass {
        let me = net.0.checked_add(1).filter(|&m| m != MANY).unwrap_or(0);
        if self.fixed != 0 && self.fixed != me {
            return Pass::Blocked;
        }
        match self.rip {
            0 => Pass::Free,
            r if r == me => Pass::Free,
            MANY => Pass::Rip(None),
            r => Pass::Rip(Some(NetId(r - 1))),
        }
    }

    /// Whether plain search may enter this cell routing `net`.
    pub fn free_for(self, net: NetId) -> bool {
        self.pass(net) == Pass::Free
    }

    /// The owners of either state (commutative and idempotent, like `add`).
    fn union(self, other: Occ) -> Occ {
        let merge = |a: u32, b: u32| if a == 0 || a == b { b } else if b == 0 { a } else { MANY };
        Occ { fixed: merge(self.fixed, other.fixed), rip: merge(self.rip, other.rip) }
    }
}

/// A probe plane's stable index within its grid.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct PlaneId(
    /// The zero-based plane index.
    pub u32,
);

/// Cumulative rasterisation work, including tiles rebuilt after invalidation.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct GridStats {
    /// Number of tiles rasterised.
    pub tiles_built: u64,
    /// Number of point/obstacle clearance tests, including repeated owners.
    pub cells_tested: u64,
    /// Number of step memo tiles built.
    pub step_tiles_built: u64,
    /// Number of point and segment clearance tests in step memo builds.
    pub steps_tested: u64,
}

type Tiles<T> = Vec<Option<Box<[T]>>>;

struct Plane {
    probe: Probe,
    layers: Vec<Tiles<Occ>>,
    steps: Vec<Vec<Option<StepTile>>>,
}

/// Lazy occupancy planes and shared per-layer soft costs. Geometry must be invalidated after
/// edits; cells, layer indices and plane IDs passed to accessors must belong to this grid.
pub struct Grid {
    /// Geometry fixed at construction; changing it requires a new grid.
    pub spec: GridSpec,
    planes: Vec<Plane>,
    soft: Vec<Tiles<u16>>,
    tile_cols: u32,
    tile_count: usize,
    stats: GridStats,
}

impl Grid {
    /// Construct an empty cache. Panics for invalid geometry, more than 256 layers (a search
    /// state keeps its layer index in a byte), or tile slots exceeding `usize`.
    pub fn new(spec: GridSpec) -> Grid {
        assert!(spec.pitch > 0 && spec.cols > 0 && spec.rows > 0, "grid dimensions must be positive");
        spec.centre(spec.all().max);
        assert!(spec.layers.len() <= 256, "a search state's layer index fits a byte");
        let tile_cols = (spec.cols - 1) / TILE + 1;
        let tile_rows = (spec.rows - 1) / TILE + 1;
        let tile_count = usize::try_from(u64::from(tile_cols) * u64::from(tile_rows))
            .expect("tile slots must fit usize");
        let soft = (0..spec.layers.len()).map(|_| vec![None; tile_count]).collect();
        Grid { spec, planes: Vec::new(), soft, tile_cols, tile_count, stats: GridStats::default() }
    }

    /// Return the same ID for equal probes, creating an empty plane otherwise.
    /// Panics for a negative width or more planes than a `PlaneId` can encode.
    pub fn plane(&mut self, probe: Probe) -> PlaneId {
        assert!(probe.w >= 0, "probe width must be nonnegative");
        if let Some(index) = self.planes.iter().position(|plane| plane.probe == probe) {
            return PlaneId(index as u32);
        }
        let id = PlaneId(u32::try_from(self.planes.len()).expect("plane index must fit u32"));
        let layers = (0..self.spec.layers.len()).map(|_| vec![None; self.tile_count]).collect();
        let steps = (0..self.spec.layers.len()).map(|_| vec![None; self.tile_count]).collect();
        self.planes.push(Plane { probe, layers, steps });
        id
    }

    /// Read occupancy, rasterising the cell's tile from `src` on first touch.
    pub fn state(&mut self, src: &dyn ObstacleSource, plane: PlaneId, li: usize, c: Cell) -> Occ {
        let (tile, offset) = self.address(c);
        if self.planes[plane.0 as usize].layers[li][tile].is_none() {
            let probe = self.planes[plane.0 as usize].probe;
            let bounds = tile_bounds(&self.spec, self.tile_cols, tile);
            let padding = self.spec.padding(&probe);
            let max_need = src.max_need(&probe);
            assert!(max_need >= 0, "maximum need must be nonnegative");
            let area = grow(centre_box(&self.spec, bounds), i64::from(max_need) + padding);
            let mut cells = vec![Occ::FREE; (TILE * TILE) as usize].into_boxed_slice();
            let mut tested = 0;
            src.visit(self.spec.layers[li], area, &probe, &mut |shape, owner, need| {
                assert!(need <= max_need, "obstacle need must respect its bound");
                let threshold = i64::from(need) + padding;
                let Some(candidate) = self.spec.cells_in(grow(shape.bbox(), threshold)) else {
                    return;
                };
                let Some(candidate) = candidate.intersection(bounds) else {
                    return;
                };
                for y in candidate.min.y..=candidate.max.y {
                    for x in candidate.min.x..=candidate.max.x {
                        tested += 1;
                        if i64::from(point_clearance(self.spec.centre(Cell { x, y }), shape)) < threshold {
                            cells[((y % TILE) * TILE + x % TILE) as usize].add(owner);
                        }
                    }
                }
            });
            self.planes[plane.0 as usize].layers[li][tile] = Some(cells);
            self.stats.tiles_built += 1;
            self.stats.cells_tested += tested;
        }
        self.planes[plane.0 as usize].layers[li][tile].as_ref().expect("tile was built")[offset]
    }

    /// The exact owners of a track of `plane`'s width along the centre segment from `a` to `b`:
    /// `b` is an eight-neighbour of `a`, or `a` itself for a track's end at its centre. Builds the
    /// owning cell's step memo tile on first touch. Panics for a via plane (a via never steps)
    /// or cells that are not neighbours.
    pub fn step(&mut self, src: &dyn ObstacleSource, plane: PlaneId, li: usize, a: Cell, b: Cell) -> Occ {
        let (cell, slot) = step_slot(a, b);
        let pi = plane.0 as usize;
        assert!(self.planes[pi].probe.drill.is_none(), "only a track plane has steps");
        let (tile, offset) = self.address(cell);
        if self.planes[pi].steps[li][tile].is_none() {
            self.build_steps(src, pi, li, tile);
        }
        let memo = self.planes[pi].steps[li][tile].as_ref().expect("step memo tile was built");
        memo.values[usize::from(memo.slots[offset][slot])]
    }

    /// What the step from `a` to `b` (as in [`Grid::step`]) is to `net`. Its exact owners are
    /// padded owners of an end, so ends padded free for `net` answer without the memo.
    pub fn step_pass(&mut self, src: &dyn ObstacleSource, plane: PlaneId, li: usize, a: Cell, b: Cell, net: NetId) -> Pass {
        let ends = self.state(src, plane, li, a).union(self.state(src, plane, li, b));
        if ends.free_for(net) {
            return Pass::Free;
        }
        self.step(src, plane, li, a, b).pass(net)
    }

    /// Fill a tile's step memo from one source visit: each obstacle tests the centres and owned
    /// steps of the tile's cells within its need, half the width and a pitch. The centres' point
    /// clearances (and those of the steps' far ends) decide every step they bound; the rest take
    /// the segment test.
    fn build_steps(&mut self, src: &dyn ObstacleSource, pi: usize, li: usize, tile: usize) {
        let probe = self.planes[pi].probe;
        let spec = &self.spec;
        let bounds = tile_bounds(spec, self.tile_cols, tile);
        let half = (i64::from(probe.w) + 1) / 2;
        let pitch = i64::from(spec.pitch);
        let max_need = src.max_need(&probe);
        assert!(max_need >= 0, "maximum need must be nonnegative");
        let area = grow(centre_box(spec, bounds), i64::from(max_need) + half + pitch);
        let mut memo = vec![[Occ::FREE; NODE + 1]; (TILE * TILE) as usize].into_boxed_slice();
        let mut tested = 0;
        // Half a step's length, rounded up, plus the 2 nm margin: a pitch on an axis, a diagonal
        // (slack is the rounded-up half diagonal).
        let reach = [(pitch + 1) / 2 + 2, i64::from(spec.slack()) + 2];
        let mut points = Vec::new();
        src.visit(spec.layers[li], area, &probe, &mut |shape, owner, need| {
            assert!(need <= max_need, "obstacle need must respect its bound");
            let threshold = i64::from(need) + half;
            let Some(candidate) = spec.cells_in(grow(shape.bbox(), threshold + pitch)) else {
                return;
            };
            let Some(candidate) = candidate.intersection(bounds) else {
                return;
            };
            // The candidates' centres and the far ends of their owned steps (one column either
            // side, one row up), clipped to the grid.
            let (x0, y0) = (candidate.min.x.saturating_sub(1), candidate.min.y);
            let (x1, y1) = ((candidate.max.x + 1).min(spec.cols - 1), (candidate.max.y + 1).min(spec.rows - 1));
            let width = (x1 - x0 + 1) as usize;
            points.clear();
            for y in y0..=y1 {
                for x in x0..=x1 {
                    points.push(i64::from(point_clearance(spec.centre(Cell { x, y }), shape)));
                }
            }
            tested += points.len() as u64;
            let point = |x: u32, y: u32| points[(y - y0) as usize * width + (x - x0) as usize];
            for y in candidate.min.y..=candidate.max.y {
                for x in candidate.min.x..=candidate.max.x {
                    let slots = &mut memo[((y % TILE) * TILE + x % TILE) as usize];
                    let at = point(x, y);
                    if at < threshold {
                        slots[NODE].add(owner);
                    }
                    for (slot, &(dx, dy)) in OWNED.iter().enumerate() {
                        let (nx, ny) = (i64::from(x) + dx, i64::from(y) + dy);
                        if nx < 0 || nx >= i64::from(spec.cols) || ny >= i64::from(spec.rows) {
                            continue;
                        }
                        let (nx, ny) = (nx as u32, ny as u32);
                        let near = at.min(point(nx, ny));
                        let owned = if near <= threshold - 2 {
                            true
                        } else if near - reach[usize::from(dx != 0 && dy != 0)] >= threshold {
                            false
                        } else {
                            tested += 1;
                            let step = Shape::Segment { a: spec.centre(Cell { x, y }), b: spec.centre(Cell { x: nx, y: ny }), w: 0 };
                            i64::from(clearance(&step, shape)) < threshold
                        };
                        if owned {
                            slots[slot].add(owner);
                        }
                    }
                }
            }
        });
        self.planes[pi].steps[li][tile] = Some(pack(&memo));
        self.stats.step_tiles_built += 1;
        self.stats.steps_tested += tested;
    }

    /// Drop built occupancy and step memo tiles on the given layer indices whose centre boxes
    /// meet `area` grown by the plane's reach: its need, half its width and a pitch, which covers
    /// both the padding's slack and the far end of a step a cell owns. Supply the union of old
    /// and new obstacle boxes, including removals. Soft costs persist; both rebuild on next touch.
    pub fn invalidate(&mut self, src: &dyn ObstacleSource, area: BBox, layers: &[usize]) {
        let pitch = i64::from(self.spec.pitch);
        for plane in &mut self.planes {
            let need = src.max_need(&plane.probe);
            assert!(need >= 0, "maximum need must be nonnegative");
            let reach = i64::from(need) + (i64::from(plane.probe.w) + 1) / 2 + pitch;
            let affected = grow(area, reach);
            for &li in layers {
                for (index, (tile, steps)) in plane.layers[li].iter_mut().zip(plane.steps[li].iter_mut()).enumerate() {
                    if (tile.is_some() || steps.is_some())
                        && centre_box(&self.spec, tile_bounds(&self.spec, self.tile_cols, index))
                            .intersects(&affected)
                    {
                        *tile = None;
                        *steps = None;
                    }
                }
            }
        }
    }

    /// Soft cost of entering this cell on this layer; zero unless explicitly added.
    pub fn extra(&self, li: usize, c: Cell) -> u32 {
        let (tile, offset) = self.address(c);
        self.soft[li][tile].as_ref().map_or(0, |values| u32::from(values[offset]))
    }

    /// Add history/congestion cost, saturating at `u16::MAX` independently per cell/layer.
    pub fn add_extra(&mut self, li: usize, c: Cell, v: u16) {
        let (tile, offset) = self.address(c);
        let values = self.soft[li][tile].get_or_insert_with(|| vec![0; (TILE * TILE) as usize].into());
        values[offset] = values[offset].saturating_add(v);
    }

    /// Cumulative tile builds and point-clearance tests.
    pub fn stats(&self) -> GridStats {
        self.stats
    }

    fn address(&self, c: Cell) -> (usize, usize) {
        assert!(c.x < self.spec.cols && c.y < self.spec.rows, "cell must be in grid");
        let tile = u64::from(c.x / TILE) + u64::from(c.y / TILE) * u64::from(self.tile_cols);
        (tile as usize, ((c.y % TILE) * TILE + c.x % TILE) as usize)
    }
}

/// A dense step memo as a [`StepTile`]: equal owner sets share one index, free is index 0.
/// O(slots) time with one hash lookup per non-free slot that differs from the one before.
fn pack(memo: &[Steps]) -> StepTile {
    let mut values = vec![Occ::FREE];
    let mut index = HashMap::from([(Occ::FREE, 0u16)]);
    let mut last = (Occ::FREE, 0u16);
    let mut slots = vec![[0u16; NODE + 1]; memo.len()].into_boxed_slice();
    for (cell, packed) in memo.iter().zip(slots.iter_mut()) {
        for (&occ, at) in cell.iter().zip(packed.iter_mut()) {
            if occ != last.0 {
                let id = *index.entry(occ).or_insert_with(|| {
                    values.push(occ);
                    u16::try_from(values.len() - 1).expect("a tile's distinct owner sets fit 16 bits")
                });
                last = (occ, id);
            }
            *at = last.1;
        }
    }
    StepTile { values, slots }
}

/// The cell owning the step from `a` to `b` (`a` itself for its centre), and its memo slot.
fn step_slot(a: Cell, b: Cell) -> (Cell, usize) {
    if a == b {
        return (a, NODE);
    }
    let d = (i64::from(b.x) - i64::from(a.x), i64::from(b.y) - i64::from(a.y));
    if let Some(slot) = OWNED.iter().position(|&o| o == d) {
        return (a, slot);
    }
    let slot = OWNED.iter().position(|&o| o == (-d.0, -d.1)).expect("a step joins eight-neighbours");
    (b, slot)
}

fn tile_bounds(spec: &GridSpec, tile_cols: u32, index: usize) -> CellBox {
    let min = Cell {
        x: (index as u64 % u64::from(tile_cols)) as u32 * TILE,
        y: (index as u64 / u64::from(tile_cols)) as u32 * TILE,
    };
    CellBox {
        min,
        max: Cell {
            x: min.x.saturating_add(TILE - 1).min(spec.cols - 1),
            y: min.y.saturating_add(TILE - 1).min(spec.rows - 1),
        },
    }
}

fn centre_box(spec: &GridSpec, cells: CellBox) -> BBox {
    BBox::new(spec.centre(cells.min), spec.centre(cells.max))
}

// Wide sums avoid narrowing a reach exceeding Nm::MAX before coordinate saturation.
fn grow(b: BBox, radius: i64) -> BBox {
    if b.is_empty() {
        return b;
    }
    let clamp = |v: i64| v.clamp(i64::from(Nm::MIN), i64::from(Nm::MAX)) as Nm;
    BBox {
        min: P::new(clamp(i64::from(b.min.x) - radius), clamp(i64::from(b.min.y) - radius)),
        max: P::new(clamp(i64::from(b.max.x) + radius), clamp(i64::from(b.max.y) + radius)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::{clearance, Angle};

    /// Deterministic obstacle fixture shared with the search tests.
    #[derive(Clone)]
    pub(super) struct VecSource {
        /// Layer, geometry and ownership in visit order.
        pub(super) items: Vec<(LayerId, Shape, Owner)>,
        /// Uniform required gap in nanometres.
        pub(super) need: Nm,
    }

    impl ObstacleSource for VecSource {
        fn visit(&self, layer: LayerId, area: BBox, _: &Probe, f: &mut dyn FnMut(&Shape, Owner, Nm)) {
            for (li, shape, owner) in &self.items {
                if *li == layer && shape.bbox().intersects(&area) {
                    f(shape, *owner, self.need);
                }
            }
        }

        fn max_need(&self, _: &Probe) -> Nm {
            self.need
        }
    }

    /// One step of a deterministic xorshift64; seeds used here are nonzero.
    pub(super) fn random(seed: &mut u64) -> u32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 32) as u32
    }

    /// Two layers and thirty small mixed obstacles, with local edits confined to the first tile.
    pub(super) fn fixture(mut seed: u64, cols: u32, rows: u32) -> (GridSpec, VecSource) {
        let pitch = 100_000;
        let area = BBox::new(P::ZERO, P::new((cols - 1) as Nm * pitch, (rows - 1) as Nm * pitch));
        let spec = GridSpec::covering(area, pitch, vec![LayerId::F_CU, LayerId::B_CU]);
        let mut items = Vec::new();
        for i in 0..30 {
            let span = if i < 6 { cols.min(20) } else { cols };
            let c = P::new(
                (3 + random(&mut seed) % (span - 6)) as Nm * pitch,
                (3 + random(&mut seed) % (rows - 6)) as Nm * pitch,
            );
            let radius = 50_000 + (random(&mut seed) % 100_001) as Nm;
            let shape = match i % 3 {
                0 => Shape::Circle { c, r: radius },
                1 => Shape::Segment { a: c, b: c.add(P::new(pitch, pitch)), w: radius },
                _ => Shape::Rect { c, half: P::new(radius, radius / 2), rot: Angle::ZERO, corner: 0 },
            };
            let owner = match random(&mut seed) % 5 {
                4 => Owner::Hard,
                3 => Owner::Fixed(NetId(random(&mut seed) % 3)),
                net => Owner::Net(NetId(net)),
            };
            items.push((spec.layers[(random(&mut seed) % 2) as usize], shape, owner));
        }
        (spec, VecSource { items, need: 150_000 })
    }

    /// Standard track and through-via probes.
    pub(super) fn planes(grid: &mut Grid) -> (PlaneId, PlaneId) {
        let track = grid.plane(Probe { class: ClassId(0), w: 150_000, drill: None });
        let via = grid.plane(Probe { class: ClassId(0), w: 450_000, drill: Some(300_000) });
        (track, via)
    }

    /// A cell itself, then the in-grid neighbours whose steps it owns.
    fn owned(spec: &GridSpec, a: Cell) -> Vec<Cell> {
        let mut out = vec![a];
        for (dx, dy) in OWNED {
            let (x, y) = (i64::from(a.x) + dx, i64::from(a.y) + dy);
            if x >= 0 && x < i64::from(spec.cols) && y < i64::from(spec.rows) {
                out.push(Cell { x: x as u32, y: y as u32 });
            }
        }
        out
    }

    /// Every centre and owned step of a track plane, in cell order.
    fn steps(grid: &mut Grid, src: &VecSource, plane: PlaneId) -> Vec<Occ> {
        let mut values = Vec::new();
        for li in 0..grid.spec.layers.len() {
            for y in 0..grid.spec.rows {
                for x in 0..grid.spec.cols {
                    let a = Cell { x, y };
                    for b in owned(&grid.spec, a) {
                        values.push(grid.step(src, plane, li, a, b));
                    }
                }
            }
        }
        values
    }

    fn states(grid: &mut Grid, src: &VecSource, planes: &[PlaneId]) -> Vec<Occ> {
        let mut values = Vec::new();
        for &plane in planes {
            for li in 0..grid.spec.layers.len() {
                for y in 0..grid.spec.rows {
                    for x in 0..grid.spec.cols {
                        values.push(grid.state(src, plane, li, Cell { x, y }));
                    }
                }
            }
        }
        values
    }

    #[test]
    fn update_equals_rebuild() {
        // 40 columns fit one tile: use 80 here to prove unaffected tiles are retained.
        let (mut spec, mut src) = fixture(0x1234_5678_9abc_def0, 40, 40);
        spec.cols = 80;
        let mut grid = Grid::new(spec.clone());
        let (track, via) = planes(&mut grid);
        assert_eq!(planes(&mut grid), (track, via), "equal probes reuse plane IDs");
        assert_eq!(spec.slack(), 70_711, "slack is the exact upward integer bound");
        assert_eq!(GridSpec::pitch_for(900_002), 300_000, "pitch divides down by three");
        assert_eq!(GridSpec::pitch_for(0), 1, "pitch is at least one nanometre");
        assert_eq!(spec.cell_at(P::new(50_000, -1)), Cell { x: 1, y: 0 }, "nearest ties and clamping");
        assert_eq!(spec.cells_in(BBox::new(P::new(1, 1), P::new(99_999, 99_999))), None,
            "a box between centres has no cells");
        assert_eq!(spec.cells_in(BBox::point(P::new(-1, -1))), None, "outside boxes have no cells");
        assert_eq!(spec.all().inflate(u32::MAX, &spec), spec.all(), "box inflation clamps without overflow");
        assert_eq!((spec.all().width(), spec.all().height()), (80, 40), "cell counts are inclusive");
        assert!(!Occ::HARD.free_for(NetId(u32::MAX - 1)), "hard never aliases a net");
        assert!(!Occ::HARD.free_for(NetId(u32::MAX)), "hard never wraps to a net");
        let before = states(&mut grid, &src, &[track, via]);
        let built = grid.stats().tiles_built;
        assert_eq!(built, 8, "each plane/layer/tile builds once despite repeated reads");
        assert!(grid.stats().cells_tested > 0, "stats count candidate clearance tests");
        let steps_before = steps(&mut grid, &src, track);
        assert_eq!(grid.stats().step_tiles_built, 4, "each track layer/tile builds its step memo once");
        assert_eq!(grid.stats().tiles_built, built, "the step memo builds no occupancy tiles");
        let c = Cell { x: 70, y: 20 };
        grid.add_extra(0, c, u16::MAX - 2);
        grid.add_extra(0, c, 4);
        assert_eq!(grid.extra(0, c), u32::from(u16::MAX), "soft costs saturate");
        assert_eq!(grid.extra(1, c), 0, "soft costs are independent per layer");
        let mut area = BBox::EMPTY;
        for (_, shape, _) in &mut src.items[..4] {
            area = area.union(shape.bbox());
            *shape = shape.translated(P::new(200_000, -100_000));
            area = area.union(shape.bbox());
        }
        for index in [5, 4] {
            area = area.union(src.items.remove(index).1.bbox());
        }
        grid.invalidate(&src, area, &[0, 1]);
        assert_eq!(grid.stats().tiles_built, built, "invalidation rebuilds lazily");
        assert_eq!(grid.stats().step_tiles_built, 4, "invalidation rebuilds step memos lazily");
        let after = states(&mut grid, &src, &[track, via]);
        assert_ne!(before, after, "the edited obstacles actually change occupancy");
        let rebuilt = grid.stats().tiles_built - built;
        assert!(rebuilt > 0 && rebuilt < built, "local updates rebuild fewer tiles than a full build");
        let steps_after = steps(&mut grid, &src, track);
        assert_ne!(steps_before, steps_after, "the edited obstacles actually change step owners");
        let rebuilt = grid.stats().step_tiles_built - 4;
        assert!(rebuilt > 0 && rebuilt < 4, "local updates rebuild fewer step memos than a full build");
        let mut fresh = Grid::new(spec.clone());
        let (fresh_track, fresh_via) = planes(&mut fresh);
        assert_eq!(after, states(&mut fresh, &src, &[fresh_track, fresh_via]), "update equals full rebuild");
        assert_eq!(steps_after, steps(&mut fresh, &src, fresh_track), "a kept step memo equals a fresh one");
        assert_eq!(grid.extra(0, c), u32::from(u16::MAX), "invalidation preserves history costs");
        src.items.reverse();
        let mut reversed = Grid::new(spec);
        let (rt, rv) = planes(&mut reversed);
        assert_eq!(after, states(&mut reversed, &src, &[rt, rv]), "owner merging ignores visit order");
        assert_eq!(steps_after, steps(&mut reversed, &src, rt), "step owners ignore visit order");
    }

    #[test]
    fn every_step_is_legal() {
        let (spec, src) = fixture(0x1234_5678_9abc_def0, 40, 40);
        let mut grid = Grid::new(spec.clone());
        let (track, via) = planes(&mut grid);
        let values = states(&mut grid, &src, &[track]);
        let vias = states(&mut grid, &src, &[via]);
        let index = |li: usize, c: Cell| (li * spec.rows as usize + c.y as usize) * spec.cols as usize
            + c.x as usize;
        let (mut checked, mut gained, mut blocked) = (0, 0, 0);
        for li in 0..spec.layers.len() {
            for net in [NetId(0), NetId(1), NetId(2)] {
                for y in 0..spec.rows {
                    for x in 0..spec.cols {
                        let a = Cell { x, y };
                        // The centre, then one orientation of each undirected pair: all eight neighbours.
                        for b in owned(&spec, a) {
                            let copper = if a == b {
                                Shape::Circle { c: spec.centre(a), r: 75_000 }
                            } else {
                                Shape::Segment { a: spec.centre(a), b: spec.centre(b), w: 150_000 }
                            };
                            let legal = src.items.iter().all(|(layer, shape, owner)| {
                                *layer != spec.layers[li] || *owner == Owner::Net(net) || *owner == Owner::Fixed(net)
                                    || clearance(&copper, shape) >= src.need
                            });
                            let step = grid.step(&src, track, li, a, b);
                            assert_eq!(step.free_for(net), legal, "the step memo is exact: {li}, {net:?}, {a:?}, {b:?}");
                            assert_eq!(grid.step_pass(&src, track, li, a, b, net), step.pass(net),
                                "padded free ends answer as the memo does: {li}, {net:?}, {a:?}, {b:?}");
                            if values[index(li, a)].free_for(net) && values[index(li, b)].free_for(net) {
                                assert!(legal, "free endpoint step keeps clearance: {li}, {net:?}, {a:?}, {b:?}");
                                checked += 1;
                            } else if legal {
                                gained += 1;
                            } else {
                                blocked += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 0 && blocked > 0, "step legality checks are nonvacuous");
        assert!(gained > 0, "exact steps admit some steps whose ends the padding blocks");
        // A via sits at its centre: its plane is exact, free just where a via pad there keeps every
        // foreign obstacle's gap, with no slack for steps it never takes.
        let (mut free, mut blocked) = (0, 0);
        for li in 0..spec.layers.len() {
            for net in [NetId(0), NetId(1), NetId(2)] {
                for y in 0..spec.rows {
                    for x in 0..spec.cols {
                        let c = Cell { x, y };
                        let pad = Shape::Circle { c: spec.centre(c), r: 225_000 };
                        let legal = src.items.iter().all(|(layer, shape, owner)| {
                            *layer != spec.layers[li] || *owner == Owner::Net(net) || *owner == Owner::Fixed(net)
                                || clearance(&pad, shape) >= src.need
                        });
                        assert_eq!(vias[index(li, c)].free_for(net), legal, "the via plane is exact: {li}, {net:?}, {c:?}");
                        if legal { free += 1 } else { blocked += 1 }
                    }
                }
            }
        }
        assert!(free > 0 && blocked > 0, "via plane checks are nonvacuous");
    }

    #[test]
    fn four_cell_states() {
        let (a, b, c) = (NetId(1), NetId(2), NetId(3));
        let mut occ = Occ::FREE;
        assert_eq!(occ.pass(a), Pass::Free, "no owner: free");
        occ.add(Owner::Net(a));
        assert_eq!((occ.pass(a), occ.pass(b)), (Pass::Free, Pass::Rip(Some(a))), "one rippable owner: own or rip it");
        occ.add(Owner::Net(a));
        assert_eq!(occ.pass(b), Pass::Rip(Some(a)), "adding an owner twice changes nothing");
        occ.add(Owner::Net(b));
        assert_eq!((occ.pass(a), occ.pass(c)), (Pass::Rip(None), Pass::Rip(None)), "two rippable nets: shared, not hard");
        assert!(!occ.free_for(a) && !occ.free_for(c), "plain search treats shared as blocked");
        let mut pad = Occ::FREE;
        pad.add(Owner::Fixed(a));
        assert_eq!((pad.pass(a), pad.pass(b)), (Pass::Free, Pass::Blocked), "a fixed owner passes its own net only");
        pad.add(Owner::Net(b));
        assert_eq!((pad.pass(a), pad.pass(b)), (Pass::Rip(Some(b)), Pass::Blocked), "next to its pad a net may rip another");
        pad.add(Owner::Fixed(b));
        assert_eq!(pad.pass(a), Pass::Blocked, "two fixed nets block both");
        let mut hard = Occ::FREE;
        hard.add(Owner::Net(a));
        hard.add(Owner::Hard);
        assert_eq!((hard.pass(a), hard.pass(c)), (Pass::Blocked, Pass::Blocked), "a hard owner blocks every net");
        let owners = [Owner::Net(a), Owner::Fixed(c), Owner::Net(b), Owner::Fixed(c)];
        let (mut fwd, mut rev) = (Occ::FREE, Occ::FREE);
        owners.iter().for_each(|&o| fwd.add(o));
        owners.iter().rev().for_each(|&o| rev.add(o));
        assert_eq!(fwd, rev, "merging ignores order");
    }
}
