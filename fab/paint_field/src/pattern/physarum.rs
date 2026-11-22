//! A slime mould over a band's skin (Physarum): it spreads from an inoculum, its agents crawling
//! out to its front and home again over the skin, each laying a trail and steering toward the
//! strongest trail ahead of it while the trail spreads and fades, so the traffic gathers onto
//! veins: trunks near the inoculum that branch into threads, and lace at the front.
//!
//! The trail lives on the skin's cells (the band cells within half a cell of the surface, one to
//! a spot of it), and spreads among each cell's neighbours of the skin, so it reads the same over
//! the skin however deep the band under it. The inoculum is the skin cell nearest the top of the
//! middle of the skin's box; the front spreads from it over the skin (by the way over the skin's
//! neighbours) at an even pace, `spread` times the way to the farthest cell by the last step. The
//! agents (a share [`POPULATION`] of the skin's cells, scattered by the seed) sleep where they lie,
//! those in the hub round the inoculum ([`HUB`]) awake from the start; an agent wakes when another
//! crawls into its cell or a neighbour of it, so the mould spreads along the ways its agents go and
//! thins out into their tips at its front. An agent is a point on the surface, a heading along it
//! (lattice cells), the skin cell it stands in, whether it heads home and whether it is awake. Each
//! step an awake agent reads the trail [`SENSE`] scales ahead over the surface from its cell, along
//! the nearest of [`WAYS`] headings round the cell and turned `sense` (to the nearest heading)
//! either way, by [`Ground`]'s table; each read leans up the way from the inoculum heading out and
//! down it heading home ([`BIAS`]). It turns `turn` toward the strongest (on when straight on is,
//! either way at random when both sides beat it) and crawls a cell on. In the fan behind the front
//! ([`FAN`]) it turns home the likelier the nearer the front ([`BACK`]); in the hub, or now and
//! then on the way home ([`RETURN`]), it turns out again; each turn is back the way it came, so the
//! traffic runs to and fro along its veins, not round the colony. Heading out it spreads as it
//! goes: up the way with a jitter ([`JITTER`]), the trail aside, laying [`OUT`] of its trail; home
//! and roaming it follows the trail, so the veins are the traffic home and they gather into trunks
//! toward the inoculum. One agent in [`ROAM`] only ever wanders. A crawl goes over the surface in
//! strides, each into the skin cell whose point of the surface is nearest, onto that cell's tangent
//! plane, its way turned as the surface turns (so round an edge it keeps its angle to the edge and
//! its length). A crawl past the front or off the band is not taken: the agent turns back the way
//! it came (home, in the fan), so none walks along the front. A crawl taken lays [`DEPOSIT`] where
//! it lands, less the further into the fan (to [`TIP`] of it at the front), so the fan's tips fade
//! out into fine threads. Then the trail spreads and fades by [`DECAY`]. The agents aim at once
//! from the trail as it was and step in their order, so a state is the same whatever the workers.
//!
//! The state, per skin cell (in [`Skin`]'s order): `u` the trail and `v` its way over the skin
//! from the inoculum (cells); and the agents.

use super::band::Level;
use super::dla::Walk;
use super::flow::Lines;
use super::rd::State;
use super::skin::{add, along, crawl, cross, dot, turned, unit, Crowd, Skin};
use super::{mix, pack, Model, Spec};
use crate::bake::Fan;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// How far ahead an agent reads the trail (a share of the scale).
const SENSE: f64 = 0.4;
/// The trail an agent lays a step.
const DEPOSIT: f32 = 1.0;
/// The share of it an agent lays at the front, the fan's tips fading out.
const TIP: f32 = 0.2;
/// The share of the trail that fades a step.
const DECAY: f32 = 0.1;
/// How much of the difference to each neighbour of the skin a cell's trail takes a step (on a
/// flat face the spread of a mean of a cell and its six lattice neighbours).
const SPREAD: f32 = 1.0 / 60.0;
/// Agents to a cell of the skin.
const POPULATION: f64 = 0.3;
/// How strongly an agent heading out or home leans up or down the way from the inoculum.
const BIAS: f32 = 0.5;
/// The trail every spot reads as besides its own, so a lean shows where there is none.
const EVEN: f32 = 0.2;
/// How much an agent heading out wavers: the most a heading's random share adds to its lean.
const JITTER: f32 = 0.4;
/// The share of the trail an agent heading out lays: the veins are the traffic home.
const OUT: f32 = 0.25;
/// The fan behind the front where the agents wander as the trail leads (a share of the scale),
/// and the hub round the inoculum where they turn out again.
const FAN: f64 = 1.0;
const HUB: f64 = 0.3;
/// At the front an agent heading out turns home one step in this many, behind it the less the
/// further from it, through the fan.
const BACK: f32 = 8.0;
/// An agent heading home turns out again one step in this many, short of the hub.
const RETURN: f32 = 256.0;
/// Where the surface turns past this (the cosine between the ways out of it), the way from the
/// inoculum takes a new anchor.
const BEND: f32 = 0.97;
/// Where the trail is a vein: this many times its mean where the mould has spread.
const THREAD: f64 = 1.5;
/// How far over the threshold (the trail's logarithm) a vein's edge blends.
const EDGE: f32 = 0.5;
/// The fewest skin cells a vein's island keeps (a thread about three scales long).
const ISLAND: usize = 120;
/// One agent in this many roams as the trail leads, never shuttling.
const ROAM: usize = 3;
/// The headings an agent reads the trail along, round each cell's own way.
const WAYS: usize = 32;
/// Agents a worker moves at a time.
const AGENTS: usize = 256;
/// Skin cells a worker spreads at a time.
const CHUNK: usize = 2048;

