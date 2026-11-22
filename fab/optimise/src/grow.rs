//! Grow: a part's material where its loads want it, as a program on the compute host
//! (`programs/grow.splash`, shader kernels on the GPU), and its exact twin in f64 ([`oracle`]) the
//! tests hold it to.
//!
//! A [`Job`] is a list of [`Level`]s, coarsest first, each a box of cells. Each design iteration
//! filters the density, projects it at the threshold that holds the volume, solves K u = f warm
//! from the last u, and moves the density by optimality criteria.
//!
//! A job that holds ([`Job::hold`]) shows the shape after every iteration ([`shape`]) and, its last
//! level done, stays open for [`Edit`]s: each runs on from where the level stands, its sharpness
//! kept and its moves small ([`Job::warm_step`]), so a small edit makes a small change. An edit the
//! shape no longer fits ([`Job::refit`]) grows again from nothing.

use makepad_compute_program::gpu::GpuLink;
use makepad_compute_program::{ElemTy, Input, Limits, OutputData, Program, Session, SessionConfig, Updates, Value};
use std::sync::Arc;

pub const GROW: &str = include_str!("../programs/grow.splash");

pub fn program() -> Program {
    Program::new("grow.splash", GROW)
}

/// Bits of [`Level::held`]: a node's axes held still.
pub const HELD_ALL: u32 = 7;
/// [`Level::mask`]: a cell that is always solid.
pub const KEEP: u32 = 1;
/// [`Level::mask`]: a cell that is always empty.
pub const AVOID: u32 = 2;

/// One level of a grow: `n` cells of side `h` mm, x fastest.
#[derive(Clone, Debug)]
pub struct Level {
    pub n: [usize; 3],
    pub h: f64,
    /// Its design iterations.
    pub its: usize,
    /// The filter's radius, mm: its window reaches ceil(rmin / h) cells each way.
    pub rmin: f64,
    /// The projection's sharpness at its first and last iteration.
    pub beta: [f64; 2],
    /// Per node: the axes held still.
    pub held: Vec<u32>,
    /// Per node: the force, N, as x y z 0.
    pub load: Vec<[f32; 4]>,
    /// Per cell: 0, [`KEEP`] or [`AVOID`].
    pub mask: Vec<u32>,
}

impl Level {
    pub fn cells(&self) -> usize {
        self.n[0] * self.n[1] * self.n[2]
    }

    pub fn nodes(&self) -> usize {
        (self.n[0] + 1) * (self.n[1] + 1) * (self.n[2] + 1)
    }

    fn count(&self, m: u32) -> usize {
        self.mask.iter().filter(|c| **c == m).count()
    }

    /// The solid volume in cells a share `volume` of the free cells comes to.
    pub fn target(&self, volume: f64) -> f64 {
        let (kept, avoided) = (self.count(KEEP), self.count(AVOID));
        volume * (self.cells() - kept - avoided) as f64 + kept as f64
    }
}

/// A grow to run.
#[derive(Clone, Debug)]
pub struct Job {
    pub levels: Vec<Level>,
    /// The solid share of the free cells.
    pub volume: f64,
    /// The solid's Young's modulus (MPa) and Poisson ratio.
    pub e0: f64,
    pub nu: f64,
    /// The stiffness of an empty cell, as a share of the solid's.
    pub emin: f64,
    /// How far a cell's density moves in one iteration.
    pub step: f64,
    /// Each solve stops at |r| <= tol |f|, or `max_its`.
    pub tol: f64,
    pub max_its: usize,
    pub smooth: usize,
    pub coarsest_sweeps: usize,
    pub coarsest_nodes: usize,
    pub omega: f64,
    /// Workgroups a dispatch: the result does not depend on it.
    pub job: usize,
    /// CG iterations sent between looks at the scalars: the result does not depend on it.
    pub batch: usize,
    /// Show every iteration and stay open for [`Edit`]s.
    pub hold: bool,
    /// The design iterations an edit runs, and how far a cell's density moves in each.
    pub warm_its: usize,
    pub warm_step: f64,
    /// Past this share of how stiff the shape was as it grew, under an edit's first look (the
    /// compliance per force squared, for the solid's share), the edit grows from nothing.
    pub refit: f64,
    /// The first level's density to start from, else the volume everywhere.
    pub start: Option<Arc<[f32]>>,
}

