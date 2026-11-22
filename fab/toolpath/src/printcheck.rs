//! The printability gate (GPU-EXEC §8): a candidate's walls (the GPU's plan, or anything that
//! makes [`IslandWalls`]) against the CPU reference, per layer and island. It need not be bit
//! exact, but it must print. Every failure is one line: `layer L island I [check] detail`.
//!
//! Wall checks ([`walls`]), each island paired with the reference island it covers:
//! - `islands`: the same islands, none missing or extra.
//! - `walls`: the same number of walls.
//! - `loops`: per wall the same loops, none missing or extra (paired within [`Tol::matched`]).
//! - `nesting`: each loop winds as the reference's (contour or hole) and belongs to the same
//!   loop of the wall outside it.
//! - `closed`: every loop has at least three points and an area; no open line where the
//!   reference closes every line of that wall.
//! - `crossing`: no loop crosses itself or another loop.
//! - `spacing`: no two loops closer than their beads allow (the level difference, or the mean
//!   bead width), less [`Tol::space`].
//! - `gap`: every point of wall k within its spacing of wall k-1, plus [`Tol::space`].
//! - `outline`: the island's outline within [`Tol::pos`] of the reference's (both ways): the
//!   walls are measured from it, so the oracle alone would pass walls around a moved outline.
//! - `fill`: the fill region inside the innermost wall has the reference's area within
//!   `perimeter × (Tol::pos + outline displacement)`. A boundary that keeps within `pos` of its
//!   level sweeps at most a band `pos` wide along the fill's perimeter; when the two sides traced
//!   different outlines, the fill boundary also moves by up to the outlines' own displacement
//!   (the offset is 1-Lipschitz). For a 20 x 14 mm fill that is 0.6 mm² of 280 mm² (0.2 %).
//! - `oracle`: positions against the f64 oracle, the exact miter or round distance from the
//!   island's own outline: no wall point shallower than its level less [`Tol::pos`], none deeper
//!   than its level plus [`Tol::pos`] unless the reference's wall is there too (where an opening
//!   cuts a wall at a neck, its caps lie deeper), and every exact level point the opening keeps
//!   has a wall within [`Tol::pos`] unless the reference has none there either.
//!
//! The excuse rule: a crossing, spacing, gap or oracle deviation is not a failure where the
//! reference deviates the same way, as badly within [`Tol::space`], within 0.5 mm: the reference
//! defines what the CPU prints. [`own_walls`] runs one side's checks with nothing excused.
//!
//! Order checks ([`order`]) on the planned toolpaths: `order` (islands and wall paths print in
//! the reference's order) and `seam` (each loop's seam within [`Tol::seam`] of the reference's).
//! [`order_within`] excuses what the reference's own noise shows: the reference planned again
//! from its own outline moved within 0.01 mm a few ways (the caller's), a seam or a thin
//! fragment's pairing or order is excused where it is off no further than the noise is. Nothing
//! else is: islands, closed loops and their order, and on a layer that used an excuse [`held`]:
//! `length` (each island's wall paths extrude the reference's length) and `class` (each island
//! prints the reference's fill classes).
//!
//! `case` is the caller's own: a fixture that no longer is what its case claims (a chosen layer
//! that the reference no longer prints as a bridge, say). `property` is a stage case's own: a property
//! the stage's output must hold (`makepad_fab_slice::stage_cases`).
use crate::{ExtrusionPath, ObjectLayer, Role};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::grid::SegGrid;
use makepad_fab_slicer::source::{IslandWalls, WallShape};
use std::collections::HashMap;
use std::fmt;

/// How the walls' corners were offset: classic walls miter (limit as a multiple of the offset),
/// Arachne beads round.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Join {
    Miter(f64),
    Round,
}

/// One wall's centre inset from the outline the walls were made from, and its bead spacing (mm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    pub inset: f64,
    pub spacing: f64,
}

/// Tolerances (mm).
#[derive(Clone, Copy, Debug)]
pub struct Tol {
    /// A wall point against the f64 oracle (GPU-EXEC §8).
    pub pos: f64,
    /// Two walls' distance: each may be off by `pos`, in opposite directions.
    pub space: f64,
    /// A loop's bounding box against its partner's: a loop further off than half a bead is
    /// another loop.
    pub matched: f64,
    /// A seam against the reference's: about a bead width, where the seam's blob lands on the
    /// same seam line.
    pub seam: f64,
}

impl Default for Tol {
    fn default() -> Self {
        Self { pos: 0.01, space: 0.02, matched: 0.25, seam: 0.5 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Check {
    Islands,
    Outline,
    Walls,
    Loops,
    Nesting,
    Closed,
    Crossing,
    Spacing,
    Gap,
    Fill,
    Oracle,
    Order,
    Seam,
    Length,
    Class,
    Case,
    Prop,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Check::Islands => "islands",
            Check::Outline => "outline",
            Check::Walls => "walls",
            Check::Loops => "loops",
            Check::Nesting => "nesting",
            Check::Closed => "closed",
            Check::Crossing => "crossing",
            Check::Spacing => "spacing",
            Check::Gap => "gap",
            Check::Fill => "fill",
            Check::Oracle => "oracle",
            Check::Order => "order",
            Check::Seam => "seam",
            Check::Length => "length",
            Check::Class => "class",
            Check::Case => "case",
            Check::Prop => "property",
        })
    }
}

/// One failed check, where.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub layer: usize,
    /// The candidate's island (None: the layer as a whole).
    pub island: Option<usize>,
    pub check: Check,
    pub detail: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.island {
            Some(i) => write!(f, "layer {} island {i} [{}] {}", self.layer, self.check, self.detail),
            None => write!(f, "layer {} [{}] {}", self.layer, self.check, self.detail),
        }
    }
}

/// The failures, one per line.
pub fn lines(failures: &[Failure]) -> String {
    failures.iter().map(|f| f.to_string()).collect::<Vec<_>>().join("\n")
}

/// An island: the outline its walls were made from (on a first layer, what elephant-foot
/// compensation left of it, or the island with the foot added to every level), and its walls.
#[derive(Clone, Copy)]
pub struct Island<'a> {
    pub outline: &'a ExPolygon,
    pub walls: &'a IslandWalls,
}

/// Caps every (island, check) at three lines, the third saying how many more there were.
const SHOWN: usize = 3;

