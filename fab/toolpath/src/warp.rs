//! In-plane displacement of a TPMS field on a regular grid of trilinear elements.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug)]
pub struct Weights {
    pub a: f64,
    pub surface: f64,
    pub b: f64,
}

impl Default for Weights {
    fn default() -> Self { Weights { a: 8.0, surface: 8.0, b: 1.0 } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Gradient { Linear, Quadratic, Exponential }

impl Gradient {
    pub fn density(self, s: f64, i: f64, t: f64) -> f64 {
        match self {
            Gradient::Linear => s + (i - s) * t,
            Gradient::Quadratic => s + (i - s) * t * t,
            Gradient::Exponential => s * (i / s).powf(t),
        }
    }
}

pub fn shrink(gradient: Gradient, s: f64, i: f64, t: f64, fade: f64) -> f32 {
    ((1.0 - gradient.density(s, i, t) / s) * fade) as f32
}

/// Lower corner, cell width in mm, and number of cells in each dimension.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub origin: [f64; 3],
    pub cell: f64,
    pub n: [usize; 3],
}

pub struct Problem<'a> {
    pub grid: Grid,
    pub solid: &'a [bool],
    pub t: &'a [f32],
    pub delta: &'a [f32],
}

type Pair = [f64; 2];
type Matrix = [[f64; 16]; 16];
const NONE: usize = usize::MAX;

fn coords(i: usize, [nx, ny, _]: [usize; 3]) -> [usize; 3] {
    [i % nx, i / nx % ny, i / (nx * ny)]
}

impl Grid {
    fn corners(&self, cell: usize) -> [usize; 8] {
        let [x, y, z] = coords(cell, self.n);
        let [row, height, _] = self.n.map(|n| n + 1);
        let plane = row * height;
        let i = z * plane + y * row + x;
        [i, i + 1, i + row, i + row + 1, i + plane, i + plane + 1, i + plane + row, i + plane + row + 1]
    }
}

// Shape value and physical x, y, z derivatives at a point of the unit cube.
fn basis(q: [f64; 3], h: f64) -> [[f64; 4]; 8] {
    std::array::from_fn(|i| {
        let factor = |axis: usize| if i & (1 << axis) == 0 { (1.0 - q[axis], -1.0 / h) } else { (q[axis], 1.0 / h) };
        let ((x, dx), (y, dy), (z, dz)) = (factor(0), factor(1), factor(2));
        [x * y * z, dx * y * z, x * dy * z, x * y * dz]
    })
}

struct Element {
    density: Matrix,
    regular: Matrix,
    load: [f64; 16],
}

impl Element {
    fn new(h: f64, b: f64) -> Element {
        let mut e = Element { density: [[0.0; 16]; 16], regular: [[0.0; 16]; 16], load: [0.0; 16] };
        let offset = 0.5 / 3.0_f64.sqrt();
        let gauss = [0.5 - offset, 0.5 + offset];
        let volume = h * h * h / 8.0;
        let tau = 1e-6 / (h * h);
        for z in gauss { for y in gauss { for x in gauss {
            let shape = basis([x, y, z], h);
            for (i, &[ni, xi, yi, zi]) in shape.iter().enumerate() {
                let u = 2 * i;
                e.load[u] += volume * xi;
                e.load[u + 1] += volume * yi;
                for (j, &[nj, xj, yj, zj]) in shape.iter().enumerate() {
                    let v = 2 * j;
                    let (xx, xy, yy) = (xi * xj, xi * yj, yi * yj);
                    // 2 (div/2)' (div/2)' supplies the half in the density Hessian.
                    e.density[u][v] += volume * 0.5 * xx;
                    e.density[u][v + 1] += volume * 0.5 * xy;
                    e.density[v + 1][u] += volume * 0.5 * xy;
                    e.density[u + 1][v + 1] += volume * 0.5 * yy;
                    let stack_pin = 2.0 * (b * zi * zj + tau * ni * nj);
                    e.regular[u][v] += volume * (xx + 2.0 * yy + stack_pin);
                    e.regular[u][v + 1] -= volume * xy;
                    e.regular[v + 1][u] -= volume * xy;
                    e.regular[u + 1][v + 1] += volume * (yy + 2.0 * xx + stack_pin);
                }
            }
        }}}
        e
    }
}

// In the product basis {1, 1-2x} × {1, 1-2y} × {1, 1-2z}, unequal
// parities integrate to zero. Each scalar block is diagonal. The only mixed
// derivative ux_x uy_y couples x to y and xz to yz. Project the two nodal
// Hessians once, so a matvec needs just 16 diagonal and two mixed entries.
struct Modes {
    diag: [Pair; 8],
    xy: Pair,
}

