//! The CPU solver (f64), the reference the GPU solver is checked against: conjugate
//! gradients preconditioned by one geometric multigrid V-cycle.
//!
//! - The analysed level is a [`Grid`] with its constrained degrees of freedom (fixtures, and
//!   every node no cell with material touches). [`Level::apply`] uses a shared matrix for
//!   cells full of one material, a packed matrix for cells with moved Gauss points, and
//!   integrates the remaining active cells point by point.
//! - Coarser levels are Galerkin: a coarse level's operator is `R A P`, the finer one's taken
//!   through the interpolation between them, kept as one 24x24 matrix per coarse cell (the
//!   sum over its children of `P_e^T K_e P_e`, trilinear `P_e` within the coarse cell), cells
//!   with the same children sharing one. A coarse level thus sees exactly the energy of the
//!   finer one's structure: a plate two cells thick stays as stiff as it is, where a
//!   re-discretised coarse cell averages it into a block that locks. The fine level's held
//!   degrees of freedom drop out of its children's matrices, so the coarse level is held
//!   where the fine one is and nowhere else (a coarse degree of freedom all of whose fine
//!   support is held has no stiffness left and is held).
//! - Transfers: trilinear interpolation up (onto the free fine degrees of freedom), its
//!   transpose (full weighting) down.
//! - Smoother: damped block Jacobi (the 3x3 node blocks), the same number of sweeps before
//!   and after, so the V-cycle is symmetric and CG may use it; the coarsest level takes many
//!   sweeps. Its damping holds for every shape: a level's matrix `A` is a sum of cell matrices,
//!   each positive semidefinite on 8 nodes, so `x'Ax <= 8 x'Dx` (`D` its node blocks, the
//!   triangle inequality in each cell's energy) and no eigenvalue of `D^-1 A` passes 8, and a
//!   sweep's `1 - omega lambda` stays within -1..1 at `omega` 1/4. A larger one fits a solid
//!   part (eigenvalues to about 3.4) and grows the modes on a thin wall's few nodes (to 5.1 on
//!   a Voronoi vase, most on the coarse levels): a sweep at 0.6 doubles them there, the CG in
//!   f64 takes 3 to 5 times the iterations and in f32 it never converges.
//!
//! This is the multigrid-preconditioned CG of voxel finite element solvers (Dick, Georgii and
//! Westermann, "A real-time multigrid finite hexahedra method for elasticity simulation using
//! CUDA", Simulation Modelling Practice and Theory 19, 2011; Wu, Dick and Westermann, "A
//! system for high-resolution topology optimization", IEEE TVCG 22, 2016), with the Galerkin
//! coarse operators of algebraic multigrid on the geometric hierarchy (Trottenberg,
//! Oosterlee and Schueller, "Multigrid", Academic Press 2001, section 2.3.2).

use crate::element::{corner, gauss_gradients, invert3, mul6, self_block, strain, traction, Mat6, V3};
use crate::grid::Grid;
use crate::material::Materials;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// A degree of freedom held at zero (its bit per axis in a node's mask).
pub const HELD: u8 = 7;

/// One level of the hierarchy.
pub struct Level {
    /// Shared with the model, the posed study and the answer it is of: one copy a pass.
    pub grid: Arc<Grid>,
    /// Per node: bit `a` set holds the displacement along `a` at zero.
    pub held: Vec<u8>,
    /// Per node: the inverse of its 3x3 diagonal block (identity where held): the in-process
    /// smoother's ([`Level::dinv`]). The analysed level builds it on first use: a level built
    /// for a hub's job never reads it (the program builds its own).
    dinv: OnceLock<Vec<[[f64; 3]; 3]>>,
    op: Op,
}

/// A cell's stiffness, symmetric: its upper triangle row by row (row `r` holds columns `r..24`).
pub type Packed = [f64; PACKED];
pub const PACKED: usize = 24 * 25 / 2;

/// A cell with no stiffness (no material, on a coarse level).
pub const EMPTY: u32 = u32::MAX;

/// How a level's stiffness is applied.
enum Op {
    /// On the analysed grid, `gp_d` indexes each point's stiffness in `ds`. Cells full of one
    /// material share `full[uniform[c]]`. Cells with moved points share `moved[moved_of[c]]`
    /// when their point materials, fills and offset bits match. Both indices use `u32::MAX`
    /// when the cell has no such matrix.
    Points {
        gp_d: Vec<[u32; 8]>, ds: Vec<Mat6>, uniform: Vec<u32>, full: Vec<Box<[[f64; 24]; 24]>>,
        moved: Vec<Packed>, moved_of: Vec<u32>,
    },
    /// A coarse level: each cell's Galerkin matrix (`mats[cell[c]]`, [`EMPTY`]: none).
    Cells { cell: Vec<u32>, mats: Vec<Packed> },
}

/// A level's cell stiffness as the GPU solver takes it ([`Level::cell_stiffness`]).
pub enum CellStiffness<'a> {
    /// A coarse level: per cell the index of its matrix ([`EMPTY`]: none), and the matrices.
    Matrices { cell: &'a [u32], mats: &'a [Packed] },
    /// The analysed level: per cell its matrix index ([`EMPTY`]: none), the first
    /// `shared.len()` the full cells' shared matrices, the rest one per cell integrated point by
    /// point, in cell order; the distinct materials, and per such cell its eight Gauss points'
    /// materials and weights (the volume each stands for, times its filled share): its matrix
    /// is the sum over them of `w B^T D B` ([`grads`] gives `B`).
    Points { cell: Vec<u32>, shared: Vec<Packed>, materials: &'a [Mat6], points: Vec<[u32; 8]>, weights: Vec<[f64; 8]> },
}

/// What a cell's stiffness is, for sharing a coarse cell's product.
enum Share {
    Empty,
    /// One of the level's shared matrices.
    Shared(u32),
    /// Its own (integrated point by point).
    Own,
}

