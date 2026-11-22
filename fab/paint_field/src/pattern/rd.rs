//! The reaction-diffusion models, stepped over a band's cells: explicit steps of the two
//! species, each cell from its own and its six neighbours' last values (so the result is the
//! same whatever the workers split it into), no flux past the band's edge.
//!
//! - Gray-Scott: `u' = Du∇²u - uv² + F(1 - u)`, `v' = Dv∇²v + uv² - (F + k)v`. It rests at
//!   `u = 1, v = 0`; it is seeded with discs of `v` about two wavelengths apart over the
//!   surface, which split and spread into the preset's pattern.
//! - Activator and inhibitor (Gierer-Meinhardt with saturation):
//!   `a' = Da∇²a + a²/((1 + κa²)h) - a`, `h' = Dh∇²h + a² - μh`. Its even state is unstable to
//!   a wavelength set by the two spreads, so a little noise grows into spots.
//!
//! A Lichtenberg figure (aggregation) is grown by [`super::dla`], scales by [`super::scales`],
//! a slime mould's network by [`super::physarum`] and flow lines by [`super::flow`]; their
//! steps, refinement and places go through here too.

use super::band::Level;
use super::dla::{self, Walk};
use super::flow;
use super::physarum;
use super::scales;
use super::skin::Crowd;
use super::{Model, Spec};
use crate::bake::Fan;
use std::sync::Mutex;

/// The inhibitor's decay rate against the activator's (over 1, so the even state is stable
/// without spreading; with the inhibitor spreading 20 times faster it is unstable to one
/// wavelength, 9.2 activator spreading lengths).
const MU: f64 = 1.5;

/// Two species per band cell (aggregation: the distance from the tree, and the tree; scales:
/// the nearest seed and the next; per skin cell, a slime mould's trail and where its agents
/// stand; flow lines: per band cell how far inside a line, per skin cell how far from one), a
/// Lichtenberg figure's walk, the scales' seed points and what goes over the skin, with the skin
/// (lattice cells; each empty for the others).
#[derive(Clone)]
pub struct State {
    pub u: Vec<f32>,
    pub v: Vec<f32>,
    pub walk: Walk,
    pub seeds: Vec<[f32; 3]>,
    pub crowd: Crowd,
}

/// One level's stepping: the model's rates, the spreads in that lattice's cells and the step.
#[derive(Clone, Copy, Debug)]
pub struct Rates {
    pub model: Model,
    pub du: f32,
    pub dv: f32,
    pub dt: f32,
}

impl Rates {
    /// `spec` stepped on a lattice `coarser` times the fine one's pitch: the spreads in its
    /// cells shrink by its square, and a step the spreads limit (the activator's) grows by it.
    pub fn of(spec: &Spec, coarser: f64) -> Rates {
        let p = spec.preset;
        let du = p.du / (coarser * coarser);
        let dt = match spec.model {
            Model::GrayScott { .. } | Model::Aggregation { .. } | Model::Scales { .. } | Model::Physarum { .. } | Model::Flow { .. } => p.dt,
            Model::Activator { .. } => p.dt * coarser * coarser,
        };
        Rates { model: spec.model, du: du as f32, dv: (du * p.ratio) as f32, dt: dt as f32 }
    }
}