fn mm(p: Pt) -> String {
    format!("({:.3}, {:.3})", p.x_mm(), p.y_mm())
}

fn bbox_cost(a: &BBox, b: &BBox) -> f64 {
    [a.min.x - b.min.x, a.min.y - b.min.y, a.max.x - b.max.x, a.max.y - b.max.y].iter().map(|d| d.abs()).max().unwrap() as f64 / SCALE
}

/// Greedy pairing by cost (ties by index), within `limit`: (pairs, unpaired a, unpaired b).
fn pair(n_a: usize, n_b: usize, limit: f64, cost: impl Fn(usize, usize) -> Option<f64>) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
    let mut all = Vec::new();
    for i in 0..n_a {
        for j in 0..n_b {
            if let Some(c) = cost(i, j).filter(|c| *c <= limit) {
                all.push((c, i, j));
            }
        }
    }
    all.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));
    let (mut used_a, mut used_b) = (vec![false; n_a], vec![false; n_b]);
    let mut pairs = Vec::new();
    for (_, i, j) in all {
        if !used_a[i] && !used_b[j] {
            used_a[i] = true;
            used_b[j] = true;
            pairs.push((i, j));
        }
    }
    pairs.sort_unstable();
    let free = |used: &[bool]| used.iter().enumerate().filter(|(_, u)| !**u).map(|(i, _)| i).collect();
    (pairs, free(&used_a), free(&used_b))
}

/// A wall line in nanometres: its wall (0 = outer), whether closed, and the bead width at each
/// point (mm).
#[derive(Clone, Debug)]
struct Line {
    inset: usize,
    closed: bool,
    pts: Vec<Pt>,
    widths: Vec<f64>,
    /// Twice the signed area (closed lines).
    area2: i128,
    bbox: BBox,
}

impl Line {
    fn new(inset: usize, closed: bool, pts: &[Pt], widths: Option<&[f64]>, spacing: f64) -> Self {
        let mut p: Vec<Pt> = Vec::with_capacity(pts.len());
        let mut w: Vec<f64> = Vec::with_capacity(pts.len());
        for (i, &q) in pts.iter().enumerate() {
            if p.last() == Some(&q) {
                continue;
            }
            p.push(q);
            w.push(widths.and_then(|w| w.get(i).copied()).unwrap_or(spacing));
        }
        if closed && p.len() > 1 && p.first() == p.last() {
            p.pop();
            w.pop();
        }
        let area2 = if closed { Polygon::new(p.clone()).area2() } else { 0 };
        let bbox = BBox::of(&p);
        Self { inset, closed, pts: p, widths: w, area2, bbox }
    }
    fn segs(&self) -> usize {
        if self.closed { self.pts.len() } else { self.pts.len().saturating_sub(1) }
    }
    fn seg(&self, i: usize) -> (Pt, Pt) {
        (self.pts[i], self.pts[(i + 1) % self.pts.len()])
    }
    fn kind(&self) -> i8 {
        if !self.closed { 0 } else if self.area2 >= 0 { 1 } else { -1 }
    }
    fn what(&self) -> &'static str {
        match self.kind() {
            0 => "open line",
            1 => "contour loop",
            _ => "hole loop",
        }
    }
    fn length_mm(&self) -> f64 {
        (0..self.segs()).map(|i| { let (a, b) = self.seg(i); a.dist(b) }).sum::<f64>() / SCALE
    }
}

fn lines_of(walls: &IslandWalls, levels: &[Level]) -> Vec<Line> {
    let spacing = |k: usize| levels.get(k).map_or(0.0, |l| l.spacing);
    match &walls.shape {
        WallShape::Rings(r) => r.iter().enumerate().flat_map(|(k, rings)| rings.iter().map(move |ring| Line::new(k, true, &ring.pts, None, spacing(k)))).collect(),
        WallShape::Beads(b) => b.iter().map(|l| Line::new(l.inset, l.closed, &l.pts, Some(&l.widths), spacing(l.inset))).collect(),
    }
}

/// The exact f64 distance from an outline, positive inside, with the walls' corner join.
pub struct Oracle {
    features: Vec<Feature>,
    /// Each feature's edge (payload: feature), for the nearest-first search.
    edges: SegGrid<u32>,
    region: SegGrid<u8>,
    join: Join,
}

struct Feature {
    a: [f64; 2],
    b: [f64; 2],
    ab: [f64; 2],
    bc: [f64; 2],
    n1: [f64; 2],
    n2: [f64; 2],
    bisector: [f64; 2],
    aa: f64,
    turn: f64,
    cosine: f64,
}

fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn crossf(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn f2(p: Pt) -> [f64; 2] {
    [p.x_mm(), p.y_mm()]
}

impl Oracle {
    pub fn new(outline: &ExPolygon, join: Join) -> Self {
        let mut features = Vec::new();
        let mut edges = Vec::new();
        for r in outline.rings() {
            let mut v: Vec<Pt> = Vec::with_capacity(r.pts.len());
            for &p in &r.pts {
                if v.last() != Some(&p) {
                    v.push(p);
                }
            }
            if v.len() > 1 && v.first() == v.last() {
                v.pop();
            }
            let n = v.len();
            if n < 3 {
                continue;
            }
            for i in 0..n {
                let (a, b, c) = (f2(v[i]), f2(v[(i + 1) % n]), f2(v[(i + 2) % n]));
                let (ab, bc) = (sub(b, a), sub(c, b));
                let (aa, cc) = (dot(ab, ab), dot(bc, bc));
                // The left normal points into the material: contours wind counter-clockwise, holes clockwise.
                let n1 = [-ab[1] / aa.sqrt(), ab[0] / aa.sqrt()];
                let n2 = [-bc[1] / cc.sqrt(), bc[0] / cc.sqrt()];
                let s = [n1[0] + n2[0], n1[1] + n2[1]];
                let l = dot(s, s).sqrt().max(1e-30);
                edges.push((v[i], v[(i + 1) % n], features.len() as u32));
                features.push(Feature { a, b, ab, bc, n1, n2, bisector: [s[0] / l, s[1] / l], aa, turn: crossf(ab, bc), cosine: dot(n1, n2) });
            }
        }
        Self { features, edges: SegGrid::new(edges), region: SegGrid::of_regions(std::slice::from_ref(outline), |_| 0u8), join }
    }

    pub fn inside(&self, p: [f64; 2]) -> bool {
        self.region.contains_any(Pt::from_mm(p[0], p[1]))
    }

    /// One feature's distance at `p`: its edge's slab, or its end vertex's corner where the
    /// join offsets round it (inside: reflex corners; outside: convex ones).
    fn feature(&self, f: &Feature, p: [f64; 2], inside: bool) -> f64 {
        let (ap, bp) = (sub(p, f.a), sub(p, f.b));
        let t = dot(ap, f.ab);
        let mut nearest = if t >= 0.0 && t <= f.aa { dot(ap, f.n1).abs() } else { f64::INFINITY };
        if t >= f.aa && dot(bp, f.bc) <= 0.0 && if inside { f.turn < 0.0 } else { f.turn > 0.0 } {
            let d = match self.join {
                Join::Round => dot(bp, bp).sqrt(),
                Join::Miter(limit) => {
                    let sign = if inside { 1.0 } else { -1.0 };
                    let mut d = (dot(bp, f.n1) * sign).max(dot(bp, f.n2) * sign);
                    if 1.0 + f.cosine <= 2.0 / (limit * limit) {
                        d = d.max(dot(bp, f.bisector) * sign);
                    }
                    d
                }
            };
            nearest = nearest.min(d);
        }
        nearest
    }

    /// The signed distance (mm) at `p`: positive inside. A feature's distance is at least its
    /// edge's Euclidean distance over the miter limit, so the search stops once every unseen
    /// edge is that far.
    pub fn metric(&self, p: [f64; 2]) -> f64 {
        let inside = self.inside(p);
        let reach = match self.join { Join::Miter(limit) => limit.max(1.0), Join::Round => 1.0 };
        let mut best = f64::INFINITY;
        self.edges.nearest_with(Pt::from_mm(p[0], p[1]), |_, _, k| {
            best = best.min(self.feature(&self.features[k as usize], p, inside));
            best * reach * SCALE
        });
        if inside { best } else { -best }
    }
}

/// One side's island prepared for the checks.
struct Side<'a> {
    island: Island<'a>,
    lines: Vec<Line>,
    walls: usize,
    /// Per wall, its lines' segments (payload: line, segment).
    by_wall: Vec<SegGrid<(u32, u32)>>,
    /// Every line's segments (payload: line, segment).
    all: SegGrid<(u32, u32)>,
}

impl<'a> Side<'a> {
    fn new(island: Island<'a>, levels: &[Level]) -> Self {
        let lines = lines_of(island.walls, levels);
        let walls = lines.iter().map(|l| l.inset + 1).max().unwrap_or(0);
        let mut per: Vec<Vec<(Pt, Pt, (u32, u32))>> = vec![Vec::new(); walls];
        let mut all = Vec::new();
        for (k, l) in lines.iter().enumerate() {
            for i in 0..l.segs() {
                let (a, b) = l.seg(i);
                per[l.inset].push((a, b, (k as u32, i as u32)));
                all.push((a, b, (k as u32, i as u32)));
            }
            if l.pts.len() == 1 {
                per[l.inset].push((l.pts[0], l.pts[0], (k as u32, 0)));
            }
        }
        Self { island, lines, walls, by_wall: per.into_iter().map(SegGrid::new).collect(), all: SegGrid::new(all) }
    }

    /// Distance (mm) from `p` to wall `k` (infinite when it has none).
    fn to_wall(&self, k: usize, p: Pt) -> f64 {
        self.by_wall.get(k).and_then(|g| g.nearest(p, |l| l)).map_or(f64::INFINITY, |(d, _)| d / SCALE)
    }
}

/// One side's deviation from what prints: its check, what it concerns (walls, and for the
/// oracle 0 shallow, 1 deep, 2 missing), where, how bad (mm, positive) and its line.
#[derive(Clone, Debug)]
struct Dev {
    check: Check,
    key: (usize, usize, u8),
    at: Pt,
    bad: f64,
    detail: String,
}

/// Deviations by 0.5 mm cell, for the excuse rule.
struct Excuses<'a>(HashMap<(i64, i64), Vec<&'a Dev>>);

const EXCUSE: f64 = 0.5;

impl<'a> Excuses<'a> {
    fn new(list: &'a [Dev]) -> Self {
        let mut m: HashMap<(i64, i64), Vec<&Dev>> = HashMap::new();
        for d in list {
            m.entry(Self::cell(d.at)).or_default().push(d);
        }
        Self(m)
    }
    fn cell(p: Pt) -> (i64, i64) {
        let c = (EXCUSE * SCALE) as i64;
        (p.x.div_euclid(c), p.y.div_euclid(c))
    }
    /// Whether the reference deviates as `d` does, as badly within `slack`, within 0.5 mm.
    fn has(&self, d: &Dev, slack: f64) -> bool {
        let (cx, cy) = Self::cell(d.at);
        (cx - 1..=cx + 1).any(|x| (cy - 1..=cy + 1).any(|y| {
            self.0.get(&(x, y)).is_some_and(|v| v.iter().any(|r| r.check == d.check && r.key == d.key && r.at.dist(d.at) <= EXCUSE * SCALE && r.bad >= d.bad - slack))
        }))
    }
}

/// The nominal centre distance of two beads: walls `i` and `j` with widths `wi` and `wj` (mm).
fn nominal(levels: &[Level], i: usize, j: usize, wi: f64, wj: f64) -> Option<f64> {
    let width = 0.5 * (wi + wj);
    if i == j {
        return Some(width);
    }
    let (a, b) = (levels.get(i)?, levels.get(j)?);
    let at_level = (wi - a.spacing).abs() <= 0.005 && (wj - b.spacing).abs() <= 0.005;
    if at_level {
        Some((a.inset - b.inset).abs())
    } else if i.abs_diff(j) == 1 {
        Some(width)
    } else {
        None
    }
}