/// The stiffness of a cell whose Gauss points have stiffness `d[q]` and weight `w[q]` (the
/// volume each stands for): the response to each unit nodal displacement.
fn assemble(d: [&Mat6; 8], w: [f64; 8], dn: &[[V3; 8]; 8]) -> Box<[[f64; 24]; 24]> {
    let mut k = Box::new([[0.0; 24]; 24]);
    for j in 0..24 {
        let mut u = [[0.0; 3]; 8];
        u[j / 3][j % 3] = 1.0;
        for q in 0..8 {
            let s = mul6(d[q], &strain(&u, &dn[q]));
            for i in 0..8 {
                let t = traction(&s, dn[q][i]);
                for a in 0..3 {
                    k[i * 3 + a][j] += w[q] * t[a];
                }
            }
        }
    }
    k
}

/// The shape functions' gradients at the Gauss points of a cell of size `h`, and the volume
/// each Gauss point stands for.
pub fn grads(h: V3) -> ([[V3; 8]; 8], f64) {
    (gauss_gradients().map(|g| g.map(|d| std::array::from_fn(|a| d[a] / h[a]))), 0.125 * h[0] * h[1] * h[2])
}

/// Physical gradients at cell `c`'s Gauss points. A moved point adds the linear and quadratic
/// terms in the cell's natural-coordinate offset to the shared gradient. A zero offset uses the table directly.
pub fn point_grads(g: &Grid, c: usize, shared: &[[V3; 8]; 8]) -> [[V3; 8]; 8] {
    let o = g.offset_of(c);
    if o == [0.0; 3] {
        return *shared;
    }
    let [ox, oy, oz] = std::array::from_fn(|a| 2.0 * o[a] / g.h[a]);
    let signs = |k| corner(k).map(|bit| (2 * bit as i32 - 1) as f64);
    std::array::from_fn(|q| {
        let [bx, by, bz] = signs(q).map(|t| t * crate::element::GP);
        std::array::from_fn(|i| {
            let [sx, sy, sz] = signs(i);
            let ex = sx * (sy * oy * (1.0 + sz * bz) + sz * oz * (1.0 + sy * by) + sy * sz * oy * oz) / 4.0 / g.h[0];
            let ey = sy * (sx * ox * (1.0 + sz * bz) + sz * oz * (1.0 + sx * bx) + sx * sz * ox * oz) / 4.0 / g.h[1];
            let ez = sz * (sx * ox * (1.0 + sy * by) + sy * oy * (1.0 + sx * bx) + sx * sy * ox * oy) / 4.0 / g.h[2];
            [shared[q][i][0] + ex, shared[q][i][1] + ey, shared[q][i][2] + ez]
        })
    })
}

fn pack(k: &[[f64; 24]; 24]) -> Packed {
    let mut p = [0.0; PACKED];
    let mut i = 0;
    for (r, row) in k.iter().enumerate() {
        for &v in &row[r..] {
            p[i] = v;
            i += 1;
        }
    }
    p
}

fn unpack(p: &Packed) -> Box<[[f64; 24]; 24]> {
    let mut k = Box::new([[0.0; 24]; 24]);
    let mut i = 0;
    for r in 0..24 {
        for c in r..24 {
            k[r][c] = p[i];
            k[c][r] = p[i];
            i += 1;
        }
    }
    k
}

/// `y += K x` for a packed `K`.
pub fn apply_packed(k: &Packed, x: &[f64; 24], y: &mut [f64; 24]) {
    let mut i = 0;
    for r in 0..24 {
        let xr = x[r];
        let mut acc = k[i] * xr;
        i += 1;
        for c in r + 1..24 {
            let v = k[i];
            i += 1;
            acc += v * x[c];
            y[c] += v * xr;
        }
        y[r] += acc;
    }
}

/// The inverses of the nodes' 3x3 diagonal blocks, held axes dropped (identity where held).
fn inverse_blocks(blocks: &[[[f64; 3]; 3]], held: &[u8]) -> Vec<[[f64; 3]; 3]> {
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    blocks.iter().zip(held).map(|(b, &h)| {
        if h == HELD {
            return identity;
        }
        let free = |a: usize| h & (1 << a) == 0;
        let mut m = *b;
        for a in 0..3 {
            if !free(a) {
                for k in 0..3 {
                    m[a][k] = 0.0;
                    m[k][a] = 0.0;
                }
                m[a][a] = 1.0;
            }
        }
        let mut inv = invert3(&m).unwrap_or(identity);
        for a in 0..3 {
            if !free(a) {
                for k in 0..3 {
                    inv[a][k] = 0.0;
                    inv[k][a] = 0.0;
                }
            }
        }
        inv
    }).collect()
}

/// The weight fine corner `c` of the child at offset `o` (along one axis) takes from coarse
/// corner `big`: along a halved axis the fine node sits at `(o + c) / 2` across the coarse
/// cell; along a kept axis it is the coarse node.
fn weight(halve: bool, o: usize, c: usize, big: usize) -> f64 {
    if halve {
        let t = 0.5 * (o + c) as f64;
        if big == 0 {
            1.0 - t
        } else {
            t
        }
    } else if c == big {
        1.0
    } else {
        0.0
    }
}

