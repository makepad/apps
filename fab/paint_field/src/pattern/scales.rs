//! Scales over a band's cells: the skin tiled by the seed points nearest it (a Voronoi tiling
//! over the surface: each band cell's nearest point of it, [`Level::surface`], measured to the
//! seeds'), grown out from the seeds a cell a step until the scales meet.
//!
//! The seeds: `@spiral` sets them as a sunflower sets its florets (phyllotaxis) round the
//! part's upright axis: each the golden angle round from the one before, at the height below
//! which its share of the part's outer surface lies, on the outermost cell that way. `@even`
//! throws them at random over the skin, none nearer another than [`APART`] scales. After the
//! spiral, darts fill what it leaves bare (a hollow part's inside, a lid).
//!
//! A scale's outline (the second material) runs where it meets the next, `edge` times the
//! scale wide, and `fill` of the scales are the second material through, spread out: none
//! touches another while there are scales enough apart (a share of random scales would fall
//! in streaks). A cell no scale has reached yet is the second material: the scales grow as
//! discs till they meet.
//!
//! The state: `u` each band cell's nearest seed and `v` its next nearest (-1 for none yet),
//! and the seeds' points (lattice cells).

use super::band::Level;
use super::dla::Walk;
use super::skin::Crowd;
use super::rd::State;
use super::{mix, pack, unit, CellMap, Model, Spec};
use crate::bake::Fan;
use std::sync::Mutex;

/// The golden angle (radians): each spiral seed this far round from the one before.
const GOLDEN: f64 = 2.399_963_229_728_653;
/// How near (as a share of the scale) a dart may land to a seed and still seed a scale.
pub const APART: f64 = 0.9;
/// How near (as a share of the scale) two seeds lie when their scales touch, at a side or a
/// corner (a corner's neighbour lies about √2 scales off on a spiral's four-sided scales).
const TOUCH: f64 = 1.6;
/// How far either side of an outline's edge the place goes from a half to 1 or 0 (cells).
const EDGE: f32 = 0.5;
/// Cells a worker steps at a time.
const CHUNK: usize = 8192;

/// The starting state on `level`: the seeds, each its own scale's first cell.
pub fn seed(spec: &Spec, level: &Level) -> State {
    let n = level.band;
    let Model::Scales { spiral, .. } = spec.model else { return State { u: vec![-1.0; n], v: vec![-1.0; n], walk: Walk::default(), seeds: Vec::new(), crowd: Crowd::default() } };
    let scale = spec.scale / level.pitch;
    let mut seeds = if spiral { spiral_seeds(spec.seed, level, scale) } else { Vec::new() };
    darts(spec.seed, level, &mut seeds, APART * scale);
    let mut u = vec![-1.0; n];
    for (k, &i) in seeds.iter().enumerate() {
        u[i] = k as f32;
    }
    State { u, v: vec![-1.0; n], walk: Walk::default(), seeds: seeds.iter().map(|&i| level.surface[i]).collect(), crowd: Crowd::default() }
}

