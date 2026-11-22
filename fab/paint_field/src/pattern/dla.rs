//! Diffusion-limited aggregation over a band's cells: a Lichtenberg figure's tree.
//!
//! The tree starts from its roots: the band's lowest cell, then each next the cell farthest from
//! the roots so far. Each step lets walkers go, one after another. A walker starts on a cell a
//! set distance from the tree (the launch shell, [`LAUNCH`] times the scale: a gap between
//! branches narrower than twice that has no cell there, gets no walkers and stays open), steps
//! to a random neighbour, and is lost past twice that distance. Beside the tree it hits the cell
//! it is on with the chance `stick` (else it walks on), and the cell joins the tree at its
//! `hits`th hit: more hits grow fewer, straighter branches; less sticking, denser ones. A cell
//! joins with its neighbours across the skin, so the tree marks the surface itself, not one
//! side of it. One walker after another, from the seed and the step: the same tree whatever the
//! workers.
//!
//! Each cell of the tree hangs from the one it joined beside, so the cells past it (its
//! branch's) count how it reads ([`places`]): a cell with fewer than [`SHOWN`] past it is fuzz
//! on a branch's side and does not show (a branch shows whole back to its root, short of its
//! tips), a twig reads a little over two cells wide over the skin, a trunk carrying thousands of
//! cells [`TRUNK`] cells more either side.
//!
//! The state: `u` each cell's distance from the tree in cells (graph steps through the band,
//! up to `far`), `v` 1 on the tree and, beside it, the hits so far over `hits`; and [`Walk`]:
//! the launch shell as a list, each tree cell's parent and the order they joined in.

use super::band::Level;
use super::skin::Crowd;
use super::rd::State;
use super::{Model, Spec};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

/// How far from the tree walkers start, as a share of the scale.
pub const LAUNCH: f64 = 1.5;
/// Band cells per walker a step lets go.
const CELLS_PER_WALKER: usize = 1500;
/// How far either side of a twig its material reaches over the skin (cells).
const TWIG: f32 = 1.15;
/// How far either side of the material's edge its place goes from a half to 1 or 0 (cells).
const EDGE: f32 = 1.0;
/// The fewest cells past a tree cell (itself too) for it to show.
pub const SHOWN: u32 = 5;
/// How much farther either side a branch reads per doubling of the cells past it beyond
/// [`TWIG_CELLS`] (cells), and the most (a trunk's).
const WIDEN: f32 = 0.15;
pub const TRUNK: f32 = 1.5;
/// The cells past a cell up to which it reads as a twig.
const TWIG_CELLS: f32 = 8.0;

/// A Lichtenberg figure's own state beside the two species: the cells at the launch distance
/// from the tree as a list (and each cell's place in it), each tree cell's parent (itself for a
/// root) and the tree's cells in the order they joined.
#[derive(Clone, Default)]
pub struct Walk {
    pub shell: Vec<u32>,
    at: Vec<u32>,
    parent: Vec<u32>,
    order: Vec<u32>,
}

impl Walk {
    fn sized(n: usize) -> Walk {
        Walk { shell: Vec::new(), at: vec![u32::MAX; n], parent: vec![u32::MAX; n], order: Vec::new() }
    }
    fn add(&mut self, i: usize) {
        if self.at[i] == u32::MAX {
            self.at[i] = self.shell.len() as u32;
            self.shell.push(i as u32);
        }
    }
    fn remove(&mut self, i: usize) {
        let k = self.at[i];
        if k == u32::MAX {
            return;
        }
        let last = self.shell.pop().expect("a listed cell");
        if last as usize != i {
            self.shell[k as usize] = last;
            self.at[last as usize] = k;
        }
        self.at[i] = u32::MAX;
    }
}

/// The launch distance and the distance a walker is lost at (cells of a lattice at `pitch`).
pub fn reach(spec: &Spec, pitch: f64) -> (u32, u32) {
    let launch = ((LAUNCH * spec.scale / pitch).round() as u32).max(2);
    (launch, 2 * launch)
}

