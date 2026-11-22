//! Whether a piece can leave the part: a way out found by search on the grid, then checked at
//! fine steps against the exact field, so the grid can't produce a wrong green. Each branch or pad
//! alone if the whole piece won't come. Then whether a tool can reach it. Nothing here estimates a
//! force: this is the state after a piece has broken free. The search samples, so no way
//! found means no way found, not that there is none.

use crate::grid::DistGrid;
use crate::pieces::{Ball, Piece, Section};
use crate::solid::Solid;
use makepad_fab_toolpath::par::Cancelled;
use std::collections::{BinaryHeap, HashSet};

/// The balls may come this much (mm) nearer than they started, or than this clearance.
const KEEP_CLEAR: f32 = 0.02;
const LEAST_STEP: f32 = 0.1;
const MOST_STEP: f32 = 2.0;
/// Poses a search visits at most, for a whole piece and for a section; translation first with
/// half of it.
const WHOLE_NODES: usize = 100_000;
const SECTION_NODES: usize = 25_000;
/// A search moves at most this many of a piece's balls, spread over it; the exact check, all of them.
const SEARCH_BALLS: usize = 256;
/// A piece this far past the part's box (mm) is out, whatever balls the search left out.
const CLEAR_BY: f32 = 1.0;
/// How far (degrees) a turn may go on each axis.
const MOST_TURN: f32 = 60.0;
/// What a millimetre of room counts for against a millimetre of way to go, in the order the search
/// tries poses: it leaves a tight place for open space, where its steps are long, rather than
/// combing along the wall that is nearest the outside.
const ROOM_WEIGHT: f32 = 2.0;
/// The way out's lattice, in grid cells. A gap narrower than this is shut to it, and only to it:
/// it orders the search and decides nothing.
const WALK_CELLS: usize = 4;
/// How many of a piece's balls, spread over it, the search's way out is the mean of.
const WALK_BALLS: usize = 8;
/// What the distance to the box's nearest side counts for in that order: once the balls that lead
/// are out, it finishes the job. Small, because that side may be through a wall.
const SIDE_WEIGHT: f32 = 0.1;
/// The exact check's steps (mm of the farthest ball's travel) and how far into a surface
/// (mm) a ball may graze.
const FINE_STEP: f64 = 0.05;
const GRAZE: f64 = 0.05;
/// How many blocked steps of a path the search tries to get around.
const RETRIES: usize = 8;
/// Tool radii: a pair of pliers or a gloved finger; a fine tip.
const TOOL_GOOD: f32 = 2.0;
const TOOL_TIGHT: f32 = 0.7;
const TOOL_DIRECTIONS: usize = 62;
const TOOL_GRIPS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    Good,
    Tight,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The whole piece has a way out and a tool can reach it.
    Free,
    /// It comes out only in sections, or reach is tight.
    Sections,
    TightReach,
    /// No way out found even in sections, or no tool reach.
    Trapped,
    NoReach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PieceVerdict {
    pub verdict: Verdict,
    /// How many of its sections were seen to come out alone before one didn't (all of them
    /// when the whole piece did).
    pub sections_out: usize,
    pub sections: usize,
    /// Whether finding the whole piece's way out took turning it.
    pub turned: bool,
}

