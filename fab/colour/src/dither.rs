//! An ink per cell, over any surface's cells.
//!
//! First by error diffusion ([`dither`]), then refined against the eye ([`refine`]).
//!
//! The cells come in the order they are chosen. Each says the colour it should show (already in
//! the gamut), which later cells take a share of the difference between that and the ink chosen
//! (error diffusion: in linear light, where side-by-side cells average), and per stack level the
//! cell before it along the line that level prints on. The ink is the one nearest in OKLab, under
//! two print rules:
//! - a run of one filament along a level's line is at least [`Rules::min_run`] cells: while the
//!   cell before has a shorter run, the level keeps its filament;
//! - a level that changes filament from the cell before costs [`Rules::switch`] (OKLab
//!   distance): fewer, longer runs, so fewer retractions, for a little more noise.
//!
//! The error is carried in linear light because that is how neighbouring cells mix; carried in
//! OKLab instead, the averages drift (measured worse on photos).

use crate::oklab;
use crate::palette::{Palette, MAX_LEVELS};
use crate::view;

/// A surface's cells, in the order they are chosen.
pub trait Cells {
    fn len(&self) -> usize;
    /// The colour cell `i` should show (linear, in the palette's gamut).
    fn target(&self, i: usize) -> [f32; 3];
    /// The later cells (after `i` in order) that take a share of `i`'s error, with the shares.
    fn spread(&self, i: usize, out: &mut Vec<(usize, f32)>);
    /// The cell before `i` (already chosen) along the line stack level `level` prints on.
    fn before(&self, i: usize, level: usize) -> Option<usize>;
    /// The two cells beside `i` along the line stack level `level` prints on, the same way
    /// round for every cell (none past the line's ends).
    fn along(&self, i: usize, level: usize) -> [Option<usize>; 2];
    /// The cells up to `reach` steps from `i` each way on the lattice the cells lie on (itself
    /// too), with their steps from it.
    fn near(&self, i: usize, reach: usize, out: &mut Vec<(usize, [i32; 2])>);
    /// Where cell `i` lies on that lattice (neighbours one step apart).
    fn place(&self, i: usize) -> [i32; 2];
    /// The distance between neighbouring cells (mm).
    fn pitch(&self) -> f64;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rules {
    /// The shortest run of one filament along a level's line (cells; 1: any).
    pub min_run: usize,
    /// What a level changing filament from the cell before costs (OKLab distance per level).
    pub switch: f32,
    /// How much a difference of lightness weighs against one of hue or chroma in picking an ink:
    /// over one, neighbouring cells keep near one lightness and differ in hue instead, which the
    /// eye blends at a far shorter distance.
    pub lightness: f32,
}

/// The ink (an index into `palette.inks`) of every cell.
pub fn dither(palette: &Palette, cells: &dyn Cells, rules: &Rules) -> Vec<u16> {
    let n = cells.len();
    let depth = palette.depth;
    let mut err = vec![[0.0f32; 3]; n];
    let mut runs = vec![[0u16; MAX_LEVELS]; n];
    let mut out = vec![0u16; n];
    let mut spread = Vec::new();
    let min_run = rules.min_run.max(1) as u16;
    for i in 0..n {
        let t = cells.target(i);
        let want: [f32; 3] = std::array::from_fn(|k| t[k] + err[i][k]);
        let want_lab = oklab::oklab(want.map(|x| x.clamp(0.0, 1.0)));
        let before: [Option<usize>; MAX_LEVELS] = std::array::from_fn(|l| if l < depth { cells.before(i, l) } else { None });
        // a level whose run is still short keeps its filament
        let forced: [Option<u8>; MAX_LEVELS] = std::array::from_fn(|l| before[l].filter(|&b| runs[b][l] < min_run).map(|b| palette.inks[out[b] as usize].stack[l]));
        let mut best = (f32::INFINITY, 0usize);
        for (k, ink) in palette.inks.iter().enumerate() {
            if (0..depth).any(|l| forced[l].is_some_and(|f| f != ink.stack[l])) {
                continue;
            }
            let switches = (0..depth).filter(|&l| before[l].is_some_and(|b| palette.inks[out[b] as usize].stack[l] != ink.stack[l])).count();
            let d = [rules.lightness * (want_lab[0] - ink.lab[0]), want_lab[1] - ink.lab[1], want_lab[2] - ink.lab[2]];
            let cost = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() + rules.switch * switches as f32;
            if cost < best.0 {
                best = (cost, k);
            }
        }
        let ink = &palette.inks[best.1];
        out[i] = best.1 as u16;
        for l in 0..depth {
            runs[i][l] = match before[l] {
                Some(b) if palette.inks[out[b] as usize].stack[l] == ink.stack[l] => runs[b][l].saturating_add(1),
                _ => 1,
            };
        }
        // the error, held so a colour the gamut map missed cannot run away (loose enough that a
        // run the rules hold carries all it owes to the cells after it)
        let held = 1.0 + min_run as f32;
        let e: [f32; 3] = std::array::from_fn(|k| (want[k] - ink.colour[k]).clamp(-held, held));
        spread.clear();
        cells.spread(i, &mut spread);
        for &(j, w) in &spread {
            for k in 0..3 {
                err[j][k] += e[k] * w;
            }
        }
    }
    out
}

/// The most times [`refine`] goes over the cells.
const PASSES: usize = 8;

/// Filters at most this wide (sigma, cells) are followed cell by cell in [`refine`]; wider ones
/// change smoothly enough to be kept on a lattice this many times coarser.
const FINE: f64 = 2.0;
const COARSE: i32 = 3;

/// A change [`refine`] weighs: one cell's ink, or two neighbours' along a level's line taking
/// one filament there together (a run born at the rules' shortest), and what it gains.
struct Move {
    gain: f64,
    cells: [(usize, usize); 2],
    two: bool,
}

/// What keeps [`refine`] from trading a patch's colour for less texture (a red made paler or
/// deeper to print it in flatter inks: at the viewing distance the eye still tells the cells'
/// lightness apart, so less texture would otherwise win): the error over patches of `PATCH` mm
/// (a Gaussian's sigma) weighs `GUARD` times as much again as the eye's. Measured on a photo, a
/// stronger guard costs more ΔE at the viewing distance than the drift it saves.
const PATCH: f64 = 1.5;
const GUARD: f64 = 8.0;

/// Filters' correlations with themselves, summed (each channel's), by lattice step up to
/// `reach` each way.
struct Table {
    reach: i32,
    side: usize,
    values: [Vec<f64>; 3],
}

impl Table {
    /// The pairs of each filter's Gaussians that `keep` takes, correlated.
    fn new(filters: &[[Vec<(f64, f64)>; 3]], keep: &dyn Fn(f64, f64) -> bool) -> Table {
        let widest = filters.iter().flatten().flat_map(|f| f.iter().flat_map(|a| f.iter().filter(|b| keep(a.1, b.1)).map(move |b| a.1.hypot(b.1)))).fold(0.5, f64::max);
        let reach = (3.0 * widest).ceil() as i32;
        let side = 2 * reach as usize + 1;
        let values = std::array::from_fn(|ch| {
            let mut sum = vec![0.0; side * side];
            for f in filters {
                for (s, v) in sum.iter_mut().zip(autocorrelation(&f[ch], reach as usize, keep)) {
                    *s += v;
                }
            }
            sum
        });
        Table { reach, side, values }
    }