impl Modes {
    fn new(k: &Matrix) -> Modes {
        let sign = |i: usize, mode: usize| if (i & mode).count_ones() % 2 == 0 { 1.0 } else { -1.0 };
        let entry = |m, a: usize, n, b: usize| {
            let mut sum = 0.0;
            for i in 0..8 { for j in 0..8 {
                sum += sign(i, m) * k[2 * i + a][2 * j + b] * sign(j, n);
            }}
            sum / 64.0 // one inverse eight-node transform on each side
        };
        Modes {
            diag: std::array::from_fn(|m| [entry(m, 0, m, 0), entry(m, 1, m, 1)]),
            xy: [entry(1, 0, 2, 1), entry(5, 0, 6, 1)],
        }
    }
}

#[inline]
fn transform(v: &mut [Pair; 8]) {
    // A sum and a difference along each axis. The transform is symmetric.
    for stride in [1, 2, 4] {
        for group in v.chunks_exact_mut(2 * stride) {
            let (lo, hi) = group.split_at_mut(stride);
            for (a, b) in lo.iter_mut().zip(hi) {
                let (x, y) = (*a, *b);
                *a = [x[0] + y[0], x[1] + y[1]];
                *b = [x[0] - y[0], x[1] - y[1]];
            }
        }
    }
}

struct Operator {
    density: Modes,
    regular: Modes,
}

impl Operator {
    #[inline]
    fn apply(&self, w: f64, mut v: [Pair; 8]) -> [Pair; 8] {
        transform(&mut v);
        let mut out = std::array::from_fn(|m| std::array::from_fn(|a|
            (w * self.density.diag[m][a] + self.regular.diag[m][a]) * v[m][a]));
        for (z, (x, y)) in [(1, 2), (5, 6)].into_iter().enumerate() {
            let xy = w * self.density.xy[z] + self.regular.xy[z];
            out[x][0] += xy * v[y][1];
            out[y][1] += xy * v[x][0];
        }
        transform(&mut out);
        out
    }
}

// Number bodies in first-cell order. Cells touching even at a single corner
// share displacement unknowns. Every solid corner receives its body's id.
fn label(p: &Problem) -> (Vec<usize>, usize) {
    let n = p.grid.n;
    let mut owner = vec![NONE; n.map(|k| k + 1).iter().product()];
    let mut seen = vec![false; p.solid.len()];
    let mut queue = VecDeque::new();
    let mut bodies = 0;
    for (start, &inside) in p.solid.iter().enumerate() {
        if !inside || seen[start] { continue; }
        seen[start] = true;
        queue.push_back(start);
        while let Some(c) = queue.pop_front() {
            for i in p.grid.corners(c) { owner[i] = bodies; }
            let [cx, cy, cz] = coords(c, n);
            for z in cz.saturating_sub(1)..=(cz + 1).min(n[2] - 1) {
                for y in cy.saturating_sub(1)..=(cy + 1).min(n[1] - 1) {
                    for x in cx.saturating_sub(1)..=(cx + 1).min(n[0] - 1) {
                        let j = (z * n[1] + y) * n[0] + x;
                        if p.solid[j] && !seen[j] {
                            seen[j] = true;
                            queue.push_back(j);
                        }
                    }
                }
            }
        }
        bodies += 1;
    }
    (owner, bodies)
}

struct SolidCell {
    nodes: [usize; 8],
    weight: f64,
}

fn dot(a: &[Pair], b: &[Pair]) -> f64 {
    a.iter().zip(b).map(|(u, v)| u[0] * v[0] + u[1] * v[1]).sum()
}

struct Solution {
    x: Vec<Pair>,
    iterations: usize,
    residual: f64,
    hit_bound: bool,
}

