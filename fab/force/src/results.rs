//! What a solved displacement means: each cell's stress (the fill-weighted mean of its
//! Gauss points' stress, the walls', skin's and infill's mixture), its von Mises and largest
//! principal stress, and its safety factor (the weakest material at the weakest point, by
//! [`Materials::failure`]); and the [`Verdict`] two cell sizes give it, away from the fixtures
//! and near them. On a model's surface, each cell also takes the stress recovered at its
//! surface quads (FORCE A6 2.1).

use crate::element::{corner, gradients_at, invert3, mul6, principal, strain, von_mises, Mat6, V3};
use crate::grid::{Gp, Grid};
use crate::material::{Checks, Materials};
use std::collections::HashMap;

/// A Gauss point counts for strength checks when at least this share of it is material.
const SOLID: u8 = 64;

/// A cell is sound when all eight octants meet the strength check's fill threshold.
fn sound(grid: &Grid, c: usize) -> bool {
    grid.gp.get(c).is_some_and(|points| points.iter().all(|g| g.fill >= SOLID))
}

/// Cell `from` meets cell `to` across its face along `axis` (`side` 0 the lower face, 1 the
/// upper): a filled point of each faces the other across it.
fn joined(grid: &Grid, from: usize, to: usize, axis: usize, side: usize) -> bool {
    (0..8).any(|q| corner(q)[axis] == side && grid.gp[from][q].fill > 0 && grid.gp[to][q ^ (1 << axis)].fill > 0)
}

/// The Gauss point in printer mm, including the cell's offset used for its strain.
fn point_at(grid: &Grid, c: usize, q: usize) -> V3 {
    let center = grid.cell_center(c);
    let xi = grid.point_xi(c, q);
    std::array::from_fn(|a| center[a] + xi[a] * grid.h[a] / 2.0)
}

/// The filled point nearest to `p` among `places`, the first of equals.
fn nearest_of(places: &[Option<V3>; 8], p: V3) -> Option<usize> {
    let mut nearest = None;
    let mut distance = f64::INFINITY;
    for (q, place) in places.iter().enumerate() {
        if let Some(x) = place {
            let d = (0..3).map(|a| (x[a] - p[a]).powi(2)).sum::<f64>();
            if d < distance {
                distance = d;
                nearest = Some(q);
            }
        }
    }
    nearest
}

/// One point's contribution to the cell's mean strain and centroid offset.
fn add_sample(sum: &mut [f64; 9], grid: &Grid, c: usize, q: usize, e: &[f64; 6], f: f64) {
    let center = grid.cell_center(c);
    let p = point_at(grid, c, q);
    for k in 0..6 {
        sum[k] += f * e[k];
    }
    for a in 0..3 {
        sum[6 + a] += f * (p[a] - center[a]);
    }
}

/// The own-direction contributions to the six Voigt strain slopes along each axis.
fn voigt_slopes(s: &[[f64; 3]; 3]) -> [[f64; 6]; 3] {
    [
        [s[0][0], 0.0, 0.0, 0.0, s[2][0], s[1][0]],
        [0.0, s[1][1], 0.0, s[2][1], 0.0, s[0][1]],
        [0.0, 0.0, s[2][2], s[1][2], s[0][2], 0.0],
    ]
}

/// A solved field and its fill-weighted samples for recovering strain at the surface.
/// Each cell takes 36 bytes for its mean strain and its centroid's offset from the centre,
/// and a cell whose centroid is off its centre 40 more for its slopes, for the pass.
/// An inactive cell's sample is zero.
pub struct Recovery<'a> {
    grid: &'a Grid,
    u: &'a [V3],
    samples: Vec<[f32; 9]>,
    /// The own-direction slopes `S[i][j]` (row-major, index `i * 3 + j`) of each cell
    /// `correct_samples` corrected, in rising cell order. They are kept for the pass so
    /// `cell_gradient` does not search again, and are freed with the samples.
    kept: Vec<(u32, [f32; 9])>,
}

/// A linear strain field about the weighted centroid of a cell and its joined neighbours.
pub struct Fit {
    x0: V3,
    e0: [f64; 6],
    g: [[f64; 6]; 3],
}

impl Fit {
    /// The six strain components at `p` in printer mm.
    pub fn at(&self, p: V3) -> [f64; 6] {
        let mut e = self.e0;
        for a in 0..3 {
            let dx = p[a] - self.x0[a];
            for k in 0..6 {
                e[k] += self.g[a][k] * dx;
            }
        }
        e
    }
}

