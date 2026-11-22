//! Deterministic label placement in world units, with Y pointing up.
//!
//! Requests are ordered by descending priority, then input index. Placement tries
//! preferred spots, slides in increasing step multiples (spot order, then negative
//! before positive), and finally square rings of label centres, clockwise from
//! the top centre. Inside spots do not slide. Request anchors block every other
//! request; explicit obstacles with a matching owner are ignored. Placed labels
//! always block later labels, including labels of the same owner.
//!
//! A uniform grid uses cells of size max(4 * step, median label width). Hash
//! tables are only queried by key, never iterated: neither hash seeds nor bucket
//! order influence placement. Boxes use Euclidean clearance; wires use the exact
//! segment-to-box distance plus their half width. A leader joins nearest boundary
//! points when a label no longer touches its anchor inflated by twice the gap. A leader
//! keeps the gap from another request's anchor, a placed label and another owner's leader
//! (wires and supplied obstacles it may cross), and placed leaders block later labels.
//! A free candidate whose leader would cross is passed over while the search finds one
//! whose leader is clear: through the slides, and through the rings up to twice the
//! radius of the first such candidate; only when none is clear does the first crossing
//! candidate stand. Within a ring, the clear candidate with the shortest leader wins,
//! then the one nearest the anchor's centre, then ring order.
//!
//! With n requests, m obstacles, k candidate attempts per request, and bounded
//! grid occupancy and object extents, expected time is O(n log n + m + n * k),
//! with O(n + m) storage. More generally, indexing costs the number of occupied
//! cells and queries cost the cells visited plus their entries; large or densely
//! overlapping objects can increase both costs. Counting overlaps uses the same
//! grid and costs O(n log n + m + n) under those same occupancy assumptions.
//!
//! Inputs must have finite, ordered boxes, nonnegative sizes, gaps and wire half
//! widths, and a finite positive step; derived coordinates must remain finite.
//! Equality at the requested clearance is allowed. At zero clearance, box
//! interiors must remain disjoint and even a zero-width wire touching a label
//! counts as an overlap. The fallback tries rings through 10,000, then returns
//! its last candidate even if blocked, so exhausting the search never panics.
//! That forced result can have overlaps. The counter reports label pairs and
//! supplied obstacles; include symbol bodies in obstacles to count them too.

use std::collections::HashMap;

/// An axis-aligned box [min_x, min_y, max_x, max_y], world units, Y up.
pub type Bx = [f64; 4];

/// Where a label may go relative to its anchor box, in caller preference order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Spot {
    /// Above the anchor, with left edges aligned.
    AboveLeft,
    /// Above the anchor, horizontally centred.
    Above,
    /// Above the anchor, with right edges aligned.
    AboveRight,
    /// Below the anchor, with left edges aligned.
    BelowLeft,
    /// Below the anchor, horizontally centred.
    Below,
    /// Below the anchor, with right edges aligned.
    BelowRight,
    /// Right of the anchor, with top edges aligned.
    RightTop,
    /// Right of the anchor, vertically centred.
    Right,
    /// Right of the anchor, with bottom edges aligned.
    RightBottom,
    /// Left of the anchor, with top edges aligned.
    LeftTop,
    /// Left of the anchor, vertically centred.
    Left,
    /// Left of the anchor, with bottom edges aligned.
    LeftBottom,
    /// Inside the anchor, with its top-left corner inset by the gap.
    InsideTopLeft,
}

/// A label's anchor, dimensions, placement preferences and obstacle ownership.
#[derive(Clone, Debug, PartialEq)]
pub struct LabelReq {
    /// The box the label belongs to (a symbol body, a tag's end, a block frame).
    pub anchor: Bx,
    /// The label's size (width, height), world units.
    pub size: [f64; 2],
    /// Candidate spots in preference order (at least one).
    pub spots: Vec<Spot>,
    /// Higher places first; ties by index.
    pub priority: i32,
    /// The label ignores its own anchor, which blocks every other label.
    /// Explicit obstacles with this owner are ignored, including its own wires.
    pub owner: u32,
    /// A name that helps but is not needed (a net's name: the wire already shows the
    /// connection). It takes only its spots and their slides, never the ring fallback, and
    /// is left out when none is free.
    pub optional: bool,
}