impl Job {
    pub fn new(levels: Vec<Level>, volume: f64) -> Job {
        Job {
            levels,
            volume,
            e0: 3000.0,
            nu: 0.35,
            emin: 1e-3,
            step: 0.2,
            tol: 1e-2,
            max_its: 60,
            smooth: 2,
            coarsest_sweeps: 8,
            coarsest_nodes: 64,
            omega: 0.25,
            job: 1 << 16,
            batch: 2,
            hold: false,
            warm_its: 8,
            warm_step: 0.05,
            refit: 1.1,
            start: None,
        }
    }
}

/// One design iteration's numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Iteration {
    pub level: usize,
    pub it: usize,
    /// The rev of the edit it answers, 0 before any.
    pub rev: usize,
    /// CG iterations of its solve (0 from the oracle).
    pub pcg: usize,
    /// f.u, N mm.
    pub compliance: f64,
    /// The projected density's sum, in cells.
    pub volume: f64,
    pub target: f64,
    /// The squared size of its solve's last residual over that of the load: how far from solved it
    /// stopped (0 from the oracle).
    pub residual: f64,
}

/// What a grow gives back: the last level's fields as they came from the device.
#[derive(Clone, Debug)]
pub struct Grown {
    pub iterations: Vec<Iteration>,
    /// The design density after the last update.
    pub x: Vec<f32>,
    /// The projected density of the last solve.
    pub xp: Vec<f32>,
    /// The last solve's displacement, x y z 0 per node.
    pub u: Vec<f32>,
    /// The stiffness share of the last solve.
    pub e: Vec<f32>,
    /// The session's kernel table: label, calls, ms, elements.
    pub stats: Value,
}

fn num(x: f64) -> Value {
    Value::Number(x)
}

fn nums(v: &[f64]) -> Value {
    Value::Array(v.iter().map(|x| num(*x)).collect())
}

/// The size of a load: its nodes' forces, whatever their directions.
fn total_force(load: &[[f32; 4]]) -> f64 {
    load.iter().map(|f| f[..3].iter().map(|c| (*c as f64).powi(2)).sum::<f64>().sqrt()).sum()
}

/// What changed for a held job: the session's message `edit`. Sending it gives its rev, which what
/// answers it tells back ([`Iteration::rev`], [`shape`], [`settled`]).
#[derive(Clone, Debug)]
pub struct Edit {
    pub volume: f64,
    /// Each level's load, as [`Level::load`].
    pub load: Vec<Vec<[f32; 4]>>,
}

impl Edit {
    pub fn value(&self) -> Value {
        let loaded = |load: &Vec<[f32; 4]>| {
            let forces = load.iter().enumerate().filter(|(_, f)| f[..3].iter().any(|c| *c != 0.0));
            Value::Array(forces.flat_map(|(node, f)| [node as f64, f[0] as f64, f[1] as f64, f[2] as f64]).map(num).collect())
        };
        let force = self.load.last().map_or(0.0, |l| total_force(l));
        Value::object(&[("volume", num(self.volume)), ("force", num(force)), ("load", Value::Array(self.load.iter().map(loaded).collect()))])
    }
}