/// A sunflower's seeds round the band's upright axis (`y`, through the middle of its box),
/// `scale` cells apart: the `k`th of `count` the golden angle round from the one before (the
/// first turned by the seed), in the layer below which `(k + ½)/count` of the outer surface
/// lies, on that layer's outermost cell on the ray that way. The outer surface below a layer: the rings
/// its outermost cells make, each its slant to the next high.
fn spiral_seeds(seed: u32, level: &Level, scale: f64) -> Vec<usize> {
    let cells = &level.cells[..level.band];
    let Some(low) = cells.iter().map(|c| c[1]).min() else { return Vec::new() };
    let high = cells.iter().map(|c| c[1]).max().unwrap_or(low);
    let (mut lo, mut hi) = ([i32::MAX; 2], [i32::MIN; 2]);
    for c in cells {
        for (k, a) in [0, 2].into_iter().enumerate() {
            lo[k] = lo[k].min(c[a]);
            hi[k] = hi[k].max(c[a]);
        }
    }
    let axis = [0.5 * (lo[0] + hi[0]) as f64, 0.5 * (lo[1] + hi[1]) as f64];
    let layers = (high - low + 1) as usize;
    let mut by_layer: Vec<Vec<u32>> = vec![Vec::new(); layers];
    let mut reach = vec![0f64; layers];
    for (i, c) in cells.iter().enumerate() {
        let l = (c[1] - low) as usize;
        by_layer[l].push(i as u32);
        reach[l] = reach[l].max((c[0] as f64 - axis[0]).hypot(c[2] as f64 - axis[1]));
    }
    let mut below = vec![0f64; layers + 1];
    for l in 0..layers {
        let dr = reach[(l + 1).min(layers - 1)] - reach[l];
        below[l + 1] = below[l] + std::f64::consts::TAU * reach[l] * (1.0 + dr * dr).sqrt();
    }
    let total = below[layers];
    // hexagonally packed, a seed to each (√3/2)·scale² of it
    let count = (total / (0.75f64.sqrt() * scale * scale)).round() as usize;
    let turn = std::f64::consts::TAU * unit(seed, 1, 0);
    let mut seeds = Vec::with_capacity(count);
    for k in 0..count {
        let want = (k as f64 + 0.5) / count as f64 * total;
        let l = below.partition_point(|a| *a <= want).saturating_sub(1).min(layers - 1);
        let (s, c) = (turn + k as f64 * GOLDEN).sin_cos();
        // out along the ray that way; off it, four times as far back (a ring's outermost cells
        // lie within a cell of its edge over a wide arc)
        let out = |i: u32| {
            let (x, z) = (cells[i as usize][0] as f64 - axis[0], cells[i as usize][2] as f64 - axis[1]);
            x * c + z * s - 4.0 * (z * c - x * s).abs()
        };
        if let Some(i) = by_layer[l].iter().copied().max_by(|a, b| out(*a).total_cmp(&out(*b)).then(b.cmp(a))) {
            seeds.push(i as usize);
        }
    }
    seeds
}

/// Darts thrown at the band's cells in an order the seed sets: each lands as a seed where no
/// seed lies within `apart` cells of its point of the surface.
fn darts(seed: u32, level: &Level, seeds: &mut Vec<usize>, apart: f64) {
    let mut taken = Apart::new(apart);
    for &i in seeds.iter() {
        taken.add(level.surface[i]);
    }
    let mut order: Vec<(u64, u32)> = (0..level.band).map(|i| (mix(mix(seed as u64 ^ 0xda27) ^ pack(level.cells[i])), i as u32)).collect();
    order.sort_unstable();
    for (_, i) in order {
        let p = level.surface[i as usize];
        if !taken.near(p) {
            taken.add(p);
            seeds.push(i as usize);
        }
    }
}

/// Points kept apart: a grid of cells `apart` wide, each listing the points in it.
struct Apart {
    apart: f64,
    grid: CellMap<Vec<[f32; 3]>>,
}

impl Apart {
    fn new(apart: f64) -> Apart {
        Apart { apart, grid: CellMap::default() }
    }

    fn at(&self, p: [f32; 3]) -> [i32; 3] {
        p.map(|v| (v as f64 / self.apart).floor() as i32)
    }

    fn add(&mut self, p: [f32; 3]) {
        let b = self.at(p);
        self.grid.entry(pack(b)).or_default().push(p);
    }

    /// Whether a point kept lies within `apart` of `p`.
    fn near(&self, p: [f32; 3]) -> bool {
        let (b, near) = (self.at(p), (self.apart * self.apart) as f32);
        (0..27).any(|o| {
            let key = pack([b[0] + o % 3 - 1, b[1] + (o / 3) % 3 - 1, b[2] + o / 9 - 1]);
            self.grid.get(&key).is_some_and(|list| list.iter().any(|q| apart2(p, *q) < near))
        })
    }
}

/// The squared distance between two points.
fn apart2(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|k| (a[k] - b[k]) * (a[k] - b[k])).sum()
}