/// Spacing and gap deviations of one side (see the module doc).
fn spacing(side: &Side<'_>, levels: &[Level], join: Join, tol: &Tol, out: &mut Vec<Dev>) {
    let reach = levels.iter().map(|l| l.inset).fold(0.0f64, f64::max) + levels.iter().map(|l| l.spacing).fold(0.0f64, f64::max) + tol.space;
    let r = (reach * SCALE) as i64;
    for (a, la) in side.lines.iter().enumerate() {
        // Every vertex (mitered corners are further apart) and every segment's midpoint.
        let n_pts = la.pts.len();
        let samples = (0..n_pts).map(|i| (la.pts[i], la.widths[i], miter_factor(la, i, join))).chain((0..la.segs()).map(|i| {
            let (p, q) = la.seg(i);
            (Pt::new(p.x + (q.x - p.x) / 2, p.y + (q.y - p.y) / 2), 0.5 * (la.widths[i] + la.widths[(i + 1) % n_pts]), 1.0)
        }));
        for (p, wp, factor) in samples {
            // The nearest point of every other line in reach, by line.
            let mut near: HashMap<u32, (f64, f64)> = HashMap::new();
            side.all.visit_box(BBox { min: Pt::new(p.x - r, p.y - r), max: Pt::new(p.x + r, p.y + r) }, |s0, s1, (b, j)| {
                if b as usize == a {
                    return;
                }
                let d = dist_point_segment(p, s0, s1) / SCALE;
                let lb = &side.lines[b as usize];
                let wq = 0.5 * (lb.widths[j as usize] + lb.widths[(j as usize + 1) % lb.pts.len()]);
                let e = near.entry(b).or_insert((f64::INFINITY, wq));
                if d < e.0 {
                    *e = (d, wq);
                }
            });
            for (b, (d, wq)) in near {
                let lb = &side.lines[b as usize];
                let Some(n) = nominal(levels, la.inset, lb.inset, wp, wq) else { continue };
                if d < n - tol.space {
                    let (lo, hi) = (la.inset.min(lb.inset), la.inset.max(lb.inset));
                    out.push(Dev { check: Check::Spacing, key: (lo, hi, 0), at: p, bad: n - d,
                        detail: format!("walls {lo} and {hi} {:.3} mm closer than their beads allow at {} (overlap)", n - d, mm(p)) });
                }
            }
            if la.inset > 0 {
                let Some((g, (outer, j))) = side.by_wall[la.inset - 1].nearest(p, |l| l) else { continue };
                let g = g / SCALE;
                let lo = &side.lines[outer as usize];
                let wq = 0.5 * (lo.widths[j as usize] + lo.widths[(j as usize + 1) % lo.pts.len()]);
                if let Some(n) = nominal(levels, la.inset - 1, la.inset, wq, wp) {
                    // Mitered rings meet a sharp corner 1 / cos(turn / 2) spacings apart, up to the limit.
                    let n = n * factor;
                    if g > n + tol.space {
                        out.push(Dev { check: Check::Gap, key: (la.inset - 1, la.inset, 0), at: p, bad: g - n,
                            detail: format!("wall {} is {:.3} mm further from wall {} than its spacing at {} (gap)", la.inset, g - n, la.inset - 1, mm(p)) });
                    }
                }
            }
        }
    }
}

/// How much further apart than their spacing two mitered rings are at line `l`'s vertex `i`.
fn miter_factor(l: &Line, i: usize, join: Join) -> f64 {
    let Join::Miter(limit) = join else { return 1.0 };
    let n = l.pts.len();
    if n < 3 || (!l.closed && (i == 0 || i + 1 == n)) {
        return 1.0;
    }
    let (a, b, c) = (f2(l.pts[(i + n - 1) % n]), f2(l.pts[i]), f2(l.pts[(i + 1) % n]));
    let (u, v) = (sub(b, a), sub(c, b));
    let cos_turn = dot(u, v) / (dot(u, u) * dot(v, v)).sqrt().max(1e-30);
    let half = (0.5 * (1.0 + cos_turn.clamp(-1.0, 1.0))).sqrt();
    (1.0 / half.max(1e-9)).min(limit.max(1.0))
}

fn intersect(a: Pt, b: Pt, c: Pt, d: Pt) -> bool {
    let (abc, abd, cda, cdb) = (orient(a, b, c), orient(a, b, d), orient(c, d, a), orient(c, d, b));
    let within = |p: Pt, q: Pt, r: Pt| r.x >= p.x.min(q.x) && r.x <= p.x.max(q.x) && r.y >= p.y.min(q.y) && r.y <= p.y.max(q.y);
    (abc == 0 && within(a, b, c)) || (abd == 0 && within(a, b, d)) || (cda == 0 && within(c, d, a)) || (cdb == 0 && within(c, d, b))
        || (abc.signum() * abd.signum() < 0 && cda.signum() * cdb.signum() < 0)
}

fn meet(a: Pt, b: Pt, c: Pt, d: Pt) -> Pt {
    let (p, r, q, s) = (f2(a), sub(f2(b), f2(a)), f2(c), sub(f2(d), f2(c)));
    let den = crossf(r, s);
    let t = if den.abs() < 1e-18 { 0.0 } else { crossf(sub(q, p), s) / den };
    Pt::from_mm(p[0] + t * r[0], p[1] + t * r[1])
}

/// Crossings: a line crossing itself, turning back on itself, or crossing another line. Open
/// bead lines may end on another line (a junction): that is a joint, not a crossing.
fn crossings(side: &Side<'_>, out: &mut Vec<Dev>) {
    let mut seen = std::collections::HashSet::new();
    let ends = |l: &Line| if l.closed || l.pts.is_empty() { Vec::new() } else { vec![l.pts[0], *l.pts.last().unwrap()] };
    for (a, la) in side.lines.iter().enumerate() {
        let n = la.pts.len();
        let ends_a = ends(la);
        for i in 0..la.segs() {
            let (p, q) = la.seg(i);
            // A spike: the next segment runs back along this one.
            if la.closed || i + 1 < la.segs() {
                let r = la.pts[(i + 2) % n];
                if n > 2 && orient(p, q, r) == 0 && makepad_fab_slicer::geom::dot(q.sub(p), r.sub(q)) < 0 {
                    out.push(Dev { check: Check::Crossing, key: (la.inset, la.inset, 0), at: q, bad: 1.0,
                        detail: format!("the wall {} loop at {} turns back on itself at {}", la.inset, mm(la.bbox.center()), mm(q)) });
                }
            }
            side.all.visit_box(BBox::of(&[p, q]), |c, d, (b, j)| {
                let (b, j) = (b as usize, j as usize);
                if (b, j) <= (a, i) {
                    return;
                }
                let lb = &side.lines[b];
                if b == a && (j == i + 1 || (la.closed && i == 0 && j + 1 == la.segs())) {
                    return;
                }
                if !intersect(p, q, c, d) || !seen.insert((a, i, b, j)) {
                    return;
                }
                let joint = ends_a.iter().any(|&e| (e == p || e == q) && dist_point_segment(e, c, d) < 2.0)
                    || ends(lb).iter().any(|&e| (e == c || e == d) && dist_point_segment(e, p, q) < 2.0);
                if joint {
                    return;
                }
                let at = meet(p, q, c, d);
                let detail = if a == b { format!("the wall {} loop at {} crosses itself at {}", la.inset, mm(la.bbox.center()), mm(at)) } else { format!("wall {} and wall {} loops cross at {}", la.inset, lb.inset, mm(at)) };
                out.push(Dev { check: Check::Crossing, key: (la.inset.min(lb.inset), la.inset.max(lb.inset), 0), at, bad: 1.0, detail });
            });
        }
    }
}

