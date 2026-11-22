//! A band's skin as a surface to go over: its cells (the band cells within half a cell of the
//! surface, one to a spot of it), a graph of neighbours over them, the way out of the surface at
//! each, smoothed so it turns round an edge over a cell or two, and a crawl over it: strides along
//! the surface, each into the skin cell nearest where it lands, onto that cell's tangent plane,
//! its way turned as the surface turns (so round an edge it keeps its angle to the edge and its
//! length). A slime mould's agents ([`super::physarum`]) and a flow's lines ([`super::flow`])
//! go over it.

use super::band::Level;
use super::flow::Lines;
use super::physarum::{Agent, Ground};
use std::sync::Arc;

/// How far apart (cells) the points of the surface of two neighbours of the skin may be.
const REACH: f32 = 1.5;
/// Two cells of the skin whose surface turns further apart than this (the cosine) lie either
/// side of a wall, and are not neighbours.
const ACROSS: f32 = -0.25;
/// How far (cells) a stride may land from the nearest point of the surface it finds and stay on
/// the band.
const STEP_OFF: f32 = 1.2;
/// The longest stride of a crawl (cells).
const STRIDE: f32 = 2.5;

/// A band's skin: its cells, each band cell's, each skin cell's neighbours (of its 26 lattice
/// neighbours the skin's, their points of the surface within [`REACH`] and not across a wall)
/// and the way out of the surface a crawl goes by there (the mean of its own and its
/// neighbours', so round an edge it turns over a cell or two and a crawl round it stays on the
/// surface).
#[derive(Default)]
pub struct Skin {
    /// The skin's band cells, in order.
    pub(crate) cells: Vec<u32>,
    /// Per band cell whether it is the skin's.
    on: Vec<bool>,
    /// Per band cell its spot's skin cell (an index of `cells`; `u32::MAX` where it has none).
    pub(crate) of: Vec<u32>,
    /// Per skin cell where the band cells of its spot start in `spot` (and after the last, the
    /// end), and those band cells.
    spot_start: Vec<u32>,
    spot: Vec<u32>,
    /// Per skin cell where its neighbours start in `next` (and after the last, the end).
    pub(crate) start: Vec<u32>,
    /// The neighbours (indices of `cells`).
    pub(crate) next: Vec<u32>,
    /// Per skin cell the way out of the surface (a unit vector).
    normal: Vec<[f32; 3]>,
}

impl Skin {
    pub(crate) fn new(level: &Level) -> Skin {
        let on: Vec<bool> = (0..level.band).map(|i| on_skin(level, i)).collect();
        let cells: Vec<u32> = (0..level.band as u32).filter(|&i| on[i as usize]).collect();
        let mut of = vec![u32::MAX; level.band];
        for (k, &i) in cells.iter().enumerate() {
            of[i as usize] = k as u32;
        }
        for i in 0..level.band {
            if of[i] == u32::MAX {
                if let Some(j) = nearest(level, &on, i, level.surface[i]) {
                    of[i] = of[j];
                }
            }
        }
        let mut spot_start = vec![0u32; cells.len() + 1];
        for &k in of.iter().filter(|&&k| k != u32::MAX) {
            spot_start[k as usize + 1] += 1;
        }
        for k in 0..cells.len() {
            spot_start[k + 1] += spot_start[k];
        }
        let mut spot = vec![0u32; spot_start[cells.len()] as usize];
        let mut fill = spot_start.clone();
        for (i, &k) in of.iter().enumerate().filter(|(_, &k)| k != u32::MAX) {
            spot[fill[k as usize] as usize] = i as u32;
            fill[k as usize] += 1;
        }
        let mut start = Vec::with_capacity(cells.len() + 1);
        let mut next = Vec::new();
        for &i in &cells {
            start.push(next.len() as u32);
            let (c, s, m) = (level.cells[i as usize], level.surface[i as usize], level.normal[i as usize]);
            for d in 0..27 {
                let d = [d % 3 - 1, d / 3 % 3 - 1, d / 9 - 1];
                if d == [0, 0, 0] {
                    continue;
                }
                let Some(j) = level.cell_index([c[0] + d[0], c[1] + d[1], c[2] + d[2]]).filter(|&j| j < level.band && on[j]) else { continue };
                let t = level.surface[j];
                let far = (0..3).map(|k| (t[k] - s[k]) * (t[k] - s[k])).sum::<f32>();
                if far <= REACH * REACH && dot(m, level.normal[j]) > ACROSS {
                    next.push(of[j]);
                }
            }
        }
        start.push(next.len() as u32);
        let normal = (0..cells.len())
            .map(|k| {
                let own = level.normal[cells[k] as usize];
                let near = next[start[k] as usize..start[k + 1] as usize].iter().fold(own, |m, &j| add(m, level.normal[cells[j as usize] as usize], 1.0));
                unit(near).unwrap_or(own)
            })
            .collect();
        Skin { cells, on, of, spot_start, spot, start, next, normal }
    }

    /// The way out of the surface at skin cell `i` (a band cell).
    pub(crate) fn up(&self, i: usize) -> [f32; 3] {
        self.normal[self.of[i] as usize]
    }

    /// The band cells of skin cell `k`'s spot (`k` an index of `cells`).
    pub(crate) fn spot(&self, k: usize) -> &[u32] {
        &self.spot[self.spot_start[k] as usize..self.spot_start[k + 1] as usize]
    }

    /// Skin cell `i`'s (a band cell's) index of `cells`.
    pub(crate) fn at(&self, i: u32) -> usize {
        self.of[i as usize] as usize
    }
}