impl<'a> Recovery<'a> {
    /// Samples the strain at the same Gauss points as [`evaluate`].
    pub fn new(grid: &'a Grid, u: &'a [V3]) -> Recovery<'a> {
        let (dn, _) = crate::solve::grads(grid.h);
        let mut samples = vec![[0.0; 9]; grid.cells()];
        for (c, sample) in samples.iter_mut().enumerate() {
            if !grid.active(c) {
                continue;
            }
            let uc = grid.cell_nodes(c).map(|n| u[n]);
            let dn_c = crate::solve::point_grads(grid, c, &dn);
            let mut sum = [0.0; 9];
            let mut fill = 0.0;
            for (q, g) in grid.gp[c].iter().enumerate() {
                if g.fill > 0 {
                    let f = g.fill as f64 / 255.0;
                    add_sample(&mut sum, grid, c, q, &strain(&uc, &dn_c[q]), f);
                    fill += f;
                }
            }
            *sample = sum.map(|v| (v / fill) as f32);
        }
        let mut recovery = Recovery { grid, u, samples, kept: Vec::new() };
        recovery.correct_samples();
        recovery
    }

    /// The active cell a quarter cell inside the surface, or the cell at the surface itself.
    pub fn cell_for(&self, p: V3, n: V3) -> Option<usize> {
        if self.samples.is_empty() {
            return None;
        }
        let [hx, hy, hz] = self.grid.h;
        let h = hx.min(hy).min(hz);
        let inside = std::array::from_fn(|a| p[a] - n[a] * h / 4.0);
        [inside, p].into_iter().map(|x| self.grid.locate(x).0).find(|&c| self.grid.active(c))
    }

    /// The cells reachable from `c` inside its three-by-three-by-three block. A filled
    /// point must face another filled point across each face the flood crosses.
    fn patch(&self, c: usize) -> ([usize; 27], usize) {
        let grid = self.grid;
        let seed = grid.cell_ijk(c);
        let lo = seed.map(|i| i.saturating_sub(1));
        let hi: [usize; 3] = std::array::from_fn(|a| (seed[a] + 1).min(grid.n[a] - 1));
        let slot = |p: [usize; 3]| (p[0] - lo[0]) + 3 * ((p[1] - lo[1]) + 3 * (p[2] - lo[2]));
        let mut seen = [false; 27];
        seen[slot(seed)] = true;
        let mut cells = [c; 27];
        let (mut next, mut count) = (0, 1);
        while next < count {
            let from = cells[next];
            next += 1;
            let p = grid.cell_ijk(from);
            for a in 0..3 {
                for side in 0..2 {
                    let mut to = p;
                    to[a] = if side == 0 { p[a].wrapping_sub(1) } else { p[a] + 1 };
                    if !(lo[a]..=hi[a]).contains(&to[a]) || seen[slot(to)] {
                        continue;
                    }
                    let j = grid.cell(to[0], to[1], to[2]);
                    if joined(grid, from, j, a, side) {
                        seen[slot(to)] = true;
                        cells[count] = j;
                        count += 1;
                    }
                }
            }
        }
        (cells, count)
    }

    /// The slope of each displacement gradient column along its own axis at `xi`: `S[i][j]`,
    /// the slope along axis j of du_i/dx_j, which a trilinear cell does not have. From the two
    /// sound neighbours along j that c meets across its faces (centred), or one (one-sided). With
    /// neither, from the sound pair along j nearest the point among the cells joined to c in its
    /// 3 x 3 x 3 block, so never across a gap. With none there either, zero: a wall too thin to
    /// have the slope. A neighbour counts through the same face test `patch` floods by.
    fn slopes(&self, c: usize, xi: V3) -> [[f64; 3]; 3] {
        let grid = self.grid;
        let neighbour = |cell, axis: usize, side: isize| {
            let mut at = grid.cell_ijk(cell);
            at[axis] = at[axis].checked_add_signed(side)?;
            if at[axis] >= grid.n[axis] {
                return None;
            }
            let n = grid.cell(at[0], at[1], at[2]);
            sound(grid, n).then_some(n)
        };
        let own = |axis: usize, side: isize| neighbour(c, axis, side).filter(|&n| joined(grid, c, n, axis, usize::from(side > 0)));
        // A slope is the same everywhere in a quadratic field, so a borrowed pair is read at the
        // point's cross coordinates clamped into it rather than extrapolated (the gradients'
        // weights grow fourfold out to three half-cells).
        let borrowed = |j: usize, cells: &[usize]| {
            let seed = grid.cell_ijk(c);
            let mut best = None;
            let mut distance = f64::INFINITY;
            for &n in cells {
                if !sound(grid, n) {
                    continue;
                }
                let at = grid.cell_ijk(n);
                let xi_n: V3 = std::array::from_fn(|a| xi[a] - 2.0 * (at[a] as f64 - seed[a] as f64));
                for end in [-1, 1] {
                    let Some(m) = neighbour(n, j, end) else { continue };
                    let mut delta = xi_n;
                    delta[j] -= end as f64;
                    let dd = (0..3).map(|a| (delta[a] * grid.h[a] / 2.0).powi(2)).sum::<f64>();
                    if dd < distance {
                        distance = dd;
                        let (lo, hi) = if end < 0 { (m, n) } else { (n, m) };
                        let inside: V3 = std::array::from_fn(|a| if a == j { xi_n[a] } else { xi_n[a].clamp(-1.0, 1.0) });
                        best = Some((lo, hi, inside, 1.0));
                    }
                }
            }
            best
        };
        let mut patch = None;
        let mut s = [[0.0; 3]; 3];
        for j in 0..3 {
            let pair = match (own(j, -1), own(j, 1)) {
                (Some(lo), Some(hi)) => Some((lo, hi, xi, 2.0)),
                (None, Some(hi)) => Some((c, hi, xi, 1.0)),
                (Some(lo), None) => Some((lo, c, xi, 1.0)),
                (None, None) => {
                    let (cells, count) = *patch.get_or_insert_with(|| self.patch(c));
                    borrowed(j, &cells[1..count])
                }
            };
            let Some((lo, hi, at, span)) = pair else { continue };
            let dn = gradients_at(at);
            let column = |cell| -> V3 {
                let mut col = [0.0; 3];
                for (a, node) in grid.cell_nodes(cell).into_iter().enumerate() {
                    let weight = dn[a][j] / grid.h[j];
                    for i in 0..3 {
                        col[i] += self.u[node][i] * weight;
                    }
                }
                col
            };
            let (lower, upper) = (column(lo), column(hi));
            for i in 0..3 {
                s[i][j] = (upper[i] - lower[i]) / (span * grid.h[j]);
            }
        }
        s
    }

    /// A trilinear cell has no slope of du_i/dx_j along j, so its sample holds that
    /// term's value at the cell's mid-plane along j. Carry each term from its mid-plane
    /// to the sample centroid using its recovered slope.
    fn correct_samples(&mut self) {
        let mut corrections = Vec::new();
        let mut kept_slopes = Vec::new();
        for (c, sample) in self.samples.iter().enumerate() {
            let d: V3 = std::array::from_fn(|a| f64::from(sample[6 + a]));
            if d == [0.0; 3] || !self.grid.active(c) {
                continue;
            }
            let xi = std::array::from_fn(|a| 2.0 * d[a] / self.grid.h[a]);
            let s = self.slopes(c, xi);
            let kept: [f32; 9] = std::array::from_fn(|n| s[n / 3][n % 3] as f32);
            let s: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| f64::from(kept[i * 3 + j])));
            let add = voigt_slopes(&s);
            let correction: [f64; 6] = std::array::from_fn(|k| (0..3).map(|a| d[a] * add[a][k]).sum());
            corrections.push((c, correction));
            kept_slopes.push((c as u32, kept));
        }
        for (c, correction) in corrections {
            for (value, delta) in self.samples[c][..6].iter_mut().zip(correction) {
                *value = (f64::from(*value) + delta) as f32;
            }
        }
        self.kept = kept_slopes;
    }

    /// The complete strain-gradient target at the sample centroid. Centred differences
    /// give the trilinear element's slopes and the own-direction slopes complete them.
    /// For a quadratic displacement, `correct_samples` gives the matching centroid
    /// strain wherever the needed slopes are available.
    fn cell_gradient(&self, c: usize) -> [[f64; 6]; 3] {
        let h = self.grid.h;
        let xi: V3 = std::array::from_fn(|a| 2.0 * self.samples[c][6 + a] as f64 / h[a]);
        let uc = self.grid.cell_nodes(c).map(|n| self.u[n]);
        let eps = |p| {
            let dn = gradients_at(p).map(|d| std::array::from_fn(|a| d[a] / h[a]));
            strain(&uc, &dn)
        };
        let s = match self.kept.binary_search_by_key(&(c as u32), |&(cell, _)| cell) {
            Ok(index) => {
                let kept = &self.kept[index].1;
                std::array::from_fn(|i| std::array::from_fn(|j| f64::from(kept[i * 3 + j])))
            }
            Err(_) => self.slopes(c, xi),
        };
        let add = voigt_slopes(&s);
        std::array::from_fn(|a| {
            let (mut plus, mut minus) = (xi, xi);
            plus[a] += 0.5;
            minus[a] -= 0.5;
            let (plus, minus) = (eps(plus), eps(minus));
            std::array::from_fn(|k| (plus[k] - minus[k]) / (h[a] / 2.0) + add[a][k])
        })
    }

    /// The linear strain fit for active cell `c` (FORCE A6 2.1, steps 1 to 3). Where the
    /// samples' weighted second moment along a direction is well over (h/4)^2, they give
    /// the slope. Where the samples span it poorly, the fit leans on the completed cell
    /// gradient. For a quadratic displacement with the needed slopes available,
    /// `correct_samples` makes the centroid strains exact. The samples and target then
    /// give the same linear strain field for any lambda, to the stored samples' precision.
    pub fn fit(&self, c: usize) -> Fit {
        let grid = self.grid;
        let (cells, count) = self.patch(c);
        let samples = cells[..count].iter().map(|&j| {
            let s = &self.samples[j];
            let center = grid.cell_center(j);
            let x: V3 = std::array::from_fn(|a| center[a] + s[6 + a] as f64);
            let w: f64 = grid.gp[j].iter().map(|g| g.fill as f64 / 255.0).sum();
            (w, x, s)
        });
        let (mut weight, mut x0, mut e0) = (0.0, [0.0; 3], [0.0; 6]);
        for (w, x, s) in samples.clone() {
            weight += w;
            for a in 0..3 {
                x0[a] += w * x[a];
            }
            for k in 0..6 {
                e0[k] += w * s[k] as f64;
            }
        }
        let x0 = x0.map(|v| v / weight);
        let e0 = e0.map(|v| v / weight);
        let mut m = [[0.0; 3]; 3];
        let mut b = [[0.0; 6]; 3];
        for (w, x, s) in samples {
            let dx: V3 = std::array::from_fn(|a| x[a] - x0[a]);
            for a in 0..3 {
                for i in 0..3 {
                    m[a][i] += w * dx[a] * dx[i];
                }
                for k in 0..6 {
                    b[a][k] += w * dx[a] * (s[k] as f64 - e0[k]);
                }
            }
        }
        let gc = self.cell_gradient(c);
        let [hx, hy, hz] = grid.h;
        let lambda = weight * (hx.min(hy).min(hz) / 4.0).powi(2);
        for a in 0..3 {
            m[a][a] += lambda;
            for k in 0..6 {
                b[a][k] += lambda * gc[a][k];
            }
        }
        let g = invert3(&m).map_or(gc, |inv| {
            std::array::from_fn(|a| std::array::from_fn(|k| (0..3).map(|i| inv[a][i] * b[i][k]).sum()))
        });
        Fit { x0, e0, g }
    }

    /// The places of cell `c`'s filled Gauss points, in printer mm (None where a point is empty).
    fn point_places(&self, c: usize) -> [Option<V3>; 8] {
        std::array::from_fn(|q| (self.grid.gp[c][q].fill > 0).then(|| point_at(self.grid, c, q)))
    }

    /// The filled Gauss point of cell `c` nearest to `p`, for its material mixture.
    pub fn nearest_point(&self, c: usize, p: V3) -> Option<usize> {
        nearest_of(&self.point_places(c), p)
    }

    /// The stress at surface point `p` with outward normal `n`, in the nearest point's material.
    pub fn stress_at(&self, m: &Materials, p: V3, n: V3) -> Option<[f64; 6]> {
        let c = self.cell_for(p, n)?;
        let g = self.grid.gp[c][self.nearest_point(c, p)?];
        let (w, s) = g.shares();
        Some(mul6(&m.mix(w, s, g.angle, g.infill), &self.fit(c).at(p)))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Results {
    /// Per cell, the largest von Mises and principal stresses (MPa), lowest safety factor
    /// and mean fill (zero when empty). A cell with surface quads takes its peak stresses
    /// from those quads and, if sound, its mean stress. A cell without quads shows its
    /// own reading. The safety factor includes the weakest point meeting the fill
    /// threshold and all its quads. An unstressed cell has infinite safety factor.
    pub cells: Vec<[f32; 4]>,
    /// Per cell: the safety factor of its own reading, before its surface quads are folded
    /// into `cells` (infinite when empty or unstressed). The optimiser's utilisation field
    /// reads it, as it read the cells before the surface was recovered.
    pub cell_sf: Vec<f32>,
    /// Per cell: its mean stress (Voigt, MPa).
    pub stress: Vec<[f32; 6]>,
    /// Whether each cell is near a fixture, so a reading can distinguish the two slots by place.
    pub near: Vec<bool>,
    /// Recovered von Mises (MPa), largest principal stress (MPa) and safety factor at each
    /// surface quad's centre. Empty without a surface, all NaN where a quad has no active cell.
    pub quads: Vec<[f32; 3]>,
    /// The surface quad giving each of `max_vm`, `max_p1`, `min_sf` and `min_sf_fixed`.
    /// None where the cell reading gives the extreme.
    pub at_quad: [Option<u32>; 4],
    /// The extremes and the cell where each is; `min_sf` away from the fixtures (a fixture
    /// holds its region rigid, so the edge of the hold concentrates stress no real support
    /// would), `min_sf_fixed` near them ([`crate::bc::near`]).
    pub max_vm: (f64, usize),
    pub max_p1: (f64, usize),
    pub min_sf: (f64, usize),
    pub min_sf_fixed: (f64, usize),
    /// The largest displacement (mm) and its node.
    pub max_u: (f64, usize),
}

impl Results {
    /// Records which cell and, for a surface reading, which quad gave each strict extreme.
    fn record(&mut self, c: usize, near: bool, quad: Option<u32>, vm: f64, p1: f64, sf: f64) {
        if vm > self.max_vm.0 {
            self.max_vm = (vm, c);
            self.at_quad[0] = quad;
        }
        if p1 > self.max_p1.0 {
            self.max_p1 = (p1, c);
            self.at_quad[1] = quad;
        }
        let (slot, i) = if near { (&mut self.min_sf_fixed, 3) } else { (&mut self.min_sf, 2) };
        if sf < slot.0 {
            *slot = (sf, c);
            self.at_quad[i] = quad;
        }
    }
}

/// Reads cell stresses and safety factors. With a surface, sound cell readings and
/// recovered quads set the stress extremes. All active cells contribute their safety
/// factors, with `near` selecting the fixture slot ([`crate::Posed::near`]).
pub fn evaluate(grid: &Grid, m: &Materials, u: &[V3], near: &[bool], surface: Option<&crate::surface::Surface>) -> Results {
    let (dn, _) = crate::solve::grads(grid.h);
    // each mixture's stiffness and strength checks, made once; the last one looked up (the
    // Gauss points of a cell and its neighbours mostly share it)
    let mut mixtures: Vec<(Mat6, Checks)> = Vec::new();
    let mut made: HashMap<(u8, u8, u8, u16), usize> = HashMap::new();
    let mut last: Option<((u8, u8, u8, u16), usize)> = None;
    let mut mixture = |g: Gp, mixtures: &mut Vec<(Mat6, Checks)>| {
        let key = (g.wall, g.skin, g.angle, g.infill);
        if let Some((at, k)) = last {
            if at == key {
                return k;
            }
        }
        let k = *made.entry(key).or_insert_with(|| {
            let (w, s) = g.shares();
            mixtures.push((m.mix(w, s, g.angle, g.infill), m.checks(w, s, g.angle, g.infill)));
            mixtures.len() - 1
        });
        last = Some((key, k));
        k
    };
    let mut samples = surface.map(|_| vec![[0.0; 9]; grid.cells()]);
    let mut r = Results {
        cells: vec![[0.0, 0.0, f32::INFINITY, 0.0]; grid.cells()],
        cell_sf: vec![f32::INFINITY; grid.cells()],
        stress: vec![[0.0; 6]; grid.cells()],
        near: near.to_vec(),
        quads: Vec::new(),
        at_quad: [None; 4],
        max_vm: (if surface.is_some() { f64::NEG_INFINITY } else { 0.0 }, 0),
        max_p1: (f64::NEG_INFINITY, 0),
        min_sf: (f64::INFINITY, 0),
        min_sf_fixed: (f64::INFINITY, 0),
        max_u: (0.0, 0),
    };
    for c in 0..grid.cells() {
        if !grid.active(c) {
            continue;
        }
        let nodes = grid.cell_nodes(c);
        let uc: [V3; 8] = nodes.map(|n| u[n]);
        let dn_c = crate::solve::point_grads(grid, c, &dn);
        let mut mean = [0.0; 6];
        let mut sample = [0.0; 9];
        let mut fill = 0.0;
        let mut worst: f64 = 0.0;
        for q in 0..8 {
            let g = grid.gp[c][q];
            if g.fill == 0 {
                continue;
            }
            let e = strain(&uc, &dn_c[q]);
            let k = mixture(g, &mut mixtures);
            let (d, checks) = &mixtures[k];
            let sigma = mul6(d, &e);
            let f = g.fill as f64 / 255.0;
            if samples.is_some() {
                add_sample(&mut sample, grid, c, q, &e, f);
            }
            for k in 0..6 {
                mean[k] += f * sigma[k];
            }
            fill += f;
            if g.fill >= SOLID {
                worst = worst.max(checks.failure(&e));
            }
        }
        if fill <= 0.0 {
            continue;
        }
        if let Some(samples) = &mut samples {
            samples[c] = sample.map(|v| (v / fill) as f32);
        }
        let mean = mean.map(|v| v / fill);
        let vm = von_mises(&mean);
        let p1 = principal(&mean)[0];
        let sf = if worst > 0.0 { 1.0 / worst.sqrt() } else { f64::INFINITY };
        r.cells[c] = [vm as f32, p1 as f32, sf as f32, (fill / 8.0) as f32];
        r.cell_sf[c] = sf as f32;
        r.stress[c] = mean.map(|v| v as f32);
        let (vm, p1) = if surface.is_some() && !sound(grid, c) { (f64::NEG_INFINITY, f64::NEG_INFINITY) } else { (vm, p1) };
        r.record(c, near[c], None, vm, p1, sf);
    }
    if let (Some(surface), Some(samples)) = (surface, samples) {
        let mut recovery = Recovery { grid, u, samples, kept: Vec::new() };
        recovery.correct_samples();
        r.quads = vec![[f32::NAN; 3]; surface.quads.len()];
        let mut by_cell = Vec::with_capacity(surface.quads.len());
        for q in 0..surface.quads.len() {
            let p = surface.quad_center[q].map(f64::from);
            let n = surface.quad_normal[q].map(f64::from);
            if let Some(c) = recovery.cell_for(p, n) {
                by_cell.push((c, q));
            }
        }
        by_cell.sort_unstable();
        for group in by_cell.chunk_by(|a, b| a.0 == b.0) {
            let c = group[0].0;
            if !sound(grid, c) {
                r.cells[c][0] = f32::NEG_INFINITY;
                r.cells[c][1] = f32::NEG_INFINITY;
            }
            let fit = recovery.fit(c);
            // the group's point places and mixtures, found once for all its quads
            let places = recovery.point_places(c);
            let mut kinds: [Option<usize>; 8] = [None; 8];
            for &(_, q) in group {
                let p = surface.quad_center[q].map(f64::from);
                let e = fit.at(p);
                let point = nearest_of(&places, p).expect("an active cell has a filled point");
                let k = *kinds[point].get_or_insert_with(|| mixture(grid.gp[c][point], &mut mixtures));
                let (d, checks) = &mixtures[k];
                let sigma = mul6(d, &e);
                let vm = von_mises(&sigma);
                let p1 = principal(&sigma)[0];
                let f = checks.failure(&e);
                let sf = if f > 0.0 { 1.0 / f.sqrt() } else { f64::INFINITY };
                r.quads[q] = [vm as f32, p1 as f32, sf as f32];
                let cell = &mut r.cells[c];
                cell[0] = cell[0].max(vm as f32);
                cell[1] = cell[1].max(p1 as f32);
                cell[2] = cell[2].min(sf as f32);
                r.record(c, near[c], Some(q as u32), vm, p1, sf);
            }
        }
    }
    for (n, v) in u.iter().enumerate() {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if l > r.max_u.0 {
            r.max_u = (l, n);
        }
    }
    if r.max_vm.0 == f64::NEG_INFINITY {
        r.max_vm.0 = 0.0;
    }
    if r.max_p1.0 == f64::NEG_INFINITY {
        r.max_p1.0 = 0.0;
    }
    r
}

/// Two cell sizes' lowest safety factors this close (a share of the lower): converged.
pub const CONVERGED: f64 = 0.1;

/// A lowest safety factor as the cell sizes solved give it: the answer only where a finer cell
/// size keeps it, and where the finest cells keep it on a grid shifted half a cell along every
/// axis ([`crate::study::shift`]). An extreme at a corner or a hold's edge moves with where the
/// cells cut it, by as much as the step to finer cells can (8 % on the bench bracket): two
/// alignments of the finest cells bound that, so it cannot pass for convergence. (Where a
/// fixture's hold concentrates stress, each finer size finds more of it: that is the hold's
/// edge, not the part.) The value reported carries a margin ([`Verdict::reported`]): two sizes
/// that agree can both read a surface peak low by about their gap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    /// One cell size only: its value, unchecked.
    Unchecked(f64),
    /// The finest cells' value (the lower of their two alignments), within [`CONVERGED`] of
    /// the coarser cells' and of the other alignment's.
    Converged { fine: f64, coarse: f64, shifted: Option<f64> },
    /// Apart: the finest cells' value (the lower alignment's), the coarser cells', the other
    /// alignment's.
    Apart { fine: f64, coarse: f64, shifted: Option<f64> },
}