fn solve_cg(op: &Operator, cells: &[SolidCell], mut r: Vec<Pair>, diag: &[Pair]) -> Solution {
    let mut x = vec![[0.0; 2]; r.len()];
    let f2 = dot(&r, &r);
    if f2 == 0.0 { return Solution { x, iterations: 0, residual: 0.0, hit_bound: false }; }
    let inv: Vec<Pair> = diag.iter().map(|d| [1.0 / d[0], 1.0 / d[1]]).collect();
    let mut dir: Vec<Pair> = r.iter().zip(&inv).map(|(v, d)| [v[0] * d[0], v[1] * d[1]]).collect();
    let mut rho = dot(&r, &dir);
    let mut ap = vec![[0.0; 2]; r.len()];
    let bound = 2 * r.len();
    let mut r2 = f2;
    for iterations in 1..=bound {
        ap.fill([0.0; 2]);
        for cell in cells {
            let v = op.apply(cell.weight, cell.nodes.map(|i| dir[i]));
            for (&i, u) in cell.nodes.iter().zip(v) {
                ap[i][0] += u[0];
                ap[i][1] += u[1];
            }
        }
        let alpha = rho / dot(&dir, &ap);
        r2 = 0.0;
        let mut next = 0.0;
        for i in 0..r.len() {
            for a in 0..2 {
                x[i][a] += alpha * dir[i][a];
                r[i][a] -= alpha * ap[i][a];
                r2 += r[i][a] * r[i][a];
                ap[i][a] = inv[i][a] * r[i][a];
                next += r[i][a] * ap[i][a];
            }
        }
        if r2 <= 1e-12 * f2 { return Solution { x, iterations, residual: (r2 / f2).sqrt(), hit_bound: false }; }
        let beta = next / rho;
        for (d, z) in dir.iter_mut().zip(&ap) {
            *d = [z[0] + beta * d[0], z[1] + beta * d[1]];
        }
        rho = next;
    }
    let residual = (r2 / f2).sqrt();
    eprintln!("TPMS warp reached the CG bound of {bound} iterations, relative residual {residual:e}");
    Solution { x, iterations: bound, residual, hit_bound: true }
}

// Multi-source BFS. Seeds enter in node order, then neighbours in -x, +x,
// -y, +y, -z, +z order. A tie keeps the first source, including its scale.
fn extend(n: [usize; 3], owner: &mut [usize], v: &mut [[f32; 2]]) {
    let mut queue: VecDeque<_> = (0..owner.len()).filter(|&i| owner[i] != NONE).collect();
    let stride = [1, n[0], n[0] * n[1]];
    while let Some(i) = queue.pop_front() {
        let p = coords(i, n);
        let mut visit = |j: usize| {
            if owner[j] == NONE {
                owner[j] = owner[i];
                v[j] = v[i];
                queue.push_back(j);
            }
        };
        for a in 0..3 {
            if p[a] > 0 { visit(i - stride[a]); }
            if p[a] + 1 < n[a] { visit(i + stride[a]); }
        }
    }
}

#[derive(Debug)]
pub struct Warp {
    grid: Grid,
    // Keep the solved f32 nodes. Applying the owner's scale in f64 avoids
    // rounding a guarded cell back across its limit and allows re-guarding.
    v: Vec<[f32; 2]>,
    owner: Vec<usize>,
    scale: Vec<f64>,
    pub iterations: usize,
    /// Euclidean residual relative to the load norm (zero for zero load).
    pub residual: f64,
    pub hit_bound: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct CellStats {
    pub t: f64,
    pub asked: f64,
    pub got: f64,
    pub sigma1: f64,
    pub sigma2: f64,
    pub shear: f64,
}

// Singular values of I - s G, with G stored as its two column vectors.
fn stretches([dx, dy]: [Pair; 2], s: f64) -> Pair {
    let (a, b, c, d) = (1.0 - s * dx[0], -s * dy[0], -s * dx[1], 1.0 - s * dy[1]);
    let plus = (a + d).hypot(c - b);
    let minus = (a - d).hypot(c + b);
    [0.5 * (plus + minus), 0.5 * (plus - minus).abs()]
}

impl Warp {
    pub fn solve(p: &Problem, w: &Weights) -> Warp {
        let grid = p.grid;
        assert!(grid.cell.is_finite() && grid.cell > 0.0, "warp cell width must be finite and positive");
        assert_eq!(p.solid.len(), grid.n.iter().product::<usize>(), "warp mask must cover the grid");
        assert_eq!(p.t.len(), p.solid.len(), "warp depths must cover the grid");
        assert_eq!(p.delta.len(), p.solid.len(), "warp shrink targets must cover the grid");
        let (mut owner, bodies) = label(p);
        let mut active = 0;
        let compact: Vec<_> = owner.iter().map(|&id| {
            if id == NONE { NONE } else { active += 1; active - 1 }
        }).collect();
        let elem = Element::new(grid.cell, w.b);
        let op = Operator { density: Modes::new(&elem.density), regular: Modes::new(&elem.regular) };
        let mut cells = Vec::new();
        let mut f = vec![[0.0; 2]; active];
        let mut diag = vec![[0.0; 2]; active];
        for (i, &inside) in p.solid.iter().enumerate() {
            if !inside { continue; }
            let nodes = grid.corners(i).map(|j| compact[j]);
            let weight = w.a * (1.0 + w.surface * (1.0 - p.t[i] as f64).powi(2));
            let load = weight * p.delta[i] as f64;
            for (k, &j) in nodes.iter().enumerate() {
                for a in 0..2 {
                    let d = 2 * k + a;
                    f[j][a] += load * elem.load[d];
                    diag[j][a] += weight * elem.density[d][d] + elem.regular[d][d];
                }
            }
            cells.push(SolidCell { nodes, weight });
        }
        let sol = solve_cg(&op, &cells, f, &diag);
        let mut v: Vec<_> = compact.into_iter().map(|i| {
            if i == NONE { [0.0; 2] } else { sol.x[i].map(|x| x as f32) }
        }).collect();
        extend(grid.n.map(|k| k + 1), &mut owner, &mut v);
        Warp { grid, v, owner, scale: vec![1.0; bodies], iterations: sol.iterations, residual: sol.residual, hit_bound: sol.hit_bound }
    }