/// The session's parameters and inputs.
pub fn config(job: &Job, gpu: Option<GpuLink>) -> SessionConfig {
    let mut inputs = Vec::new();
    if let Some(x0) = &job.start {
        inputs.push(Input::f32("x0", x0));
    }
    let levels = job.levels.iter().enumerate().map(|(i, l)| {
        let words: Vec<u32> = l.load.iter().flatten().map(|x| x.to_bits()).collect();
        inputs.push(Input::words(&format!("held{i}"), ElemTy::U32, l.held.clone().into()));
        inputs.push(Input::words(&format!("load{i}"), ElemTy::Vec4, words.into()));
        inputs.push(Input::words(&format!("mask{i}"), ElemTy::U32, l.mask.clone().into()));
        Value::object(&[
            ("n", nums(&l.n.map(|c| c as f64))),
            ("h", num(l.h)),
            ("its", num(l.its as f64)),
            ("rmin", num(l.rmin)),
            ("beta", nums(&l.beta)),
            ("kept_cells", num(l.count(KEEP) as f64)),
            ("avoided_cells", num(l.count(AVOID) as f64)),
        ])
    });
    let levels = Value::Array(levels.collect());
    let params = Value::object(&[
        ("levels", levels),
        ("volume", num(job.volume)),
        ("e0", num(job.e0)),
        ("nu", num(job.nu)),
        ("emin", num(job.emin)),
        ("step", num(job.step)),
        ("tol", num(job.tol)),
        ("max_its", num(job.max_its as f64)),
        ("smooth", num(job.smooth as f64)),
        ("coarsest_sweeps", num(job.coarsest_sweeps as f64)),
        ("coarsest_nodes", num(job.coarsest_nodes as f64)),
        ("omega", num(job.omega)),
        ("job", num(job.job as f64)),
        ("batch", num(job.batch as f64)),
        ("hold", Value::Bool(job.hold)),
        ("warm_its", num(job.warm_its as f64)),
        ("warm_step", num(job.warm_step)),
        ("refit", num(job.refit)),
        ("force", num(job.levels.last().map_or(0.0, |l| total_force(&l.load)))),
        ("start", Value::Bool(job.start.is_some())),
    ]);
    SessionConfig { params, inputs, limits: limits(), gpu, ..SessionConfig::default() }
}

/// Counted work and bytes never stop a grow before the machine does.
pub fn limits() -> Limits {
    Limits { fuel: u64::MAX, work: u64::MAX, buffer_bytes: u64::MAX, ..Limits::default() }
}

fn iteration(v: &Value) -> Result<Iteration, String> {
    let n = |k: &str| v.get(k).and_then(Value::as_number).ok_or(format!("an iteration without {k}"));
    Ok(Iteration {
        level: n("level")? as usize,
        it: n("it")? as usize,
        rev: n("rev")? as usize,
        pcg: n("pcg")? as usize,
        compliance: n("compliance")?,
        volume: n("volume")?,
        target: n("target")?,
        residual: n("rr")? / n("ff")?,
    })
}

/// A held job's latest iteration in these updates.
pub fn latest(u: &Updates) -> Option<Iteration> {
    u.value("iteration").and_then(|v| iteration(v).ok())
}

/// The compliance a held job settled at for edit `rev`, once it has.
pub fn settled(u: &Updates, rev: usize) -> Option<f64> {
    let v = u.value("settled")?;
    (v.get("rev")?.as_number()? as usize == rev).then(|| v.get("compliance")?.as_number())?
}

/// The level the device had no room for: the grow settles at the one before it, or, the first one
/// refused, ends with nothing.
pub fn no_room(u: &Updates) -> Option<usize> {
    u.value("no_room")?.get("level")?.as_number().map(|l| l as usize)
}

/// A held job's newest projected density at `level`, and the rev of the edit it answers.
pub fn shape(u: &Updates, level: usize) -> Option<(usize, Vec<f32>)> {
    let o = u.output(&format!("xp{level}"))?;
    Some((o.rev as usize, o.data.f32s()?))
}

/// The grow from a session's updates once it ended.
pub fn answer(u: &Updates) -> Option<Result<Grown, String>> {
    let end = match u.end.as_ref()? {
        Ok(v) => v,
        Err(e) => return Some(Err(format!("{e}"))),
    };
    let field = |name: &str| match u.output(name).map(|o| &o.data) {
        Some(OutputData::Words(_)) => u.output(name).and_then(|o| o.data.f32s()).ok_or(format!("{name}: not f32")),
        _ => Err(format!("the program published no {name}")),
    };
    let done = (|| {
        let history = end.get("history").and_then(Value::as_array).ok_or(format!("the program ended with {end:?}"))?;
        Ok(Grown {
            iterations: history.iter().map(iteration).collect::<Result<_, _>>()?,
            x: field("x")?,
            xp: field("xp")?,
            u: field("u")?,
            e: field("e")?,
            stats: end.get("stats").cloned().unwrap_or(Value::Nil),
        })
    })();
    Some(done)
}