/// splitmix64 as a stream.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        ((self.next() >> 32) * n as u64 >> 32) as usize
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// From `sources` (cells now on the tree) out: each cell nearer the tree than it was takes its
/// new distance, up to `far`, and the shell follows.
fn spread(level: &Level, u: &mut [f32], walk: &mut Walk, launch: u32, far: u32, sources: &[usize]) {
    let mut queue = VecDeque::new();
    for &s in sources {
        if u[s] != 0.0 {
            if u[s] == launch as f32 {
                walk.remove(s);
            }
            u[s] = 0.0;
        }
        queue.push_back(s);
    }
    while let Some(i) = queue.pop_front() {
        let d = u[i] + 1.0;
        if d > far as f32 {
            continue;
        }
        for &j in &level.nb[i] {
            let j = j as usize;
            if u[j] > d {
                if u[j] == launch as f32 {
                    walk.remove(j);
                }
                u[j] = d;
                if d == launch as f32 {
                    walk.add(j);
                }
                queue.push_back(j);
            }
        }
    }
}

/// Cell `i` and its neighbours across the skin (on the surface's other side).
fn with_across(level: &Level, i: usize) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(i).chain(level.nb[i].iter().map(|&j| j as usize).filter(move |&j| j != i && level.outside[j] != level.outside[i]))
}

/// The starting state on `level`: the roots `roots` (cells of its band, see
/// [`super::rd::roots_of`]) with their neighbours across the skin, its distances and shell.
pub fn seed(spec: &Spec, level: &Level, roots: &[usize]) -> State {
    let n = level.band;
    let (launch, far) = reach(spec, level.pitch);
    let mut u = vec![far as f32; n];
    let mut v = vec![0.0; n];
    let mut walk = Walk::sized(n);
    let mut tree = Vec::new();
    for &r in roots {
        for i in with_across(level, r) {
            if v[i] < 1.0 {
                v[i] = 1.0;
                walk.parent[i] = if i == r { i } else { r } as u32;
                walk.order.push(i as u32);
                tree.push(i);
            }
        }
    }
    spread(level, &mut u, &mut walk, launch, far, &tree);
    State { u, v, walk, seeds: Vec::new(), crowd: Crowd::default() }
}

/// Per tree cell (in the order they joined) the cell it hangs from (itself for a root) and
/// whether it shows: a test's look inside.
#[cfg(test)]
pub(crate) fn branches(state: &State) -> Vec<(usize, usize, bool)> {
    let past = past(state);
    state.walk.order.iter().map(|&i| (i as usize, state.walk.parent[i as usize] as usize, past[i as usize] >= SHOWN)).collect()
}

/// The `step`th step of `state`, in place: its walkers, one after another. Returns how many
/// cells joined the tree.
pub fn step(spec: &Spec, level: &Level, state: &mut State, step: u32) -> f64 {
    let Model::Aggregation { stick, hits, .. } = spec.model else { return 0.0 };
    let (launch, far) = reach(spec, level.pitch);
    let State { u, v, walk, .. } = state;
    let mut rng = Rng(((spec.seed as u64) << 32 | step as u64) ^ 0xd1a5_7e3d_0000_0000);
    let walkers = level.band.div_ceil(CELLS_PER_WALKER).max(1);
    let hit = 1.0 / hits.max(1) as f32;
    let mut joined = 0usize;
    for _ in 0..walkers {
        if walk.shell.is_empty() {
            break;
        }
        let mut i = walk.shell[rng.below(walk.shell.len())] as usize;
        loop {
            if u[i] == 1.0 && rng.unit() < stick {
                v[i] += hit;
                if v[i] >= 1.0 - 1e-4 {
                    // it hangs from a tree cell beside it, its neighbours across the skin from it
                    let parent = level.nb[i].iter().map(|&j| j as usize).find(|&j| v[j] >= 1.0 && j != i).expect("a cell beside the tree");
                    let cells: Vec<usize> = with_across(level, i).filter(|&j| j == i || v[j] < 1.0).collect();
                    for &j in &cells {
                        v[j] = 1.0;
                        walk.parent[j] = if j == i { parent } else { i } as u32;
                        walk.order.push(j as u32);
                    }
                    spread(level, u, walk, launch, far, &cells);
                    joined += cells.len();
                }
                break;
            }
            let j = level.nb[i][rng.below(6)] as usize;
            if u[j] >= far as f32 {
                break;
            }
            // the tree is not walked onto
            if u[j] > 0.0 {
                i = j;
            }
        }
    }
    joined as f64
}