impl Verdict {
    /// From the finest step's value, the coarser step's if there is one, and the finest
    /// step's on the shifted grid if it was solved (infinite: nothing loads it; they agree
    /// only where all are).
    pub fn of(fine: f64, coarse: Option<f64>, shifted: Option<f64>) -> Verdict {
        let Some(coarse) = coarse else { return Verdict::Unchecked(shifted.map_or(fine, |s| s.min(fine))) };
        let (fine, shifted) = match shifted {
            Some(s) if s < fine => (s, Some(fine)),
            s => (fine, s),
        };
        let (lo, hi) = [Some(fine), Some(coarse), shifted].into_iter().flatten().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let agree = if hi.is_finite() { hi - lo <= CONVERGED * lo } else { lo == hi };
        if agree {
            Verdict::Converged { fine, coarse, shifted }
        } else {
            Verdict::Apart { fine, coarse, shifted }
        }
    }

    /// The safety factor it gives, [`Verdict::reported`]: none while the cell sizes disagree.
    pub fn value(&self) -> Option<f64> {
        match *self {
            Verdict::Apart { .. } => None,
            _ => Some(self.reported()),
        }
    }

    /// The safety factor reported, margin included (FORCE A6 2.2): in utilisation (`1 / SF`,
    /// linear in the load), the highest of the readings plus the rise from the coarser cells to
    /// the finer ones (the higher of their two alignments), once more where it rose. A stress
    /// read half a cell inside the face reads a surface peak low by about the step's own
    /// change, so two sizes that agree within [`CONVERGED`] can both read it low by that much:
    /// the rise added once is what puts what is left on the safe side. One cell size: its
    /// reading.
    pub fn reported(&self) -> f64 {
        match *self {
            Verdict::Unchecked(v) => v,
            Verdict::Converged { fine, coarse, shifted } | Verdict::Apart { fine, coarse, shifted } => {
                // `fine` is the lower alignment's: the higher utilisation of the two
                let lowest = shifted.map_or(fine, |s| s.min(fine)).min(coarse);
                let rise = 1.0 / fine - 1.0 / coarse;
                if rise > 0.0 {
                    1.0 / (1.0 / lowest + rise)
                } else {
                    lowest
                }
            }
        }
    }
}