/// The oracle's deviations on one side's own outline: shallow and deep points (key 0 and 1), and
/// exact level points the opening keeps with no wall (key 2).
fn oracle(side: &Side<'_>, levels: &[Level], join: Join, tol: &Tol, out: &mut Vec<Dev>) {
    let oracle = Oracle::new(side.island.outline, join);
    for l in &side.lines {
        let Some(level) = levels.get(l.inset) else { continue };
        for (k, &p) in l.pts.iter().enumerate() {
            if (l.widths[k] - level.spacing).abs() > 0.005 {
                continue; // a variable-width bead: thin-wall territory
            }
            let e = oracle.metric(f2(p)) - level.inset;
            if e < -tol.pos {
                out.push(Dev { check: Check::Oracle, key: (l.inset, 0, 0), at: p, bad: -e, detail: format!("wall {} at {} is {:.4} mm shallower than its level {:.3}", l.inset, mm(p), -e, level.inset) });
            } else if e > tol.pos {
                out.push(Dev { check: Check::Oracle, key: (l.inset, 0, 1), at: p, bad: e, detail: format!("wall {} at {} is {:.4} mm deeper than its level {:.3}", l.inset, mm(p), e, level.inset) });
            }
        }
    }
    for (k, level) in levels.iter().enumerate().take(side.walls) {
        for ring in side.island.outline.rings() {
            let n = ring.pts.len();
            for i in 0..n {
                let (a, b) = (f2(ring.pts[i]), f2(ring.pts[(i + 1) % n]));
                let ab = sub(b, a);
                let len = dot(ab, ab).sqrt();
                if len < 1e-9 {
                    continue;
                }
                let nrm = [-ab[1] / len, ab[0] / len];
                for t in [0.25, 0.5, 0.75] {
                    let q = [a[0] + t * ab[0] + nrm[0] * level.inset, a[1] + t * ab[1] + nrm[1] * level.inset];
                    if !oracle.inside(q) || (oracle.metric(q) - level.inset).abs() > 1e-6 {
                        continue;
                    }
                    // The opening keeps the level where a bead's disk fits behind it.
                    let half = 0.5 * level.spacing;
                    if oracle.metric([q[0] + nrm[0] * half, q[1] + nrm[1] * half]) < level.inset + half - 1e-6 {
                        continue;
                    }
                    let qp = Pt::from_mm(q[0], q[1]);
                    let d = side.to_wall(k, qp);
                    if d > tol.pos {
                        out.push(Dev { check: Check::Oracle, key: (k, 0, 2), at: qp, bad: d.min(1.0), detail: format!("wall {k}: no wall within {:.3} mm of its level at {} (nearest {:.4} mm)", tol.pos, mm(qp), d) });
                    }
                }
            }
        }
    }
}

/// Everything one side's walls must hold by themselves: crossings, spacing, gaps, the oracle.
fn own(side: &Side<'_>, levels: &[Level], join: Join, tol: &Tol) -> Vec<Dev> {
    let mut out = Vec::new();
    crossings(side, &mut out);
    spacing(side, levels, join, tol, &mut out);
    oracle(side, levels, join, tol, &mut out);
    out
}

/// Failures, at most [`SHOWN`] per check, the last saying how many more.
fn capped(layer: usize, island: Option<usize>, mut f: Vec<(Check, String)>, out: &mut Vec<Failure>) {
    f.sort_by_key(|(c, _)| *c);
    let mut total: HashMap<Check, usize> = HashMap::new();
    for (c, _) in &f {
        *total.entry(*c).or_default() += 1;
    }
    let mut taken: HashMap<Check, usize> = HashMap::new();
    for (check, detail) in f {
        let k = taken.entry(check).or_default();
        *k += 1;
        if *k > SHOWN {
            continue;
        }
        let all = total[&check];
        let detail = if *k == SHOWN && all > SHOWN { format!("{detail} (+{} more)", all - SHOWN) } else { detail };
        out.push(Failure { layer, island, check, detail });
    }
}