/// One step from `from` into `to`: each band cell takes the two nearest seeds of its own and its
/// neighbours' (the same whatever the workers split it into). Returns how many cells changed.
pub fn step(level: &Level, from: &State, to: &mut State, fan: Fan) -> f64 {
    let chunks: Vec<Mutex<(&mut [f32], &mut [f32], f64)>> = to.u.chunks_mut(CHUNK).zip(to.v.chunks_mut(CHUNK)).map(|(u, v)| Mutex::new((u, v, 0.0))).collect();
    let (pu, pv, nb, seeds) = (&from.u[..], &from.v[..], &level.nb[..], &from.seeds[..]);
    fan(chunks.len(), &|c| {
        let mut chunk = chunks[c].lock().unwrap_or_else(|e| e.into_inner());
        let (u2, v2, moved) = &mut *chunk;
        let base = c * CHUNK;
        let mut changed = 0usize;
        for o in 0..u2.len() {
            let i = base + o;
            let p = level.surface[i];
            // the nearest two, ties to the lower seed
            let (mut best, mut next) = ((f32::INFINITY, -1.0f32), (f32::INFINITY, -1.0f32));
            let mut offer = |s: f32| {
                if s < 0.0 || s == best.1 || s == next.1 {
                    return;
                }
                let d = apart2(p, seeds[s as usize]);
                if d < best.0 || (d == best.0 && s < best.1) {
                    next = best;
                    best = (d, s);
                } else if d < next.0 || (d == next.0 && s < next.1) {
                    next = (d, s);
                }
            };
            offer(pu[i]);
            offer(pv[i]);
            for &j in &nb[i] {
                offer(pu[j as usize]);
                offer(pv[j as usize]);
            }
            u2[o] = best.1;
            v2[o] = next.1;
            changed += (best.1 != pu[i] || next.1 != pv[i]) as usize;
        }
        *moved = changed as f64;
    });
    chunks.into_iter().map(|c| c.into_inner().unwrap_or_else(|e| e.into_inner()).2).sum()
}

/// Which scales are the second material through: `fill` of them, `scale` cells across, taken in
/// an order the seed sets, first each whose scale touches none taken yet ([`TOUCH`]), then, once
/// none is left apart, the rest in the same order.
pub fn filled(seed: u32, fill: f64, seeds: &[[f32; 3]], scale: f64) -> Vec<bool> {
    let want = (fill * seeds.len() as f64).round() as usize;
    let mut taken = vec![false; seeds.len()];
    let mut order: Vec<(u64, usize)> = (0..seeds.len()).map(|k| (mix(mix(seed as u64 ^ 0xf111) ^ k as u64), k)).collect();
    order.sort_unstable();
    let mut apart = Apart::new(TOUCH * scale);
    let mut count = 0;
    for &(_, k) in &order {
        if count < want && !apart.near(seeds[k]) {
            apart.add(seeds[k]);
            taken[k] = true;
            count += 1;
        }
    }
    for &(_, k) in &order {
        if count < want && !taken[k] {
            taken[k] = true;
            count += 1;
        }
    }
    taken
}

/// Every band cell's place (0..1, the second material from a half): 1 off every scale yet and
/// through a [`filled`] one; on the others a half `edge` times the scale either side of where its
/// scale meets the next (the distance to the plane halfway between their seeds), 1 inside that
/// and 0 [`EDGE`] cells past it.
pub fn places(spec: &Spec, level: &Level, state: &State) -> Vec<f32> {
    let Model::Scales { edge, fill, .. } = spec.model else { return vec![0.0; level.band] };
    let half = (0.5 * edge * spec.scale / level.pitch) as f32;
    let through = filled(spec.seed, fill, &state.seeds, spec.scale / level.pitch);
    (0..level.band)
        .map(|i| {
            let (a, b) = (state.u[i], state.v[i]);
            if a < 0.0 || through[a as usize] {
                return 1.0;
            }
            if b < 0.0 {
                return 0.0;
            }
            let (p, sa, sb) = (level.surface[i], state.seeds[a as usize], state.seeds[b as usize]);
            let between = apart2(sa, sb).sqrt().max(1e-6);
            let off = (apart2(p, sb) - apart2(p, sa)) / (2.0 * between);
            (0.5 + 0.5 * (half - off) / EDGE).clamp(0.0, 1.0)
        })
        .collect()
}