    // Average the four parallel edge differences along each axis. This is
    // the exact centre derivative of the trilinear field, before scaling.
    fn gradient(&self, corners: [usize; 8]) -> [Pair; 3] {
        let v = corners.map(|i| self.v[i].map(f64::from));
        let k = 0.25 / self.grid.cell;
        std::array::from_fn(|axis| {
            let step = 1 << axis;
            let mut g = [0.0; 2];
            for i in 0..8 {
                if i & step == 0 {
                    for a in 0..2 { g[a] += v[i + step][a] - v[i][a]; }
                }
            }
            [k * g[0], k * g[1]]
        })
    }

    pub fn scales(&self) -> &[f64] { &self.scale }

    pub fn guard(&mut self, p: &Problem, sparse: f64) {
        let mut gradients = vec![Vec::new(); self.scale.len()];
        for (i, &inside) in p.solid.iter().enumerate() {
            if !inside { continue; }
            let corners = self.grid.corners(i);
            let [dx, dy, _] = self.gradient(corners);
            gradients[self.owner[corners[0]]].push([dx, dy]);
        }
        for (scale, body) in self.scale.iter_mut().zip(gradients) {
            let valid = |s| body.iter().all(|&g| {
                let [hi, lo] = stretches(g, s);
                lo >= 0.35 && sparse * hi <= 1.0
            });
            *scale = 1.0;
            if valid(1.0) { continue; }
            let (mut lo, mut hi) = (0.0, 1.0);
            for _ in 0..40 {
                let mid = 0.5 * (lo + hi);
                if valid(mid) { lo = mid; } else { hi = mid; }
            }
            *scale = lo;
        }
    }

    pub fn cells(&self, p: &Problem) -> Vec<CellStats> {
        let mut out = Vec::new();
        for (i, &inside) in p.solid.iter().enumerate() {
            if !inside { continue; }
            let corners = self.grid.corners(i);
            let [dx, dy, dz] = self.gradient(corners);
            let s = self.scale[self.owner[corners[0]]];
            let [sigma1, sigma2] = stretches([dx, dy], s);
            let got = 0.5 * (sigma1 + sigma2);
            out.push(CellStats { t: p.t[i] as f64, asked: 1.0 - p.delta[i] as f64, got, sigma1, sigma2, shear: s * dz[0].hypot(dz[1]) / got });
        }
        out
    }

    #[inline]
    fn node(&self, i: usize) -> Pair {
        let id = self.owner[i];
        if id == NONE { [0.0; 2] } else { self.v[i].map(|v| v as f64 * self.scale[id]) }
    }

    pub fn at(&self, p: [f64; 3]) -> [f64; 2] {
        let inv = 1.0 / self.grid.cell;
        let [x, y, z] = std::array::from_fn(|a| bracket((p[a] - self.grid.origin[a]) * inv, self.grid.n[a]));
        let [nx, ny, _] = self.grid.n.map(|k| k + 1);
        let plane = |k| bilinear(x, y, nx, |i| self.node(k * nx * ny + i));
        lerp(plane(z.lo), plane(z.hi), z.f)
    }