impl Level {
    /// The analysed level. `fixed`: per node, the axes a fixture holds.
    pub fn new(grid: impl Into<Arc<Grid>>, fixed: &[u8], m: &Materials) -> Level {
        let grid: Arc<Grid> = grid.into();
        let mut held = vec![HELD; grid.nodes()];
        for c in 0..grid.cells() {
            if grid.active(c) {
                for n in grid.cell_nodes(c) {
                    held[n] = 0;
                }
            }
        }
        for (h, f) in held.iter_mut().zip(fixed) {
            *h |= *f & HELD;
        }
        let mut index: HashMap<(u8, u8, u8, u16), u32> = HashMap::new();
        let mut ds = Vec::new();
        let gp_d: Vec<[u32; 8]> = grid.gp.iter().map(|gps| gps.map(|g| {
            let key = (g.wall, g.skin, g.angle, g.infill);
            *index.entry(key).or_insert_with(|| {
                let (w, s) = g.shares();
                ds.push(m.mix(w, s, g.angle, g.infill));
                (ds.len() - 1) as u32
            })
        })).collect();
        let (dn, vol) = grads(grid.h);
        let mut full = Vec::new();
        let mut by_d: HashMap<u32, u32> = HashMap::new();
        let uniform: Vec<u32> = grid.gp.iter().zip(&gp_d).map(|(gps, ids): (&[crate::grid::Gp; 8], &[u32; 8])| {
            if gps.iter().all(|g| g.fill == 255) && ids.iter().all(|&i| i == ids[0]) {
                *by_d.entry(ids[0]).or_insert_with(|| {
                    let d = &ds[ids[0] as usize];
                    full.push(assemble([d; 8], [vol; 8], &dn));
                    (full.len() - 1) as u32
                })
            } else {
                u32::MAX
            }
        }).collect();
        let mut moved = Vec::new();
        let mut moved_of = vec![u32::MAX; grid.cells()];
        let mut by_cut: HashMap<([u32; 8], [u8; 8], [u64; 3]), u32> = HashMap::new();
        for c in 0..grid.cells() {
            if uniform[c] != u32::MAX || !grid.active(c) {
                continue;
            }
            let offset = grid.offset_of(c);
            if offset == [0.0; 3] {
                continue;
            }
            let g = &grid.gp[c];
            // At fixed h, point material IDs, fill bytes and offset bits determine every assembly input.
            let key = (gp_d[c], g.map(|p| p.fill), offset.map(f64::to_bits));
            moved_of[c] = *by_cut.entry(key).or_insert_with(|| {
                let materials = std::array::from_fn(|q| &ds[gp_d[c][q] as usize]);
                let weights = std::array::from_fn(|q| vol * g[q].weight());
                moved.push(pack(&assemble(materials, weights, &point_grads(&grid, c, &dn))));
                (moved.len() - 1) as u32
            });
        }
        let op = Op::Points { gp_d, ds, uniform, full, moved, moved_of };
        Level { grid, held, dinv: OnceLock::new(), op }
    }

    /// Per node: the inverse of its 3x3 diagonal block (identity where held), built on first use.
    pub fn dinv(&self) -> &[[[f64; 3]; 3]] {
        self.dinv.get_or_init(|| inverse_blocks(&self.diagonal(), &self.held))
    }

    /// The level one coarser, Galerkin: each coarse cell's matrix is the sum over its
    /// children (this level's cells within it) of `P_e^T K_e P_e`, `K_e` the child's stiffness
    /// on its free degrees of freedom and `P_e` the trilinear interpolation from the coarse
    /// cell's corners to the child's. Coarse cells whose children share their matrices and
    /// holds share theirs.
    pub fn coarser(&self) -> Level {
        let fine = &self.grid;
        let grid = fine.coarser();
        let hv = halved(fine, &grid);
        let mut cell = vec![EMPTY; grid.cells()];
        let mut mats: Vec<Packed> = Vec::new();
        let mut seen: HashMap<([u32; 8], [u8; 27]), u32> = HashMap::new();
        for (c, out) in cell.iter_mut().enumerate() {
            let ijk = grid.cell_ijk(c);
            // the children by their offset in the coarse cell (0 along kept axes)
            let mut kids = [None; 8];
            let mut ids = [EMPTY; 8];
            let mut holds = [0u8; 27];
            let mut shared = true;
            for (slot, kid) in kids.iter_mut().enumerate() {
                let o = corner(slot);
                if (0..3).any(|a| !hv[a] && o[a] == 1) {
                    continue;
                }
                let f: [usize; 3] = std::array::from_fn(|a| if hv[a] { 2 * ijk[a] + o[a] } else { ijk[a] });
                if (0..3).any(|a| f[a] >= fine.n[a]) {
                    continue;
                }
                let fc = fine.cell(f[0], f[1], f[2]);
                match self.share(fc) {
                    Share::Empty => continue,
                    Share::Shared(id) => ids[slot] = id,
                    Share::Own => shared = false,
                }
                *kid = Some(fc);
                for (q, n) in fine.cell_nodes(fc).into_iter().enumerate() {
                    let fq = corner(q);
                    let at: [usize; 3] = std::array::from_fn(|a| o[a] + fq[a]);
                    holds[at[0] + 3 * (at[1] + 3 * at[2])] = self.held[n];
                }
            }
            if kids.iter().all(Option::is_none) {
                continue;
            }
            let key = (ids, holds);
            if shared {
                if let Some(&id) = seen.get(&key) {
                    *out = id;
                    continue;
                }
            }
            let mut kc = [[0.0; 24]; 24];
            for (slot, kid) in kids.iter().enumerate() {
                let Some(fc) = *kid else { continue };
                let o = corner(slot);
                let ke = self.stiffness(fc);
                // the coarse corners each fine corner takes from, with their weights
                let p: [Vec<(usize, f64)>; 8] = std::array::from_fn(|i| {
                    let ci = corner(i);
                    (0..8).filter_map(|big| {
                        let cb = corner(big);
                        let w: f64 = (0..3).map(|a| weight(hv[a], o[a], ci[a], cb[a])).product();
                        (w != 0.0).then_some((big, w))
                    }).collect()
                });
                // t = K_e P, then kc += P^T t
                let mut t = [[0.0; 24]; 24];
                for (row, trow) in ke.iter().zip(t.iter_mut()) {
                    for (j, pj) in p.iter().enumerate() {
                        for &(big, w) in pj {
                            for b in 0..3 {
                                trow[big * 3 + b] += w * row[j * 3 + b];
                            }
                        }
                    }
                }
                for (i, pi) in p.iter().enumerate() {
                    for &(big, w) in pi {
                        for a in 0..3 {
                            for (k, v) in kc[big * 3 + a].iter_mut().zip(&t[i * 3 + a]) {
                                *k += w * v;
                            }
                        }
                    }
                }
            }
            mats.push(pack(&kc));
            let id = (mats.len() - 1) as u32;
            if shared {
                seen.insert(key, id);
            }
            *out = id;
        }
        let mut level = Level { held: vec![0; grid.nodes()], grid: Arc::new(grid), dinv: OnceLock::new(), op: Op::Cells { cell, mats } };
        let blocks = level.diagonal();
        let touched = level.touched();
        for ((h, b), t) in level.held.iter_mut().zip(&blocks).zip(touched) {
            *h = if !t { HELD } else { (0..3).filter(|&a| b[a][a] <= 0.0).fold(0, |m, a| m | (1 << a)) };
        }
        let dinv = inverse_blocks(&blocks, &level.held);
        let _ = level.dinv.set(dinv);
        level
    }