/// A study's safety factor off its last two steps: away from the fixtures and near them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub away: Verdict,
    pub near: Verdict,
}

/// The lowest safety factor within one coarser cell of `at` in printer mm, so the two sizes
/// compare one spot (FORCE A6 2.2). Prefer material in the same fixture slot.
fn paired(coarse: &crate::Outcome, at: V3, near: bool) -> f64 {
    let grid = &coarse.grid;
    let r = &coarse.results;
    let ijk = grid.cell_ijk(grid.locate(at).0);
    let around = |a: usize| ijk[a].saturating_sub(1)..=(ijk[a] + 1).min(grid.n[a] - 1);
    let mut lowest = [None::<f64>; 2];
    for k in around(2) {
        for j in around(1) {
            for i in around(0) {
                let c = grid.cell(i, j, k);
                if r.cells[c][3] > 0.0 {
                    let sf = f64::from(r.cells[c][2]);
                    let slot = &mut lowest[usize::from(r.near[c])];
                    *slot = Some(slot.map_or(sf, |v| v.min(sf)));
                }
            }
        }
    }
    lowest[usize::from(near)].or(lowest[usize::from(!near)]).unwrap_or(if near { r.min_sf_fixed.0 } else { r.min_sf.0 })
}

impl Reading {
    /// The coarser grid's value is read within one coarser cell of the finer grid's lowest
    /// safety factor. When the shifted grid reads lower, its cell gives the place.
    pub fn of(fine: &crate::Outcome, coarse: Option<&crate::Outcome>, shifted: Option<&crate::Outcome>) -> Reading {
        let verdict = |near| {
            let slot = |o: &crate::Outcome| if near { o.results.min_sf_fixed } else { o.results.min_sf };
            let f = slot(fine).0;
            let s = shifted.map(|o| slot(o).0);
            let coarse = coarse.map(|o| {
                let lowest = shifted.filter(|a| slot(a).0 < f).unwrap_or(fine);
                let (sf, cell) = slot(lowest);
                if sf.is_finite() {
                    paired(o, lowest.grid.cell_center(cell), near)
                } else {
                    slot(o).0
                }
            });
            Verdict::of(f, coarse, s)
        };
        Reading { away: verdict(false), near: verdict(true) }
    }