/// Runs `job` on a GPU device to its end.
pub fn run(job: &Job, gpu: GpuLink) -> Result<Grown, String> {
    let session = Session::open(&program(), config(job, Some(gpu)));
    let mut all = Updates::default();
    loop {
        all.merge(session.wait());
        if let Some(done) = answer(&all) {
            return done;
        }
    }
}

/// A box of `n` cells held by its x = 0 face, `force` N down along its far end's lower edge, the
/// load shared by the length each node stands for.
pub fn cantilever(n: [usize; 3], h: f64, its: usize, rmin: f64, beta: [f64; 2], force: f32) -> Level {
    let (nx, ny) = (n[0] + 1, n[1] + 1);
    let nodes = nx * ny * (n[2] + 1);
    let mut held = vec![0; nodes];
    let mut load = vec![[0.0; 4]; nodes];
    for k in 0..=n[2] {
        for j in 0..ny {
            held[nx * (j + ny * k)] = HELD_ALL;
        }
    }
    for j in 0..ny {
        let share = if j == 0 || j == n[1] { 0.5 } else { 1.0 };
        load[n[0] + nx * j][2] = -force * share / n[1] as f32;
    }
    Level { n, h, its, rmin, beta, held, load, mask: vec![0; n[0] * n[1] * n[2]] }
}

// ---- The oracle: the same steps in f64, the solves exact --------------------------------------

/// A hex8 matrix of a cell `h` mm, as `grow_element` builds it.
fn element(e0: f64, nu: f64, h: [f64; 3]) -> [[f64; 24]; 24] {
    let shape = |k: usize, xi: f64, eta: f64, zeta: f64| {
        let (sx, sy, sz) = ((2 * (k % 2)) as f64 - 1.0, (2 * ((k / 2) % 2)) as f64 - 1.0, (2 * (k / 4)) as f64 - 1.0);
        [
            0.25 * sx * (1.0 + eta * sy) * (1.0 + zeta * sz) / h[0],
            0.25 * (1.0 + xi * sx) * sy * (1.0 + zeta * sz) / h[1],
            0.25 * (1.0 + xi * sx) * (1.0 + eta * sy) * sz / h[2],
        ]
    };
    // xx yy zz xy yz zx of a unit displacement along d
    let strain = |d: usize, g: [f64; 3]| match d {
        0 => [g[0], 0.0, 0.0, g[1], 0.0, g[2]],
        1 => [0.0, g[1], 0.0, g[0], g[2], 0.0],
        _ => [0.0, 0.0, g[2], 0.0, g[1], g[0]],
    };
    let lam = nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
    let mu = 1.0 / (2.0 * (1.0 + nu));
    let g = 0.577_350_269_189_625_8;
    let mut k0 = [[0.0; 24]; 24];
    for q in 0..8 {
        let at = |bit: usize| g * (2 * ((q >> bit) & 1)) as f64 - g;
        let (xi, eta, zeta) = (at(0), at(1), at(2));
        for (a, row) in k0.iter_mut().enumerate() {
            let ea = strain(a % 3, shape(a / 3, xi, eta, zeta));
            for (b, v) in row.iter_mut().enumerate() {
                let eb = strain(b % 3, shape(b / 3, xi, eta, zeta));
                let tr = lam * (eb[0] + eb[1] + eb[2]);
                *v += ea[0] * (tr + 2.0 * mu * eb[0]) + ea[1] * (tr + 2.0 * mu * eb[1]) + ea[2] * (tr + 2.0 * mu * eb[2]);
                *v += mu * (ea[3] * eb[3] + ea[4] * eb[4] + ea[5] * eb[5]);
            }
        }
    }
    let scale = e0 * h[0] * h[1] * h[2] * 0.125;
    k0.iter_mut().flatten().for_each(|v| *v *= scale);
    k0
}

