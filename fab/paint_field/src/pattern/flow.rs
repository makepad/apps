//! Flow lines over a band's skin (curl noise): lines along a swirling flow over the skin, a
//! spacing (the scale) apart, each stopping short of the others, as a field's streamlines are
//! drawn evenly.
//!
//! The flow, per skin cell, is the curl of three noises (Perlin's, `swirl` scales across) laid
//! into the surface there; or, for contours, the way round one noise's slope along it, so its
//! lines are the noise's contours. A line starts a spacing either side of a line drawn (a spacing
//! apart along it) or a spacing on from its ends; the first, and any where those run out, at the
//! skin cells in the seed's order; [`GAP`] of a spacing or more from every line. It goes along the
//! flow and against it in strides of [`STEP`] (over the surface, [`super::skin`]'s crawl), each
//! way till it comes [`STOP`] of a spacing of another line or of itself further back, leaves the
//! band, finds the flow still, or is `length` spacings long. It is `width` of the spacing wide,
//! `taper` of that narrowing to its ends. A step traces [`SEEDS`] lines at once from the lines as
//! they were, then draws each in their order, what of it stays [`STOP`] of a spacing from the
//! lines drawn before it, when that is [`SHORT`] spacings or longer; till one is drawn. So a state
//! is the same whatever the workers, and a step that draws none is the last.
//!
//! The state: per band cell `u`, how far inside its spot's nearest line its own point of the
//! surface lies (cells, below 0 outside), so a line's edge is as sharp off the skin as on it; per
//! skin cell (in [`Skin`]'s order) `v`, how far from the nearest line (cells; past a spacing,
//! infinite); and the lines' flow and where the next ones start.

use super::band::Level;
use super::dla::Walk;
use super::rd::State;
use super::skin::{add, crawl, cross, dot, unit, Crowd, Skin};
use super::{mix, pack, CellMap, Model, Spec};
use crate::bake::Fan;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

/// A line's stride along the flow (cells).
const STEP: f32 = 0.5;
/// How near another line (or itself further back) a line comes before it stops (a share of the
/// spacing).
const STOP: f32 = 0.6;
/// How far from every line a line starts, at least (a share of the spacing; a start a spacing
/// beside one lands about there).
const GAP: f32 = 0.9;
/// The shortest line drawn (spacings).
const SHORT: f32 = 1.0;
/// Lines a step traces at once.
const SEEDS: usize = 16;
/// Skin cells a worker lays the flow into at a time.
const CHUNK: usize = 4096;

/// Where a line starts: a point of the surface and the band cell it lies in.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Start {
    at: [f32; 3],
    cell: u32,
}

/// A flow's lines so far: the flow, where the next ones start, and how many are drawn.
#[derive(Clone, Default)]
pub struct Lines {
    /// Per skin cell the way the flow goes there (a unit vector along the surface; 0 where it is
    /// still).
    flow: Arc<Vec<[f32; 3]>>,
    /// The skin's band cells in the seed's order, where lines start when none is left beside the
    /// lines drawn; and how far through it.
    order: Arc<Vec<u32>>,
    next: usize,
    /// Where lines start next: a spacing either side of the lines drawn and on from their ends, in
    /// the order drawn.
    starts: VecDeque<Start>,
    /// The lines drawn.
    pub drawn: u32,
}

impl Lines {
    /// The next start: beside a line drawn, else the next skin cell of the seed's order.
    fn next(&mut self, level: &Level) -> Option<Start> {
        if let Some(s) = self.starts.pop_front() {
            return Some(s);
        }
        let &i = self.order.get(self.next)?;
        self.next += 1;
        Some(Start { at: level.surface[i as usize], cell: i })
    }
}