    pub fn layer(&self, z: f64) -> Layer {
        let [nx, ny, nz] = self.grid.n;
        let [ox, oy, oz] = self.grid.origin;
        let inv_cell = 1.0 / self.grid.cell;
        let z = bracket((z - oz) * inv_cell, nz);
        let size = (nx + 1) * (ny + 1);
        let v = (0..size).map(|i| lerp(self.node(z.lo * size + i), self.node(z.hi * size + i), z.f)).collect();
        Layer { origin: [ox, oy], inv_cell, n: [nx, ny], v }
    }
}

#[derive(Clone, Copy)]
struct Bracket { lo: usize, hi: usize, f: f64 }

#[inline]
fn bracket(x: f64, n: usize) -> Bracket {
    let x = x.clamp(0.0, n as f64);
    let lo = x as usize;
    Bracket { lo, hi: (lo + 1).min(n), f: x - lo as f64 }
}

#[inline]
fn lerp(a: Pair, b: Pair, t: f64) -> Pair {
    [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]
}

#[inline]
fn bilinear(x: Bracket, y: Bracket, stride: usize, node: impl Fn(usize) -> Pair) -> Pair {
    let row = |j| lerp(node(j * stride + x.lo), node(j * stride + x.hi), x.f);
    lerp(row(y.lo), row(y.hi), y.f)
}

#[derive(Debug)]
pub struct Layer {
    origin: [f64; 2],
    inv_cell: f64,
    n: [usize; 2],
    v: Vec<Pair>,
}

impl Layer {
    #[inline]
    pub fn at(&self, x: f64, y: f64) -> [f64; 2] {
        let x = bracket((x - self.origin[0]) * self.inv_cell, self.n[0]);
        let y = bracket((y - self.origin[1]) * self.inv_cell, self.n[1]);
        bilinear(x, y, self.n[0] + 1, |i| self.v[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    // Evaluate the stated energy directly, without either Hessian or the
    // assembly's basis routine. All finite differences below are exact for
    // a quadratic in exact arithmetic, so the step can be appreciable.
    fn energy(v: &[f64; 16], h: f64, b: f64, delta: f64) -> Pair {
        let mut energy = [0.0; 2];
        let gauss = [0.5 - 0.5 / 3.0_f64.sqrt(), 0.5 + 0.5 / 3.0_f64.sqrt()];
        for z in gauss { for y in gauss { for x in gauss {
            let mut value = [0.0; 2];
            let mut grad = [[0.0; 3]; 2];
            for k in 0..2 { for j in 0..2 { for i in 0..2 {
                let (nx, ny, nz) = ([1.0 - x, x][i], [1.0 - y, y][j], [1.0 - z, z][k]);
                for a in 0..2 {
                    let u = v[2 * (4 * k + 2 * j + i) + a];
                    value[a] += u * nx * ny * nz;
                    grad[a][0] += u * [-1.0, 1.0][i] * ny * nz / h;
                    grad[a][1] += u * nx * [-1.0, 1.0][j] * nz / h;
                    grad[a][2] += u * nx * ny * [-1.0, 1.0][k] / h;
                }
            }}}
            let [[xx, xy, xz], [yx, yy, yz]] = grad;
            let volume = h.powi(3) / 8.0;
            energy[0] += volume * (0.5 * (xx + yy) - delta).powi(2);
            energy[1] += volume * (0.5 * (xx - yy).powi(2) + xy * xy + yx * yx
                + b * (xz * xz + yz * yz) + 1e-6 / h.powi(2) * (value[0].powi(2) + value[1].powi(2)));
        }}}
        energy
    }

    fn close_vector(measured: [f64; 16], want: [f64; 16], what: &str) {
        let norm = |v: [f64; 16]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
        let err = norm(std::array::from_fn(|i| measured[i] - want[i]));
        let rel = err / norm(want);
        assert!(rel <= 1e-9, "{what} relative error {rel:e} must be at most 1e-9");
    }

    #[test]
    fn element_factors_and_matvec() {
        let (h, b, step) = (1.75, 2.25, 0.5);
        let elem = Element::new(h, b);
        let op = Operator { density: Modes::new(&elem.density), regular: Modes::new(&elem.regular) };
        let mut seed = 0x71ae9_u32;
        let v: [f64; 16] = std::array::from_fn(|_| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed as f64 / u32::MAX as f64 - 0.5
        });
        let derivative = |mut x: [f64; 16], i, delta| {
            x[i] += step;
            let hi = energy(&x, h, b, delta);
            x[i] -= 2.0 * step;
            let lo = energy(&x, h, b, delta);
            [(hi[0] - lo[0]) / (2.0 * step), (hi[1] - lo[1]) / (2.0 * step)]
        };
        // Compare every column (including zeros), not just one action K v.
        for j in 0..16 {
            let (mut hi, mut lo) = (v, v);
            hi[j] += step;
            lo[j] -= step;
            let column: [Pair; 16] = std::array::from_fn(|i| {
                let (a, b) = (derivative(hi, i, 0.0), derivative(lo, i, 0.0));
                [(a[0] - b[0]) / (2.0 * step), (a[1] - b[1]) / (2.0 * step)]
            });
            close_vector(column.map(|d| d[0]), std::array::from_fn(|i| elem.density[i][j]), "density Hessian column");
            close_vector(column.map(|d| d[1]), std::array::from_fn(|i| elem.regular[i][j]), "regularisation Hessian column");
            let mut unit = [[0.0; 2]; 8];
            unit[j / 2][j % 2] = 1.0;
            let applied = op.apply(3.125, unit);
            close_vector(std::array::from_fn(|i| applied[i / 2][i % 2]),
                std::array::from_fn(|i| 3.125 * elem.density[i][j] + elem.regular[i][j]), "matrix-free element column");
        }
        let load = std::array::from_fn(|i| derivative(v, i, 0.0)[0] - derivative(v, i, 1.0)[0]);
        close_vector(load, elem.load, "load from unit shrink");
    }

    struct Sample {
        grid: Grid,
        solid: Vec<bool>,
        t: Vec<f32>,
        delta: Vec<f32>,
    }

    impl Sample {
        fn new(n: [usize; 3], h: f64, cell: impl Fn([f64; 3]) -> Option<(f64, f32)>) -> Sample {
            let grid = Grid { origin: [0.0; 3], cell: h, n };
            let mut s = Sample { grid, solid: Vec::new(), t: Vec::new(), delta: Vec::new() };
            for i in 0..n.iter().product() {
                let value = cell(coords(i, n).map(|v| (v as f64 + 0.5) * h));
                let (t, delta) = value.unwrap_or((0.0, 0.0));
                s.solid.push(value.is_some());
                s.t.push(t as f32);
                s.delta.push(delta);
            }
            s
        }

        fn problem(&self) -> Problem<'_> {
            Problem { grid: self.grid, solid: &self.solid, t: &self.t, delta: &self.delta }
        }
    }

    fn linear(t: f64, sparse: f64, interior: f64) -> (f64, f32) {
        (t, shrink(Gradient::Linear, sparse, interior, t, 1.0))
    }

    // Origin at the low corner. Both discs have the same voxel phase as a
    // disc solved alone. The optional second disc starts 10 mm past the first.
    fn discs(radius: usize, sparse: f64, interior: f64, second: Option<f64>) -> Sample {
        let r = radius as f64;
        let width = if second.is_some() { 4 * radius + 10 } else { 2 * radius };
        Sample::new([width, 2 * radius, 1], 1.0, |[x, y, _]| {
            let (cx, core) = if x < 2.0 * r { (r, interior) } else { (3.0 * r + 10.0, second?) };
            let t = 1.0 - (x - cx).hypot(y - r) / r;
            (t >= 0.0).then(|| linear(t, sparse, core))
        })
    }

    fn solved(p: &Problem) -> Warp {
        let warp = Warp::solve(p, &Weights::default());
        assert!(!warp.hit_bound && warp.residual <= 1e-6, "CG must reach relative tolerance, got {:e} in {} iterations", warp.residual, warp.iterations);
        warp
    }

    fn mean(values: impl Iterator<Item = f64>) -> f64 {
        let (sum, count) = values.fold((0.0, 0), |(sum, n), v| (sum + v, n + 1));
        sum / count as f64
    }

    fn p95(mut values: Vec<f64>) -> f64 {
        values.sort_unstable_by(f64::total_cmp);
        values[values.len() - 1 - values.len() / 20]
    }

    fn min_stretch(stats: &[CellStats]) -> f64 {
        stats.iter().map(|s| s.sigma2).fold(f64::INFINITY, f64::min)
    }

    #[test]
    fn rectangle_dilates_uniformly() {
        let sample = Sample::new([30, 20, 1], 1.0, |_| Some((0.5, 0.3)));
        let p = sample.problem();
        let mut warp = solved(&p);
        for i in 0..p.solid.len() {
            let [dx, dy, _] = warp.gradient(p.grid.corners(i));
            for (got, want) in [(dx, [0.3, 0.0]), (dy, [0.0, 0.3])] {
                for a in 0..2 {
                    assert!((got[a] - want[a]).abs() <= 1e-3, "cell {i} dilation column {got:?} must match {want:?}");
                }
            }
        }
        warp.guard(&p, 0.2);
        assert_eq!(warp.scales(), &[1.0], "uniform 30% shrink must keep its scale");

        let zero = vec![0.0; p.solid.len()];
        let warp = solved(&Problem { delta: &zero, ..p });
        assert_eq!(warp.iterations, 0, "zero load must finish without a CG step");
        assert!(warp.v.iter().all(|&v| v == [0.0; 2]), "zero load must preserve the identity map");
    }

    #[test]
    fn disc_follows_depth_profile() {
        let sample = discs(20, 0.2, 0.1, None);
        let p = sample.problem();
        let stats = solved(&p).cells(&p);
        let asked = mean(stats.iter().map(|s| s.asked));
        let got = mean(stats.iter().map(|s| s.got));
        assert!((got - asked).abs() <= 0.02, "disc area-mean factor {got} must follow {asked}");
        let core = || stats.iter().filter(|s| s.t > 0.9);
        let asked = mean(core().map(|s| s.asked));
        let got = mean(core().map(|s| s.got));
        assert!((got - asked).abs() <= 0.1, "disc core factor {got} must follow {asked}");
        let mut last = 0.0;
        for bin in (0..8).rev() {
            let ring = mean(stats.iter().filter(|s| ((8.0 * s.t) as usize).min(7) == bin).map(|s| s.got));
            assert!(ring >= last, "disc factor must increase outwards, bin {bin} gave {ring} after {last}");
            last = ring;
        }
        let lo = min_stretch(&stats);
        assert!(lo >= 0.35, "unguarded disc minimum stretch {lo} must be at least 0.35");
    }

    #[test]
    fn bar_preserves_mean_and_limits_distortion() {
        let sample = Sample::new([120, 20, 1], 1.0, |[x, y, _]| Some(linear(x.min(120.0 - x).min(y.min(20.0 - y)) / 10.0, 0.2, 0.1)));
        let p = sample.problem();
        let stats = solved(&p).cells(&p);
        let asked = mean(stats.iter().map(|s| s.asked));
        let got = mean(stats.iter().map(|s| s.got));
        assert!((got - asked).abs() <= 0.02, "bar mean factor {got} must follow {asked}");
        let ratio = p95(stats.iter().map(|s| s.sigma1 / s.sigma2).collect());
        assert!(ratio <= 2.5, "bar p95 stretch ratio {ratio} must be at most 2.5");
        let lo = min_stretch(&stats);
        assert!(lo >= 0.3, "unguarded bar minimum stretch {lo} must be at least 0.3");
    }

    #[test]
    fn guard_scales_only_the_body_that_needs_it() {
        let sample = discs(15, 0.2, 0.1, Some(0.02));
        let single = discs(15, 0.2, 0.1, None);
        let (p, one) = (sample.problem(), single.problem());
        let mut warp = solved(&p);
        assert_eq!(warp.scales(), &[1.0, 1.0], "two separate discs must start at unit scale");
        let left = |i: usize| i % p.grid.n[0] < 30;
        let solid = || (0..p.solid.len()).filter(|&i| p.solid[i]);
        let before = warp.cells(&p);
        let min_b = solid().zip(&before).filter(|&(i, _)| !left(i)).map(|(_, s)| s.sigma2).fold(f64::INFINITY, f64::min);
        assert!(min_b < 0.35, "disc B must cross the fold limit, got {min_b}");
        warp.guard(&p, 0.2);
        assert_eq!(warp.scales()[0], 1.0, "guard must leave disc A at full scale");
        assert!(warp.scales()[1] < 1.0, "guard must reduce disc B");
        let after = warp.cells(&p);
        for (i, s) in solid().zip(&after) {
            assert!(s.sigma2 >= 0.35 && 0.2 * s.sigma1 <= 1.0, "guarded cell {i} must pass both limits, got {s:?}");
        }
        let reference = solved(&one).cells(&one);
        for ((_, s), r) in solid().zip(after).filter(|&(i, _)| left(i)).zip(reference) {
            assert!((s.got - r.got).abs() <= 1e-3, "disc A factor {} must match isolated factor {}", s.got, r.got);
        }
        for (gap, edge) in [(35.0, 30.0), (36.0, 40.0)] {
            assert_eq!(warp.at([gap, 15.0, 0.5]), warp.at([edge, 15.0, 0.5]), "gap at {gap} must inherit displacement and scale from {edge}");
        }

        let sample = discs(15, 0.1, 1.0, None);
        let p = sample.problem();
        let mut warp = solved(&p);
        let before = warp.cells(&p);
        let max_hi = |stats: &[CellStats]| stats.iter().map(|s| s.sigma1).fold(0.0, f64::max);
        warp.guard(&p, 0.1);
        let after = warp.cells(&p);
        // A tenfold denser core squeezes cells across the gradient long before lines meet:
        // whichever limit binds, the guard must leave every cell inside both.
        let occupied = 0.1 * max_hi(&after);
        assert!(warp.scales()[0] < 1.0 && occupied <= 1.0 && min_stretch(&after) >= 0.35 - 1e-9,
            "the expanding disc must be held inside both limits: raw sigma1 {}, sigma2 {}, scale {}, guarded sparse*sigma1 {occupied}, sigma2 {}",
            max_hi(&before), min_stretch(&before), warp.scales()[0], min_stretch(&after));
    }

    #[test]
    fn repeated_disc_solve_is_bit_identical() {
        let sample = discs(20, 0.2, 0.1, None);
        let p = sample.problem();
        let (first, second) = (solved(&p), solved(&p));
        for (i, (&a, &b)) in first.v.iter().zip(&second.v).enumerate() {
            assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits), "repeat solve must reproduce both components at node {i}");
        }
    }