/// The wall checks of one layer: the candidate's islands against the reference's, with the
/// walls' `levels` (from the outline each island gives) and corner `join`.
pub fn walls(layer: usize, levels: &[Level], join: Join, candidate: &[Island<'_>], reference: &[Island<'_>], tol: &Tol) -> Vec<Failure> {
    let mut out = Vec::new();
    let boxes = |s: &[Island<'_>]| s.iter().map(|i| (i.outline.bbox(), i.outline.area_mm2())).collect::<Vec<_>>();
    let (cb, rb) = (boxes(candidate), boxes(reference));
    let (pairs, extra, missing) = pair(cb.len(), rb.len(), 4.0 * tol.matched, |i, j| Some(bbox_cost(&cb[i].0, &rb[j].0) + (cb[i].1.abs().sqrt() - rb[j].1.abs().sqrt()).abs()));
    for i in extra {
        out.push(Failure { layer, island: Some(i), check: Check::Islands, detail: format!("extra island at {}, {:.3} mm², the CPU has none there", mm(cb[i].0.center()), cb[i].1) });
    }
    for j in missing {
        out.push(Failure { layer, island: None, check: Check::Islands, detail: format!("missing island at {}, {:.3} mm² (CPU island {j})", mm(rb[j].0.center()), rb[j].1) });
    }
    for (i, j) in pairs {
        capped(layer, Some(i), island(levels, join, candidate[i], reference[j], tol), &mut out);
    }
    out
}

/// One side's walls by themselves, no reference and nothing excused: the closed loops,
/// crossings, spacing, gaps and the f64 oracle. Classic CPU walls pass it; Arachne's simplified
/// beads and their transitions do not, and are held to the reference by [`walls`].
pub fn own_walls(layer: usize, levels: &[Level], join: Join, islands: &[Island<'_>], tol: &Tol) -> Vec<Failure> {
    let mut out = Vec::new();
    for (i, &isl) in islands.iter().enumerate() {
        let side = Side::new(isl, levels);
        let mut f: Vec<(Check, String)> = closed(&side, None).into_iter().map(|d| (Check::Closed, d)).collect();
        f.extend(own(&side, levels, join, tol).into_iter().map(|d| (d.check, d.detail)));
        capped(layer, Some(i), f, &mut out);
    }
    out
}

/// Closed-loop failures: degenerate loops, and open lines where the reference (when given)
/// closes every line of that wall.
fn closed(c: &Side<'_>, reference: Option<&Side<'_>>) -> Vec<String> {
    let mut out = Vec::new();
    for l in &c.lines {
        if l.closed && (l.pts.len() < 3 || l.area2 == 0) {
            out.push(format!("wall {}: the loop at {} has {} points and {:.4} mm²", l.inset, mm(l.bbox.center()), l.pts.len(), l.area2 as f64 * 0.5 / (SCALE * SCALE)));
        } else if !l.closed && reference.is_some_and(|r| !r.lines.iter().any(|x| x.inset == l.inset && !x.closed)) {
            out.push(format!("wall {}: open line from {} to {}, the CPU closes every line of this wall", l.inset, mm(l.pts[0]), mm(*l.pts.last().unwrap())));
        }
    }
    out
}

/// One island pair's failures: (check, detail).
fn island(levels: &[Level], join: Join, candidate: Island<'_>, reference: Island<'_>, tol: &Tol) -> Vec<(Check, String)> {
    let mut out = Vec::new();
    let (c, r) = (Side::new(candidate, levels), Side::new(reference, levels));
    if c.walls != r.walls {
        out.push((Check::Walls, format!("{} walls, CPU {}", c.walls, r.walls)));
    }
    // Loops per wall, paired by kind and box.
    let mut partner: Vec<Option<usize>> = vec![None; c.lines.len()];
    for k in 0..c.walls.max(r.walls) {
        let ci: Vec<usize> = (0..c.lines.len()).filter(|&a| c.lines[a].inset == k).collect();
        let ri: Vec<usize> = (0..r.lines.len()).filter(|&b| r.lines[b].inset == k).collect();
        let (pairs, extra, missing) = pair(ci.len(), ri.len(), tol.matched, |a, b| {
            let (la, lb) = (&c.lines[ci[a]], &r.lines[ri[b]]);
            (la.kind() == lb.kind()).then(|| bbox_cost(&la.bbox, &lb.bbox))
        });
        for (a, b) in pairs {
            partner[ci[a]] = Some(ri[b]);
        }
        // An unpaired loop that pairs with an unpaired loop of the other winding is a nesting
        // error; the rest are missing or extra.
        let (flip, extra, missing) = pair(extra.len(), missing.len(), tol.matched, |a, b| {
            let (la, lb) = (&c.lines[ci[extra[a]]], &r.lines[ri[missing[b]]]);
            (la.kind() != 0 && lb.kind() != 0).then(|| bbox_cost(&la.bbox, &lb.bbox))
        }).map_pair(&extra, &missing);
        for (a, b) in flip {
            let (la, lb) = (&c.lines[ci[a]], &r.lines[ri[b]]);
            out.push((Check::Nesting, format!("wall {k}: the loop at {} winds as a {}, the CPU's as a {}", mm(la.bbox.center()), la.what(), lb.what())));
        }
        for a in extra {
            let l = &c.lines[ci[a]];
            out.push((Check::Loops, format!("wall {k}: extra {} at {}, {:.2} mm long", l.what(), mm(l.bbox.center()), l.length_mm())));
        }
        for b in missing {
            let l = &r.lines[ri[b]];
            out.push((Check::Loops, format!("wall {k}: missing {} at {}, {:.2} mm long", l.what(), mm(l.bbox.center()), l.length_mm())));
        }
    }
    // Each loop belongs to the nearest loop of the wall outside it (walls::paths' rule).
    let owner = |s: &Side<'_>, l: &Line| -> Option<usize> {
        let g = s.by_wall.get(l.inset.checked_sub(1)?)?;
        let step = (l.pts.len() / 8).max(1);
        l.pts.iter().step_by(step).filter_map(|&q| g.nearest(q, |x| x)).min_by(|x, y| x.0.total_cmp(&y.0)).map(|(_, o)| o.0 as usize)
    };
    for (a, la) in c.lines.iter().enumerate() {
        let Some(b) = partner[a] else { continue };
        let (Some(oa), Some(ob)) = (owner(&c, la), owner(&r, &r.lines[b])) else { continue };
        if partner[oa].is_some_and(|p| p != ob) {
            out.push((Check::Nesting, format!("wall {}: the loop at {} belongs to the wall {} loop at {}, the CPU's to the one at {}", la.inset, mm(la.bbox.center()), la.inset - 1, mm(c.lines[oa].bbox.center()), mm(r.lines[ob].bbox.center()))));
        }
    }
    out.extend(closed(&c, Some(&r)).into_iter().map(|d| (Check::Closed, d)));
    // The outline the walls are measured from.
    let (moved, at) = outline_displacement(candidate.outline, reference.outline);
    if moved > tol.pos {
        out.push((Check::Outline, format!("the outline is {moved:.4} mm from the CPU's at {}", mm(at))));
    }
    // Crossings, spacing, gaps and the oracle: the reference's own deviations excused.
    let theirs = own(&r, levels, join, tol);
    let excuses = Excuses::new(&theirs);
    for d in own(&c, levels, join, tol) {
        if excuses.has(&d, tol.space) {
            continue;
        }
        let (k, _, how) = d.key;
        // Deep where the CPU's wall is too (an opening's cap); no wall where the CPU has none.
        if d.check == Check::Oracle && how == 1 && r.to_wall(k, d.at) <= tol.pos + moved {
            continue;
        }
        if d.check == Check::Oracle && how == 2 && r.to_wall(k, d.at) > 2.0 * tol.pos + moved {
            continue;
        }
        out.push((d.check, d.detail));
    }
    // Fill area.
    let ring_perimeter = |e: &[ExPolygon]| e.iter().flat_map(|x| x.rings()).map(|r| r.perimeter()).sum::<f64>() / SCALE;
    let (ac, ar) = (total_area_mm2(&candidate.walls.fill), total_area_mm2(&reference.walls.fill));
    let allowed = ring_perimeter(&reference.walls.fill).max(ring_perimeter(&candidate.walls.fill)) * (tol.pos + moved) + 1e-4;
    if (ac - ar).abs() > allowed {
        out.push((Check::Fill, format!("{ac:.4} mm², CPU {ar:.4} mm², allowed ±{allowed:.4} (fill perimeter × ({:.3} + outline displacement {moved:.4}) mm)", tol.pos)));
    }
    out
}

trait MapPair {
    fn map_pair(self, a: &[usize], b: &[usize]) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>);
}
impl MapPair for (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
    fn map_pair(self, a: &[usize], b: &[usize]) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
        (self.0.into_iter().map(|(x, y)| (a[x], b[y])).collect(), self.1.into_iter().map(|x| a[x]).collect(), self.2.into_iter().map(|y| b[y]).collect())
    }
}