/// An agent: its point on the surface and its heading along it (a unit vector), lattice cells,
/// the band cell it stands in, whether it heads home and whether it is awake.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Agent {
    pub at: [f32; 3],
    pub heading: [f32; 3],
    pub cell: u32,
    /// Whether it heads home to the inoculum (else out to the front).
    pub home: bool,
    /// Whether it is awake: in the hub from the start, else since an agent crawled into its cell
    /// or a neighbour of it.
    pub awake: bool,
}

/// What a slime mould's agents find on their part: per skin cell and each of [`WAYS`] headings
/// round its own way, the skin cell [`SENSE`] scales ahead over the surface from its point of it
/// (1 more than its index; 0 till first asked, `u32::MAX` off the band), worked out the first time
/// it is asked, so a read is a lookup; per skin cell the agents that start there; and what each
/// band cell reads of the trail ([`Reads`]), worked out the first time a state shows.
#[derive(Default)]
pub struct Ground {
    ahead: Vec<AtomicU32>,
    lying_start: Vec<u32>,
    lying: Vec<u32>,
    reads: OnceLock<Reads>,
}

/// What each band cell reads of the trail for its place ([`places`]): its spot's skin cell and
/// the spot's neighbours, each weighed by how near its point of the surface is to the cell's,
/// and the weights' sum. The surface stays as it is, so every state shown reads by the same
/// weights.
struct Reads {
    /// Per band cell where its weights start in `weight` (and after the last, the end): its
    /// spot's first, then its neighbours' in their order; none where it has no spot.
    start: Vec<u32>,
    weight: Vec<f32>,
    /// Per band cell its weights' sum.
    total: Vec<f32>,
}

impl Reads {
    fn new(level: &Level, skin: &Skin) -> Reads {
        let next = |k: usize| &skin.next[skin.start[k] as usize..skin.start[k + 1] as usize];
        let (mut start, mut weight, mut total) = (Vec::with_capacity(level.band + 1), Vec::new(), vec![0f32; level.band]);
        start.push(0);
        for (b, sum) in total.iter_mut().enumerate() {
            let k = skin.of[b];
            if k != u32::MAX {
                let p = level.surface[b];
                for j in std::iter::once(k).chain(next(k as usize).iter().copied()) {
                    let d = far(p, level.surface[skin.cells[j as usize] as usize]);
                    let w = (-2.0 * d * d).exp();
                    weight.push(w);
                    *sum += w;
                }
            }
            start.push(weight.len() as u32);
        }
        Reads { start, weight, total }
    }
}

impl Ground {
    fn new(skin: &Skin, agents: &[Agent]) -> Ground {
        let n = skin.cells.len();
        let mut lying_start = vec![0u32; n + 1];
        for a in agents {
            lying_start[skin.at(a.cell) + 1] += 1;
        }
        for k in 0..n {
            lying_start[k + 1] += lying_start[k];
        }
        let mut fill = lying_start.clone();
        let mut lying = vec![0u32; agents.len()];
        for (j, a) in agents.iter().enumerate() {
            let k = skin.at(a.cell);
            lying[fill[k] as usize] = j as u32;
            fill[k] += 1;
        }
        Ground { ahead: (0..n * WAYS).map(|_| AtomicU32::new(0)).collect(), lying_start, lying, reads: OnceLock::new() }
    }