    /// Per node: some cell with stiffness has it.
    fn touched(&self) -> Vec<bool> {
        let mut t = vec![false; self.grid.nodes()];
        for c in 0..self.grid.cells() {
            if self.has(c) {
                for n in self.grid.cell_nodes(c) {
                    t[n] = true;
                }
            }
        }
        t
    }

    /// The cell has stiffness.
    fn has(&self, c: usize) -> bool {
        match &self.op {
            Op::Points { .. } => self.grid.active(c),
            Op::Cells { cell, .. } => cell[c] != EMPTY,
        }
    }

    fn share(&self, c: usize) -> Share {
        match &self.op {
            Op::Points { uniform, .. } if self.grid.active(c) && uniform[c] != u32::MAX => Share::Shared(uniform[c]),
            Op::Points { .. } if self.grid.active(c) => Share::Own,
            Op::Cells { cell, .. } if cell[c] != EMPTY => Share::Shared(cell[c]),
            _ => Share::Empty,
        }
    }

    /// A cell's stiffness on its free degrees of freedom (held rows and columns zero).
    pub fn stiffness(&self, c: usize) -> Box<[[f64; 24]; 24]> {
        let mut k = match &self.op {
            Op::Points { gp_d, ds, uniform, full, moved, moved_of } => match full.get(uniform[c] as usize) {
                Some(k) => k.clone(),
                None if moved_of[c] != u32::MAX => unpack(&moved[moved_of[c] as usize]),
                None => {
                    let (dn, vol) = grads(self.grid.h);
                    let dn_c = point_grads(&self.grid, c, &dn);
                    let g = &self.grid.gp[c];
                    assemble(std::array::from_fn(|q| &ds[gp_d[c][q] as usize]), std::array::from_fn(|q| vol * g[q].weight()), &dn_c)
                }
            },
            Op::Cells { cell, mats } => unpack(&mats[cell[c] as usize]),
        };
        for (i, n) in self.grid.cell_nodes(c).into_iter().enumerate() {
            for a in 0..3 {
                if !self.free(n, a) {
                    let r = i * 3 + a;
                    for x in 0..24 {
                        k[r][x] = 0.0;
                        k[x][r] = 0.0;
                    }
                }
            }
        }
        k
    }