impl Verdict {
    /// The worst to the best: red, amber, green.
    pub fn colour(self) -> Light {
        match self {
            Verdict::Free => Light::Green,
            Verdict::Sections | Verdict::TightReach => Light::Amber,
            Verdict::Trapped | Verdict::NoReach => Light::Red,
        }
    }
    pub fn words(self) -> &'static str {
        match self {
            Verdict::Free => "comes out whole",
            Verdict::Sections => "comes out in sections",
            Verdict::TightReach => "comes out; tight reach for a tool",
            Verdict::Trapped => "no way out found",
            Verdict::NoReach => "comes out, but no tool reach found",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    Green,
    Amber,
    Red,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pose {
    t: [f32; 3],
    /// Degrees about x, y and z, in that order.
    turn: [f32; 3],
}

const AT_REST: Pose = Pose { t: [0.0; 3], turn: [0.0; 3] };

struct Turning {
    m: [[f32; 3]; 3],
}

impl Turning {
    fn of(turn: [f32; 3]) -> Turning {
        let [a, b, c] = turn.map(f32::to_radians);
        let (sa, ca, sb, cb, sc, cc) = (a.sin(), a.cos(), b.sin(), b.cos(), c.sin(), c.cos());
        Turning { m: [[cc * cb, cc * sb * sa - sc * ca, cc * sb * ca + sc * sa], [sc * cb, sc * sb * sa + cc * ca, sc * sb * ca - cc * sa], [-sb, cb * sa, cb * ca]] }
    }
    #[inline]
    fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.m;
        std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2])
    }
}

/// The balls about their centre, and how far they reach from it.
struct Moving<'a> {
    balls: &'a [Ball],
    centre: [f32; 3],
    rel: Vec<[f32; 3]>,
    reach: f32,
}