    /// Channel `ch`'s value at step `k` (none past the reach).
    fn at(&self, ch: usize, k: [i32; 2]) -> f64 {
        if k[0].abs() > self.reach || k[1].abs() > self.reach {
            return 0.0;
        }
        self.values[ch][(k[0] + self.reach) as usize * self.side + (k[1] + self.reach) as usize]
    }
}

/// The wide filters' share of what a change meets, on a lattice [`COARSE`] times coarser than
/// the cells', read between its points by bilinear weights.
struct Coarse {
    origin: [i32; 2],
    size: [usize; 2],
    values: Vec<[f64; 3]>,
}

impl Coarse {
    fn point(&self, u: usize, v: usize) -> [i32; 2] {
        [self.origin[0] + COARSE * u as i32, self.origin[1] + COARSE * v as i32]
    }

    /// Its value at a cell's place.
    fn at(&self, p: [i32; 2]) -> [f64; 3] {
        let (fu, fv) = ((p[0] - self.origin[0]) as f64 / COARSE as f64, (p[1] - self.origin[1]) as f64 / COARSE as f64);
        let (u, v) = ((fu.floor() as usize).min(self.size[0] - 2), (fv.floor() as usize).min(self.size[1] - 2));
        let (a, b) = (fu - u as f64, fv - v as f64);
        let mut out = [0.0; 3];
        for (du, dv, w) in [(0, 0, (1.0 - a) * (1.0 - b)), (1, 0, a * (1.0 - b)), (0, 1, (1.0 - a) * b), (1, 1, a * b)] {
            let c = &self.values[(v + dv) * self.size[0] + u + du];
            for ch in 0..3 {
                out[ch] += w * c[ch];
            }
        }
        out
    }

