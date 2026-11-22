//! Linear buckling of a solved study: the factor on all its loads at which the part bows out
//! sideways, and how it bows.
//!
//! Static: `K u0 = f`. Buckling: `K phi = lambda (-K_G(u0)) phi`, the smallest positive
//! `lambda` the factor. Posed the other way round, `(-K_G) phi = mu K phi` with `mu = 1/lambda`,
//! the largest `mu` is wanted: `K` (positive definite on the free degrees of freedom) is the
//! inner product and what the multigrid inverts, `K_G` (indefinite) is only applied. `K_G` is
//! linear in `u0`, so in the load: a load scaled by `s` buckles at `lambda / s`. A largest `mu`
//! at most 0 means nothing is in compression enough to buckle: the part does not buckle under
//! this load (a valid answer, not an error).
//!
//! - [`geometric_apply`]: `K_G(u0) x`, matrix-free, point by point on the analysed level. At
//!   each Gauss point the reference stress `sigma0 = D strain(u0)` (recomputed from `u0`, not
//!   stored) gives `y_i += w (grad N_i . sigma0 . grad N_j) x_j`, axis by axis, with the weights
//!   `K` takes (the fictitious share of empty octants included): `K_G` is then the second
//!   variation of the same discrete energy `K` comes from, so an octant with almost no
//!   stiffness has almost no geometric stiffness either and cannot host a low mode.
//! - [`lowest`]: block inverse (subspace) iteration with Rayleigh-Ritz, `X <- K^-1 (-K_G X)`,
//!   each `K^-1` the multigrid-preconditioned CG solve to a tight tolerance (Bathe, "Finite
//!   Element Procedures", Prentice Hall 1996, section 11.6, subspace iteration). The plainest
//!   correct method on purpose: the reference the production eigensolver is checked against,
//!   slow (one full solve per vector and step) but with nothing in it that could hide an error
//!   in `K_G`. It converges to the modes of largest `|mu|`: a mode under the load reversed
//!   (negative `mu`) can take a place in the block, so the block keeps one more than wanted.
//!
//! What this is: linear buckling of an elastic part under dead loads. A pressure keeps its
//! direction as the part bows (a follower pressure adds a load-stiffness term, which matters for
//! shells under pressure, not for printed brackets). The factor is an upper bound that a real,
//! imperfect part reaches less of; no knock-down is applied here. The printed material enters
//! through `K` and through `sigma0` (the mixture's mean stress, the one for modes larger than a
//! cell); the buckling of infill walls or of a skin between infill lines is under the cell size
//! and is not seen.

use crate::element::{mul6, strain, V3};
use crate::solve::{grads, solve_until, Hierarchy, Level, Options};

/// `y = K_G(u0) x` on `level`, the analysed level (held degrees of freedom come out zero).
pub fn geometric_apply(level: &Level, u0: &[V3], x: &[V3], y: &mut [V3]) {
    let g = &level.grid;
    let (gp_d, ds) = level.point_materials().expect("the analysed level has Gauss points");
    let (dn, vol) = grads(g.h);
    y.iter_mut().for_each(|v| *v = [0.0; 3]);
    for c in 0..g.cells() {
        if !g.active(c) {
            continue;
        }
        let nodes = g.cell_nodes(c);
        let u: [V3; 8] = nodes.map(|n| u0[n]);
        let v: [V3; 8] = nodes.map(|n| x[n]);
        let dn_c = crate::solve::point_grads(g, c, &dn);
        let mut f = [[0.0; 3]; 8];
        for q in 0..8 {
            let w = vol * g.gp[c][q].weight();
            let s = mul6(&ds[gp_d[c][q] as usize], &strain(&u, &dn_c[q]));
            let sigma = [[s[0], s[5], s[4]], [s[5], s[1], s[3]], [s[4], s[3], s[2]]];
            // the displacement gradient H = sum_j x_j (x) grad N_j, then P = H sigma0
            let mut hg = [[0.0; 3]; 3];
            for (vj, dj) in v.iter().zip(&dn_c[q]) {
                for a in 0..3 {
                    for b in 0..3 {
                        hg[a][b] += vj[a] * dj[b];
                    }
                }
            }
            let p: [[f64; 3]; 3] = std::array::from_fn(|a| std::array::from_fn(|k| (0..3).map(|b| hg[a][b] * sigma[b][k]).sum()));
            for (fi, di) in f.iter_mut().zip(&dn_c[q]) {
                for a in 0..3 {
                    fi[a] += w * (p[a][0] * di[0] + p[a][1] * di[1] + p[a][2] * di[2]);
                }
            }
        }
        for (fi, &n) in f.iter().zip(&nodes) {
            for a in 0..3 {
                y[n][a] += fi[a];
            }
        }
    }
    level.mask(y);
}