/// The cells past each band cell on the tree (itself too; 0 off it): the tree's cells latest
/// first, each adding to the one it hangs from.
fn past(state: &State) -> Vec<u32> {
    let walk = &state.walk;
    let mut past = vec![0u32; walk.parent.len()];
    for &i in walk.order.iter().rev() {
        let i = i as usize;
        past[i] += 1;
        let p = walk.parent[i] as usize;
        if p != i {
            past[p] += past[i];
        }
    }
    past
}

/// A cell and how far its branch's material reaches past it (cells), for the heap.
struct Reach(f32, u32);

impl PartialEq for Reach {
    fn eq(&self, o: &Reach) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Reach {}
impl PartialOrd for Reach {
    fn partial_cmp(&self, o: &Reach) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Reach {
    fn cmp(&self, o: &Reach) -> Ordering {
        self.0.total_cmp(&o.0).then(o.1.cmp(&self.1))
    }
}

/// Every band cell's place (0..1, the tree's material from a half). A tree cell shows with
/// [`SHOWN`] cells or more past it (so a branch shows whole from the root to [`SHOWN`] short of
/// each tip, and a cell carrying one that shows shows too): its material reaches [`TWIG`] cells
/// over the skin round it, and [`WIDEN`] more per doubling of the cells past it beyond
/// [`TWIG_CELLS`], up to [`TRUNK`] more. Distances are the cells' nearest points of the surface
/// apart, so the cells either side of a spot of the skin read alike. The material spreads
/// through the band from the cells that show, a cell past its edge passing on no more than it
/// has: a cell reads it only through cells that read it, back to the tree. The place is a half
/// at the edge, 1 [`EDGE`] cells inside it and 0 as far outside.
pub fn places(level: &Level, state: &State) -> Vec<f32> {
    let n = level.band;
    let walk = &state.walk;
    let past = past(state);
    // how far over the skin each cell that shows reaches, and the cell whose reach each has
    let mut radius = vec![0f32; n];
    let mut from = vec![u32::MAX; n];
    let mut reach = vec![f32::NEG_INFINITY; n];
    let mut heap = BinaryHeap::new();
    for &i in &walk.order {
        let i = i as usize;
        if past[i] < SHOWN {
            continue;
        }
        let r = TWIG + (WIDEN * (past[i] as f32 / TWIG_CELLS).log2()).clamp(0.0, TRUNK);
        radius[i] = r;
        if r > reach[i] {
            reach[i] = r;
            from[i] = i as u32;
            heap.push(Reach(r, i as u32));
        }
    }
    let apart = |a: usize, b: usize| (0..3).map(|k| (level.surface[a][k] - level.surface[b][k]).powi(2)).sum::<f32>().sqrt();
    while let Some(Reach(r, i)) = heap.pop() {
        let i = i as usize;
        if r < reach[i] {
            continue;
        }
        let s = from[i] as usize;
        for &j in &level.nb[i] {
            let j = j as usize;
            let mut to = radius[s] - apart(j, s);
            if r <= 0.0 {
                to = to.min(r);
            }
            if to > reach[j] && to > -EDGE {
                reach[j] = to;
                from[j] = s as u32;
                heap.push(Reach(to, j as u32));
            }
        }
    }
    reach.iter().map(|r| (0.5 + 0.5 * r / EDGE).clamp(0.0, 1.0)).collect()
}