    /// Each cell's stiffness as the GPU solver takes it, not masked (the solver masks what
    /// it gathers, as [`Level::apply`] does): a coarse level's matrices; the analysed level's
    /// shared full-cell matrices and, for the cells integrated point by point, their Gauss
    /// points, which the GPU assembles itself.
    pub fn cell_stiffness(&self) -> CellStiffness<'_> {
        match &self.op {
            Op::Cells { cell, mats } => CellStiffness::Matrices { cell, mats },
            Op::Points { gp_d, ds, uniform, full, .. } => {
                let (_, vol) = grads(self.grid.h);
                let shared: Vec<Packed> = full.iter().map(|k| pack(k)).collect();
                let mut points = Vec::new();
                let mut weights = Vec::new();
                let cell = (0..self.grid.cells()).map(|c| {
                    if !self.grid.active(c) {
                        return EMPTY;
                    }
                    if uniform[c] != u32::MAX {
                        return uniform[c];
                    }
                    let g = &self.grid.gp[c];
                    points.push(gp_d[c]);
                    weights.push(std::array::from_fn(|q| vol * g[q].weight()));
                    (shared.len() + points.len() - 1) as u32
                }).collect();
                CellStiffness::Points { cell, shared, materials: ds, points, weights }
            }
        }
    }

    /// Returns the number of cells with stiffness and the number of distinct cell matrices.
    /// The analysed level counts its shared full and moved matrices once each, plus one per
    /// remaining active cell integrated point by point.
    pub fn matrices(&self) -> (usize, usize) {
        let cells = (0..self.grid.cells()).filter(|&c| self.has(c)).count();
        match &self.op {
            Op::Points { uniform, full, moved, moved_of, .. } => {
                let points = (0..self.grid.cells()).filter(|&c| {
                    self.grid.active(c) && uniform[c] == u32::MAX && moved_of[c] == u32::MAX
                }).count();
                (cells, full.len() + moved.len() + points)
            }
            Op::Cells { mats, .. } => (cells, mats.len()),
        }
    }

    pub fn free(&self, n: usize, a: usize) -> bool {
        self.held[n] & (1 << a) == 0
    }

    /// The analysed level's Gauss points' materials: per cell its eight points' indices into
    /// the distinct materials, and those (none on a coarse level). What another operator on
    /// the same points takes ([`crate::buckle::geometric_apply`]).
    pub fn point_materials(&self) -> Option<(&[[u32; 8]], &[Mat6])> {
        match &self.op {
            Op::Points { gp_d, ds, .. } => Some((gp_d, ds)),
            Op::Cells { .. } => None,
        }
    }

    /// The nodes' 3x3 diagonal blocks.
    fn diagonal(&self) -> Vec<[[f64; 3]; 3]> {
        let g = &self.grid;
        let mut blocks = vec![[[0.0; 3]; 3]; g.nodes()];
        let mut add = |n: usize, k: [[f64; 3]; 3]| {
            for r in 0..3 {
                for s in 0..3 {
                    blocks[n][r][s] += k[r][s];
                }
            }
        };
        match &self.op {
            Op::Points { gp_d, ds, .. } => {
                let (dn, vol) = grads(g.h);
                for c in 0..g.cells() {
                    if !g.active(c) {
                        continue;
                    }
                    let nodes = g.cell_nodes(c);
                    let dn_c = point_grads(g, c, &dn);
                    for q in 0..8 {
                        let w = vol * g.gp[c][q].weight();
                        let d = &ds[gp_d[c][q] as usize];
                        for (i, &n) in nodes.iter().enumerate() {
                            add(n, self_block(d, dn_c[q][i], w));
                        }
                    }
                }
            }
            Op::Cells { cell, mats } => {
                for c in 0..g.cells() {
                    if cell[c] == EMPTY {
                        continue;
                    }
                    let k = unpack(&mats[cell[c] as usize]);
                    for (i, n) in g.cell_nodes(c).into_iter().enumerate() {
                        add(n, std::array::from_fn(|r| std::array::from_fn(|s| k[i * 3 + r][i * 3 + s])));
                    }
                }
            }
        }
        blocks
    }

    /// `y = A x` on the free degrees of freedom (held ones come out zero).
    pub fn apply(&self, x: &[V3], y: &mut [V3]) {
        let g = &self.grid;
        y.iter_mut().for_each(|v| *v = [0.0; 3]);
        let scatter = |y: &mut [V3], nodes: &[usize; 8], f: &[f64; 24]| {
            for (i, &n) in nodes.iter().enumerate() {
                for a in 0..3 {
                    y[n][a] += f[i * 3 + a];
                }
            }
        };
        match &self.op {
            Op::Points { gp_d, ds, uniform, full, moved, moved_of } => {
                let (dn, vol) = grads(g.h);
                for c in 0..g.cells() {
                    if !g.active(c) {
                        continue;
                    }
                    let nodes = g.cell_nodes(c);
                    let mut f = [0.0; 24];
                    if let Some(k) = full.get(uniform[c] as usize) {
                        let flat: [f64; 24] = std::array::from_fn(|j| x[nodes[j / 3]][j % 3]);
                        for (r, row) in k.iter().enumerate() {
                            f[r] = row.iter().zip(&flat).map(|(a, b)| a * b).sum();
                        }
                    } else if moved_of[c] != u32::MAX {
                        let flat: [f64; 24] = std::array::from_fn(|j| x[nodes[j / 3]][j % 3]);
                        apply_packed(&moved[moved_of[c] as usize], &flat, &mut f);
                    } else {
                        let u: [V3; 8] = nodes.map(|n| x[n]);
                        for q in 0..8 {
                            let w = vol * g.gp[c][q].weight();
                            let s = mul6(&ds[gp_d[c][q] as usize], &strain(&u, &dn[q]));
                            for i in 0..8 {
                                let t = traction(&s, dn[q][i]);
                                for a in 0..3 {
                                    f[i * 3 + a] += w * t[a];
                                }
                            }
                        }
                    }
                    scatter(y, &nodes, &f);
                }
            }
            Op::Cells { cell, mats } => {
                for c in 0..g.cells() {
                    if cell[c] == EMPTY {
                        continue;
                    }
                    let nodes = g.cell_nodes(c);
                    let flat: [f64; 24] = std::array::from_fn(|j| x[nodes[j / 3]][j % 3]);
                    let mut f = [0.0; 24];
                    apply_packed(&mats[cell[c] as usize], &flat, &mut f);
                    scatter(y, &nodes, &f);
                }
            }
        }
        self.mask(y);
    }

    pub fn mask(&self, v: &mut [V3]) {
        for (n, v) in v.iter_mut().enumerate() {
            let h = self.held[n];
            if h != 0 {
                for (a, x) in v.iter_mut().enumerate() {
                    if h & (1 << a) != 0 {
                        *x = 0.0;
                    }
                }
            }
        }
    }

    /// One damped Jacobi sweep for `A x = b`.
    pub(crate) fn jacobi(&self, x: &mut [V3], b: &[V3], ax: &mut [V3], omega: f64) {
        self.apply(x, ax);
        let dinv = self.dinv();
        for n in 0..x.len() {
            let r = [b[n][0] - ax[n][0], b[n][1] - ax[n][1], b[n][2] - ax[n][2]];
            let d = &dinv[n];
            for a in 0..3 {
                x[n][a] += omega * (d[a][0] * r[0] + d[a][1] * r[1] + d[a][2] * r[2]);
            }
        }
    }
}

/// The axes the coarser of two levels halved (see [`Grid::coarser`]).
pub fn halved(fine: &Grid, coarse: &Grid) -> [bool; 3] {
    std::array::from_fn(|a| coarse.n[a] != fine.n[a])
}

/// The fine nodes a coarse node index `i` reaches along one axis, with their weights: along a
/// halved axis the node it coincides with (1) and its two neighbours (1/2); along a kept axis
/// only itself.
fn reach(i: usize, halve: bool, n: usize) -> impl Iterator<Item = (usize, f64)> {
    let (centre, spread) = if halve { (2 * i as isize, 1) } else { (i as isize, 0) };
    (-spread..=spread).filter_map(move |d| {
        let f = centre + d;
        (f >= 0 && (f as usize) < n).then(|| (f as usize, if d == 0 { 1.0 } else { 0.5 }))
    })
}

/// The coarse nodes fine node `i` interpolates from along one axis, with their weights (one
/// where it coincides, two halves between).
fn parents(i: usize, halve: bool, n: usize) -> impl Iterator<Item = (usize, f64)> {
    let p: [Option<(usize, f64)>; 2] = if !halve {
        [Some((i, 1.0)), None]
    } else if i % 2 == 0 {
        [Some((i / 2, 1.0)), None]
    } else {
        [Some((i / 2, 0.5)), Some((i / 2 + 1, 0.5)).filter(|&(c, _)| c < n)]
    };
    p.into_iter().flatten()
}

/// Fine += P coarse (trilinear interpolation).
pub fn prolong(fine: &Level, coarse: &Level, xc: &[V3], xf: &mut [V3]) {
    let fd = fine.grid.node_dims();
    let cd = coarse.grid.node_dims();
    let hv = halved(&fine.grid, &coarse.grid);
    for k in 0..fd[2] {
        for j in 0..fd[1] {
            for i in 0..fd[0] {
                let f = fine.grid.node(i, j, k);
                if fine.held[f] == HELD {
                    continue;
                }
                let mut v = [0.0; 3];
                for (ck, wk) in parents(k, hv[2], cd[2]) {
                    for (cj, wj) in parents(j, hv[1], cd[1]) {
                        for (ci, wi) in parents(i, hv[0], cd[0]) {
                            let w = wi * wj * wk;
                            let c = coarse.grid.node(ci, cj, ck);
                            for a in 0..3 {
                                v[a] += w * xc[c][a];
                            }
                        }
                    }
                }
                for a in 0..3 {
                    if fine.free(f, a) {
                        xf[f][a] += v[a];
                    }
                }
            }
        }
    }
}