/// The starting state of `spec` on `level`: no line, the flow (round contours or not, `swirl`
/// spacings across) laid into the skin.
pub fn seed(spec: &Spec, level: &Level, contour: bool, swirl: f64, fan: Fan) -> State {
    let skin = Arc::new(Skin::new(level));
    let n = skin.cells.len();
    // the noise's lattice `swirl` spacings across
    let across = (1.0 / (swirl * spec.preset.cells)) as f32;
    let salt = mix(spec.seed as u64 ^ 0xf10e);
    let mut flow = vec![[0.0; 3]; n];
    let chunks: Vec<Mutex<&mut [[f32; 3]]>> = flow.chunks_mut(CHUNK).map(Mutex::new).collect();
    fan(chunks.len(), &|c| {
        let mut out = chunks[c].lock().unwrap_or_else(|e| e.into_inner());
        for (o, w) in out.iter_mut().enumerate() {
            let i = skin.cells[c * CHUNK + o] as usize;
            *w = way(level.surface[i].map(|v| v * across), skin.up(i), salt, contour);
        }
    });
    let mut order: Vec<(u64, u32)> = skin.cells.iter().map(|&i| (mix(mix(spec.seed as u64 ^ 0xf10e_5eed) ^ pack(level.cells[i as usize])), i)).collect();
    order.sort_unstable();
    let lines = Lines { flow: Arc::new(flow), order: Arc::new(order.into_iter().map(|(_, i)| i).collect()), next: 0, starts: VecDeque::new(), drawn: 0 };
    State { u: vec![f32::NEG_INFINITY; level.band], v: vec![f32::INFINITY; n], walk: Walk::default(), seeds: Vec::new(), crowd: Crowd { skin, agents: Vec::new(), lines, ground: Arc::default(), trodden: Vec::new() } }
}

/// Gradient noise's corner slopes (Perlin's twelve).
const SLOPES: [[f32; 3]; 12] = [[1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], [1.0, -1.0, 0.0], [-1.0, -1.0, 0.0], [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0], [1.0, 0.0, -1.0], [-1.0, 0.0, -1.0], [0.0, 1.0, 1.0], [0.0, -1.0, 1.0], [0.0, 1.0, -1.0], [0.0, -1.0, -1.0]];

/// The slope of Perlin's gradient noise for `salt` at `p` (its lattice's cells).
fn slope(p: [f32; 3], salt: u64) -> [f32; 3] {
    let base = p.map(f32::floor);
    let f: [f32; 3] = std::array::from_fn(|k| p[k] - base[k]);
    let fade = f.map(|t| t * t * t * (t * (t * 6.0 - 15.0) + 10.0));
    let rate = f.map(|t| 30.0 * t * t * (t * (t - 2.0) + 1.0));
    let mut out = [0.0; 3];
    for c in 0..8 {
        let o = [c & 1, c >> 1 & 1, c >> 2 & 1];
        let corner: [i32; 3] = std::array::from_fn(|k| base[k] as i32 + o[k]);
        let g = SLOPES[(mix(salt ^ pack(corner)) % 12) as usize];
        let a = dot(g, std::array::from_fn(|k| f[k] - o[k] as f32));
        let w: [f32; 3] = std::array::from_fn(|k| if o[k] == 1 { fade[k] } else { 1.0 - fade[k] });
        let dw: [f32; 3] = std::array::from_fn(|k| if o[k] == 1 { rate[k] } else { -rate[k] });
        out[0] += w[0] * w[1] * w[2] * g[0] + dw[0] * w[1] * w[2] * a;
        out[1] += w[0] * w[1] * w[2] * g[1] + w[0] * dw[1] * w[2] * a;
        out[2] += w[0] * w[1] * w[2] * g[2] + w[0] * w[1] * dw[2] * a;
    }
    out
}

/// The flow at `p` (the noise's cells) where the way out of the surface is `n`: the curl of
/// three noises laid into the surface, or round one noise's slope along it; a unit vector, 0
/// where it is still.
fn way(p: [f32; 3], n: [f32; 3], salt: u64, contour: bool) -> [f32; 3] {
    let along = |g: [f32; 3]| add(g, n, -dot(g, n));
    let w = if contour {
        cross(n, along(slope(p, salt)))
    } else {
        let [a, b, c] = [0u64, 1, 2].map(|k| slope(p, mix(salt ^ k)));
        along([c[1] - b[2], a[2] - c[0], b[0] - a[1]])
    };
    unit(w).unwrap_or([0.0; 3])
}

/// A point of a line: on the surface, its band cell, and how far along the line from its start
/// (cells, against the flow below 0).
#[derive(Clone, Copy, Debug)]
struct Point {
    at: [f32; 3],
    cell: u32,
    along: f32,
}

/// A line traced: its points in order along the flow, which is its start, the starts beside it
/// (by the point they are beside) and a spacing on from either end (against the flow, then along
/// it).
struct Line {
    points: Vec<Point>,
    start: usize,
    sides: Vec<(usize, Start)>,
    ends: [Option<Start>; 2],
}