    /// The agents that start in skin cell `k`.
    fn lying(&self, k: usize) -> &[u32] {
        &self.lying[self.lying_start[k] as usize..self.lying_start[k + 1] as usize]
    }

    /// The skin cell `ahead` cells along heading `d` from skin cell `k`, none off the band.
    fn sees(&self, level: &Level, skin: &Skin, k: usize, d: usize, ahead: f32) -> Option<usize> {
        let slot = &self.ahead[k * WAYS + d];
        let mut seen = slot.load(Ordering::Relaxed);
        if seen == 0 {
            let i = skin.cells[k] as usize;
            let way = along(skin, i, std::f64::consts::TAU * d as f64 / WAYS as f64);
            seen = crawl(level, skin, i, level.surface[i], way, ahead).map_or(u32::MAX, |(_, _, j)| skin.at(j as u32) as u32 + 1);
            slot.store(seen, Ordering::Relaxed);
        }
        (seen != u32::MAX).then(|| seen as usize - 1)
    }
}

/// The starting state on `level`: no trail; the agents on skin cells the seed picks, each
/// heading a way the seed picks, those in the hub awake; and per skin cell its way from the
/// inoculum over the skin.
pub fn seed(spec: &Spec, level: &Level) -> State {
    let skin = Arc::new(Skin::new(level));
    let n = skin.cells.len();
    let count = (POPULATION * n as f64).round() as usize;
    let mut order: Vec<(u64, u32)> = skin.cells.iter().map(|&i| (mix(mix(spec.seed as u64 ^ 0x5113) ^ pack(level.cells[i as usize])), i)).collect();
    order.sort_unstable();
    order.truncate(count);
    let v = reach(level, &skin);
    let hub = (HUB * spec.scale / level.pitch) as f32;
    let agents: Vec<Agent> = order.iter().map(|&(h, i)| Agent { at: level.surface[i as usize], heading: along(&skin, i as usize, angle(mix(h))), cell: i, home: false, awake: v[skin.at(i)] <= hub }).collect();
    let ground = Arc::new(Ground::new(&skin, &agents));
    State { u: vec![0.0; n], v, walk: Walk::default(), seeds: Vec::new(), crowd: Crowd { agents, skin, lines: Lines::default(), ground, trodden: vec![false; n] } }
}

/// Per skin cell its way (cells) over the skin from the inoculum, the skin cell nearest the
/// top (up the y axis) of the middle of the skin's box (the first such), where a dish's floor or
/// a pebble's crown is; `f32::INFINITY` where the skin does not join it. Each cell's way is
/// straight from an anchor (the inoculum, or where the surface last turned past [`BEND`] on the
/// way to it) plus the anchor's own, so over a face it is the straight distance in every
/// direction, not the lattice's octagon.
fn reach(level: &Level, skin: &Skin) -> Vec<f32> {
    let n = skin.cells.len();
    let point = |k: usize| level.surface[skin.cells[k] as usize];
    let up = |k: usize| skin.up(skin.cells[k] as usize);
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for k in 0..n {
        let p = point(k);
        lo = std::array::from_fn(|a| lo[a].min(p[a]));
        hi = std::array::from_fn(|a| hi[a].max(p[a]));
    }
    let mid = [0.5 * (lo[0] + hi[0]), hi[1], 0.5 * (lo[2] + hi[2])];
    let mut way = vec![f32::INFINITY; n];
    let mut anchor = vec![u32::MAX; n];
    let Some(from) = (0..n).min_by(|&a, &b| far(point(a), mid).total_cmp(&far(point(b), mid)).then(a.cmp(&b))) else { return way };
    way[from] = 0.0;
    anchor[from] = from as u32;
    let mut heap = std::collections::BinaryHeap::from([std::cmp::Reverse((0u32, from as u32))]);
    while let Some(std::cmp::Reverse((bits, k))) = heap.pop() {
        let (k, d) = (k as usize, f32::from_bits(bits));
        if d > way[k] {
            continue;
        }
        for &j in &skin.next[skin.start[k] as usize..skin.start[k + 1] as usize] {
            let j = j as usize;
            // on from k's anchor while the surface faces as it does there, else from k
            let a = anchor[k] as usize;
            let a = if dot(up(j), up(a)) >= BEND { a } else { k };
            let e = way[a] + far(point(j), point(a));
            if e < way[j] {
                way[j] = e;
                anchor[j] = a as u32;
                heap.push(std::cmp::Reverse((e.to_bits(), j as u32)));
            }
        }
    }
    way
}