/// Things labels must not cover.
#[derive(Clone, Debug, PartialEq)]
pub enum Obstacle {
    /// An axis-aligned solid box.
    Box {
        /// The occupied box in world units.
        b: Bx,
        /// Labels with this owner ignore the box.
        owner: u32,
    },
    /// A wire segment with its half width, in world units.
    Seg {
        /// The first endpoint.
        a: [f64; 2],
        /// The second endpoint.
        b: [f64; 2],
        /// The wire's radius about its centre segment, including its endpoints.
        half: f64,
        /// Labels with this owner ignore the wire.
        owner: u32,
    },
}

/// Clearance and search spacing for label placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelOpts {
    /// The gap between a label and its anchor, and clearance from everything else.
    pub gap: f64,
    /// The positive step of the fallback and slide searches, in world units.
    pub step: f64,
}

impl Default for LabelOpts {
    /// Uses a gap of 0.25 and a search step of 0.5 world units.
    fn default() -> Self {
        Self {
            gap: 0.25,
            step: 0.5,
        }
    }
}

/// Label boxes and their placement annotations, all in request order.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    /// Per request, in request order: the label's box.
    pub boxes: Vec<Bx>,
    /// The spot used, including slides, or None for a fallback placement.
    pub spot: Vec<Option<Spot>>,
    /// When away from its anchor, a leader from the nearest anchor boundary point
    /// to the nearest label boundary point; otherwise None.
    pub leader: Vec<Option<[[f64; 2]; 2]>>,
    /// Per request: an optional label with no free spot or slide, not drawn and blocking
    /// nothing (its box, spot and leader are then meaningless).
    pub left_out: Vec<bool>,
}

/// Places every label. Pure; expected O(n log n + n * k) after indexing obstacles,
/// with a uniform grid over the obstacles and placed labels (k = candidates tried).
/// See the module documentation for occupancy assumptions and the fallback cap.
pub fn place_labels(reqs: &[LabelReq], obstacles: &[Obstacle], o: &LabelOpts) -> Placed {
    let mut grid = Grid::new(cell_size(reqs, o.step), obstacles);
    for (request, req) in reqs.iter().enumerate() {
        grid.insert(req.anchor, Shape::Anchor { request });
    }
    let mut order: Vec<usize> = (0..reqs.len()).collect();
    order.sort_unstable_by(|&a, &b| {
        reqs[b]
            .priority
            .cmp(&reqs[a].priority)
            .then_with(|| a.cmp(&b))
    });
    let mut placed = Placed {
        boxes: vec![[0.0; 4]; reqs.len()],
        spot: vec![None; reqs.len()],
        leader: vec![None; reqs.len()],
        left_out: vec![false; reqs.len()],
    };
    for index in order {
        let req = &reqs[index];
        let (b, spot) = choose_box(req, index, o, &grid);
        if req.optional && spot.is_none() {
            placed.left_out[index] = true;
            continue;
        }
        placed.boxes[index] = b;
        placed.spot[index] = spot;
        placed.leader[index] = leader(req.anchor, b, o.gap);
        grid.insert(b, Shape::Label);
        if let Some([a, end]) = placed.leader[index] {
            let bounds = [a[0].min(end[0]), a[1].min(end[1]), a[0].max(end[0]), a[1].max(end[1])];
            grid.insert(bounds, Shape::Leader { a, b: end, owner: req.owner });
        }
    }
    placed
}