impl<'a> Moving<'a> {
    fn new(balls: &'a [Ball]) -> Moving<'a> {
        let n = balls.len().max(1) as f32;
        Moving::about(balls, std::array::from_fn(|k| balls.iter().map(|b| b.c[k]).sum::<f32>() / n))
    }
    fn about(balls: &'a [Ball], centre: [f32; 3]) -> Moving<'a> {
        let rel: Vec<[f32; 3]> = balls.iter().map(|b| std::array::from_fn(|k| b.c[k] - centre[k])).collect();
        let reach = balls.iter().zip(&rel).map(|(b, r)| (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt() + b.r).fold(0.0, f32::max);
        Moving { balls, centre, rel, reach }
    }
    fn at(&self, turn: &Turning, pose: &Pose, i: usize) -> [f32; 3] {
        let q = turn.apply(self.rel[i]);
        std::array::from_fn(|k| self.centre[k] + pose.t[k] + q[k])
    }
}

/// How far the moved piece still has to go to clear the part's box on the nearest side; at or
/// below zero it is out. The bed isn't in the way: the part has come off it.
fn to_clear(m: &Moving, pose: &Pose, grid: &DistGrid) -> f32 {
    let turn = Turning::of(pose.turn);
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for i in 0..m.balls.len() {
        let c = m.at(&turn, pose, i);
        for k in 0..3 {
            lo[k] = lo[k].min(c[k] - m.balls[i].r);
            hi[k] = hi[k].max(c[k] + m.balls[i].r);
        }
    }
    (0..3).map(|k| (grid.box_hi[k] - lo[k]).min(hi[k] - grid.box_lo[k])).fold(f32::MAX, f32::min) + CLEAR_BY
}

/// How far a point has to walk, around the part, to get past each side of the part's box. A piece
/// leaves by one side, all of it, and the nearest side can be through a wall.
struct WalkOut {
    lo: [f32; 3],
    cell: f32,
    n: [usize; 3],
    /// One walk per side of the box, on the lattice.
    walks: Vec<Vec<f32>>,
}

impl WalkOut {
    fn of(grid: &DistGrid) -> WalkOut {
        let (lo, cell, n) = grid.coarse(WALK_CELLS);
        let strides = [1, n[0], n[0] * n[1]];
        let index = |at: usize| -> [usize; 3] { [at % n[0], at / n[0] % n[1], at / strides[2]] };
        let point = |at: usize| -> [f32; 3] {
            let i = index(at);
            std::array::from_fn(|k| lo[k] + i[k] as f32 * cell)
        };
        let count = n[0] * n[1] * n[2];
        let room: Vec<f32> = (0..count).map(|at| grid.distance(point(at))).collect();
        let walk_from = |past: &dyn Fn([f32; 3]) -> bool| {
            let mut walk = vec![f32::MAX; count];
            let mut front: Vec<usize> = (0..count).filter(|&at| past(point(at))).collect();
            front.iter().for_each(|&at| walk[at] = 0.0);
            // In from there a step at a time, over steps whose ends' distances cover them.
            let mut walked = 0.0;
            let mut next = Vec::new();
            while !front.is_empty() {
                walked += cell;
                for &at in &front {
                    let i = index(at);
                    for k in 0..3 {
                        for to in [(i[k] > 0).then(|| at - strides[k]), (i[k] + 1 < n[k]).then(|| at + strides[k])].into_iter().flatten() {
                            if walk[to] == f32::MAX && room[to] > 0.0 && room[at] + room[to] >= cell {
                                walk[to] = walked;
                                next.push(to);
                            }
                        }
                    }
                }
                std::mem::swap(&mut front, &mut next);
                next.clear();
            }
            // Past anywhere a walk reached: inside the part, or in a hollow with no way out.
            walk.iter_mut().filter(|w| **w == f32::MAX).for_each(|w| *w = walked + cell);
            walk
        };
        let mut walks = Vec::with_capacity(6);
        for k in 0..3 {
            walks.push(walk_from(&|p| p[k] < grid.box_lo[k]));
            walks.push(walk_from(&|p| p[k] > grid.box_hi[k]));
        }
        WalkOut { lo, cell, n, walks }
    }

    /// The shortest of the sides' mean walks, for the balls that lead a piece ([`grips`] puts them
    /// first); between the lattice's points, and past its edge, the edge.
    fn at(&self, m: &Moving, pose: &Pose) -> f32 {
        let turn = Turning::of(pose.turn);
        let [nx, ny, _] = self.n;
        let mut sums = [0.0f32; 6];
        let balls = m.balls.len().min(WALK_BALLS);
        for b in 0..balls {
            let p = m.at(&turn, pose, b);
            let u: [f32; 3] = std::array::from_fn(|k| ((p[k] - self.lo[k]) / self.cell).clamp(0.0, (self.n[k] - 1) as f32));
            let i: [usize; 3] = std::array::from_fn(|k| (u[k] as usize).min(self.n[k] - 2));
            for corner in 0..8usize {
                let side = [corner & 1, corner >> 1 & 1, corner >> 2];
                let weight: f32 = (0..3).map(|k| {
                    let t = u[k] - i[k] as f32;
                    if side[k] == 1 { t } else { 1.0 - t }
                }).product();
                let at = i[0] + side[0] + nx * (i[1] + side[1]) + nx * ny * (i[2] + side[2]);
                for (sum, walk) in sums.iter_mut().zip(&self.walks) {
                    *sum += weight * walk[at];
                }
            }
        }
        sums.iter().fold(f32::MAX, |a, &b| a.min(b)) / balls.max(1) as f32
    }
}

/// The clearance at a pose, giving up as soon as it is below `need`; `last` is the ball that
/// failed last time, the likeliest to fail again.
fn clearance(m: &Moving, pose: &Pose, grid: &DistGrid, need: f32, last: &mut usize) -> Option<f32> {
    let turn = Turning::of(pose.turn);
    if m.balls.is_empty() {
        return Some(f32::MAX);
    }
    let check = |i: usize| grid.distance(m.at(&turn, pose, i)) - m.balls[i].r;
    let first = check(*last);
    if first < need {
        return None;
    }
    let mut least = first;
    for i in 0..m.balls.len() {
        let c = check(i);
        if c < need {
            *last = i;
            return None;
        }
        least = least.min(c);
    }
    Some(least)
}

/// A pose's cell at a level: the cells' sides double with each level, so a search with room to spare
/// doesn't fill a place at the fineness it needs only in a tight one.
type Key = [i32; 7];

fn key_of(p: &Pose, level: u32, radians: f32) -> Key {
    let cell = LEAST_STEP * (1u32 << level) as f32;
    let t = p.t.map(|v| (v / cell).round() as i32);
    let turn = (cell / radians).to_degrees().max(0.5);
    let a = p.turn.map(|v| (v / turn).round() as i32);
    [level as i32, t[0], t[1], t[2], a[0], a[1], a[2]]
}

/// The level whose cells are within half of a step.
fn level_of(step: f32) -> u32 {
    (step / (2.0 * LEAST_STEP)).max(1.0).log2().floor() as u32
}

struct Node {
    pose: Pose,
    parent: u32,
    clear: f32,
    /// [`to_clear`] at this pose.
    left: f32,
}

#[derive(PartialEq)]
struct Open(f32, u32);

impl Eq for Open {}

impl PartialOrd for Open {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Open {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        // The heap's top is the lowest, then the oldest: one order on every run.
        o.0.total_cmp(&self.0).then(o.1.cmp(&self.1))
    }
}

/// A path out by best-first search over poses, from rest to clear of the part's box.
fn search(m: &Moving, grid: &DistGrid, walk: &WalkOut, need: f32, most: usize, turning: bool, blocked: &HashSet<Key>, cancelled: &dyn Fn() -> bool) -> Result<Option<Vec<Pose>>, Cancelled> {
    let mut last = 0;
    let Some(start) = clearance(m, &AT_REST, grid, need, &mut last) else { return Ok(None) };
    let mut nodes = vec![Node { pose: AT_REST, parent: u32::MAX, clear: start, left: to_clear(m, &AT_REST, grid) }];
    let mut open = BinaryHeap::new();
    open.push(Open(0.0, 0));
    let mut seen: HashSet<Key> = HashSet::new();
    let radians = m.reach.max(0.1);
    seen.insert(key_of(&AT_REST, 0, radians));
    let mut directions: Vec<[f32; 3]> = Vec::with_capacity(26);
    for dz in -1i32..=1 {
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                if (dx, dy, dz) != (0, 0, 0) {
                    let l = ((dx * dx + dy * dy + dz * dz) as f32).sqrt();
                    directions.push([dx as f32 / l, dy as f32 / l, dz as f32 / l]);
                }
            }
        }
    }
    while let Some(Open(_, at)) = open.pop() {
        if nodes[at as usize].left <= 0.0 {
            let mut path = Vec::new();
            let mut i = at;
            while i != u32::MAX {
                path.push(nodes[i as usize].pose);
                i = nodes[i as usize].parent;
            }
            path.reverse();
            return Ok(Some(path));
        }
        if nodes.len() >= most {
            return Ok(None);
        }
        if nodes.len() % 1024 == 0 && cancelled() {
            return Err(Cancelled);
        }
        let (pose, clear) = (nodes[at as usize].pose, nodes[at as usize].clear);
        let step = clear.clamp(LEAST_STEP, MOST_STEP);
        let level = level_of(step);
        let mut go = |next: Pose, nodes: &mut Vec<Node>, open: &mut BinaryHeap<Open>| {
            if blocked.contains(&key_of(&next, 0, radians)) || !seen.insert(key_of(&next, level, radians)) {
                return;
            }
            if let Some(clear) = clearance(m, &next, grid, need, &mut last) {
                let id = nodes.len() as u32;
                let left = to_clear(m, &next, grid);
                nodes.push(Node { pose: next, parent: at, clear, left });
                open.push(Open(walk.at(m, &next) + SIDE_WEIGHT * left - ROOM_WEIGHT * clear, id));
            }
        };
        for d in &directions {
            let next = Pose { t: std::array::from_fn(|k| pose.t[k] + d[k] * step), turn: pose.turn };
            go(next, &mut nodes, &mut open);
        }
        if turning {
            let by = (step / radians).to_degrees().clamp(0.5, 15.0);
            for axis in 0..3 {
                for sign in [-1.0, 1.0] {
                    let mut turn = pose.turn;
                    turn[axis] += sign * by;
                    if turn[axis].abs() <= MOST_TURN {
                        go(Pose { t: pose.t, turn }, &mut nodes, &mut open);
                    }
                }
            }
        }
    }
    Ok(None)
}

/// What the exact check reuses between its steps, and the balls it found in the way.
#[derive(Default)]
struct ExactBuffers {
    asked: Vec<usize>,
    at: Vec<[f64; 3]>,
    values: Vec<f64>,
    grazing: Vec<usize>,
    near: Vec<[f64; 3]>,
    stopped: Vec<usize>,
}

/// Whether the ball at each centre stays out of the solid; the balls that don't go in `stopped`.
/// A ball the grid has well clear needs no more; the rest go to the exact field, and `grazing`
/// balls, too near for its value to prove it, are tried at points on them.
fn exact_clear(centres: &[[f64; 3]], radii: &[f64], grid: &DistGrid, solid: &Solid, b: &mut ExactBuffers, directions: &[[f64; 3]]) -> (bool, f64) {
    let mut margin = f64::MAX;
    b.asked.clear();
    b.at.clear();
    for i in 0..centres.len() {
        let p = centres[i].map(|v| v as f32);
        let spare = grid.least_at(p, grid.distance(p)) - radii[i] as f32;
        if spare > 0.0 {
            margin = margin.min(spare as f64);
        } else {
            b.asked.push(i);
            b.at.push(centres[i]);
        }
    }
    b.values.resize(b.at.len(), 0.0);
    solid.values(&b.at, &mut b.values);
    let ExactBuffers { asked, values, grazing, near, stopped, .. } = b;
    stopped.clear();
    stopped.extend(asked.iter().zip(values.iter()).filter(|(_, &v)| v < 0.0).map(|(&i, _)| i));
    if !stopped.is_empty() {
        return (false, margin);
    }
    grazing.clear();
    near.clear();
    for (&i, &v) in asked.iter().zip(values.iter()) {
        let spare = solid.clearance.radius(v) - radii[i];
        margin = margin.min(spare);
        if spare < GRAZE {
            let r = (radii[i] - GRAZE).max(0.0);
            grazing.push(i);
            for d in directions {
                for shell in [r, 0.5 * r] {
                    near.push(std::array::from_fn(|k| centres[i][k] + d[k] * shell));
                }
            }
        }
    }
    if near.is_empty() {
        return (true, margin);
    }
    values.resize(near.len(), 0.0);
    solid.values(near, values);
    let per = 2 * directions.len();
    stopped.extend(grazing.iter().enumerate().filter(|(g, _)| values[g * per..(g + 1) * per].iter().any(|&v| v < 0.0)).map(|(_, &i)| i));
    (stopped.is_empty(), margin)
}

/// The first step of `path` that the exact field stops and the balls it stopped, or None all the
/// way.
fn blocked_at(m: &Moving, path: &[Pose], grid: &DistGrid, solid: &Solid, cancelled: &dyn Fn() -> bool) -> Result<Option<(usize, Vec<usize>)>, Cancelled> {
    let directions = sphere_directions(TOOL_DIRECTIONS).map(|d| d.map(|v| v as f64)).collect::<Vec<_>>();
    let radii: Vec<f64> = m.balls.iter().map(|b| b.r as f64).collect();
    let mut centres = vec![[0.0; 3]; m.balls.len()];
    let mut buffers = ExactBuffers::default();
    for s in 0..path.len().saturating_sub(1) {
        let (a, b) = (path[s], path[s + 1]);
        let shift = ((b.t[0] - a.t[0]).powi(2) + (b.t[1] - a.t[1]).powi(2) + (b.t[2] - a.t[2]).powi(2)).sqrt() as f64;
        let turned = (0..3).map(|k| (b.turn[k] - a.turn[k]).abs()).fold(0.0, f32::max).to_radians() as f64 * m.reach as f64;
        let travel = (shift + turned).max(1e-6);
        let mut along = 0.0;
        loop {
            if cancelled() {
                return Err(Cancelled);
            }
            let u = (along / travel).min(1.0) as f32;
            let pose = Pose { t: std::array::from_fn(|k| a.t[k] + (b.t[k] - a.t[k]) * u), turn: std::array::from_fn(|k| a.turn[k] + (b.turn[k] - a.turn[k]) * u) };
            let turn = Turning::of(pose.turn);
            for (i, c) in centres.iter_mut().enumerate() {
                *c = m.at(&turn, &pose, i).map(|v| v as f64);
            }
            let (clear, margin) = exact_clear(&centres, &radii, grid, solid, &mut buffers, &directions);
            if !clear {
                return Ok(Some((s, std::mem::take(&mut buffers.stopped))));
            }
            if along >= travel {
                break;
            }
            along += margin.max(FINE_STEP);
        }
    }
    Ok(None)
}

/// A checked way out for these balls, if the search finds one: whether it took turning them. The
/// search moves a spread of them; a ball the exact check finds in the way of its path joins the
/// spread for the next try, so that what the spread passed between can't stop every try alike.
fn way_out(balls: &[Ball], grid: &DistGrid, solid: &Solid, walk: &WalkOut, most: usize, cancelled: &dyn Fn() -> bool) -> Result<Option<bool>, Cancelled> {
    if balls.is_empty() {
        return Ok(Some(false));
    }
    let mut spread = grips(balls, SEARCH_BALLS);
    // All of them turn about the centre the first spread turns about, whatever joins it.
    let centre = Moving::new(&spread).centre;
    let all = Moving::about(balls, centre);
    let at_rest = Turning::of(AT_REST.turn);
    for (turning, nodes) in [(false, most / 2), (true, most)] {
        let mut blocked = HashSet::new();
        for _ in 0..RETRIES {
            let m = Moving::about(&spread, centre);
            let start = (0..spread.len()).map(|i| grid.distance(m.at(&at_rest, &AT_REST, i)) - spread[i].r).fold(f32::MAX, f32::min);
            let radians = m.reach.max(0.1);
            let Some(path) = search(&m, grid, walk, start.min(KEEP_CLEAR), nodes, turning, &blocked, cancelled)? else { break };
            match blocked_at(&all, &path, grid, solid, cancelled)? {
                None => return Ok(Some(turning)),
                Some((s, stopped)) => {
                    let joining: Vec<Ball> = stopped.iter().map(|&i| balls[i]).filter(|b| !spread.iter().any(|g| g.c == b.c && g.r == b.r)).collect();
                    spread.extend(joining);
                    blocked.insert(key_of(&path[s + 1], 0, radians));
                    if s > 0 {
                        blocked.insert(key_of(&path[s], 0, radians));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// `n` directions spread over a sphere, in one order on every run.
fn sphere_directions(n: usize) -> impl Iterator<Item = [f32; 3]> {
    (0..n).map(move |i| {
        let z = 1.0 - 2.0 * (i as f32 + 0.5) / n as f32;
        let r = (1.0 - z * z).sqrt();
        let a = i as f32 * 2.399_963_2;
        [r * a.cos(), r * a.sin(), z]
    })
}

/// Balls spread over the piece: each the farthest from those already chosen.
fn grips(balls: &[Ball], most: usize) -> Vec<Ball> {
    let Some(&first) = balls.iter().max_by(|a, b| a.r.total_cmp(&b.r)) else { return Vec::new() };
    let mut out = vec![first];
    let d2 = |a: &Ball, b: &Ball| (0..3).map(|k| (a.c[k] - b.c[k]).powi(2)).sum::<f32>();
    let mut far: Vec<f32> = balls.iter().map(|b| d2(b, &first)).collect();
    while out.len() < most.min(balls.len()) {
        let (i, &d) = far.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
        if d <= 0.0 {
            break;
        }
        out.push(balls[i]);
        for (f, b) in far.iter_mut().zip(balls) {
            *f = f.min(d2(b, &balls[i]));
        }
    }
    out
}

fn tool_gets_to(g: &Ball, u: [f32; 3], grid: &DistGrid, tool: f32) -> bool {
    let start: [f32; 3] = std::array::from_fn(|k| g.c[k] + u[k] * (g.r + tool));
    let mut t = 0.0;
    loop {
        let p: [f32; 3] = std::array::from_fn(|k| start[k] + u[k] * t);
        if (0..3).any(|k| p[k] < grid.box_lo[k] - tool || p[k] > grid.box_hi[k] + tool) {
            return true;
        }
        let d = grid.distance(p);
        if d < tool {
            return false;
        }
        t += (d - tool).max(0.25 * tool);
    }
}

/// Whether a tool can come to these balls from outside the part in a straight line. Other pieces
/// aren't in its way: what's in front comes out first.
fn tool_reach(balls: &[Ball], grid: &DistGrid) -> Reach {
    let grips = grips(balls, TOOL_GRIPS);
    for (tool, reach) in [(TOOL_GOOD, Reach::Good), (TOOL_TIGHT, Reach::Tight)] {
        for g in &grips {
            if sphere_directions(TOOL_DIRECTIONS).any(|u| tool_gets_to(g, u, grid, tool)) {
                return reach;
            }
        }
    }
    Reach::None
}

/// The verdict on one piece, at whatever speed the caller's thread gets.
pub fn check_piece(piece: &Piece, grid: &DistGrid, solid: &Solid, cancelled: &dyn Fn() -> bool) -> Result<PieceVerdict, Cancelled> {
    let sections = piece.sections.len();
    let fine = piece.fine_balls();
    let walk = WalkOut::of(grid);
    if let Some(turned) = way_out(&fine, grid, solid, &walk, WHOLE_NODES, cancelled)? {
        let verdict = match tool_reach(&fine, grid) {
            Reach::Good => Verdict::Free,
            Reach::Tight => Verdict::TightReach,
            Reach::None => Verdict::NoReach,
        };
        return Ok(PieceVerdict { verdict, sections_out: sections, sections, turned });
    }
    // The widest first: the likeliest to stay in, and the first one that does decides.
    let mut order: Vec<&Section> = piece.sections.iter().collect();
    order.sort_by(|a, b| extent(&b.balls).total_cmp(&extent(&a.balls)));
    let mut out = 0;
    let mut all_reached = true;
    for s in &order {
        if way_out(&s.balls, grid, solid, &walk, SECTION_NODES, cancelled)?.is_none() {
            return Ok(PieceVerdict { verdict: Verdict::Trapped, sections_out: out, sections, turned: false });
        }
        out += 1;
        all_reached &= tool_reach(&s.balls, grid) != Reach::None;
    }
    let verdict = if sections < 2 {
        Verdict::Trapped
    } else if all_reached {
        Verdict::Sections
    } else {
        Verdict::NoReach
    };
    Ok(PieceVerdict { verdict, sections_out: out, sections, turned: false })
}

/// How far the balls reach along their widest axis.
fn extent(balls: &[Ball]) -> f32 {
    (0..3).map(|k| {
        let hi = balls.iter().map(|b| b.c[k] + b.r).fold(f32::MIN, f32::max);
        let lo = balls.iter().map(|b| b.c[k] - b.r).fold(f32::MAX, f32::min);
        hi - lo
    }).fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_csg_sdf::{SdfNode, SdfOp, SdfShape};
    use makepad_fab_toolpath::par::Ctl;

    fn solid_of(root: SdfNode, half: [f64; 3]) -> (Solid, DistGrid) {
        let shape = SdfShape { root, min: half.map(|v| -v), max: half, materials: vec![] };
        let solid = Solid::of_field(&shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], None);
        let grid = DistGrid::check(Ctl::default(), &solid, 0.2, crate::grid::Contact::Fast).unwrap();
        (solid, grid)
    }

    fn cuboid(half: [f64; 3]) -> SdfNode {
        SdfNode::Box { size: half.map(|v| v * 2.0), radius: 0.0 }
    }

    fn difference(of: Vec<SdfNode>) -> SdfNode {
        SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of }
    }

    fn clear(c: [f64; 3], r: f64, solid: &Solid, grid: &DistGrid) -> bool {
        let directions = sphere_directions(TOOL_DIRECTIONS).map(|d| d.map(|v| v as f64)).collect::<Vec<_>>();
        exact_clear(&[c], &[r], grid, solid, &mut ExactBuffers::default(), &directions).0
    }

    /// Whether a ball gets out, on its own.
    fn gets_out(r: f32, solid: &Solid, grid: &DistGrid) -> bool {
        let ball = Ball { c: [0.0; 3], r };
        let piece = Piece { name: "ball".into(), balls: vec![ball], sections: vec![Section { balls: vec![ball] }], ..Default::default() };
        check_piece(&piece, grid, solid, &|| false).unwrap().sections_out == 1
    }

    /// In a flat hollow the field's value runs far over the distance to the walls: 4.5 mm where
    /// the wall is 0.26 away. A ball that reaches the wall must not be called clear on that.
    #[test]
    fn a_ball_into_the_wall_of_a_flat_hollow_is_not_clear() {
        let radii = [30.0, 30.0, 0.5];
        let (solid, grid) = solid_of(difference(vec![cuboid([33.0, 33.0, 3.0]), SdfNode::Ellipsoid { radii }]), [33.0, 33.0, 3.0]);
        for r in [0.6f64, 1.0, 2.0] {
            for i in 0..400 {
                let (angle, out) = (i as f64 * 2.399963229728653, 0.3 + 0.6 * (i as f64 / 400.0));
                let c = [radii[0] * out * angle.cos(), radii[1] * out * angle.sin(), 0.0];
                assert!(!clear(c, r, &solid, &grid), "a ball of {r} at {c:?} in a hollow at most 0.5 tall");
            }
        }
    }

    /// The field says which side the centre is on, however thin the wall is for the points tried.
    #[test]
    fn a_ball_centred_in_a_thin_wall_is_not_clear() {
        let (solid, grid) = solid_of(cuboid([20.0, 20.0, 0.004]), [20.0, 20.0, 1.0]);
        assert!(!clear([0.0; 3], 1.0, &solid, &grid));
    }

    /// A way out cut by a flat ellipsoid, narrowing to 0.3 where its rim breaks the wall: what
    /// fits comes out and what doesn't stays.
    #[test]
    fn a_hollow_of_a_flat_ellipsoid_lets_out_what_fits() {
        let (neck, c) = (0.3f64, 1.0f64);
        let x = 10.0 * (1.0 - (neck / c) * (neck / c)).sqrt();
        let (solid, grid) = solid_of(difference(vec![cuboid([x, 40.0, 10.0]), SdfNode::Ellipsoid { radii: [10.0, 10.0, c] }]), [x, 40.0, 10.0]);
        assert!(gets_out(0.24, &solid, &grid));
        assert!(!gets_out(0.45, &solid, &grid));
    }

    /// A tube with a flat ellipsoid across it: the way out is the 1 mm ring round its rim.
    #[test]
    fn a_flat_ellipsoid_across_a_tube_lets_by_what_fits() {
        let at = |at: [f64; 3], of: SdfNode| SdfNode::Transform { at, rotate: [0.0; 3], scale: 1.0, of: Box::new(of) };
        let tube = SdfNode::Cylinder { radius: 10.0, top_radius: 10.0, height: 40.0 };
        let body = difference(vec![cuboid([20.0, 20.0, 20.0]), at([0.0, 5.0, 0.0], tube)]);
        let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![body, at([0.0, 10.0, 0.0], SdfNode::Ellipsoid { radii: [9.0, 0.5, 9.0] })] };
        let (solid, grid) = solid_of(root, [20.0, 20.0, 20.0]);
        assert!(gets_out(0.45, &solid, &grid));
        assert!(!gets_out(0.6, &solid, &grid));
    }
}