/// Coarse = R fine (the transpose of [`prolong`]).
pub fn restrict(fine: &Level, coarse: &Level, rf: &[V3], rc: &mut [V3]) {
    let fd = fine.grid.node_dims();
    let hv = halved(&fine.grid, &coarse.grid);
    for (c, out) in rc.iter_mut().enumerate() {
        *out = [0.0; 3];
        if coarse.held[c] == HELD {
            continue;
        }
        let [i, j, k] = coarse.grid.node_ijk(c);
        for (z, wk) in reach(k, hv[2], fd[2]) {
            for (y, wj) in reach(j, hv[1], fd[1]) {
                for (x, wi) in reach(i, hv[0], fd[0]) {
                    let w = wi * wj * wk;
                    let f = fine.grid.node(x, y, z);
                    for a in 0..3 {
                        out[a] += w * rf[f][a];
                    }
                }
            }
        }
        for a in 0..3 {
            if !coarse.free(c, a) {
                out[a] = 0.0;
            }
        }
    }
}

/// The solver's knobs.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Stop when the residual is this share of the load.
    pub tolerance: f64,
    pub max_iterations: usize,
    /// Jacobi sweeps before and after the coarse correction.
    pub smooth: usize,
    pub omega: f64,
    /// Stop coarsening at this many nodes (at most [`DENSE_NODES`]: past it, coarsening goes on
    /// to that many).
    pub coarsest_nodes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { tolerance: 1e-6, max_iterations: 400, smooth: 2, omega: 0.25, coarsest_nodes: 600 }
    }
}

impl Options {
    /// The most nodes the coarsest level has: [`Options::coarsest_nodes`] within [`DENSE_NODES`].
    pub fn coarsest(&self) -> usize {
        self.coarsest_nodes.min(DENSE_NODES)
    }
}

/// The most nodes a coarsest level is factored with: three degrees of freedom a node, 8190 in
/// all, within the Force program's dense kernels' reach (8192), and a factor of 0.5 GB in f64.
pub const DENSE_NODES: usize = 2730;

/// The levels, finest first.
pub struct Hierarchy {
    pub levels: Vec<Level>,
    /// The coarsest level's exact solve.
    pub coarsest: Dense,
}

impl Hierarchy {
    /// Coarsens while a level has more than [`Options::coarsest`] nodes and an axis to halve,
    /// and factors the coarsest level ([`Dense::of`]).
    pub fn new(grid: impl Into<Arc<Grid>>, fixed: &[u8], m: &Materials, opts: &Options) -> Hierarchy {
        let mut levels = vec![Level::new(grid, fixed, m)];
        loop {
            let last = levels.last().unwrap();
            let g = &last.grid;
            if g.nodes() <= opts.coarsest() || !g.coarsenable().iter().any(|&c| c) {
                break;
            }
            levels.push(last.coarser());
        }
        let coarsest = Dense::of(levels.last().unwrap());
        Hierarchy { levels, coarsest }
    }

    /// One V-cycle from level `l`: `x` approximates `A^-1 b`.
    pub fn vcycle(&self, l: usize, b: &[V3], x: &mut [V3], opts: &Options) {
        if l + 1 == self.levels.len() {
            self.coarsest.solve(b, x);
            return;
        }
        let level = &self.levels[l];
        let mut ax = vec![[0.0; 3]; b.len()];
        x.iter_mut().for_each(|v| *v = [0.0; 3]);
        for _ in 0..opts.smooth {
            level.jacobi(x, b, &mut ax, opts.omega);
        }
        level.apply(x, &mut ax);
        let r: Vec<V3> = b.iter().zip(&ax).map(|(b, a)| [b[0] - a[0], b[1] - a[1], b[2] - a[2]]).collect();
        let coarse = &self.levels[l + 1];
        let mut rc = vec![[0.0; 3]; coarse.grid.nodes()];
        restrict(level, coarse, &r, &mut rc);
        let mut xc = vec![[0.0; 3]; rc.len()];
        self.vcycle(l + 1, &rc, &mut xc, opts);
        prolong(level, coarse, &xc, x);
        for _ in 0..opts.smooth {
            level.jacobi(x, b, &mut ax, opts.omega);
        }
    }
}

/// A level's stiffness on its free degrees of freedom, solved exactly: `A = L L'` factored once
/// in f64 (dense Cholesky) and the factor inverted, `R = L^-1`, so a solve is two triangular
/// products, `x = R'(R b)`. As an operator that is `R'R`, symmetric positive definite however
/// `R`'s entries are rounded (so the V-cycle stays a CG preconditioner in f32 too), and its
/// rounding goes with the square root of the level's condition number, where an inverse of `A`
/// itself goes with the condition number (FORCE A3 1.2). Every output element is an
/// independent sum, the shape the device and the program's kernels run.
pub struct Dense {
    /// The factored degrees of freedom, as node * 3 + axis.
    free: Vec<usize>,
    /// `R` and its mirror, row-major, `free.len()` square: row `i` holds `R`'s row `i` up to the
    /// diagonal and `R`'s column `i` past it, so both products read rows.
    r: Vec<f64>,
    /// `r` rounded to f32, when this solve is to do the GPU engine's arithmetic ([`Dense::narrow`]).
    r32: Option<Vec<f32>>,
    /// Free degrees of freedom with no pivot left past the ones before (stiffness that only
    /// moves with them, such as a part's rigid motion where nothing holds it): solved as zero.
    pub dropped: usize,
}