/// Counts overlapping label pairs plus label-obstacle overlaps, respecting owners,
/// with `clear` as the minimum spacing (zero is the goal).
/// `placed.boxes` must be in request order, with one box per request. Anchors are
/// included in this count when supplied as box obstacles; each supplied obstacle
/// is counted separately. Spot and leader annotations do not affect the count.
pub fn overlap_count(
    reqs: &[LabelReq],
    placed: &Placed,
    obstacles: &[Obstacle],
    clear: f64,
) -> usize {
    let mut grid = Grid::new(cell_size(reqs, LabelOpts::default().step), obstacles);
    let mut count = 0;
    for (index, (req, &b)) in reqs.iter().zip(&placed.boxes).enumerate() {
        if placed.left_out[index] {
            continue;
        }
        grid.visit(inflate(b, clear), |entry| {
            if entry.blocks(b, index, req.owner, clear) {
                count += 1;
            }
            true
        });
        // Only earlier labels are indexed, so each pair is counted once.
        grid.insert(b, Shape::Label);
    }
    count
}

fn cell_size(reqs: &[LabelReq], step: f64) -> f64 {
    let mut widths: Vec<f64> = reqs.iter().map(|req| req.size[0]).collect();
    widths.sort_unstable_by(f64::total_cmp);
    let typical = widths.get(widths.len() / 2).copied().unwrap_or(0.0);
    (4.0 * step).max(typical)
}

enum Shape {
    Anchor { request: usize },
    OwnedBox { owner: u32 },
    Wire {
        a: [f64; 2],
        b: [f64; 2],
        half: f64,
        owner: u32,
    },
    Label,
    /// A placed label's leader; blocks later labels and leaders of other owners.
    Leader {
        a: [f64; 2],
        b: [f64; 2],
        owner: u32,
    },
}

struct Entry {
    bounds: Bx,
    cells: [i64; 4],
    shape: Shape,
}

impl Entry {
    fn blocks(&self, b: Bx, request: usize, owner: u32, clear: f64) -> bool {
        match &self.shape {
            Shape::Anchor { request: other } => {
                *other != request && boxes_overlap(b, self.bounds, clear)
            }
            Shape::OwnedBox { owner: other } => {
                *other != owner && boxes_overlap(b, self.bounds, clear)
            }
            Shape::Wire {
                a,
                b: end,
                half,
                owner: other,
            } => {
                if *other == owner {
                    return false;
                }
                let distance = segment_box_distance(*a, *end, b);
                distance == 0.0 || distance < *half + clear
            }
            Shape::Label => boxes_overlap(b, self.bounds, clear),
            Shape::Leader {
                a,
                b: end,
                owner: other,
            } => *other != owner && segment_box_distance(*a, *end, b) < clear.max(f64::MIN_POSITIVE),
        }
    }

    /// Whether a leader from `a` to `b` crosses this entry or comes closer than `clear`:
    /// another request's anchor, a placed label, or another owner's leader. Wires and
    /// supplied obstacles (tags, frames) may be crossed, as a wire crosses them.
    fn crossed_by(&self, a: [f64; 2], b: [f64; 2], request: usize, owner: u32, clear: f64) -> bool {
        let near = |distance: f64| distance == 0.0 || distance < clear;
        match &self.shape {
            Shape::Anchor { request: other } => {
                *other != request && near(segment_box_distance(a, b, self.bounds))
            }
            Shape::OwnedBox { .. } | Shape::Wire { .. } => false,
            Shape::Label => near(segment_box_distance(a, b, self.bounds)),
            Shape::Leader {
                a: start,
                b: end,
                owner: other,
            } => *other != owner && near(segment_segment_distance(a, b, *start, *end)),
        }
    }
}

struct Grid {
    cell: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
    entries: Vec<Entry>,
}