/// The largest distance (mm) from either outline's points to the other outline, and where.
fn outline_displacement(a: &ExPolygon, b: &ExPolygon) -> (f64, Pt) {
    if a == b {
        return (0.0, a.contour.pts.first().copied().unwrap_or_default());
    }
    let grid = |e: &ExPolygon| SegGrid::of_regions(std::slice::from_ref(e), |_| 0u8);
    let (ga, gb) = (grid(a), grid(b));
    let one = |from: &ExPolygon, to: &SegGrid<u8>| from.rings().flat_map(|r| r.pts.iter()).filter_map(|&p| to.nearest(p, |_| 0u8).map(|(d, _)| (d, p))).fold((0.0f64, Pt::default()), |m, x| if x.0 > m.0 { x } else { m });
    let (x, y) = (one(a, &gb), one(b, &ga));
    let (d, p) = if x.0 >= y.0 { x } else { y };
    (d / SCALE, p)
}

fn is_wall(p: &ExtrusionPath) -> bool {
    matches!(p.role, Role::OuterWall | Role::InnerWall | Role::OverhangWall)
}

fn path_box(p: &ExtrusionPath) -> BBox {
    BBox::of(if p.loop_pts.is_empty() { &p.pts } else { &p.loop_pts })
}

fn seam_of(p: &ExtrusionPath) -> Pt {
    p.loop_pts.first().or(p.pts.first()).copied().unwrap_or_default()
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::OuterWall => "outer wall",
        Role::InnerWall => "inner wall",
        Role::OverhangWall => "overhang wall",
        _ => "path",
    }
}

/// What an order deviation is, keyed by the reference's island (and wall path), so the reference's
/// own noise can be measured on the same key ([`order_within`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OrderKey {
    /// A planned island extra or missing, or the islands' order: never excused.
    Islands,
    /// A closed wall loop extra or missing on the reference's island: a wall count, never excused.
    Loops(usize),
    /// Open wall lines (thin fragments) unpaired on the reference's island; size: their count.
    Fragments(usize),
    /// The wall paths' order on the reference's island; size: its descents.
    PathOrder(usize),
    /// The seam of the reference's island's wall path; size: its distance (mm).
    Seam(usize, usize),
}

impl OrderKey {
    /// The kind's name, for the excused counts.
    pub fn kind(self) -> &'static str {
        match self {
            OrderKey::Islands => "islands",
            OrderKey::Loops(_) => "loops",
            OrderKey::Fragments(_) => "fragments",
            OrderKey::PathOrder(_) => "path order",
            OrderKey::Seam(..) => "seams",
        }
    }
}

/// One order measure of a planned layer against the reference's: every seam's distance, and each
/// deviation, with the failure it is on its own.
#[derive(Clone, Debug)]
pub struct OrderDev {
    pub key: OrderKey,
    pub size: f64,
    /// Whether the reference's noise may excuse it: a seam, or a deviation of open lines only.
    pub excusable: bool,
    /// The candidate's island (None: the layer as a whole).
    pub island: Option<usize>,
    /// The failure, when it fails on its own.
    pub fails: Option<(Check, String)>,
}

fn island_box(i: &crate::Island) -> BBox {
    let mut b = BBox::empty();
    for p in i.walls.iter().chain(&i.fills) {
        b.merge(&path_box(p));
    }
    b
}

/// The planned islands paired with the reference's: (pairs, extra, missing).
fn island_pairs(candidate: &ObjectLayer, reference: &ObjectLayer, tol: &Tol) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>, Vec<BBox>, Vec<BBox>) {
    let (cb, rb): (Vec<BBox>, Vec<BBox>) = (candidate.islands.iter().map(|i| island_box(i)).collect(), reference.islands.iter().map(|i| island_box(i)).collect());
    let (pairs, extra, missing) = pair(cb.len(), rb.len(), 4.0 * tol.matched, |i, j| Some(bbox_cost(&cb[i], &rb[j])));
    (pairs, extra, missing, cb, rb)
}