/// A cell's corner nodes.
fn corners(l: &Level, s: usize) -> [usize; 8] {
    let (cx, cy) = (l.n[0], l.n[1]);
    let (i, j, k) = (s % cx, (s / cx) % cy, s / (cx * cy));
    std::array::from_fn(|c| (i + c % 2) + (cx + 1) * ((j + (c / 2) % 2) + (cy + 1) * (k + c / 4)))
}

/// K u = f, exact: a banded Cholesky over the box with its longest axis slowest (the narrowest
/// band), the held axes cut to the identity.
struct Banded {
    /// A degree of freedom's place, per node and axis.
    place: Vec<usize>,
    band: usize,
}

impl Banded {
    fn new(l: &Level) -> Banded {
        let mut axes = [0, 1, 2];
        axes.sort_by_key(|a| std::cmp::Reverse(l.n[*a]));
        let dims = l.n.map(|c| c + 1);
        let (slow, mid, fast) = (axes[0], axes[1], axes[2]);
        let stride = [dims[mid] * dims[fast], dims[fast], 1];
        let mut place = vec![0; l.nodes()];
        for (node, p) in place.iter_mut().enumerate() {
            let at = [node % dims[0], (node / dims[0]) % dims[1], node / (dims[0] * dims[1])];
            *p = at[slow] * stride[0] + at[mid] * stride[1] + at[fast];
        }
        Banded { place, band: 3 * (stride[0] + stride[1] + 2) }
    }

    fn solve(&self, l: &Level, k0: &[[f64; 24]; 24], e: &[f64]) -> Vec<f64> {
        let (b, dofs) = (self.band, 3 * l.nodes());
        // a[r][r - c]: the lower band
        let mut a = vec![0.0; dofs * (b + 1)];
        let free = |node: usize, axis: usize| l.held[node] >> axis & 1 == 0;
        for (s, stiffness) in e.iter().enumerate() {
            let nodes = corners(l, s);
            for r in 0..24 {
                if !free(nodes[r / 3], r % 3) {
                    continue;
                }
                let pr = 3 * self.place[nodes[r / 3]] + r % 3;
                for c in 0..24 {
                    if !free(nodes[c / 3], c % 3) {
                        continue;
                    }
                    let pc = 3 * self.place[nodes[c / 3]] + c % 3;
                    if pc <= pr {
                        a[pr * (b + 1) + pr - pc] += stiffness * k0[r][c];
                    }
                }
            }
        }
        for node in 0..l.nodes() {
            for axis in 0..3 {
                if !free(node, axis) {
                    a[(3 * self.place[node] + axis) * (b + 1)] = 1.0;
                }
            }
        }
        for r in 0..dofs {
            let low = r.saturating_sub(b);
            for c in low..=r {
                let mut sum = a[r * (b + 1) + r - c];
                for t in low.max(c.saturating_sub(b))..c {
                    sum -= a[r * (b + 1) + r - t] * a[c * (b + 1) + c - t];
                }
                a[r * (b + 1) + r - c] = if c == r { sum.sqrt() } else { sum / a[c * (b + 1)] };
            }
        }
        let mut y = vec![0.0; dofs];
        for node in 0..l.nodes() {
            for axis in 0..3 {
                if free(node, axis) {
                    y[3 * self.place[node] + axis] = l.load[node][axis] as f64;
                }
            }
        }
        for r in 0..dofs {
            let mut sum = y[r];
            for c in r.saturating_sub(b)..r {
                sum -= a[r * (b + 1) + r - c] * y[c];
            }
            y[r] = sum / a[r * (b + 1)];
        }
        for r in (0..dofs).rev() {
            let mut sum = y[r];
            for c in r + 1..(r + b + 1).min(dofs) {
                sum -= a[c * (b + 1) + c - r] * y[c];
            }
            y[r] = sum / a[r * (b + 1)];
        }
        // back to node order
        let mut u = vec![0.0; dofs];
        for node in 0..l.nodes() {
            for axis in 0..3 {
                u[3 * node + axis] = y[3 * self.place[node] + axis];
            }
        }
        u
    }
}