/// What goes over the skin: a slime mould's agents or a flow's lines, and the skin (none for
/// the other families).
#[derive(Clone, Default)]
pub struct Crowd {
    pub skin: Arc<Skin>,
    pub agents: Vec<Agent>,
    pub lines: Lines,
    /// What a slime mould's agents find on their part.
    pub ground: Arc<Ground>,
    /// Per skin cell whether a slime mould's agent has crawled into it (and so woken the agents
    /// that start there and round it).
    pub trodden: Vec<bool>,
}

pub(crate) fn add(a: [f32; 3], b: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
}

pub(crate) fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub(crate) fn unit(a: [f32; 3]) -> Option<[f32; 3]> {
    let l = dot(a, a).sqrt();
    (l > 1e-6).then(|| a.map(|v| v / l))
}

/// Whether band cell `i` is the skin's: within half a cell of its point of the surface.
fn on_skin(level: &Level, i: usize) -> bool {
    let (c, s) = (level.cells[i], level.surface[i]);
    (0..3).map(|k| (c[k] as f32 - s[k]) * (c[k] as f32 - s[k])).sum::<f32>() <= 0.25
}

/// From band cell `i`, the skin cell (`on`) whose point of the surface is nearest `p`: from
/// neighbour to nearer neighbour while one is, the nearest skin cell of those it passes; if
/// within [`STEP_OFF`] of it.
fn nearest(level: &Level, on: &[bool], i: usize, p: [f32; 3]) -> Option<usize> {
    let far = |j: usize| {
        let s = level.surface[j];
        (0..3).map(|k| (s[k] - p[k]) * (s[k] - p[k])).sum::<f32>()
    };
    let before = |a: (f32, usize), b: (f32, usize)| a.0 < b.0 || (a.0 == b.0 && a.1 < b.1);
    let mut walk = (far(i), i);
    let mut best = on[i].then_some(walk);
    loop {
        let from = walk.1;
        for &j in &level.nb[from] {
            let next = (far(j as usize), j as usize);
            if before(next, walk) {
                walk = next;
            }
            if on[next.1] && best.is_none_or(|b| before(next, b)) {
                best = Some(next);
            }
        }
        if walk.1 == from {
            break;
        }
    }
    best.filter(|b| b.0 <= STEP_OFF * STEP_OFF).map(|b| b.1)
}

/// `way` along the surface where its normal is `from`, carried to where it is `to`: turned as the
/// one turns into the other (so a way over an edge keeps its angle to the edge), then laid into
/// the plane there.
fn carry(way: [f32; 3], from: [f32; 3], to: [f32; 3]) -> [f32; 3] {
    let (k, c) = (cross(from, to), dot(from, to));
    let s2 = dot(k, k);
    let turned = if s2 > 1e-12 { add(add(way.map(|v| v * c), cross(k, way), 1.0), k, dot(k, way) * (1.0 - c) / s2) } else { way };
    unit(add(turned, to, -dot(turned, to))).unwrap_or(way)
}

/// From `p` on the surface in skin cell `i`, `way` along it: `by` cells over the surface, in
/// strides of at most [`STRIDE`] (halved down to a cell while one lands off the surface, as past
/// an edge), each onto the tangent plane of the skin cell it lands in and on for what that took
/// off it (round an edge, where a stride cuts the corner); the point, the way carried into that
/// plane and the cell, none off the band.
pub(crate) fn crawl(level: &Level, skin: &Skin, i: usize, p: [f32; 3], way: [f32; 3], by: f32) -> Option<([f32; 3], [f32; 3], usize)> {
    let (mut p, mut way, mut i, mut left) = (p, way, i, by);
    // a stride round an edge takes off at most about a third of itself
    for _ in 0..2 * by.ceil() as u32 + 2 {
        if left < 0.05 {
            break;
        }
        let mut stride = left.min(STRIDE);
        let (to, j) = loop {
            let to = add(p, way, stride);
            match nearest(level, &skin.on, i, to) {
                Some(j) => break (to, j),
                None if stride > 1.0 => stride = (stride * 0.5).max(1.0),
                None => return None,
            }
        };
        let (s, m) = (level.surface[j], skin.up(j));
        let q = add(to, m, -dot([to[0] - s[0], to[1] - s[1], to[2] - s[2]], m));
        let d = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
        left -= dot(d, d).sqrt();
        p = q;
        way = carry(way, skin.up(i), m);
        i = j;
    }
    Some((p, way, i))
}

/// Band cell `i`'s own way along the surface (the one [`along`] turns from).
fn own_way(skin: &Skin, i: usize) -> Option<[f32; 3]> {
    let n = skin.up(i);
    let other = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    unit(cross(n, other))
}

/// A heading along the surface at band cell `i`, turned by `turn` (radians) from a way of its own.
pub(crate) fn along(skin: &Skin, i: usize, turn: f64) -> [f32; 3] {
    let Some(t) = own_way(skin, i) else { return [1.0, 0.0, 0.0] };
    let (s, c) = (turn as f32).sin_cos();
    add(t.map(|v| v * c), cross(skin.up(i), t), s)
}

/// How far (radians) `way` along the surface at band cell `i` is turned from its own way: the
/// `turn` [`along`] takes to it.
pub(crate) fn turned(skin: &Skin, i: usize, way: [f32; 3]) -> f64 {
    let Some(t) = own_way(skin, i) else { return 0.0 };
    (dot(way, cross(skin.up(i), t)) as f64).atan2(dot(way, t) as f64)
}