    /// The part's safety factor as reported ([`Verdict::reported`]): away from the fixtures,
    /// or near them where the cell sizes agree there and it is lower (near a fixture apart is
    /// the hold's edge, left out); none while away is apart.
    pub fn sf(&self) -> Option<f64> {
        let away = self.away.value()?;
        Some(match self.near {
            Verdict::Converged { .. } => self.near.reported().min(away),
            _ => away,
        })
    }

    /// The value a row shows: the reported safety factor where the cell sizes agree, else the
    /// finer cells' reading (said as not converged).
    pub fn shown(&self) -> f64 {
        self.sf().unwrap_or_else(|| self.finest())
    }

    /// What the finer cells read, the hold's edge left out: away's finer value, or near's where
    /// the cell sizes agree there and it is lower (no margin).
    pub fn finest(&self) -> f64 {
        let fine = |v: Verdict| match v {
            Verdict::Unchecked(v) | Verdict::Converged { fine: v, .. } | Verdict::Apart { fine: v, .. } => v,
        };
        match self.near {
            Verdict::Converged { fine: near, .. } => near.min(fine(self.away)),
            _ => fine(self.away),
        }
    }
}

/// A cell value averaged onto the nodes (fill-weighted over the cells around each), so a
/// surface coloured by it is smooth. `k` indexes [`Results::cells`]' values.
pub fn nodal(grid: &Grid, r: &Results, k: usize) -> Vec<f32> {
    nodal_by(grid, r, |c| c[k])
}