/// The filter over a level: for each cell its window's cells and weights, and its normaliser.
struct Filter {
    near: Vec<Vec<(usize, f64)>>,
    hs: Vec<f64>,
}

impl Filter {
    fn new(l: &Level) -> Filter {
        let reach = (l.rmin / l.h).ceil() as isize;
        let n = l.n.map(|c| c as isize);
        let mut near = Vec::with_capacity(l.cells());
        for k in 0..n[2] {
            for j in 0..n[1] {
                for i in 0..n[0] {
                    let mut row = Vec::new();
                    for dz in -reach..=reach {
                        for dy in -reach..=reach {
                            for dx in -reach..=reach {
                                let (x, y, z) = (i + dx, j + dy, k + dz);
                                if x < 0 || y < 0 || z < 0 || x >= n[0] || y >= n[1] || z >= n[2] {
                                    continue;
                                }
                                let w = (l.rmin - l.h * ((dx * dx + dy * dy + dz * dz) as f64).sqrt()).max(0.0);
                                row.push(((x + n[0] * (y + n[1] * z)) as usize, w));
                            }
                        }
                    }
                    near.push(row);
                }
            }
        }
        let hs = near.iter().map(|row| row.iter().map(|(_, w)| w).sum()).collect();
        Filter { near, hs }
    }

    fn forward(&self, v: &[f64]) -> Vec<f64> {
        self.near.iter().zip(&self.hs).map(|(row, hs)| row.iter().map(|(j, w)| w * v[*j]).sum::<f64>() / hs).collect()
    }

    fn transpose(&self, v: &[f64]) -> Vec<f64> {
        self.near.iter().map(|row| row.iter().map(|(j, w)| w * v[*j] / self.hs[*j]).sum()).collect()
    }
}

/// `sum` falling from lo to hi: where it crosses `target`.
fn bisect(mut lo: f64, mut hi: f64, target: f64, sum: impl Fn(f64) -> f64) -> f64 {
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if sum(mid) <= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    0.5 * (lo + hi)
}

/// One level of the oracle: what its iterations need and the density they move.
struct Twin {
    k0: [[f64; 24]; 24],
    filter: Filter,
    banded: Banded,
    x: Vec<f64>,
    /// The last iteration's, for what its slopes round to.
    compliance: f64,
}