impl Dense {
    /// Probes `level`'s stiffness (nodes three apart along an axis share no cell, so 27 node
    /// colours times 3 axes, 81 products, read every column), factors it and inverts the factor.
    pub fn of(level: &Level) -> Dense {
        let g = &level.grid;
        let dims = g.node_dims();
        let nodes = g.nodes();
        let colour = |n: usize| {
            let p = g.node_ijk(n);
            p[0] % 3 + 3 * (p[1] % 3) + 9 * (p[2] % 3)
        };
        let mut probes = vec![vec![[0.0; 3]; nodes]; 81];
        let mut x = vec![[0.0; 3]; nodes];
        for c in 0..27 {
            for a in 0..3 {
                x.iter_mut().enumerate().for_each(|(n, v)| *v = if colour(n) == c { std::array::from_fn(|b| if b == a { 1.0 } else { 0.0 }) } else { [0.0; 3] });
                level.mask(&mut x);
                level.apply(&x, &mut probes[c * 3 + a]);
            }
        }
        // node n's neighbour (within one along each axis) of colour c, if the grid has it
        let near = |n: usize, c: usize| -> Option<usize> {
            let p = g.node_ijk(n);
            let want = [c % 3, (c / 3) % 3, c / 9];
            let mut q = [0; 3];
            for a in 0..3 {
                q[a] = (p[a].saturating_sub(1)..=(p[a] + 1).min(dims[a] - 1)).find(|i| i % 3 == want[a])?;
            }
            Some(g.node(q[0], q[1], q[2]))
        };
        let held = |d: usize| level.held[d / 3] & (1 << (d % 3)) != 0;
        let diagonal = |d: usize| probes[colour(d / 3) * 3 + d % 3][d / 3][d % 3];
        let free: Vec<usize> = (0..nodes * 3).filter(|&d| !held(d) && diagonal(d) > 0.0).collect();
        let mut index = vec![usize::MAX; nodes * 3];
        free.iter().enumerate().for_each(|(i, &d)| index[d] = i);
        let n = free.len();
        let mut l = vec![0.0; n * n];
        for (i, &d) in free.iter().enumerate() {
            for c in 0..27 {
                let Some(m) = near(d / 3, c) else { continue };
                for a in 0..3 {
                    let j = index[m * 3 + a];
                    if j != usize::MAX && j <= i {
                        l[i * n + j] = probes[c * 3 + a][d / 3][d % 3];
                    }
                }
            }
        }
        drop(probes);
        let dropped = cholesky(&mut l, n);
        Dense { free, r: inverted(&l, n), r32: None, dropped }
    }

    /// The degrees of freedom factored.
    pub fn size(&self) -> usize {
        self.free.len()
    }

    /// From now on, solves as the GPU engine does: `R` rounded to f32 and both products in f32
    /// arithmetic (the CPU's reference for that engine).
    pub fn narrow(&mut self) {
        self.r32 = Some(self.r.iter().map(|&v| v as f32).collect());
    }

    /// `x = A^-1 b` on the factored degrees of freedom (zero elsewhere).
    pub fn solve(&self, b: &[V3], x: &mut [V3]) {
        let n = self.free.len();
        let gathered = self.free.iter().map(|&d| b[d / 3][d % 3]);
        let out: Vec<f64> = match &self.r32 {
            None => {
                let b: Vec<f64> = gathered.collect();
                triangular_pair(&self.r, &b, n)
            }
            Some(r) => {
                let b: Vec<f32> = gathered.map(|v| v as f32).collect();
                triangular_pair(r, &b, n).into_iter().map(f64::from).collect()
            }
        };
        x.iter_mut().for_each(|v| *v = [0.0; 3]);
        for (&d, v) in self.free.iter().zip(&out) {
            x[d / 3][d % 3] = *v;
        }
    }
}

/// `A = L L'` in place (Cholesky-Banachiewicz, row by row, row-major `n` square, the lower
/// triangle read and written). A pivot gone to round-off of its diagonal drops: its row and
/// column of `L` stay zero. Returns how many dropped.
fn cholesky(l: &mut [f64], n: usize) -> usize {
    let mut dropped = 0;
    for i in 0..n {
        let (done, row) = l.split_at_mut(i * n);
        let row = &mut row[..n];
        for j in 0..=i {
            let rj = if j == i { &row[..j] } else { &done[j * n..j * n + j] };
            let s = sum_products(&row[..j], rj);
            if j < i {
                let pivot = done[j * n + j];
                row[j] = if pivot > 0.0 { (row[j] - s) / pivot } else { 0.0 };
            } else {
                let left = row[i] - s;
                if left > 1e-12 * row[i] {
                    row[i] = left.sqrt();
                } else {
                    row[i] = 0.0;
                    dropped += 1;
                }
            }
        }
    }
    dropped
}

/// `R = L^-1` for a lower triangular `L` (a dropped pivot's row of `R` zero), returned with
/// `R`'s columns mirrored past the diagonal (see [`Dense`]). Row by row: `R`'s row `i` is
/// `-(1 / L_ii)` times the sum of `L_ik` times `R`'s row `k`, `k < i`, plus `1 / L_ii` on the
/// diagonal; every step reads and writes rows.
fn inverted(l: &[f64], n: usize) -> Vec<f64> {
    let mut r = vec![0.0; n * n];
    for i in 0..n {
        let pivot = l[i * n + i];
        if pivot <= 0.0 {
            continue;
        }
        let (done, row) = r.split_at_mut(i * n);
        let row = &mut row[..n];
        for k in 0..i {
            let lik = l[i * n + k];
            if lik != 0.0 {
                for (o, v) in row[..=k].iter_mut().zip(&done[k * n..k * n + k + 1]) {
                    *o += lik * v;
                }
            }
        }
        row[..i].iter_mut().for_each(|v| *v *= -1.0 / pivot);
        row[i] = 1.0 / pivot;
    }
    for i in 0..n {
        for j in i + 1..n {
            r[i * n + j] = r[j * n + i];
        }
    }
    r
}