/// splitmix64.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// A number in 0..1 from the seed and a cell.
fn noise(seed: u32, salt: u64, c: [i32; 3]) -> f64 {
    let h = mix(mix(seed as u64 ^ (salt << 32)) ^ super::pack(c));
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Where the activator model's even state sits: `a = μ/(1 + κa²)` (Newton), `h = a²/μ`.
fn activator_rest(saturation: f64) -> (f64, f64) {
    let mut a = MU;
    for _ in 0..32 {
        let f = saturation * a * a * a + a - MU;
        let df = 3.0 * saturation * a * a + 1.0;
        a -= f / df;
    }
    (a, a * a / MU)
}

/// The resting state of a model.
pub fn rest(model: Model) -> (f32, f32) {
    match model {
        Model::GrayScott { .. } | Model::Aggregation { .. } | Model::Scales { .. } => (1.0, 0.0),
        Model::Physarum { .. } | Model::Flow { .. } => (0.0, 0.0),
        Model::Activator { saturation } => {
            let (a, h) = activator_rest(saturation);
            (a as f32, h as f32)
        }
    }
}

/// The starting state on `level`: Gray-Scott's discs, the activator's noise, both from the
/// seed and the cells alone.
pub fn seed(spec: &Spec, level: &Level, fan: Fan) -> State {
    let n = level.band;
    if let Model::Aggregation { roots, .. } = spec.model {
        return dla::seed(spec, level, &roots_of(spec.seed, roots, &level.cells[..n]));
    }
    if let Model::Scales { .. } = spec.model {
        return scales::seed(spec, level);
    }
    if let Model::Physarum { .. } = spec.model {
        return physarum::seed(spec, level);
    }
    if let Model::Flow { contour, swirl, .. } = spec.model {
        return flow::seed(spec, level, contour, swirl, fan);
    }
    let (ru, rv) = rest(spec.model);
    let (mut u, mut v) = (vec![ru; n], vec![rv; n]);
    match spec.model {
        Model::GrayScott { .. } => {
            // a disc's centre per wavelength² of surface (per wavelength cubed through a band
            // deeper than that)
            let apart = spec.scale;
            let thick = super::band::thickness(level.pitch, spec.depth).min(apart);
            let chance = level.pitch.powi(3) / (apart * apart * thick);
            let r = 0.3 * spec.scale / level.pitch;
            let reach = r.ceil() as i32;
            for i in 0..n {
                let c = level.cells[i];
                if noise(spec.seed, 1, c) >= chance {
                    continue;
                }
                for z in -reach..=reach {
                    for y in -reach..=reach {
                        for x in -reach..=reach {
                            if ((x * x + y * y + z * z) as f64) > r * r {
                                continue;
                            }
                            let Some(j) = level.cell_index([c[0] + x, c[1] + y, c[2] + z]) else { continue };
                            u[j] = 0.5;
                            v[j] = 0.25 + 0.05 * noise(spec.seed, 2, level.cells[j]) as f32;
                        }
                    }
                }
            }
        }
        Model::Activator { .. } => {
            for i in 0..n {
                u[i] = ru * (1.0 + 0.1 * (noise(spec.seed, 3, level.cells[i]) as f32 - 0.5));
            }
        }
        Model::Aggregation { .. } | Model::Scales { .. } | Model::Physarum { .. } | Model::Flow { .. } => {}
    }
    State { u, v, walk: Walk::default(), seeds: Vec::new(), crowd: Crowd::default() }
}

/// A Lichtenberg figure's roots among `cells`: the lowest (of those, the seed's pick), then each
/// next the cell farthest from the roots so far, `count` in all (fewer when cells run out).
pub fn roots_of(seed: u32, count: u32, cells: &[[i32; 3]]) -> Vec<usize> {
    let Some(low) = cells.iter().map(|c| c[1]).min() else { return Vec::new() };
    let pick = |i: usize| noise(seed, 4, cells[i]);
    let Some(first) = (0..cells.len()).filter(|&i| cells[i][1] == low).min_by(|&a, &b| pick(a).total_cmp(&pick(b)).then(a.cmp(&b))) else { return Vec::new() };
    let d2 = |a: [i32; 3], b: [i32; 3]| (0..3).map(|k| ((a[k] - b[k]) as i64).pow(2)).sum::<i64>();
    let mut roots = vec![first];
    let mut far: Vec<i64> = cells.iter().map(|c| d2(*c, cells[first])).collect();
    while roots.len() < count as usize {
        let Some(next) = (0..cells.len()).max_by(|&a, &b| far[a].cmp(&far[b]).then(b.cmp(&a))) else { break };
        if far[next] == 0 {
            break;
        }
        roots.push(next);
        for (f, c) in far.iter_mut().zip(cells) {
            *f = (*f).min(d2(*c, cells[next]));
        }
    }
    roots
}

/// Cells a worker steps at a time.
const CHUNK: usize = 8192;

/// One step (the `step`th) of `spec` over `level`'s band: `now` becomes the next state (stepped
/// into `spare` and swapped, or, for aggregation, in place). Returns how far the species a place
/// reads (`places`) moved, summed over the band's cells (aggregation: the cells that joined the
/// tree; scales: the cells whose nearest seeds changed; a slime mould: its trail; flow lines,
/// in place too: the cells the lines drawn took).
pub fn step(spec: &Spec, rates: &Rates, level: &Level, now: &mut State, spare: &mut State, step: u32, fan: Fan) -> f64 {
    match rates.model {
        Model::Aggregation { .. } => return dla::step(spec, level, now, step),
        Model::Flow { .. } => return flow::step(spec, level, now, fan),
        _ => {}
    }
    let moved = match rates.model {
        Model::Scales { .. } => scales::step(level, now, spare, fan),
        Model::Physarum { .. } => physarum::step(spec, level, now, spare, step, fan),
        _ => step_into(rates, level, now, spare, fan),
    };
    std::mem::swap(now, spare);
    moved
}

/// A reaction-diffusion step from `from` into `to`.
fn step_into(rates: &Rates, level: &Level, from: &State, to: &mut State, fan: Fan) -> f64 {
    let chunks: Vec<Mutex<(&mut [f32], &mut [f32], f64)>> = to.u.chunks_mut(CHUNK).zip(to.v.chunks_mut(CHUNK)).map(|(u, v)| Mutex::new((u, v, 0.0))).collect();
    let (pu, pv, nb) = (&from.u[..], &from.v[..], &level.nb[..]);
    let r = *rates;
    fan(chunks.len(), &|c| {
        let mut chunk = chunks[c].lock().unwrap_or_else(|e| e.into_inner());
        let (u2, v2, moved) = &mut *chunk;
        let base = c * CHUNK;
        let mut sum = 0f32;
        match r.model {
            Model::GrayScott { feed, kill } => {
                let (f, k) = (feed as f32, kill as f32);
                for o in 0..u2.len() {
                    let i = base + o;
                    let n = &nb[i];
                    let (u, v) = (pu[i], pv[i]);
                    let lu = pu[n[0] as usize] + pu[n[1] as usize] + pu[n[2] as usize] + pu[n[3] as usize] + pu[n[4] as usize] + pu[n[5] as usize] - 6.0 * u;
                    let lv = pv[n[0] as usize] + pv[n[1] as usize] + pv[n[2] as usize] + pv[n[3] as usize] + pv[n[4] as usize] + pv[n[5] as usize] - 6.0 * v;
                    let uvv = u * v * v;
                    u2[o] = u + r.dt * (r.du * lu - uvv + f * (1.0 - u));
                    v2[o] = v + r.dt * (r.dv * lv + uvv - (f + k) * v);
                    sum += (v2[o] - v).abs();
                }
            }
            Model::Activator { saturation } => {
                let (s, mu) = (saturation as f32, MU as f32);
                for o in 0..u2.len() {
                    let i = base + o;
                    let n = &nb[i];
                    let (a, h) = (pu[i], pv[i]);
                    let la = pu[n[0] as usize] + pu[n[1] as usize] + pu[n[2] as usize] + pu[n[3] as usize] + pu[n[4] as usize] + pu[n[5] as usize] - 6.0 * a;
                    let lh = pv[n[0] as usize] + pv[n[1] as usize] + pv[n[2] as usize] + pv[n[3] as usize] + pv[n[4] as usize] + pv[n[5] as usize] - 6.0 * h;
                    let aa = a * a;
                    let made = aa / ((1.0 + s * aa) * h.max(1e-3));
                    u2[o] = (a + r.dt * (r.du * la + made - a)).max(0.0);
                    v2[o] = (h + r.dt * (r.dv * lh + aa - mu * h)).max(1e-4);
                    sum += (u2[o] - a).abs();
                }
            }
            Model::Aggregation { .. } | Model::Scales { .. } | Model::Physarum { .. } | Model::Flow { .. } => {}
        }
        *moved = sum as f64;
    });
    chunks.into_iter().map(|c| c.into_inner().unwrap_or_else(|e| e.into_inner()).2).sum()
}

/// `state` on `coarse` carried onto `fine` (half its pitch): each fine cell trilinear from the
/// coarse band cells round it, the resting state where there are none.
pub fn refine(spec: &Spec, coarse: &Level, state: &State, fine: &Level, fan: Fan) -> State {
    if let Model::Aggregation { .. } | Model::Scales { .. } | Model::Physarum { .. } | Model::Flow { .. } = spec.model {
        // a figure, scales, a slime mould or flow lines take no steps on the coarse lattice
        // (the first state shows their roots, seeds or agents there): they start again on the
        // fine one
        return seed(spec, fine, fan);
    }
    let (ru, rv) = rest(spec.model);
    let ratio = coarse.pitch / fine.pitch;
    let mut u = Vec::with_capacity(fine.band);
    let mut v = Vec::with_capacity(fine.band);
    for c in &fine.cells[..fine.band] {
        let q = c.map(|x| x as f64 / ratio);
        u.push(coarse.between(&state.u, q).unwrap_or(ru));
        v.push(coarse.between(&state.v, q).unwrap_or(rv));
    }
    State { u, v, walk: Walk::default(), seeds: Vec::new(), crowd: Crowd::default() }
}

/// Every cell's place in the mix (band, then skin): the pattern's species between the preset's
/// levels, 0..1. Aggregation: the tree's branches, each as thick as the cells past it
/// ([`dla::places`]); scales: their outlines ([`scales::places`]); a slime mould: its veins
/// ([`physarum::places`]); flow lines: the lines ([`flow::places`]).
pub fn places(spec: &Spec, level: &Level, state: &State, fan: Fan) -> Vec<f32> {
    let (lo, hi) = (spec.preset.lo as f32, spec.preset.hi as f32);
    let band: Vec<f32> = match spec.model {
        Model::GrayScott { .. } => state.v.iter().map(|x| ((x - lo) / (hi - lo)).clamp(0.0, 1.0)).collect(),
        Model::Activator { .. } => state.u.iter().map(|x| ((x - lo) / (hi - lo)).clamp(0.0, 1.0)).collect(),
        Model::Aggregation { .. } => dla::places(level, state),
        Model::Scales { .. } => scales::places(spec, level, state),
        Model::Physarum { .. } => physarum::places(level, state, fan),
        Model::Flow { .. } => flow::places(state),
    };
    level.with_skin(band)
}