/// Where the mould has spread to and how its agents shuttle: the front (cells of the way from
/// the inoculum), the fan behind it (cells), the hub round the inoculum (cells).
struct Spread<'a> {
    way: &'a [f32],
    front: f32,
    fan: f32,
    hub: f32,
}

/// An agent's next state over `trail`, reading `ahead` cells off `look` of the [`WAYS`] headings
/// either side, `turn` its turn's cosine and sine, `roll` its random bits; and the trail it lays
/// (none where it did not crawl): less the further into the fan, to [`TIP`] of it at the front.
fn aim(level: &Level, skin: &Skin, ground: &Ground, trail: &[f32], spread: &Spread, a: &Agent, roams: bool, ahead: f32, look: isize, turn: (f32, f32), roll: u64) -> (Agent, f32) {
    let here = a.cell as usize;
    let at = spread.way[skin.at(a.cell)];
    let mut a = *a;
    // how far into the fan behind the front it is (0 short of it, 1 at the front)
    let deep = ((at - (spread.front - spread.fan)) / spread.fan).clamp(0.0, 1.0);
    // home from the fan, the likelier the nearer the front, out again from the hub or now and
    // then on the way: each back the way it came
    let chance = ((roll >> 40) & 0xffff) as f32 / 65536.0;
    let turns = (!roams && !a.home && chance < deep / BACK) || (a.home && (at < spread.hub || chance < 1.0 / RETURN));
    if turns {
        a.home = !a.home;
        a.heading = a.heading.map(|v| -v);
    }
    // out up the way from the inoculum, home down it; roaming as the trail leads
    let pull = if roams { 0.0 } else if a.home { -BIAS } else { BIAS };
    let n = skin.up(here);
    let side = cross(n, a.heading);
    // the trail along the nearest of the WAYS headings and `look` of them either side
    let (k, d) = (skin.at(a.cell), (turned(skin, here, a.heading) * WAYS as f64 / std::f64::consts::TAU).round() as isize);
    // heading out it spreads as it goes, up the way with a jitter, the trail aside; home and
    // roaming it follows the trail
    let out = !roams && !a.home;
    // (each of the three reads' jitter from its own ten of the random bits)
    let read = |s: isize, bits: u32| {
        ground.sees(level, skin, k, (d + s).rem_euclid(WAYS as isize) as usize, ahead).map_or(0.0, |j| {
            let lean = 1.0 + pull * ((spread.way[j] - at) / ahead).clamp(-1.0, 1.0);
            if out {
                lean + JITTER * (((roll >> bits) & 0x3ff) as f32 / 1024.0)
            } else {
                (trail[j] + EVEN) * lean
            }
        })
    };
    let (left, on, right) = (read(look, 8), read(0, 18), read(-look, 28));
    let way = if on > left && on > right {
        0.0
    } else if on < left && on < right {
        if roll & 1 == 0 { 1.0 } else { -1.0 }
    } else if left < right {
        -1.0
    } else if right < left {
        1.0
    } else {
        0.0
    };
    let heading = unit(add(a.heading.map(|v| v * turn.0), side, way * turn.1)).unwrap_or(a.heading);
    match crawl(level, skin, here, a.at, heading, 1.0).filter(|&(_, _, i)| spread.way[skin.at(i as u32)] <= spread.front) {
        Some((at, heading, i)) => (Agent { at, heading, cell: i as u32, ..a }, DEPOSIT * (1.0 - (1.0 - TIP) * deep) * if out { OUT } else { 1.0 }),
        // at the front or the band's edge: back the way it came, home if in the fan
        None => (Agent { heading: a.heading.map(|v| -v), home: !roams && (a.home || deep > 0.0), ..a }, 0.0),
    }
}

/// An angle (radians) from random bits.
fn angle(roll: u64) -> f64 {
    std::f64::consts::TAU * ((roll >> 11) as f64 / (1u64 << 53) as f64)
}