    /// Adds a change `d` at place `p` through `table`.
    fn add(&mut self, table: &Table, p: [i32; 2], d: [f64; 3]) {
        // the lattice points within the table's reach of `p`, along axis k
        let span = |k: usize| {
            let (a, b) = (p[k] - table.reach - self.origin[k], p[k] + table.reach - self.origin[k]);
            let lo = (a + COARSE - 1).div_euclid(COARSE).max(0) as usize;
            let hi = (b.div_euclid(COARSE) + 1).clamp(0, self.size[k] as i32) as usize;
            lo..hi.max(lo)
        };
        for v in span(1) {
            for u in span(0) {
                let q = self.point(u, v);
                let k = [q[0] - p[0], q[1] - p[1]];
                let c = &mut self.values[v * self.size[0] + u];
                for ch in 0..3 {
                    c[ch] += table.at(ch, k) * d[ch];
                }
            }
        }
    }
}

/// `inks` (a [`dither`] of `cells`) refined against the eye from `distance` mm (direct binary
/// search): cell by cell, the change that brings the cells as seen from there nearest to their
/// targets as seen from there, where every run along every level's lines still holds `rules`.
/// The difference is weighed as OKLab's, the eye's filters are each opponent channel's (with a
/// guard on patches' colours, [`GUARD`]), and the passes stop when they hardly change a cell.
pub fn refine(palette: &Palette, cells: &dyn Cells, rules: &Rules, distance: f64, inks: &mut [u16]) {
    let n = cells.len();
    if n == 0 {
        return;
    }
    let patch = [(GUARD.sqrt(), PATCH / cells.pitch())];
    let filters = [view::eye(distance, cells.pitch()), std::array::from_fn(|_| patch.to_vec())];
    // pairs of fine filters are followed cell by cell, any pair with a wide one on the coarse lattice
    let fine = Table::new(&filters, &|a, b| a <= FINE && b <= FINE);
    let wide = Table::new(&filters, &|a, b| a > FINE || b > FINE);
    let colour: Vec<[f64; 3]> = palette.inks.iter().map(|k| view::opponent(k.colour)).collect();
    let target: Vec<[f64; 3]> = (0..n).map(|i| view::opponent(cells.target(i))).collect();
    let weight: Vec<[f64; 3]> = (0..n).map(|i| view::weights(cells.target(i))).collect();
    let place: Vec<[i32; 2]> = (0..n).map(|i| cells.place(i)).collect();
    let error = |j: usize, inks: &[u16]| -> [f64; 3] { std::array::from_fn(|ch| colour[inks[j] as usize][ch] - target[j][ch]) };
    // what a change at each cell meets: the error round it, through the filter twice
    let mut near = Vec::new();
    let mut meet = vec![[0.0f64; 3]; n];
    for i in 0..n {
        cells.near(i, fine.reach as usize, &mut near);
        for &(j, k) in &near {
            let e = error(j, inks);
            for ch in 0..3 {
                meet[i][ch] += fine.at(ch, k) * e[ch];
            }
        }
    }
    let lo = [0, 1].map(|k| place.iter().map(|p| p[k]).min().unwrap_or(0));
    let hi = [0, 1].map(|k| place.iter().map(|p| p[k]).max().unwrap_or(0));
    let size = [0, 1].map(|k| ((hi[k] - lo[k]) / COARSE + 2) as usize);
    let mut broad = Coarse { origin: lo, size, values: vec![[0.0; 3]; size[0] * size[1]] };
    for j in 0..n {
        broad.add(&wide, place[j], error(j, inks));
    }
    let delta = |from: usize, to: usize| -> [f64; 3] { std::array::from_fn(|ch| colour[to][ch] - colour[from][ch]) };
    let both = |ch: usize, k: [i32; 2]| fine.at(ch, k) + wide.at(ch, k);
    // the ink of `ink`'s stack with level `l`'s filament `f`
    let with = |ink: usize, l: usize, f: u8| -> usize {
        let mut stack = palette.inks[ink].stack;
        stack[l] = f;
        palette.index(&stack)
    };
    let mut moves: Vec<Move> = Vec::new();
    for _ in 0..PASSES {
        let mut changed = 0;
        for i in 0..n {
            // what a change at `c` meets, and what changing it to ink `k` gains
            let meets = |c: usize| -> [f64; 3] {
                let m = broad.at(place[c]);
                std::array::from_fn(|ch| meet[c][ch] + m[ch])
            };
            let alone = |c: usize, k: usize, m: &[f64; 3]| -> f64 {
                let d = delta(inks[c] as usize, k);
                (0..3).map(|ch| weight[i][ch] * d[ch] * (2.0 * m[ch] + d[ch] * both(ch, [0, 0]))).sum()
            };
            moves.clear();
            let old = inks[i] as usize;
            let mi = meets(i);
            for k in (0..palette.inks.len()).filter(|&k| k != old) {
                let gain = alone(i, k, &mi);
                if gain < 0.0 {
                    moves.push(Move { gain, cells: [(i, k), (i, k)], two: false });
                }
            }
            if rules.min_run > 1 {
                for l in 0..palette.depth {
                    let Some(j) = cells.along(i, l)[1] else { continue };
                    let step = [place[j][0] - place[i][0], place[j][1] - place[i][1]];
                    let mj = meets(j);
                    for f in 0..palette.filaments as u8 {
                        let (ki, kj) = (with(old, l, f), with(inks[j] as usize, l, f));
                        if ki == old || kj == inks[j] as usize {
                            continue;
                        }
                        let (di, dj) = (delta(old, ki), delta(inks[j] as usize, kj));
                        let cross: f64 = (0..3).map(|ch| 2.0 * weight[i][ch] * di[ch] * dj[ch] * both(ch, step)).sum();
                        let gain = alone(i, ki, &mi) + alone(j, kj, &mj) + cross;
                        if gain < 0.0 {
                            moves.push(Move { gain, cells: [(i, ki), (j, kj)], two: true });
                        }
                    }
                }
            }
            moves.sort_by(|a, b| a.gain.total_cmp(&b.gain));
            for m in &moves {
                let parts = if m.two { &m.cells[..] } else { &m.cells[..1] };
                let was: Vec<u16> = parts.iter().map(|&(c, _)| inks[c]).collect();
                for &(c, k) in parts {
                    inks[c] = k as u16;
                }
                if parts.iter().all(|&(c, _)| holds(palette, cells, rules, inks, c)) {
                    for (&(c, k), &w) in parts.iter().zip(&was) {
                        let d = delta(w as usize, k);
                        cells.near(c, fine.reach as usize, &mut near);
                        for &(x, step) in &near {
                            for ch in 0..3 {
                                meet[x][ch] += fine.at(ch, step) * d[ch];
                            }
                        }
                        broad.add(&wide, place[c], d);
                    }
                    changed += 1;
                    break;
                }
                for (&(c, _), &w) in parts.iter().zip(&was) {
                    inks[c] = w;
                }
            }
        }
        if changed * 1000 < n {
            break;
        }
    }
}

/// A filter (Gaussians as weight and sigma in cells, sampled on the lattice) correlated with
/// itself, the pairs of its Gaussians `keep` takes, for every step up to `reach` each way.
fn autocorrelation(filter: &[(f64, f64)], reach: usize, keep: &dyn Fn(f64, f64) -> bool) -> Vec<f64> {
    let t = reach as isize;
    let sample = |sigma: f64| -> Vec<f64> {
        let v: Vec<f64> = (-t..=t).map(|x| if sigma < 0.25 { (x == 0) as u8 as f64 } else { (-((x * x) as f64) / (2.0 * sigma * sigma)).exp() }).collect();
        let sum: f64 = v.iter().sum();
        v.into_iter().map(|x| x / sum).collect()
    };
    let g: Vec<Vec<f64>> = filter.iter().map(|c| sample(c.1)).collect();
    let side = 2 * reach + 1;
    let mut out = vec![0.0; side * side];
    for (m, &(wm, sm)) in filter.iter().enumerate() {
        for (q, &(wq, sq)) in filter.iter().enumerate() {
            if !keep(sm, sq) {
                continue;
            }
            let one: Vec<f64> = (-t..=t).map(|k| (-t..=t).filter(|x| (-t..=t).contains(&(x + k))).map(|x| g[m][(x + t) as usize] * g[q][(x + k + t) as usize]).sum()).collect();
            for a in 0..side {
                for b in 0..side {
                    out[a * side + b] += wm * wq * one[a] * one[b];
                }
            }
        }
    }
    out
}

/// Whether the runs at cell `i` hold the rules: along each level's line, its run and the runs
/// beside it are at least the shortest, or reach the line's end.
fn holds(palette: &Palette, cells: &dyn Cells, rules: &Rules, inks: &[u16], i: usize) -> bool {
    let min = rules.min_run.max(1);
    if min == 1 {
        return true;
    }
    let fil = |j: usize, l: usize| palette.inks[inks[j] as usize].stack[l];
    for l in 0..palette.depth {
        // filament `f`'s run from `from` outward on side `s`: its length (counted up to the
        // shortest) and whether it reaches the line's end
        let run = |from: Option<usize>, s: usize, f: u8| -> (usize, bool) {
            let (mut len, mut at) = (0, from);
            while let Some(j) = at {
                if fil(j, l) != f {
                    return (len, false);
                }
                len += 1;
                if len >= min {
                    return (len, false);
                }
                at = cells.along(j, l)[s];
            }
            (len, true)
        };
        let f = fil(i, l);
        let beside = cells.along(i, l);
        let (a, b) = (run(beside[0], 0, f), run(beside[1], 1, f));
        if 1 + a.0 + b.0 < min && !a.1 && !b.1 {
            return false;
        }
        for (s, j) in beside.iter().enumerate() {
            if let Some(j) = *j {
                let g = fil(j, l);
                if g != f {
                    let (len, end) = run(Some(j), s, g);
                    if len < min && !end {
                        return false;
                    }
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::{Backing, Filament, FilamentSet, StackModel};

    /// One line of cells, each level printing along it, the error passed to the next.
    struct Line(Vec<[f32; 3]>);
    impl Cells for Line {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn target(&self, i: usize) -> [f32; 3] {
            self.0[i]
        }
        fn spread(&self, i: usize, out: &mut Vec<(usize, f32)>) {
            if i + 1 < self.0.len() {
                out.push((i + 1, 1.0));
            }
        }
        fn before(&self, i: usize, _: usize) -> Option<usize> {
            i.checked_sub(1)
        }
        fn along(&self, i: usize, _: usize) -> [Option<usize>; 2] {
            [i.checked_sub(1), (i + 1 < self.0.len()).then_some(i + 1)]
        }
        fn near(&self, i: usize, reach: usize, out: &mut Vec<(usize, [i32; 2])>) {
            out.clear();
            for j in i.saturating_sub(reach)..(i + reach + 1).min(self.0.len()) {
                out.push((j, [0, j as i32 - i as i32]));
            }
        }
        fn place(&self, i: usize) -> [i32; 2] {
            [0, i as i32]
        }
        fn pitch(&self) -> f64 {
            0.4
        }
    }

    fn runs(fils: &[u8]) -> Vec<usize> {
        let mut out = vec![1];
        for w in fils.windows(2) {
            if w[0] == w[1] {
                *out.last_mut().unwrap() += 1;
            } else {
                out.push(1);
            }
        }
        out
    }

    /// A mid grey from black and white averages to the grey; every run but the last is at least
    /// the rule's; a switch cost makes fewer runs.
    #[test]
    fn a_dither_averages_to_the_target_under_its_run_rules() {
        let set = FilamentSet::new("kw", vec![Filament::guess("Black", [20, 20, 20]), Filament::guess("White", [240, 240, 240])]);
        let model = StackModel { levels: vec![0.16, 0.16], backing: Backing::Filament { index: 1, mm: 0.32 }, background: [0.8; 3] };
        let p = Palette::new(&set, &model);
        let grey = [0.25f32; 3];
        let line = Line(vec![grey; 400]);
        for (min_run, switch) in [(1, 0.0), (3, 0.0), (3, 0.05)] {
            let inks = dither(&p, &line, &Rules { min_run, switch, lightness: 1.0 });
            let mean: f32 = inks.iter().map(|&k| p.inks[k as usize].colour[0]).sum::<f32>() / inks.len() as f32;
            assert!((mean - grey[0]).abs() < 0.02, "min run {min_run}: mean {mean}");
            for level in 0..2 {
                let fils: Vec<u8> = inks.iter().map(|&k| p.inks[k as usize].stack[level]).collect();
                let r = runs(&fils);
                assert!(r[..r.len() - 1].iter().all(|&x| x >= min_run), "min run {min_run} level {level}: {r:?}");
            }
        }
        // where two inks are close, a switch decides
        let near = FilamentSet::new("wg", vec![Filament::guess("White", [240, 240, 240]), Filament::guess("Grey", [200, 200, 200])]);
        let q = Palette::new(&near, &StackModel { backing: Backing::Filament { index: 0, mm: 0.32 }, ..model });
        let between = Line(vec![[0.75f32; 3]; 400]);
        let count = |switch| {
            let inks = dither(&q, &between, &Rules { min_run: 2, switch, lightness: 1.0 });
            runs(&inks.iter().map(|&k| q.inks[k as usize].stack[0]).collect::<Vec<_>>()).len()
        };
        assert!(count(0.08) < count(0.0), "a switch cost makes fewer runs: {} vs {}", count(0.08), count(0.0));
    }

    /// Refined against the eye, a dither of a mid grey from black and white looks no farther from
    /// the grey (its error through the eye's filters weighs no more), still averages to it, and
    /// keeps every run but a line's last at least the rule's.
    #[test]
    fn a_refined_dither_looks_nearer_and_keeps_its_runs() {
        let set = FilamentSet::new("kw", vec![Filament::guess("Black", [20, 20, 20]), Filament::guess("White", [240, 240, 240])]);
        let model = StackModel { levels: vec![0.16, 0.16], backing: Backing::Filament { index: 1, mm: 0.32 }, background: [0.8; 3] };
        let p = Palette::new(&set, &model);
        let line = Line(vec![[0.3f32; 3]; 300]);
        let eye = view::eye(view::VIEWING, line.pitch());
        let reach = 12;
        let table: Vec<Vec<f64>> = eye.iter().map(|f| autocorrelation(f, reach, &|_, _| true)).collect();
        let side = 2 * reach + 1;
        let seen = |inks: &[u16]| -> f64 {
            let d: Vec<[f64; 3]> = inks.iter().enumerate().map(|(i, &k)| std::array::from_fn(|ch| view::opponent(p.inks[k as usize].colour)[ch] - view::opponent(line.0[i])[ch])).collect();
            let w = view::weights(line.0[0]);
            let mut sum = 0.0;
            for i in 0..d.len() {
                for j in i.saturating_sub(reach)..(i + reach + 1).min(d.len()) {
                    let a = reach * side + (j as isize - i as isize + reach as isize) as usize;
                    for ch in 0..3 {
                        sum += w[ch] * table[ch][a] * d[i][ch] * d[j][ch];
                    }
                }
            }
            sum
        };
        for min_run in [1, 2, 3] {
            let rules = Rules { min_run, switch: 0.0, lightness: 1.0 };
            let mut inks = dither(&p, &line, &rules);
            let before = seen(&inks);
            refine(&p, &line, &rules, view::VIEWING, &mut inks);
            let after = seen(&inks);
            assert!(after <= before + 1e-12, "min run {min_run}: {after} over {before}");
            let mean: f32 = inks.iter().map(|&k| p.inks[k as usize].colour[0]).sum::<f32>() / inks.len() as f32;
            assert!((mean - 0.3).abs() < 0.03, "min run {min_run}: mean {mean}");
            for level in 0..2 {
                let r = runs(&inks.iter().map(|&k| p.inks[k as usize].stack[level]).collect::<Vec<_>>());
                assert!(r.len() < 3 || r[1..r.len() - 1].iter().all(|&x| x >= min_run), "min run {min_run} level {level}: {r:?}");
            }
        }
    }
}