/// [`nodal`] of what `value` makes of each cell's [`Results::cells`] entry.
pub fn nodal_by(grid: &Grid, r: &Results, value: impl Fn(&[f32; 4]) -> f32) -> Vec<f32> {
    let mut sum = vec![0.0f32; grid.nodes()];
    let mut w = vec![0.0f32; grid.nodes()];
    for c in 0..grid.cells() {
        let f = r.cells[c][3];
        let v = value(&r.cells[c]);
        if f <= 0.0 || !v.is_finite() {
            continue;
        }
        for n in grid.cell_nodes(c) {
            sum[n] += f * v;
            w[n] += f;
        }
    }
    sum.iter().zip(&w).map(|(s, w)| if *w > 0.0 { s / w } else { f32::INFINITY }).collect()
}

/// A displacement field carried onto another grid's nodes (each sampled where it sits), as
/// a solve on that grid starts ([`crate::solve::start`]): the shifted grid's from the finest.
pub fn carry(from: &Grid, u: &[V3], to: &Grid) -> Vec<V3> {
    (0..to.nodes()).map(|n| displacement(from, u, to.node_pos(n))).collect()
}

/// The displacement at a point (the element's own interpolation).
pub fn displacement(grid: &Grid, u: &[V3], p: V3) -> V3 {
    let (c, xi) = grid.locate(p);
    let n = crate::element::shape_at(xi);
    let nodes = grid.cell_nodes(c);
    let mut v = [0.0; 3];
    for i in 0..8 {
        for a in 0..3 {
            v[a] += n[i] * u[nodes[i]][a];
        }
    }
    v
}