/// One step (the `step`th) of `spec` from `now` into `spare`: the agents crawl and lay their
/// trail on `now`'s, which spreads and fades into `spare`'s. Returns how far the trail moved,
/// summed over the skin's cells.
pub fn step(spec: &Spec, level: &Level, now: &mut State, spare: &mut State, step: u32, fan: Fan) -> f64 {
    let Model::Physarum { sense, turn, spread } = spec.model else { return 0.0 };
    let ahead = (SENSE * spec.scale / level.pitch) as f32;
    // an angle (degrees) as its cosine and sine
    let by = |deg: f64| {
        let (s, c) = (deg.to_radians() as f32).sin_cos();
        (c, s)
    };
    let (look, turn) = (((sense / 360.0 * WAYS as f64).round() as isize).max(1), by(turn));
    let base = mix(spec.seed as u64 ^ ((step as u64) << 32) ^ 0x5113_0000);
    // how far the mould has spread from the inoculum: `spread` times the farthest way by its last step
    let farthest = now.v.iter().filter(|d| d.is_finite()).fold(0f32, |m, &d| m.max(d));
    let front = (farthest as f64 * spread * (step + 1) as f64 / spec.steps.max(1) as f64) as f32;
    let (skin, ground) = (now.crowd.skin.clone(), now.crowd.ground.clone());
    let cells = |share: f64| (share * spec.scale / level.pitch) as f32;
    let spread = Spread { way: &now.v, front, fan: cells(FAN), hub: cells(HUB) };
    // the agents the mould has reached move, each from the trail as it was
    let trail = &now.u[..];
    let agents = &now.crowd.agents[..];
    let moves: Vec<Mutex<Vec<(Agent, f32)>>> = (0..agents.len().div_ceil(AGENTS)).map(|_| Mutex::new(Vec::new())).collect();
    fan(moves.len(), &|c| {
        let mut out = moves[c].lock().unwrap_or_else(|e| e.into_inner());
        let start = c * AGENTS;
        out.extend(agents[start..(start + AGENTS).min(agents.len())].iter().enumerate().map(|(k, a)| {
            if a.awake { aim(level, &skin, &ground, trail, &spread, a, (start + k) % ROAM == 0, ahead, look, turn, mix(base ^ (start + k) as u64)) } else { (*a, 0.0) }
        }));
    });
    // and lay their trail in their order, the first into a cell waking the agents asleep there
    // and round it
    let mut woken = Vec::new();
    for (k, (moved, laid)) in moves.into_iter().flat_map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner())).enumerate() {
        if laid > 0.0 {
            let c = skin.at(moved.cell);
            now.u[c] += laid;
            if !now.crowd.trodden[c] {
                now.crowd.trodden[c] = true;
                woken.extend_from_slice(ground.lying(c));
                for &j in &skin.next[skin.start[c] as usize..skin.start[c + 1] as usize] {
                    woken.extend_from_slice(ground.lying(j as usize));
                }
            }
        }
        now.crowd.agents[k] = moved;
    }
    for j in woken {
        now.crowd.agents[j as usize].awake = true;
    }
    // the trail spreads over the skin and fades into the spare state
    let from = &now.u[..];
    spare.u.resize(from.len(), 0.0);
    let chunks: Vec<Mutex<(&mut [f32], f64)>> = spare.u.chunks_mut(CHUNK).map(|u| Mutex::new((u, 0.0))).collect();
    fan(chunks.len(), &|c| {
        let mut chunk = chunks[c].lock().unwrap_or_else(|e| e.into_inner());
        let (to, moved) = &mut *chunk;
        let mut sum = 0f32;
        for (o, t) in to.iter_mut().enumerate() {
            let k = c * CHUNK + o;
            let here = from[k];
            let pull: f32 = skin.next[skin.start[k] as usize..skin.start[k + 1] as usize].iter().map(|&j| from[j as usize] - here).sum();
            *t = (1.0 - DECAY) * (here + SPREAD * pull);
            sum += (*t - here).abs();
        }
        *moved = sum as f64;
    });
    let moved = chunks.into_iter().map(|c| c.into_inner().unwrap_or_else(|e| e.into_inner()).1).sum();
    std::mem::swap(&mut spare.v, &mut now.v);
    spare.crowd = std::mem::take(&mut now.crowd);
    moved
}