/// `y = S x = -K_G(u0) x`.
fn stress_apply(level: &Level, u0: &[V3], x: &[V3], y: &mut [V3]) {
    geometric_apply(level, u0, x, y);
    y.iter_mut().for_each(|v| *v = v.map(|c| -c));
}

/// The eigensolver's knobs.
#[derive(Clone, Copy, Debug)]
pub struct Eigen {
    /// Vectors in the block: the modes wanted and one more.
    pub block: usize,
    /// Stop when each wanted `mu` moved less than this share of the largest in a step.
    pub settle: f64,
    /// Steps before it returns what it has, unconverged (never an error).
    pub max_steps: usize,
    /// The inner solves (`K^-1`): the hierarchy's options with a tight tolerance.
    pub solve: Options,
}

impl Default for Eigen {
    fn default() -> Self {
        Eigen { block: 3, settle: 1e-6, max_steps: 100, solve: Options { tolerance: 1e-8, max_iterations: 4000, ..Options::default() } }
    }
}

/// The modes found, the most critical first (largest `mu`, algebraically).
#[derive(Clone, Debug, Default)]
pub struct Modes {
    /// `1/lambda` per mode; at most 0: it does not buckle under the load.
    pub mu: Vec<f64>,
    /// Each mode, K-normalised (`phi' K phi = 1`); a mode has no sign.
    pub vectors: Vec<Vec<V3>>,
    /// Each mode's residual `|S phi - mu K phi| / (|mu| |K phi|)`.
    pub residual: Vec<f64>,
    /// Steps taken, and CG iterations over all the inner solves.
    pub steps: usize,
    pub cg: usize,
    /// The wanted modes settled within [`Eigen::max_steps`].
    pub converged: bool,
    /// [`lobpcg`]: the preconditioner's solves, and whether the block grew a third vector (a
    /// cluster of modes held the first back).
    pub solves: usize,
    pub grew: bool,
}

impl Modes {
    /// Mode `k`'s load factor: `1/mu`, infinite where it does not buckle.
    pub fn factor(&self, k: usize) -> f64 {
        match self.mu.get(k) {
            Some(&mu) if mu > 0.0 => 1.0 / mu,
            _ => f64::INFINITY,
        }
    }
}

fn dot(a: &[V3], b: &[V3]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).sum()
}

/// `sum_i c_i v_i`.
fn combine(v: &[Vec<V3>], c: impl Fn(usize) -> f64) -> Vec<V3> {
    let mut out = vec![[0.0; 3]; v[0].len()];
    for (i, vi) in v.iter().enumerate() {
        let ci = c(i);
        if ci == 0.0 {
            continue;
        }
        for (o, x) in out.iter_mut().zip(vi) {
            for a in 0..3 {
                o[a] += ci * x[a];
            }
        }
    }
    out
}