fn far(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    dot(d, d).sqrt()
}

/// The line from `start` along the flow and against it over the lines as they are (`gap`, per
/// skin cell how far from the nearest), `spacing` and `long` cells.
fn trace(level: &Level, skin: &Skin, flow: &[[f32; 3]], gap: &[f32], start: Start, spacing: f32, long: f32) -> Line {
    let stop = STOP * spacing;
    // its points by a lattice `stop` across, to find itself further back
    let mut near: CellMap<Vec<u32>> = CellMap::default();
    let key = |p: [f32; 3]| p.map(|v| (v / stop).floor() as i32);
    let mut points = vec![Point { at: start.at, cell: start.cell, along: 0.0 }];
    near.entry(pack(key(start.at))).or_default().push(0);
    for dir in [1.0f32, -1.0] {
        let (mut p, mut i, mut along) = (start.at, start.cell as usize, 0.0f32);
        loop {
            let w = flow[skin.at(i as u32)];
            if w == [0.0; 3] {
                break;
            }
            let Some((q, _, j)) = crawl(level, skin, i, p, w.map(|v| v * dir), STEP) else { break };
            let by = far(p, q);
            along += by;
            if by < 0.25 * STEP || along > 0.5 * long || gap[skin.at(j as u32)] < stop {
                break;
            }
            let (k, mine) = (key(q), along * dir);
            let back = (0..27).any(|o| {
                let c = [k[0] + o % 3 - 1, k[1] + o / 3 % 3 - 1, k[2] + o / 9 - 1];
                near.get(&pack(c)).is_some_and(|m| m.iter().any(|&m| (points[m as usize].along - mine).abs() > 3.0 * stop && far(points[m as usize].at, q) < stop))
            });
            if back {
                break;
            }
            near.entry(pack(k)).or_default().push(points.len() as u32);
            points.push(Point { at: q, cell: j as u32, along: mine });
            (p, i) = (q, j);
        }
    }
    points.sort_by(|a, b| a.along.total_cmp(&b.along));
    let start = points.iter().position(|p| p.along == 0.0).unwrap_or(0);
    // the starts a spacing either side, a spacing apart along it
    let mut sides = Vec::new();
    let mut last = f32::NEG_INFINITY;
    for (k, p) in points.iter().enumerate() {
        if p.along - last < spacing {
            continue;
        }
        last = p.along;
        let i = p.cell as usize;
        let Some(side) = unit(cross(skin.up(i), flow[skin.at(p.cell)])) else { continue };
        for s in [1.0f32, -1.0] {
            if let Some((at, _, c)) = crawl(level, skin, i, p.at, side.map(|v| v * s), spacing) {
                sides.push((k, Start { at, cell: c as u32 }));
            }
        }
    }
    let on = |p: &Point, dir: f32| crawl(level, skin, p.cell as usize, p.at, flow[skin.at(p.cell)].map(|v| v * dir), spacing).map(|(at, _, c)| Start { at, cell: c as u32 });
    let ends = [points.first().and_then(|p| on(p, -1.0)), points.last().and_then(|p| on(p, 1.0))];
    Line { points, start, sides, ends }
}

/// How far `x` is from the segment `a`..`b`, and where along it the nearest point lies (0..1).
fn to_segment(x: [f32; 3], a: [f32; 3], b: [f32; 3]) -> (f32, f32) {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let l = dot(ab, ab);
    let t = if l > 0.0 { (dot([x[0] - a[0], x[1] - a[1], x[2] - a[2]], ab) / l).clamp(0.0, 1.0) } else { 0.0 };
    (far(x, add(a, ab, t)), t)
}