/// `x = R'(R b)` with `r` as [`Dense`] keeps it: `y_i` sums row `i` up to the diagonal against
/// `b`, `x_i` sums it from the diagonal on against `y`.
fn triangular_pair<T: Copy + Default + std::ops::Add<Output = T> + std::ops::Mul<Output = T>>(r: &[T], b: &[T], n: usize) -> Vec<T> {
    let y: Vec<T> = (0..n).map(|i| sum_products(&r[i * n..i * n + i + 1], &b[..=i])).collect();
    (0..n).map(|i| sum_products(&r[i * n + i..i * n + n], &y[i..])).collect()
}

/// A dot product summed in four partial sums: less rounding than one running sum, and nothing
/// a compiler's reordering undoes.
fn sum_products<T: Copy + Default + std::ops::Add<Output = T> + std::ops::Mul<Output = T>>(a: &[T], b: &[T]) -> T {
    let mut s = [T::default(); 4];
    let mut a4 = a.chunks_exact(4);
    let mut b4 = b.chunks_exact(4);
    for (a, b) in (&mut a4).zip(&mut b4) {
        for k in 0..4 {
            s[k] = s[k] + a[k] * b[k];
        }
    }
    for (k, (a, b)) in a4.remainder().iter().zip(b4.remainder()).enumerate() {
        s[k] = s[k] + *a * *b;
    }
    (s[0] + s[1]) + (s[2] + s[3])
}

fn dot(a: &[V3], b: &[V3]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).sum()
}

/// A solve's outcome.
#[derive(Clone, Debug)]
pub struct Solution {
    /// Per node displacement (mm).
    pub u: Vec<V3>,
    pub iterations: usize,
    /// The final residual's share of the load.
    pub residual: f64,
}

impl Solution {
    /// Whether it reached `tolerance` (the solve's [`Options::tolerance`]). A solve stops at
    /// [`Options::max_iterations`] with what it has: then its answer is approximate.
    pub fn solved(&self, tolerance: f64) -> bool {
        self.residual <= tolerance
    }
}

/// Solves `A u = f` by multigrid-preconditioned CG from rest.
pub fn solve(h: &Hierarchy, f: &[V3], opts: &Options) -> Solution {
    solve_until(h, f, None, opts, &mut |_, _| false)
}

/// Where a solve of `A u = b` (`b` masked) starts, and its residual `b - A u`: from `guess`
/// (an answer on the same grid, as the last solve of an edited study left it), the multiple
/// of it nearest the answer in energy (Galerkin projection onto the one vector, as Fischer
/// 1998 starts solves of successive right-hand sides); from rest without one. A load only
/// scaled starts at its answer, and no guess starts farther from it than rest. Nodes no cell
/// of `fine` uses start at rest.
pub fn start(fine: &Level, b: &[V3], guess: Option<&[V3]>) -> (Vec<V3>, Vec<V3>) {
    let rest = || (vec![[0.0; 3]; b.len()], b.to_vec());
    let Some(guess) = guess.filter(|g| g.len() == b.len()) else { return rest() };
    let mut u = guess_on(fine, guess);
    let mut au = vec![[0.0; 3]; u.len()];
    fine.apply(&u, &mut au);
    let energy = dot(&u, &au);
    if !(energy > 0.0) {
        return rest();
    }
    let s = dot(&u, b) / energy;
    u.iter_mut().for_each(|v| *v = [s * v[0], s * v[1], s * v[2]]);
    let r = b.iter().zip(&au).map(|(b, a)| [b[0] - s * a[0], b[1] - s * a[1], b[2] - s * a[2]]).collect();
    (u, r)
}

/// `guess` (an answer on `fine`'s grid) as a solve on `fine` starts from it, before [`start`]
/// scales it: the nodes no cell of `fine` uses at rest, the held components zero.
pub fn guess_on(fine: &Level, guess: &[V3]) -> Vec<V3> {
    let g = &fine.grid;
    let mut used = vec![false; guess.len()];
    for c in (0..g.cells()).filter(|&c| g.active(c)) {
        g.cell_nodes(c).iter().for_each(|&n| used[n] = true);
    }
    let mut u: Vec<V3> = guess.iter().zip(&used).map(|(v, &used)| if used { *v } else { [0.0; 3] }).collect();
    fine.mask(&mut u);
    u
}

/// [`solve`] from `guess` ([`start`]), asking `stop` after each iteration (its count, the
/// residual's share of the load) whether to give up: a stale request stops within one
/// iteration. A stopped solve returns where it got to.
pub fn solve_until(h: &Hierarchy, f: &[V3], guess: Option<&[V3]>, opts: &Options, stop: &mut dyn FnMut(usize, f64) -> bool) -> Solution {
    let fine = &h.levels[0];
    let mut b = f.to_vec();
    fine.mask(&mut b);
    let norm_b = dot(&b, &b).sqrt();
    if norm_b == 0.0 {
        return Solution { u: vec![[0.0; 3]; b.len()], iterations: 0, residual: 0.0 };
    }
    let n = b.len();
    let (mut u, mut r) = start(fine, &b, guess);
    let mut residual = dot(&r, &r).sqrt() / norm_b;
    if residual <= opts.tolerance {
        return Solution { u, iterations: 0, residual };
    }
    let mut q = vec![[0.0; 3]; n];
    let mut z = vec![[0.0; 3]; n];
    h.vcycle(0, &r, &mut z, opts);
    let mut p = z.clone();
    let mut rz = dot(&r, &z);
    let mut it = 0;
    while it < opts.max_iterations && residual > opts.tolerance {
        fine.apply(&p, &mut q);
        let alpha = rz / dot(&p, &q);
        for n in 0..u.len() {
            for a in 0..3 {
                u[n][a] += alpha * p[n][a];
                r[n][a] -= alpha * q[n][a];
            }
        }
        residual = dot(&r, &r).sqrt() / norm_b;
        it += 1;
        if residual <= opts.tolerance || stop(it, residual) {
            break;
        }
        h.vcycle(0, &r, &mut z, opts);
        let rz_new = dot(&r, &z);
        let beta = rz_new / rz;
        rz = rz_new;
        for n in 0..p.len() {
            for a in 0..3 {
                p[n][a] = z[n][a] + beta * p[n][a];
            }
        }
    }
    Solution { u, iterations: it, residual }
}