/// Every band cell's place (0..1, the second material from a half): inside a vein. The veins are
/// where the trail (twice each skin cell the mean of itself and its neighbours) is over [`THREAD`]
/// times its mean where the mould has spread; the trail falls off alike either side of a vein's
/// ridge, so a stronger vein crosses that further out and a vein is as wide as its trail is
/// strong, a trunk tapering into its threads. A band cell reads the trail at its own point of the
/// surface (its spot's and the spot's neighbours', by nearness), blending over [`EDGE`] of the
/// trail's logarithm; a vein's island of fewer than [`ISLAND`] skin cells is left out.
pub fn places(level: &Level, state: &State, fan: Fan) -> Vec<f32> {
    let skin = &*state.crowd.skin;
    let next = |k: usize| &skin.next[skin.start[k] as usize..skin.start[k + 1] as usize];
    let Some((trail, lo)) = veined(state, fan) else { return vec![0.0; level.band] };
    let reads = state.crowd.ground.reads.get_or_init(|| Reads::new(level, skin));
    let mut places = vec![0f32; level.band];
    let chunks: Vec<Mutex<&mut [f32]>> = places.chunks_mut(CHUNK).map(Mutex::new).collect();
    fan(chunks.len(), &|c| {
        let mut chunk = chunks[c].lock().unwrap_or_else(|e| e.into_inner());
        for (o, place) in chunk.iter_mut().enumerate() {
            let b = c * CHUNK + o;
            let k = skin.of[b];
            if k == u32::MAX {
                continue;
            }
            let weights = &reads.weight[reads.start[b] as usize..reads.start[b + 1] as usize];
            let mut sum = 0f32;
            for (w, j) in weights.iter().zip(std::iter::once(k).chain(next(k as usize).iter().copied())) {
                sum += w * trail[j as usize];
            }
            let t = sum / reads.total[b].max(1e-12);
            *place = if t <= 0.0 { 0.0 } else { (0.5 + (t / lo).ln() / (2.0 * EDGE)).clamp(0.0, 1.0) };
        }
    });
    drop(chunks);
    places
}

/// The trail as a slime mould's veins read it, per skin cell (twice each cell the mean of itself
/// and its neighbours of the skin), its veins' islands of fewer than [`ISLAND`] cells under the
/// threshold; and the threshold the veins are over ([`THREAD`] times the trail's mean where the
/// mould has spread). None before there is any trail.
pub(crate) fn veined(state: &State, fan: Fan) -> Option<(Vec<f32>, f32)> {
    let skin = &*state.crowd.skin;
    let n = state.u.len();
    let next = |k: usize| &skin.next[skin.start[k] as usize..skin.start[k + 1] as usize];
    // the trail as its veins read: twice each cell the mean of itself and its neighbours of the skin
    let mean_of = |u: &[f32]| -> Vec<f32> {
        let mut mean = vec![0f32; n];
        let chunks: Vec<Mutex<&mut [f32]>> = mean.chunks_mut(CHUNK).map(Mutex::new).collect();
        fan(chunks.len(), &|c| {
            let mut chunk = chunks[c].lock().unwrap_or_else(|e| e.into_inner());
            for (o, m) in chunk.iter_mut().enumerate() {
                let k = c * CHUNK + o;
                *m = (u[k] + next(k).iter().map(|&j| u[j as usize]).sum::<f32>()) / (1 + next(k).len()) as f32;
            }
        });
        drop(chunks);
        mean
    };
    let mut trail = mean_of(&mean_of(&state.u));
    // its mean over where the mould has spread: the cells no farther from the inoculum than the
    // farthest the trail reaches
    let reach = (0..n).filter(|&k| state.u[k] > 0.0).map(|k| state.v[k]).fold(0f32, f32::max);
    let spread: Vec<usize> = (0..n).filter(|&k| state.v[k] <= reach).collect();
    let mean = spread.iter().map(|&k| trail[k] as f64).sum::<f64>() / spread.len().max(1) as f64;
    if mean <= 0.0 {
        return None;
    }
    let lo = (THREAD * mean) as f32;
    // the veins' islands (the cells over the threshold, joined over the skin): under ISLAND cells go
    let mut seen = vec![false; n];
    for k in 0..n {
        if trail[k] <= lo || seen[k] {
            continue;
        }
        seen[k] = true;
        let (mut stack, mut island) = (vec![k], vec![k]);
        while let Some(i) = stack.pop() {
            for &j in next(i) {
                if trail[j as usize] > lo && !seen[j as usize] {
                    seen[j as usize] = true;
                    stack.push(j as usize);
                    island.push(j as usize);
                }
            }
        }
        if island.len() < ISLAND {
            for i in island {
                trail[i] = trail[i].min(0.5 * lo);
            }
        }
    }
    Some((trail, lo))
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn far(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = sub(a, b);
    dot(d, d).sqrt()
}