/// Draws what of `line` stays [`STOP`] of a spacing from the lines `now` has, when [`SHORT`]
/// spacings or longer: each skin cell within a spacing of it that lies nearer it than any line
/// takes how far (`v`), and the band cells of its spot how far inside it (`u`); and the starts
/// beside it queue. Returns the skin cells it took (0: not drawn).
fn draw(level: &Level, skin: &Skin, now: &mut State, line: &Line, spacing: f32, width: f32, taper: f32) -> f64 {
    let (stop, pts) = (STOP * spacing, &line.points);
    let gap = |k: usize| now.v[skin.at(pts[k].cell)];
    if gap(line.start) < GAP * spacing {
        return 0.0;
    }
    let (mut a, mut b) = (line.start, line.start);
    while a > 0 && gap(a - 1) >= stop {
        a -= 1;
    }
    while b + 1 < pts.len() && gap(b + 1) >= stop {
        b += 1;
    }
    let long = pts[b].along - pts[a].along;
    if long < SHORT * spacing {
        return 0.0;
    }
    let pts = &pts[a..=b];
    let half: Vec<f32> = pts.iter().map(|p| 0.5 * width * spacing * (1.0 - taper * (2.0 * (p.along - pts[0].along) / long - 1.0).powi(2))).collect();
    let reach = spacing + 1.0;
    let mut queue = VecDeque::new();
    let mut took = 0usize;
    // how far a point is from the segments by point `k`, and the line's half width there
    let by = |x: [f32; 3], k: usize| {
        let mut best = (far(x, pts[k].at), half[k]);
        for s in k.checked_sub(1).into_iter().chain([k]) {
            if s + 1 < pts.len() {
                let (d, t) = to_segment(x, pts[s].at, pts[s + 1].at);
                if d < best.0 {
                    best = (d, half[s] + t * (half[s + 1] - half[s]));
                }
            }
        }
        best
    };
    // a skin cell takes the line where its point of the surface lies nearer it than any line
    let mut take = |now: &mut State, queue: &mut VecDeque<(u32, u32)>, c: usize, k: usize| {
        let d = by(level.surface[skin.cells[c] as usize], k).0;
        if d < reach && d < now.v[c] {
            now.v[c] = d;
            for &i in skin.spot(c) {
                let (d, h) = by(level.surface[i as usize], k);
                now.u[i as usize] = h - d;
            }
            queue.push_back((c as u32, k as u32));
            took += 1;
        }
    };
    for (k, p) in pts.iter().enumerate() {
        take(now, &mut queue, skin.at(p.cell), k);
    }
    // and spreads to their neighbours from the point it came by
    while let Some((c, k)) = queue.pop_front() {
        let (c, k) = (c as usize, k as usize);
        for &j in &skin.next[skin.start[c] as usize..skin.start[c + 1] as usize] {
            take(now, &mut queue, j as usize, k);
        }
    }
    let lines = &mut now.crowd.lines;
    lines.starts.extend(line.sides.iter().filter(|(k, _)| (a..=b).contains(k)).map(|(_, s)| *s));
    // on from an end it was not cut back from
    lines.starts.extend([(a == 0).then_some(line.ends[0]).flatten(), (b + 1 == line.points.len()).then_some(line.ends[1]).flatten()].into_iter().flatten());
    lines.drawn += 1;
    took as f64
}

/// One step of `spec` on `now`, in place: the next [`SEEDS`] starts a gap from every line traced
/// at once, then drawn in their order; again till one is drawn. Returns the cells the lines
/// drawn took (0 once no start is left).
pub fn step(spec: &Spec, level: &Level, now: &mut State, fan: Fan) -> f64 {
    let Model::Flow { length, width, taper, .. } = spec.model else { return 0.0 };
    let spacing = spec.preset.cells as f32;
    let long = (length * spec.preset.cells) as f32;
    let skin = now.crowd.skin.clone();
    let flow = now.crowd.lines.flow.clone();
    loop {
        let mut starts = Vec::with_capacity(SEEDS);
        while starts.len() < SEEDS {
            let Some(s) = now.crowd.lines.next(level) else { break };
            if now.v[skin.at(s.cell)] >= GAP * spacing {
                starts.push(s);
            }
        }
        if starts.is_empty() {
            return 0.0;
        }
        let traced: Vec<OnceLock<Line>> = (0..starts.len()).map(|_| OnceLock::new()).collect();
        let gap = &now.v[..];
        fan(starts.len(), &|k| {
            let _ = traced[k].set(trace(level, &skin, &flow, gap, starts[k], spacing, long));
        });
        let mut took = 0.0;
        for line in traced.into_iter().filter_map(OnceLock::into_inner) {
            took += draw(level, &skin, now, &line, spacing, width as f32, taper as f32);
        }
        if took > 0.0 {
            return took;
        }
    }
}

/// Every band cell's place (0..1, the second material from a half): how far inside its spot's
/// nearest line, a cell of it from a half to all.
pub fn places(state: &State) -> Vec<f32> {
    state.u.iter().map(|u| (0.5 + 0.5 * u).clamp(0.0, 1.0)).collect()
}