impl Grid {
    fn new(cell: f64, obstacles: &[Obstacle]) -> Self {
        let mut grid = Self {
            cell,
            cells: HashMap::new(),
            entries: Vec::new(),
        };
        for obstacle in obstacles {
            match *obstacle {
                Obstacle::Box { b, owner } => grid.insert(b, Shape::OwnedBox { owner }),
                Obstacle::Seg { a, b, half, owner } => grid.insert(
                    inflate(
                        [
                            a[0].min(b[0]),
                            a[1].min(b[1]),
                            a[0].max(b[0]),
                            a[1].max(b[1]),
                        ],
                        half,
                    ),
                    Shape::Wire { a, b, half, owner },
                ),
            }
        }
        grid
    }

    fn range(&self, b: Bx) -> [i64; 4] {
        b.map(|coordinate| (coordinate / self.cell).floor() as i64)
    }

    fn insert(&mut self, bounds: Bx, shape: Shape) {
        let cells = self.range(bounds);
        let index = self.entries.len();
        self.entries.push(Entry {
            bounds,
            cells,
            shape,
        });
        for x in cells[0]..=cells[2] {
            for y in cells[1]..=cells[3] {
                self.cells.entry((x, y)).or_default().push(index);
            }
        }
    }

    // Visit each neighbour once; false from the visitor stops the search.
    fn visit(&self, bounds: Bx, mut visitor: impl FnMut(&Entry) -> bool) -> bool {
        let cells = self.range(bounds);
        for x in cells[0]..=cells[2] {
            for y in cells[1]..=cells[3] {
                if let Some(indices) = self.cells.get(&(x, y)) {
                    for &index in indices {
                        let entry = &self.entries[index];
                        // The lower-left shared cell is unique, even for boxes
                        // spanning many cells. No per-query set is needed.
                        if x == cells[0].max(entry.cells[0])
                            && y == cells[1].max(entry.cells[1])
                            && !visitor(entry)
                        {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    fn free(&self, b: Bx, request: usize, owner: u32, gap: f64) -> bool {
        self.visit(inflate(b, gap), |entry| {
            !entry.blocks(b, request, owner, gap)
        })
    }

    /// Whether the leader a label at `b` would draw keeps the gap from everything foreign
    /// it may not cross. Its ends are pulled in by `LEADER_INSET` so meeting its own anchor
    /// and label is allowed.
    fn leader_clear(&self, req: &LabelReq, b: Bx, request: usize, gap: f64) -> bool {
        let Some([start, end]) = leader(req.anchor, b, gap) else {
            return true;
        };
        let length = (end[0] - start[0]).hypot(end[1] - start[1]);
        if length <= 2.0 * LEADER_INSET {
            return true;
        }
        let pull = LEADER_INSET / length;
        let a = [
            start[0] + (end[0] - start[0]) * pull,
            start[1] + (end[1] - start[1]) * pull,
        ];
        let e = [
            end[0] - (end[0] - start[0]) * pull,
            end[1] - (end[1] - start[1]) * pull,
        ];
        let bounds = [a[0].min(e[0]), a[1].min(e[1]), a[0].max(e[0]), a[1].max(e[1])];
        self.visit(inflate(bounds, gap), |entry| !entry.crossed_by(a, e, request, req.owner, gap))
    }
}

/// How far a leader's ends are pulled in before testing what it crosses.
const LEADER_INSET: f64 = 0.05;

/// A label never covers what it names: its own anchor is not an obstacle for it (its own
/// wires may meet it), but no candidate may overlap the anchor itself.
fn clear_of_anchor(req: &LabelReq, b: Bx) -> bool {
    !boxes_overlap(b, req.anchor, 0.0)
}

/// A free candidate whose leader would cross something foreign is kept only as a last resort:
/// the search goes on for one whose leader is clear (or that needs none).
fn choose_box(req: &LabelReq, index: usize, o: &LabelOpts, grid: &Grid) -> (Bx, Option<Spot>) {
    // None: blocked; Some(clear): free, with or without a leader crossing something foreign.
    let check = |b: Bx| {
        (clear_of_anchor(req, b) && grid.free(b, index, req.owner, o.gap))
            .then(|| grid.leader_clear(req, b, index, o.gap))
    };
    let mut crossed: Option<(Bx, Option<Spot>)> = None;
    for &spot in &req.spots {
        let b = spot_box(req, spot, o.gap);
        match check(b) {
            Some(true) => return (b, Some(spot)),
            Some(false) => _ = crossed.get_or_insert((b, Some(spot))),
            None => {}
        }
    }
    let max_shift = req
        .spots
        .iter()
        .filter_map(|&spot| slide_axis(spot))
        .map(|axis| req.anchor[axis + 2] - req.anchor[axis] + req.size[axis])
        .fold(0.0, f64::max);
    let steps = (max_shift / o.step).floor() as usize;
    for multiple in 1..=steps {
        let shift = multiple as f64 * o.step;
        for &spot in &req.spots {
            let Some(axis) = slide_axis(spot) else {
                continue;
            };
            if shift > req.anchor[axis + 2] - req.anchor[axis] + req.size[axis] {
                continue;
            }
            for direction in [-1.0, 1.0] {
                let mut b = spot_box(req, spot, o.gap);
                b[axis] += direction * shift;
                b[axis + 2] += direction * shift;
                match check(b) {
                    Some(true) => return (b, Some(spot)),
                    Some(false) => _ = crossed.get_or_insert((b, Some(spot))),
                    None => {}
                }
            }
        }
    }
    if req.optional {
        return crossed.unwrap_or((spot_box(req, req.spots[0], o.gap), None));
    }

    let centre = box_centre(req.anchor);
    let mut last = centred_box(centre, req.size);
    let mut first_crossed = None;
    for radius in 1..=10_000 {
        let mut ring_crossed = false;
        // The ring's clear candidate with the shortest leader, then nearest the centre.
        let mut best: Option<(f64, _, Bx)> = None;
        for offset in 0..8 * radius {
            // One visit per lattice point on the perimeter, in Y-up order.
            let (x, y) = if offset <= radius {
                (offset, radius)
            } else if offset <= 3 * radius {
                (radius, 2 * radius - offset)
            } else if offset <= 5 * radius {
                (4 * radius - offset, -radius)
            } else if offset <= 7 * radius {
                (-radius, offset - 6 * radius)
            } else {
                (offset - 8 * radius, radius)
            };
            last = centred_box(
                [
                    centre[0] + x as f64 * o.step,
                    centre[1] + y as f64 * o.step,
                ],
                req.size,
            );
            match check(last) {
                Some(true) => {
                    let length = leader(req.anchor, last, o.gap)
                        .map_or(0.0, |[a, e]| (e[0] - a[0]).hypot(e[1] - a[1]));
                    let near = x * x + y * y;
                    if best.is_none_or(|(l, n, _)| length < l || (length == l && near < n)) {
                        best = Some((length, near, last));
                    }
                }
                Some(false) => {
                    ring_crossed = true;
                    _ = crossed.get_or_insert((last, None));
                }
                None => {}
            }
        }
        if let Some((_, _, b)) = best {
            return (b, None);
        }
        // A free ring whose leaders all cross: rings up to twice as far may still hold a clear
        // one; beyond that leaders only get longer, so the first free candidate stands.
        match first_crossed {
            Some(r0) if radius > 2 * r0 => break,
            None if ring_crossed => first_crossed = Some(radius),
            _ => {}
        }
    }
    crossed.unwrap_or((last, None))
}

fn slide_axis(spot: Spot) -> Option<usize> {
    match spot {
        Spot::AboveLeft
        | Spot::Above
        | Spot::AboveRight
        | Spot::BelowLeft
        | Spot::Below
        | Spot::BelowRight => Some(0),
        Spot::RightTop
        | Spot::Right
        | Spot::RightBottom
        | Spot::LeftTop
        | Spot::Left
        | Spot::LeftBottom => Some(1),
        Spot::InsideTopLeft => None,
    }
}

fn spot_box(req: &LabelReq, spot: Spot, gap: f64) -> Bx {
    let [left, bottom, right, top] = req.anchor;
    let [width, height] = req.size;
    let centre = box_centre(req.anchor);
    let middle_x = centre[0] - width * 0.5;
    let middle_y = centre[1] - height * 0.5;
    let (x, y) = match spot {
        Spot::AboveLeft => (left, top + gap),
        Spot::Above => (middle_x, top + gap),
        Spot::AboveRight => (right - width, top + gap),
        Spot::BelowLeft => (left, bottom - gap - height),
        Spot::Below => (middle_x, bottom - gap - height),
        Spot::BelowRight => (right - width, bottom - gap - height),
        Spot::RightTop => (right + gap, top - height),
        Spot::Right => (right + gap, middle_y),
        Spot::RightBottom => (right + gap, bottom),
        Spot::LeftTop => (left - gap - width, top - height),
        Spot::Left => (left - gap - width, middle_y),
        Spot::LeftBottom => (left - gap - width, bottom),
        Spot::InsideTopLeft => (left + gap, top - gap - height),
    };
    [x, y, x + width, y + height]
}

fn inflate(b: Bx, amount: f64) -> Bx {
    [b[0] - amount, b[1] - amount, b[2] + amount, b[3] + amount]
}

fn box_centre(b: Bx) -> [f64; 2] {
    [b[0] * 0.5 + b[2] * 0.5, b[1] * 0.5 + b[3] * 0.5]
}

fn centred_box(centre: [f64; 2], size: [f64; 2]) -> Bx {
    [
        centre[0] - size[0] * 0.5,
        centre[1] - size[1] * 0.5,
        centre[0] + size[0] * 0.5,
        centre[1] + size[1] * 0.5,
    ]
}

fn boxes_overlap(a: Bx, b: Bx, clear: f64) -> bool {
    if a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3] {
        return true;
    }
    let dx = (a[0] - b[2]).max(b[0] - a[2]).max(0.0);
    let dy = (a[1] - b[3]).max(b[1] - a[3]).max(0.0);
    dx.hypot(dy) < clear
}

fn point_box_distance(point: [f64; 2], b: Bx) -> f64 {
    let dx = (b[0] - point[0]).max(point[0] - b[2]).max(0.0);
    let dy = (b[1] - point[1]).max(point[1] - b[3]).max(0.0);
    dx.hypot(dy)
}

fn point_segment_distance(point: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let length = dx.hypot(dy);
    if length == 0.0 {
        return (point[0] - a[0]).hypot(point[1] - a[1]);
    }
    let ux = dx / length;
    let uy = dy / length;
    let along = ((point[0] - a[0]) * ux + (point[1] - a[1]) * uy).clamp(0.0, length);
    (point[0] - (a[0] + along * ux)).hypot(point[1] - (a[1] + along * uy))
}

fn segment_segment_distance(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> f64 {
    let cross = |o: [f64; 2], p: [f64; 2], q: [f64; 2]| {
        (p[0] - o[0]) * (q[1] - o[1]) - (p[1] - o[1]) * (q[0] - o[0])
    };
    let (d1, d2) = (cross(c, d, a), cross(c, d, b));
    let (d3, d4) = (cross(a, b, c), cross(a, b, d));
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return 0.0;
    }
    point_segment_distance(a, c, d)
        .min(point_segment_distance(b, c, d))
        .min(point_segment_distance(c, a, b))
        .min(point_segment_distance(d, a, b))
}

fn segment_box_distance(a: [f64; 2], b: [f64; 2], bounds: Bx) -> f64 {
    // Slab clipping detects crossings whose endpoints are both outside.
    let mut enter: f64 = 0.0;
    let mut leave: f64 = 1.0;
    let mut intersects = true;
    for axis in 0..2 {
        let delta = b[axis] - a[axis];
        if delta == 0.0 {
            if a[axis] < bounds[axis] || a[axis] > bounds[axis + 2] {
                intersects = false;
                break;
            }
        } else {
            let t0 = (bounds[axis] - a[axis]) / delta;
            let t1 = (bounds[axis + 2] - a[axis]) / delta;
            enter = enter.max(t0.min(t1));
            leave = leave.min(t0.max(t1));
            if enter > leave {
                intersects = false;
                break;
            }
        }
    }
    if intersects {
        return 0.0;
    }
    // For disjoint segments and rectangles, a nearest pair includes either a
    // wire endpoint or a rectangle corner (also covering parallel edges).
    let mut distance = point_box_distance(a, bounds).min(point_box_distance(b, bounds));
    for corner in [
        [bounds[0], bounds[1]],
        [bounds[0], bounds[3]],
        [bounds[2], bounds[1]],
        [bounds[2], bounds[3]],
    ] {
        distance = distance.min(point_segment_distance(corner, a, b));
    }
    distance
}

fn leader(anchor: Bx, label: Bx, gap: f64) -> Option<[[f64; 2]; 2]> {
    let expanded = inflate(anchor, 2.0 * gap);
    if label[0] <= expanded[2]
        && expanded[0] <= label[2]
        && label[1] <= expanded[3]
        && expanded[1] <= label[3]
    {
        return None;
    }
    let mut start = [0.0; 2];
    let mut end = [0.0; 2];
    for axis in 0..2 {
        if anchor[axis + 2] < label[axis] {
            start[axis] = anchor[axis + 2];
            end[axis] = label[axis];
        } else if label[axis + 2] < anchor[axis] {
            start[axis] = anchor[axis];
            end[axis] = label[axis + 2];
        } else {
            let middle = anchor[axis].max(label[axis]) * 0.5
                + anchor[axis + 2].min(label[axis + 2]) * 0.5;
            start[axis] = middle;
            end[axis] = middle;
        }
    }
    Some([start, end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_free_spot_wins_and_owners_are_respected() {
        let opts = LabelOpts::default();
        let reqs: Vec<LabelReq> = (0..3)
            .map(|index| {
                let x = index as f64 * 10.0;
                LabelReq {
                    anchor: [x, 0.0, x + 2.0, 1.0],
                    size: [1.5, 0.5],
                    spots: vec![Spot::Above, Spot::Below],
                    priority: 0,
                    owner: index + 1,
                    optional: false,
                }
            })
            .collect();
        let mut obstacles: Vec<Obstacle> = reqs
            .iter()
            .map(|req| Obstacle::Box {
                b: req.anchor,
                owner: req.owner,
            })
            .collect();
        obstacles.extend([
            // Its bounding box crosses the first candidate, but the wire itself
            // stays more than the requested clearance away from that label.
            Obstacle::Seg {
                a: [-1.0, 1.0],
                b: [0.5, 3.0],
                half: 0.05,
                owner: 4,
            },
            Obstacle::Seg {
                a: [9.5, 1.5],
                b: [12.5, 1.5],
                half: 0.05,
                owner: 2,
            },
            Obstacle::Seg {
                a: [19.5, 1.5],
                b: [22.5, 1.5],
                half: 0.05,
                owner: 4,
            },
        ]);
        let placed = place_labels(&reqs, &obstacles, &opts);
        assert_eq!(
            placed.spot[0],
            Some(Spot::Above),
            "first free spot wins using exact diagonal-wire clearance"
        );
        assert_eq!(
            placed.spot[1],
            Some(Spot::Above),
            "same-owner wire is ignored"
        );
        assert_eq!(
            placed.spot[2],
            Some(Spot::Below),
            "foreign wire blocks the first spot"
        );
        assert_eq!(
            overlap_count(&reqs, &placed, &obstacles, opts.gap),
            0,
            "owner-aware placement has zero overlaps"
        );
        let mut blocked = placed.clone();
        blocked.boxes[2] = [20.25, 1.25, 21.75, 1.75];
        assert_eq!(
            overlap_count(&reqs, &blocked, &obstacles, opts.gap),
            1,
            "a foreign wire crossing counts once even across multiple grid cells"
        );
    }

    #[test]
    fn a_crowded_row_slides_and_falls_back_without_overlaps() {
        let opts = LabelOpts::default();
        let mut reqs = Vec::new();
        let mut obstacles = Vec::new();
        for index in 0..20 {
            let x = index as f64 * 2.5;
            let anchor = [x, 0.0, x + 2.0, 1.0];
            obstacles.push(Obstacle::Box {
                b: anchor,
                owner: index,
            });
            for (spot, priority) in [(Spot::Above, 1), (Spot::Below, 0)] {
                reqs.push(LabelReq {
                    anchor,
                    size: [3.0, 0.5],
                    spots: vec![spot],
                    priority,
                    owner: index,
                    optional: false,
                });
            }
        }
        let placed = place_labels(&reqs, &obstacles, &opts);
        assert_eq!(placed.boxes.len(), 40, "every crowded-row label is placed");
        assert_eq!(
            overlap_count(&reqs, &placed, &obstacles, opts.gap),
            0,
            "crowded-row labels have zero overlaps with labels and bodies"
        );
        for (index, req) in reqs.iter().enumerate() {
            let a = box_centre(req.anchor);
            let b = box_centre(placed.boxes[index]);
            assert!(
                (a[0] - b[0]).hypot(a[1] - b[1]) <= 6.0,
                "crowded-row label {index} stays within six units of its anchor"
            );
        }
        assert!(
            reqs.iter().enumerate().any(|(index, req)| {
                placed.spot[index].is_none()
                    || placed.boxes[index] != spot_box(req, req.spots[0], opts.gap)
            }),
            "crowding requires at least one slide or fallback"
        );
        assert_eq!(
            placed,
            place_labels(&reqs, &obstacles, &opts),
            "identical input produces identical boxes, spots and leaders"
        );
    }

    #[test]
    fn fallback_terminates_and_draws_a_leader() {
        let opts = LabelOpts::default();
        let reqs = [LabelReq {
            anchor: [-0.5, -0.5, 0.5, 0.5],
            size: [1.0, 0.5],
            spots: vec![Spot::Above, Spot::Below, Spot::Right, Spot::Left],
            priority: 0,
            owner: 1,
            optional: false,
        }];
        let obstacles: Vec<Obstacle> = [
            [-3.0, 0.5, 3.0, 3.0],
            [-3.0, -3.0, 3.0, -0.5],
            [-3.0, -0.5, -0.5, 0.5],
            [0.5, -0.5, 3.0, 0.5],
        ]
        .into_iter()
        .map(|b| Obstacle::Box { b, owner: 2 })
        .collect();
        let placed = place_labels(&reqs, &obstacles, &opts);
        assert_eq!(
            placed.boxes.len(),
            1,
            "surrounded-anchor fallback terminates and places the label"
        );
        assert_eq!(
            placed.spot[0],
            None,
            "surrounded-anchor placement uses the ring fallback"
        );
        assert_eq!(
            overlap_count(&reqs, &placed, &obstacles, opts.gap),
            0,
            "ring fallback escapes all surrounding obstacles"
        );
        assert!(
            placed.leader[0].is_some(),
            "a remote fallback placement has a leader"
        );
        if let Some([start, end]) = placed.leader[0] {
            for (point, bounds, name) in [
                (start, reqs[0].anchor, "anchor"),
                (end, placed.boxes[0], "label"),
            ] {
                assert!(
                    point[0] >= bounds[0]
                        && point[0] <= bounds[2]
                        && point[1] >= bounds[1]
                        && point[1] <= bounds[3]
                        && (point[0] == bounds[0]
                            || point[0] == bounds[2]
                            || point[1] == bounds[1]
                            || point[1] == bounds[3]),
                    "leader endpoint lies on the {name} boundary"
                );
            }
        }
    }
}