/// A start vector: a deterministic noise (`seed` per vector) on the free degrees of freedom.
fn noise(level: &Level, seed: u64) -> Vec<V3> {
    let mut v: Vec<V3> = (0..level.grid.nodes()).map(|n| {
        std::array::from_fn(|a| {
            // splitmix64 of (seed, node, axis), to -1..1
            let mut z = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ ((n as u64) << 2 | a as u64).wrapping_add(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            (z >> 11) as f64 / (1u64 << 52) as f64 - 1.0
        })
    }).collect();
    level.mask(&mut v);
    v
}

/// The eigenvalues (descending) and unit eigenvectors of a symmetric `n x n` matrix (row major):
/// `vecs[i * n + k]` is component `i` of vector `k`. Cyclic Jacobi rotations (Numerical
/// Recipes, 11.1), as [`crate::surface::eigen3`] for 3 x 3.
pub fn symmetric_eigen(n: usize, m: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let mut a = m.to_vec();
    let mut v = vec![0.0; n * n];
    (0..n).for_each(|i| v[i * n + i] = 1.0);
    let scale: f64 = m.iter().map(|x| x * x).sum();
    for _ in 0..64 {
        let off: f64 = (0..n).flat_map(|p| (p + 1..n).map(move |q| (p, q))).map(|(p, q)| a[p * n + q] * a[p * n + q]).sum();
        if off <= 1e-30 * scale {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                if a[p * n + q] == 0.0 {
                    continue;
                }
                let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * a[p * n + q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (kp, kq) = (a[k * n + p], a[k * n + q]);
                    a[k * n + p] = c * kp - s * kq;
                    a[k * n + q] = s * kp + c * kq;
                }
                for k in 0..n {
                    let (pk, qk) = (a[p * n + k], a[q * n + k]);
                    a[p * n + k] = c * pk - s * qk;
                    a[q * n + k] = s * pk + c * qk;
                }
                for k in 0..n {
                    let (kp, kq) = (v[k * n + p], v[k * n + q]);
                    v[k * n + p] = c * kp - s * kq;
                    v[k * n + q] = s * kp + c * kq;
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&x, &y| a[y * n + y].total_cmp(&a[x * n + x]));
    let vals = order.iter().map(|&k| a[k * n + k]).collect();
    let mut vecs = vec![0.0; n * n];
    for (j, &k) in order.iter().enumerate() {
        for i in 0..n {
            vecs[i * n + j] = v[i * n + k];
        }
    }
    (vals, vecs)
}

/// Rayleigh-Ritz of `(S, K)` on the span of `y` (`ky`, `sy` its images): the Ritz values,
/// largest first, and per value its coefficients on `y` (K-orthonormal Ritz vectors). Directions
/// the span lacks (its K-Gram nearly singular) are dropped: fewer values come back.
fn ritz(y: &[Vec<V3>], ky: &[Vec<V3>], sy: &[Vec<V3>]) -> (Vec<f64>, Vec<Vec<f64>>) {
    let m = y.len();
    let gram = |b: &[Vec<V3>]| -> Vec<f64> {
        let mut g = vec![0.0; m * m];
        for i in 0..m {
            for j in i..m {
                let v = 0.5 * (dot(&y[i], &b[j]) + dot(&y[j], &b[i]));
                g[i * m + j] = v;
                g[j * m + i] = v;
            }
        }
        g
    };
    let (gk, gs) = (gram(ky), gram(sy));
    let (kv, kvec) = symmetric_eigen(m, &gk);
    let top = kv.first().copied().unwrap_or(0.0);
    let keep: Vec<usize> = (0..m).filter(|&k| top > 0.0 && kv[k] > 1e-12 * top).collect();
    let r = keep.len();
    // Z = V D^-1/2: K-orthonormal combinations of y
    let z: Vec<Vec<f64>> = keep.iter().map(|&k| (0..m).map(|i| kvec[i * m + k] / kv[k].sqrt()).collect()).collect();
    let mut c = vec![0.0; r * r];
    for a in 0..r {
        for b in 0..r {
            c[a * r + b] = (0..m).map(|i| (0..m).map(|j| z[a][i] * gs[i * m + j] * z[b][j]).sum::<f64>()).sum();
        }
    }
    for a in 0..r {
        for b in a + 1..r {
            let v = 0.5 * (c[a * r + b] + c[b * r + a]);
            c[a * r + b] = v;
            c[b * r + a] = v;
        }
    }
    let (mu, w) = symmetric_eigen(r, &c);
    let q = (0..r).map(|k| (0..m).map(|i| (0..r).map(|a| z[a][i] * w[a * r + k]).sum()).collect()).collect();
    (mu, q)
}

/// The modes of `h`'s analysed level under the static answer `u0`: [`Eigen::block`] of them by
/// block inverse iteration with Rayleigh-Ritz, the wanted ones (all but the last) settled to
/// [`Eigen::settle`]. `stop` hears each step's count and Ritz values (largest `|mu|` first) and
/// may end it: a stopped or unsettled run returns what it has, `converged` false.
pub fn lowest(h: &Hierarchy, u0: &[V3], e: &Eigen, stop: &mut dyn FnMut(usize, &[f64]) -> bool) -> Modes {
    let fine = &h.levels[0];
    let n = u0.len();
    let m = e.block.max(2);
    let wanted = m - 1;
    let image = |v: &[V3], s: bool| {
        let mut out = vec![[0.0; 3]; n];
        if s {
            stress_apply(fine, u0, v, &mut out);
        } else {
            fine.apply(v, &mut out);
        }
        out
    };
    let mut seed = 0u64;
    let mut x: Vec<Vec<V3>> = (0..m).map(|_| {
        seed += 1;
        noise(fine, seed)
    }).collect();
    let mut last: Vec<f64> = Vec::new();
    let mut out = Modes::default();
    for step in 1..=e.max_steps {
        // Y = K^-1 S X, each solve from its vector (scaled to fit, as a converged mode's is)
        let mut y = Vec::with_capacity(m);
        for xj in &x {
            let b = image(xj, true);
            let s = solve_until(h, &b, Some(xj), &e.solve, &mut |_, _| false);
            out.cg += s.iterations;
            y.push(s.u);
        }
        if y.iter().all(|v| dot(v, v) == 0.0) {
            // nothing is stressed: no mode buckles
            out.steps = step;
            out.converged = true;
            out.mu = vec![0.0];
            out.vectors = vec![vec![[0.0; 3]; n]];
            out.residual = vec![0.0];
            return out;
        }
        let ky: Vec<Vec<V3>> = y.iter().map(|v| image(v, false)).collect();
        let sy: Vec<Vec<V3>> = y.iter().map(|v| image(v, true)).collect();
        let (mu, q) = ritz(&y, &ky, &sy);
        x = q.iter().map(|c| combine(&y, |i| c[i])).collect();
        // the span lost a direction (S has a null space): fresh noise in its place
        while x.len() < m {
            seed += 1;
            x.push(noise(fine, seed));
        }
        // the wanted values: the largest |mu|, which the iteration converges first
        let mut by_size = mu.clone();
        by_size.sort_by(|a, b| b.abs().total_cmp(&a.abs()));
        let top = by_size.first().map_or(0.0, |v| v.abs());
        let settled = last.len() == by_size.len() && by_size.iter().zip(&last).take(wanted).all(|(a, b)| (a - b).abs() <= e.settle * top);
        last = by_size.clone();
        out.steps = step;
        let stopped = stop(step, &by_size);
        if settled || stopped || step == e.max_steps {
            out.converged = settled;
            // the Ritz pairs, most critical first, with their residuals
            out.residual = q.iter().zip(&mu).map(|(c, &mu)| {
                let kx = combine(&ky, |i| c[i]);
                let sx = combine(&sy, |i| c[i]);
                let r: Vec<V3> = sx.iter().zip(&kx).map(|(s, k)| [s[0] - mu * k[0], s[1] - mu * k[1], s[2] - mu * k[2]]).collect();
                dot(&r, &r).sqrt() / (mu.abs() * dot(&kx, &kx).sqrt()).max(f64::MIN_POSITIVE)
            }).collect();
            out.mu = mu;
            out.vectors = x.into_iter().take(out.mu.len()).collect();
            return out;
        }
    }
    out
}

/// The production eigensolver's knobs: A1's loose stops, a 1-5 % discretisation error deserves
/// no finer.
#[derive(Clone, Copy, Debug)]
pub struct Loose {
    /// Vectors in the block: the mode and a guard (a close second mode shows in the guard
    /// without the guard converging).
    pub block: usize,
    /// Settled: the first `mu` moved less than this share of itself in a step, and its residual
    /// is under [`Loose::residual`].
    pub settle: f64,
    pub residual: f64,
    /// Steps after which it returns what it has, unsettled and said so (never an error).
    pub max_steps: usize,
    /// Steps between recomputing the block's images (`K X`, `S X`), else kept by combination.
    pub refresh: usize,
}

impl Default for Loose {
    fn default() -> Self {
        Loose { block: 2, settle: 1e-4, residual: 1e-2, max_steps: 60, refresh: 8 }
    }
}

/// Where [`lobpcg`] starts.
pub enum Start {
    /// The first step of a study: the three rigid rotations about the held nodes' centroid,
    /// each passed once through `T S`: `K_G` turns a rotation into the sideways load of the
    /// compressed members, so these are sway shapes weighted by where the compression is.
    Rotations,
    /// Modes carried from another grid of the same part ([`crate::results::carry`]).
    Modes(Vec<Vec<V3>>),
}

/// The three rigid rotations of `level`'s free degrees of freedom about its held nodes'
/// centroid (the grid's centre when nothing is held).
pub fn rotations(level: &Level) -> Vec<Vec<V3>> {
    let g = &level.grid;
    let held: Vec<usize> = (0..g.nodes()).filter(|&n| level.held[n] != 0).collect();
    let c: V3 = if held.is_empty() {
        std::array::from_fn(|a| g.origin[a] + 0.5 * g.n[a] as f64 * g.h[a])
    } else {
        let mut c = [0.0; 3];
        for &n in &held {
            let p = g.node_pos(n);
            (0..3).for_each(|a| c[a] += p[a]);
        }
        c.map(|v| v / held.len() as f64)
    };
    (0..3).map(|axis| {
        let mut v: Vec<V3> = (0..g.nodes()).map(|n| {
            let p = g.node_pos(n);
            let r = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
            let e: V3 = std::array::from_fn(|a| if a == axis { 1.0 } else { 0.0 });
            [e[1] * r[2] - e[2] * r[1], e[2] * r[0] - e[0] * r[2], e[0] * r[1] - e[1] * r[0]]
        }).collect();
        level.mask(&mut v);
        v
    }).collect()
}

/// [`lobpcg`]'s preconditioner: `T` (about `K^-1`) of each vector of a block, in its order; None
/// when stale.
pub type Precondition<'a> = dyn FnMut(&[Vec<V3>]) -> Result<Option<Vec<Vec<V3>>>, String> + 'a;

/// `precondition` of `block`, checked to answer each vector of it.
fn solve_block(precondition: &mut Precondition, block: &[Vec<V3>]) -> Result<Option<Vec<Vec<V3>>>, String> {
    let Some(t) = precondition(block)? else { return Ok(None) };
    if t.len() != block.len() {
        return Err(format!("the preconditioner answered {} of a block of {} vectors", t.len(), block.len()));
    }
    Ok(Some(t))
}

/// The most critical modes of `fine` under the static answer `u0`, the production way: LOBPCG
/// (Knyazev, "Toward the optimal preconditioned eigensolver", SIAM J. Sci. Comput. 23, 2001) on
/// `(-K_G) phi = mu K phi`, largest `mu` first, with `precondition` (`T`, about `K^-1`: a
/// multigrid solve to a loose tolerance) on each residual, a step's residuals in one call (a
/// block, which a hub solves in one session). Each step one `T`, one `K` and one
/// `K_G` per vector; the rest of the images are combined from the last ones and recomputed every
/// [`Loose::refresh`] steps against drift. The block grows from [`Loose::block`] by one when the
/// first residual has not halved in 5 steps (three modes in a cluster). `stop` hears each step's
/// count, first `mu` and its residual. None when `precondition` or `stop` says stale; a run
/// past [`Loose::max_steps`] returns what it has, `converged` false.
pub fn lobpcg(fine: &Level, u0: &[V3], start: Start, k: &Loose, precondition: &mut Precondition, stop: &mut dyn FnMut(usize, f64, f64) -> bool) -> Result<Option<Modes>, String> {
    let n = u0.len();
    let kx = |v: &[V3]| {
        let mut out = vec![[0.0; 3]; n];
        fine.apply(v, &mut out);
        out
    };
    let sx = |v: &[V3]| {
        let mut out = vec![[0.0; 3]; n];
        stress_apply(fine, u0, v, &mut out);
        out
    };
    let mut out = Modes::default();
    let mut seed = 0u64;
    // T S v of each v in one block (zero where S v is), None when stale
    let through = |vs: &[Vec<V3>], out: &mut Modes, precondition: &mut Precondition| -> Result<Option<Vec<Vec<V3>>>, String> {
        let s: Vec<Vec<V3>> = vs.iter().map(|v| sx(v)).collect();
        let stressed: Vec<usize> = (0..s.len()).filter(|&j| dot(&s[j], &s[j]) != 0.0).collect();
        let mut t = vec![vec![[0.0; 3]; n]; s.len()];
        if stressed.is_empty() {
            return Ok(Some(t));
        }
        out.solves += stressed.len();
        let block: Vec<Vec<V3>> = stressed.iter().map(|&j| s[j].clone()).collect();
        let Some(solved) = solve_block(precondition, &block)? else { return Ok(None) };
        for (j, v) in stressed.into_iter().zip(solved) {
            t[j] = v;
        }
        Ok(Some(t))
    };
    let mut y: Vec<Vec<V3>> = Vec::new();
    match start {
        Start::Rotations => {
            let Some(ts) = through(&rotations(fine), &mut out, precondition)? else { return Ok(None) };
            y.extend(ts.into_iter().filter(|t| dot(t, t) > 0.0));
        }
        Start::Modes(modes) => y.extend(modes.into_iter().filter(|m| m.len() == n).map(|mut m| {
            fine.mask(&mut m);
            m
        }).filter(|m| dot(m, m) > 0.0)),
    }
    let mut m = k.block.max(1);
    // noise passed through T S where the starts fall short of the block
    if y.len() < m {
        let more: Vec<Vec<V3>> = (y.len()..m).map(|_| {
            seed += 1;
            noise(fine, seed)
        }).collect();
        let Some(ts) = through(&more, &mut out, precondition)? else { return Ok(None) };
        for t in ts {
            if dot(&t, &t) == 0.0 {
                // nothing is stressed: no mode buckles
                out.converged = true;
                out.mu = vec![0.0];
                out.vectors = vec![vec![[0.0; 3]; n]];
                out.residual = vec![0.0];
                return Ok(Some(out));
            }
            y.push(t);
        }
    }
    let ky: Vec<Vec<V3>> = y.iter().map(|v| kx(v)).collect();
    let sy: Vec<Vec<V3>> = y.iter().map(|v| sx(v)).collect();
    let (mut mu, q) = ritz(&y, &ky, &sy);
    let take = |q: &[Vec<f64>], from: &[Vec<V3>], m: usize| -> Vec<Vec<V3>> { q.iter().take(m).map(|c| combine(from, |i| c[i])).collect() };
    let (mut x, mut kxs, mut sxs) = (take(&q, &y, m), take(&q, &ky, m), take(&q, &sy, m));
    mu.truncate(x.len());
    let (mut p, mut kp, mut sp): (Vec<Vec<V3>>, Vec<Vec<V3>>, Vec<Vec<V3>>) = (Vec::new(), Vec::new(), Vec::new());
    let mut firsts: Vec<f64> = Vec::new();
    let mut last_mu = f64::NAN;
    for step in 1..=k.max_steps {
        if step % k.refresh.max(1) == 0 {
            kxs = x.iter().map(|v| kx(v)).collect();
            sxs = x.iter().map(|v| sx(v)).collect();
            kp = p.iter().map(|v| kx(v)).collect();
            sp = p.iter().map(|v| sx(v)).collect();
        }
        // residuals S x - mu K x and their sizes
        let r: Vec<Vec<V3>> = (0..x.len()).map(|j| sxs[j].iter().zip(&kxs[j]).map(|(s, kv)| std::array::from_fn(|a| s[a] - mu[j] * kv[a])).collect()).collect();
        let size: Vec<f64> = (0..x.len()).map(|j| dot(&r[j], &r[j]).sqrt() / (mu[j].abs() * dot(&kxs[j], &kxs[j]).sqrt()).max(f64::MIN_POSITIVE)).collect();
        out.steps = step;
        let settled = (mu[0] - last_mu).abs() <= k.settle * mu[0].abs() && size[0] < k.residual;
        last_mu = mu[0];
        firsts.push(size[0]);
        if stop(step, mu[0], size[0]) {
            return Ok(None);
        }
        if settled || mu[0] <= 0.0 && step > 1 || step == k.max_steps {
            out.converged = settled || mu[0] <= 0.0;
            out.residual = size;
            out.mu = mu;
            out.vectors = x;
            return Ok(Some(out));
        }
        // a cluster: one more vector, from noise
        if !out.grew && step > 5 && size[0] > 0.5 * firsts[step - 6] {
            out.grew = true;
            m += 1;
            seed += 1;
            let Some(mut ts) = through(&[noise(fine, seed)], &mut out, precondition)? else { return Ok(None) };
            let t = ts.swap_remove(0);
            if dot(&t, &t) > 0.0 {
                kxs.push(kx(&t));
                sxs.push(sx(&t));
                x.push(t);
                mu.push(0.0);
            }
        }
        // W = T R, the step's residuals in one block
        out.solves += r.len();
        let Some(w) = solve_block(precondition, &r)? else { return Ok(None) };
        let kw: Vec<Vec<V3>> = w.iter().map(|v| kx(v)).collect();
        let sw: Vec<Vec<V3>> = w.iter().map(|v| sx(v)).collect();
        // Rayleigh-Ritz on [X W P]
        let nx = x.len();
        let ys: Vec<Vec<V3>> = x.iter().chain(&w).chain(&p).cloned().collect();
        let kys: Vec<Vec<V3>> = kxs.iter().chain(&kw).chain(&kp).cloned().collect();
        let sys: Vec<Vec<V3>> = sxs.iter().chain(&sw).chain(&sp).cloned().collect();
        let (vals, q) = ritz(&ys, &kys, &sys);
        if q.is_empty() {
            return Ok(Some(out));
        }
        let m_now = m.min(q.len());
        x = take(&q, &ys, m_now);
        kxs = take(&q, &kys, m_now);
        sxs = take(&q, &sys, m_now);
        mu = vals.into_iter().take(m_now).collect();
        // P: the new block's part from W and P (the X part dropped)
        let no_x: Vec<Vec<f64>> = q.iter().take(m_now).map(|c| c.iter().enumerate().map(|(i, &v)| if i < nx { 0.0 } else { v }).collect()).collect();
        p = take(&no_x, &ys, m_now);
        kp = take(&no_x, &kys, m_now);
        sp = take(&no_x, &sys, m_now);
    }
    Ok(Some(out))
}