impl Twin {
    /// One design iteration under `l`'s load, as the program's.
    fn iterate(&mut self, job: &Job, l: &Level, index: usize, it: usize, rev: usize, target: f64, beta: f64, step: f64) -> Iteration {
        let cells = l.cells();
        let (k0, filter, x) = (&self.k0, &self.filter, &mut self.x);
        let xt = filter.forward(x);
        let projected = |eta: f64, xt: f64| {
            let tb = (beta * eta).tanh();
            (tb + (beta * (xt - eta)).tanh()) / (tb + (beta * (1.0 - eta)).tanh())
        };
        let eta = bisect(0.0, 1.0, target, |eta| {
            (0..cells).map(|s| match l.mask[s] { KEEP => 1.0, AVOID => 0.0, _ => projected(eta, xt[s]) }).sum()
        });
        let id = 1.0 / ((beta * eta).tanh() + (beta * (1.0 - eta)).tanh());
        let mut xp = vec![0.0; cells];
        let mut dh = vec![0.0; cells];
        for s in 0..cells {
            match l.mask[s] {
                KEEP => xp[s] = 1.0,
                AVOID => {}
                _ => {
                    xp[s] = projected(eta, xt[s]);
                    let c = (beta * (xt[s] - eta)).cosh();
                    dh[s] = beta * id / (c * c);
                }
            }
        }
        let e: Vec<f64> = xp.iter().map(|h| job.emin + h * h * h * (1.0 - job.emin)).collect();
        let u = self.banded.solve(l, k0, &e);
        let compliance = (0..l.nodes()).map(|n| (0..3).map(|a| l.load[n][a] as f64 * u[3 * n + a]).sum::<f64>()).sum();
        let dc: Vec<f64> = (0..cells)
            .map(|s| {
                let nodes = corners(l, s);
                let mut acc = 0.0;
                for r in 0..24 {
                    for c in 0..24 {
                        acc += u[3 * nodes[r / 3] + r % 3] * k0[r][c] * u[3 * nodes[c / 3] + c % 3];
                    }
                }
                -3.0 * xp[s] * xp[s] * (1.0 - job.emin) * acc * dh[s]
            })
            .collect();
        let dcf = filter.transpose(&dc);
        let dvf = filter.transpose(&dh);
        let floor = 3e-9 * self.compliance / target;
        self.compliance = compliance;
        let ratio: Vec<f64> = (0..cells).map(|s| if l.mask[s] == 0 && dvf[s] > 1e-20 { (-dcf[s] / dvf[s]).max(floor).sqrt() } else { -1.0 }).collect();
        let moved = |s: usize, ln: f64| {
            if ratio[s] < 0.0 {
                return x[s];
            }
            (x[s] * ratio[s] * (-0.5 * ln).exp()).clamp((x[s] - step).max(0.0), (x[s] + step).min(1.0))
        };
        // the free cells' density stays at what the volume asks, so a threshold holds the volume
        let density = target - l.mask.iter().filter(|m| **m == KEEP).count() as f64;
        let ln = bisect(-88.0, 88.0, density, |ln| (0..cells).filter(|s| l.mask[*s] != KEEP && l.mask[*s] != AVOID).map(|s| moved(s, ln)).sum());
        *x = (0..cells).map(|s| moved(s, ln)).collect();
        Iteration { level: index, it, rev, pcg: 0, compliance, volume: xp.iter().sum(), target, residual: 0.0 }
    }
}

/// The same grow in f64 with exact solves, iteration by iteration, then `edit`'s on its last level.
pub fn oracle(job: &Job, edit: Option<(usize, &Edit)>) -> Vec<Iteration> {
    let mut out = Vec::new();
    let mut twin: Option<(Twin, [usize; 3])> = None;
    for (index, l) in job.levels.iter().enumerate() {
        let cells = l.cells();
        // a kept cell is solid and an avoided one empty, whatever it starts from
        let x = (0..cells)
            .map(|s| match (l.mask[s], &twin) {
                (KEEP, _) => 1.0,
                (AVOID, _) => 0.0,
                (_, None) => job.volume,
                (_, Some((coarse, c))) => {
                    let (i, j, k) = (s % l.n[0], (s / l.n[0]) % l.n[1], s / (l.n[0] * l.n[1]));
                    coarse.x[i / 2 + c[0] * (j / 2 + c[1] * (k / 2))]
                }
            })
            .collect();
        let compliance = twin.as_ref().map_or(0.0, |(coarse, _)| coarse.compliance);
        let mut level = Twin { k0: element(job.e0, job.nu, [l.h; 3]), filter: Filter::new(l), banded: Banded::new(l), x, compliance };
        let target = l.target(job.volume);
        for it in 0..l.its {
            let beta = if l.its > 1 { l.beta[0] * (l.beta[1] / l.beta[0]).powf(it as f64 / (l.its - 1) as f64) } else { l.beta[0] };
            out.push(level.iterate(job, l, index, it, 0, target, beta, job.step));
        }
        twin = Some((level, l.n));
    }
    if let (Some((rev, edit)), Some((level, _)), Some(l)) = (edit, twin.as_mut(), job.levels.last()) {
        let index = job.levels.len() - 1;
        let edited = Level { load: edit.load[index].clone(), ..l.clone() };
        let target = l.target(edit.volume);
        for it in 0..job.warm_its {
            out.push(level.iterate(job, &edited, index, l.its + it, rev, target, l.beta[1], job.warm_step));
        }
    }
    out
}