    #[test]
    fn box_measurements() {
        eprintln!("shape    mean_asked mean_got core_got top_got shear_p95 shear_max iterations solve_ms");
        for (name, n) in [("cube", [20, 20, 20]), ("plate", [50, 50, 5]), ("column", [10, 10, 50])] {
            let h = 2.0;
            let span = n.map(|k| k as f64 * h);
            // Normalise by the deepest sampled cell centre, including even-sized axes.
            let depth_max = n.iter().map(|&k| (((k - 1) / 2) as f64 + 0.5) * h).fold(f64::INFINITY, f64::min);
            let sample = Sample::new(n, h, |q| {
                let depth = (0..3).map(|a| q[a].min(span[a] - q[a])).fold(f64::INFINITY, f64::min);
                Some(linear(depth / depth_max, 0.2, 0.1))
            });
            let p = sample.problem();
            let start = Instant::now();
            let warp = Warp::solve(&p, &Weights::default());
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let stats = warp.cells(&p);
            let asked = mean(stats.iter().map(|s| s.asked));
            let got = mean(stats.iter().map(|s| s.got));
            let core = mean(stats.iter().filter(|s| s.t > 0.9).map(|s| s.got));
            let top = mean(stats.iter().enumerate().filter(|(i, _)| {
                let [x, y, z] = coords(*i, n);
                let middle = |u, k| 4 * u + 2 >= k && 4 * u + 2 < 3 * k;
                z == n[2] - 1 && middle(x, n[0]) && middle(y, n[1])
            }).map(|(_, s)| s.got));
            let shear95 = p95(stats.iter().map(|s| s.shear).collect());
            let shear_max = stats.iter().map(|s| s.shear).fold(0.0, f64::max);
            eprintln!("{name:7} {asked:10.6} {got:8.6} {core:8.6} {top:8.6} {shear95:9.6} {shear_max:9.6} {:10} {ms:8.2}", warp.iterations);
            // Banding: between face neighbours the density steps no more than asked, plus 5 %.
            let mut jump: f64 = 0.0;
            for (i, s) in stats.iter().enumerate() {
                let [x, y, z] = coords(i, n);
                for (step, more) in [(1, x + 1 < n[0]), (n[0], y + 1 < n[1]), (n[0] * n[1], z + 1 < n[2])] {
                    if more { jump = jump.max((stats[i + step].got - s.got).abs() - (stats[i + step].asked - s.asked).abs()); }
                }
            }
            assert!(jump <= 0.05, "{name}: neighbouring cells' density steps {jump} more than asked");
            if name == "plate" {
                // A plate cannot grade across its thickness without shearing its layers apart
                // (the shear grows with width over thickness), so the solve keeps it nearer the
                // sparse density than asked, and stacked.
                assert!(shear_max < 1.0, "plate maximum shear {shear_max} must be under one");
                assert!(got >= asked - 0.03 && got <= 1.0, "plate mean factor {got} must lie between {asked} and the sparse density");
            } else {
                assert!((asked - got).abs() <= 0.03, "{name} mean factor {got} must be within 0.03 of {asked}");
            }
        }
    }
}