/// Every order measure of one layer's planned toolpaths against the reference's.
pub fn order_devs(candidate: &ObjectLayer, reference: &ObjectLayer, tol: &Tol) -> Vec<OrderDev> {
    let mut out = Vec::new();
    let (pairs, extra, missing, cb, rb) = island_pairs(candidate, reference, tol);
    let never = |island, check, detail| OrderDev { key: OrderKey::Islands, size: 1.0, excusable: false, island, fails: Some((check, detail)) };
    for i in extra {
        out.push(never(Some(i), Check::Islands, format!("extra planned island at {}", mm(cb[i].center()))));
    }
    for j in missing {
        out.push(never(None, Check::Islands, format!("missing planned island at {} (CPU island {j})", mm(rb[j].center()))));
    }
    let printed: Vec<usize> = pairs.iter().map(|p| p.1).collect();
    if printed.windows(2).any(|w| w[0] > w[1]) {
        out.push(never(None, Check::Order, format!("islands print as the CPU's {printed:?}")));
    }
    for (i, j) in pairs {
        let cw: Vec<&ExtrusionPath> = candidate.islands[i].walls.iter().filter(|p| is_wall(p)).collect();
        let rw: Vec<&ExtrusionPath> = reference.islands[j].walls.iter().filter(|p| is_wall(p)).collect();
        let (wp, extra, missing) = pair(cw.len(), rw.len(), tol.matched, |a, b| (cw[a].role == rw[b].role).then(|| bbox_cost(&path_box(cw[a]), &path_box(rw[b]))));
        let fragments = extra.iter().filter(|&&a| !cw[a].closed).count() + missing.iter().filter(|&&b| !rw[b].closed).count();
        let unpaired = |closed: bool, detail: String| {
            let key = if closed { OrderKey::Loops(j) } else { OrderKey::Fragments(j) };
            OrderDev { key, size: if closed { 1.0 } else { fragments as f64 }, excusable: !closed, island: Some(i), fails: Some((Check::Order, detail)) }
        };
        for a in extra {
            out.push(unpaired(cw[a].closed, format!("extra {} path at {}", role_name(cw[a].role), mm(path_box(cw[a]).center()))));
        }
        for b in missing {
            out.push(unpaired(rw[b].closed, format!("missing {} path at {}", role_name(rw[b].role), mm(path_box(rw[b]).center()))));
        }
        let seq: Vec<usize> = wp.iter().map(|p| p.1).collect();
        let descents: Vec<usize> = (0..seq.len().saturating_sub(1)).filter(|&k| seq[k] > seq[k + 1]).collect();
        if let Some(&k) = descents.first() {
            let (a, b) = wp[k + 1];
            // only open lines out of order: a thin fragment's order
            let open = descents.iter().all(|&d| !cw[wp[d].0].closed || !cw[wp[d + 1].0].closed);
            let detail = format!("wall path {a} ({} at {}) is the CPU's path {b} but prints after its path {}: CPU order {seq:?}", role_name(cw[a].role), mm(path_box(cw[a]).center()), seq[k]);
            out.push(OrderDev { key: OrderKey::PathOrder(j), size: descents.len() as f64, excusable: open, island: Some(i), fails: Some((Check::Order, detail)) });
        }
        for (a, b) in wp {
            if !(cw[a].closed && rw[b].closed) {
                continue;
            }
            let (sa, sb) = (seam_of(cw[a]), seam_of(rw[b]));
            let d = sa.dist(sb) / SCALE;
            let fails = (d > tol.seam).then(|| (Check::Seam, format!("{} at {}: seam at {}, the CPU's at {}, {d:.3} mm apart", role_name(cw[a].role), mm(path_box(cw[a]).center()), mm(sa), mm(sb))));
            out.push(OrderDev { key: OrderKey::Seam(j, b), size: d, excusable: true, island: Some(i), fails });
        }
    }
    out
}

/// The order checks of one layer's planned toolpaths: the candidate's (planned from its walls)
/// against the reference's.
pub fn order(layer: usize, candidate: &ObjectLayer, reference: &ObjectLayer, tol: &Tol) -> Vec<Failure> {
    order_within(layer, candidate, reference, &[], tol).0
}

/// [`order`] with the reference's own noise excused: `noise` is the reference planned again
/// from its own outline moved within 0.01 mm. A seam, or open lines' pairing or order, is
/// excused where the noise is off on the same key at least as far. The failures, and each
/// excused deviation's key.
pub fn order_within(layer: usize, candidate: &ObjectLayer, reference: &ObjectLayer, noise: &[&ObjectLayer], tol: &Tol) -> (Vec<Failure>, Vec<OrderKey>) {
    let devs = order_devs(candidate, reference, tol);
    let mut spread: HashMap<OrderKey, f64> = HashMap::new();
    if devs.iter().any(|d| d.fails.is_some() && d.excusable) {
        for n in noise {
            for d in order_devs(n, reference, tol) {
                let s = spread.entry(d.key).or_insert(0.0);
                *s = s.max(d.size);
            }
        }
    }
    let mut out = Vec::new();
    let mut excused = Vec::new();
    let mut per_island: Vec<(usize, Vec<(Check, String)>)> = Vec::new();
    for d in devs {
        let Some((check, detail)) = d.fails else { continue };
        if d.excusable && spread.get(&d.key).is_some_and(|&s| d.size <= s) {
            excused.push(d.key);
            continue;
        }
        match d.island {
            Some(i) if d.key != OrderKey::Islands => match per_island.iter_mut().find(|(x, _)| *x == i) {
                Some((_, f)) => f.push((check, detail)),
                None => per_island.push((i, vec![(check, detail)])),
            },
            island => out.push(Failure { layer, island, check, detail }),
        }
    }
    for (i, f) in per_island {
        capped(layer, Some(i), f, &mut out);
    }
    (out, excused)
}

/// The length a path extrudes (mm): its segments that are not dry moves.
fn extruded_mm(p: &ExtrusionPath) -> f64 {
    p.pts.windows(2).enumerate().filter(|(k, _)| !p.dry.get(*k).copied().unwrap_or(false)).map(|(_, s)| s[0].dist(s[1])).sum::<f64>() / SCALE
}

/// What an excuse never covers, on a layer that used one: each paired island's wall paths
/// extrude the reference's length within 1 % and half a millimetre (`length`), and print the
/// reference's fill classes (`class`).
pub fn held(layer: usize, candidate: &ObjectLayer, reference: &ObjectLayer, tol: &Tol) -> Vec<Failure> {
    let mut out = Vec::new();
    let (pairs, ..) = island_pairs(candidate, reference, tol);
    for (i, j) in pairs {
        let (c, r) = (&candidate.islands[i], &reference.islands[j]);
        let walls = |x: &crate::Island| x.walls.iter().filter(|p| is_wall(p)).map(extruded_mm).sum::<f64>();
        let (lc, lr) = (walls(c), walls(r));
        if (lc - lr).abs() > 0.01 * lr + 0.5 {
            out.push(Failure { layer, island: Some(i), check: Check::Length, detail: format!("the walls extrude {lc:.2} mm, the CPU's {lr:.2} mm") });
        }
        let classes = |x: &crate::Island| {
            let mut v: Vec<String> = x.fills.iter().map(|p| format!("{:?}", p.role)).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        let (kc, kr) = (classes(c), classes(r));
        if kc != kr {
            out.push(Failure { layer, island: Some(i), check: Check::Class, detail: format!("fills {kc:?}, the CPU's {kr:?}") });
        }
    }
    out
}
