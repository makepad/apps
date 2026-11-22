//! Fab Force's solve as a compute program ([`FORCE`]): `A u = f` on the [`crate::solve::Hierarchy`] of an
//! analysed [`Level`] by multigrid-preconditioned CG in f64, the solver of [`crate::solve`] in
//! Splash kernels. A program runs the same in-process and on a node (`makepad_compute_program`),
//! so Fab can send a study's solve to the fleet and get back the bits it would have computed
//! itself.
//!
//! [`job`] lays the analysed level, a load and a guess out as the program's parameters and
//! inputs (its held masks and shared cell matrices; its cells integrated point by point as their
//! Gauss points' materials and weights; the load where it is not zero; the guess as the solve
//! starts from it): what a node is sent, so nothing the program can build itself (the coarser
//! levels, Galerkin as [`crate::solve::Hierarchy::new`] builds them, and the coarsest one's
//! factor; the inverse diagonal blocks; the dense load). A node keeps the coarser levels and the
//! factor it built in the stream's keep slot, so a load edit solves on them at once. The GPU
//! engine's cycle takes the factor narrowed to f32.
//! [`answer`] reads the displacement and the solve's outcome back; [`solve`] runs it in this
//! process. [`job_block`] sends several loads, solved in turn in one session on the levels it
//! sets up once (the buckling stage's preconditioner solves a LOBPCG step's residuals so), and
//! [`answers`] reads them back. The program picks its engine: with a GPU device that holds the solve, its GPU
//! engine's (f32 runs to [`INNER_TOLERANCE`], corrected in f64, at most [`MAX_RUNS`] of them),
//! else the CPU solver's in f64.

use crate::solve::{grads, guess_on, CellStiffness, Level, Options, Solution, EMPTY};
use crate::V3;
use makepad_compute_program::gpu::GpuLink;
use makepad_compute_program::{ElemTy, Input, Limits, OutputData, Program, ProgramError, Session, SessionConfig, Updates, Value};

/// The program's Splash source.
pub const FORCE: &str = include_str!("../programs/force.splash");

/// The relative residual each GPU run solves its defect to: f32 alone stops near 1e-3 on a
/// slender part, and a looser run restarts sooner from the f64 defect (measured on the bench:
/// 1e-2 takes the fewest iterations on the bracket and the cantilever, 1e-3 and 1e-4 up to
/// half again as many).
const INNER_TOLERANCE: f64 = 1e-2;

/// Corrections a GPU solve takes at most (each gains about [`INNER_TOLERANCE`]).
const MAX_RUNS: usize = 12;

/// Values a dot product's parts cover, and the most parts the program sums.
const PART: usize = 4096;
const MAX_PARTS: usize = 16384;

pub fn program() -> Program {
    Program::new("force.splash", FORCE)
}

/// f64s as words, low word first (little-endian, as every target makepad runs on).
fn f64_words(values: &[f64]) -> Vec<u32> {
    bytemuck::cast_slice(values).to_vec()
}

/// An input of at least one element: the program binds every one of its inputs.
fn input(name: &str, ty: ElemTy, mut words: Vec<u32>) -> Input {
    if words.is_empty() {
        words.resize(ty.words(), 0);
    }
    Input::words(name, ty, words.into())
}

/// The program's parameters and inputs for `A u = f` on the hierarchy [`crate::solve::Hierarchy::new`] builds
/// from `fine` (an analysed level, [`Level::new`]) with `opts`, from `guess` with `opts`'s
/// tolerance, iteration cap and smoother: the solve [`crate::solve::solve_until`] runs (a guess
/// not on `fine`'s grid starts at rest, as there).
pub fn job(fine: &Level, f: &[V3], guess: Option<&[V3]>, opts: &Options) -> Result<(Value, Vec<Input>), String> {
    job_block(fine, &[f], guess, opts, Solves::ToTolerance)
}

/// How a block's loads are solved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Solves {
    /// Each to the options' tolerance.
    ToTolerance,
    /// A preconditioner's solves: on the GPU engine one run each (to [`INNER_TOLERANCE`]), its
    /// defect not taken, which is what LOBPCG's `T` needs (FORCE A4 2.2); on the CPU engine to
    /// the tolerance.
    OneRun,
}

/// [`job`] for a block of loads, solved in turn in one session on the levels it sets up once
/// (each from rest, or from `guess` for a block of one): what each would be alone, bit for bit.
pub fn job_block(fine: &Level, loads: &[&[V3]], guess: Option<&[V3]>, opts: &Options, solves: Solves) -> Result<(Value, Vec<Input>), String> {
    let n = Value::Number;
    let g = &fine.grid;
    let values = g.nodes() * 3;
    let parts = values.div_ceil(PART);
    if parts > MAX_PARTS {
        return Err(format!("{} nodes: the program takes at most {}", g.nodes(), MAX_PARTS * PART / 3));
    }
    if loads.is_empty() {
        return Err("a block of no loads".into());
    }
    if let Some(f) = loads.iter().find(|f| f.len() != g.nodes()) {
        return Err(format!("a load of {} nodes on a grid of {}", f.len(), g.nodes()));
    }
    if guess.is_some() && loads.len() > 1 {
        return Err("a guess starts a block of one load".into());
    }
    let CellStiffness::Points { cell, shared, materials, points: gauss, weights } = fine.cell_stiffness() else {
        return Err("the program solves from an analysed level, not a coarse one".into());
    };
    // each point cell's corner 0 node and offset in natural coordinates, in cell index order
    let (pnode, pxi): (Vec<u32>, Vec<V3>) = cell.iter().enumerate().filter(|&(_, &i)| i != EMPTY && i as usize >= shared.len()).map(|(c, _)| {
        let o = g.offset_of(c);
        (g.cell_nodes(c)[0] as u32, std::array::from_fn(|a| 2.0 * o[a] / g.h[a]))
    }).unzip();
    let points = pnode.len();
    // the cells with stiffness (the GPU engine lists them)
    let active = cell.iter().filter(|&&m| m != EMPTY).count();
    let nshared = shared.len();
    let mut inputs = vec![
        input("pnode", ElemTy::I32, pnode),
        input("pxi", ElemTy::F64, f64_words(pxi.as_flattened())),
        input("gpd", ElemTy::I32, gauss.as_flattened().to_vec()),
        input("gpw", ElemTy::F64, f64_words(weights.as_flattened())),
        input("ds", ElemTy::F64, f64_words(materials.as_flattened().as_flattened())),
        input("dn", ElemTy::F64, f64_words(grads(g.h).0.as_flattened().as_flattened())),
        input("held", ElemTy::I32, fine.held.iter().map(|&b| b as u32).collect()),
        input("mats", ElemTy::F64, f64_words(shared.as_flattened())),
        input("cell", ElemTy::I32, cell),
    ];
    // each load where it is not zero (most nodes carry none), one after another
    let loaded: Vec<Vec<usize>> = loads.iter().map(|f| (0..f.len()).filter(|&n| f[n].iter().any(|&v| v != 0.0)).collect()).collect();
    inputs.push(input("fnode", ElemTy::I32, loaded.iter().flatten().map(|&n| n as u32).collect()));
    inputs.push(input("fval", ElemTy::F64, f64_words(&loads.iter().zip(&loaded).flat_map(|(f, at)| at.iter().flat_map(|&n| f[n])).collect::<Vec<f64>>())));
    let guess = guess.filter(|u| u.len() == g.nodes()).map(|u| guess_on(fine, u));
    if let Some(u) = &guess {
        inputs.push(input("guess", ElemTy::F64, f64_words(u.as_flattened())));
    }
    let params = Value::object(&[
        ("n", Value::Array(g.n.iter().map(|&c| n(c as f64)).collect())),
        ("h", Value::Array(g.h.iter().map(|&h| n(h)).collect())),
        ("gp", n(crate::element::GP)),
        ("nshared", n(nshared as f64)),
        ("active", n(active as f64)),
        ("points", n(points as f64)),
        ("coarsest_nodes", n(opts.coarsest() as f64)),
        ("loads", Value::Array(loaded.iter().map(|at| n(at.len() as f64)).collect())),
        ("parts", n(parts as f64)),
        ("tol", n(opts.tolerance)),
        ("max_its", n(opts.max_iterations as f64)),
        ("smooth", n(opts.smooth as f64)),
        ("omega", n(opts.omega)),
        ("guess", n(if guess.is_some() { 1.0 } else { 0.0 })),
        ("inner", n(INNER_TOLERANCE)),
        ("max_runs", n(MAX_RUNS as f64)),
        ("runs", n(if solves == Solves::OneRun { 1.0 } else { 0.0 })),
    ]);
    Ok((params, inputs))
}

/// The solve from the program's updates once it ended: its published `u` and its outcome (a
/// block's first load's: [`answers`]).
pub fn answer(u: &Updates) -> Option<Result<Solution, String>> {
    answers(u).map(|done| done.map(|mut all| all.swap_remove(0)))
}

/// A block's solves ([`job_block`]) from the program's updates once it ended, in its loads'
/// order: each one's published `u` and outcome.
pub fn answers(u: &Updates) -> Option<Result<Vec<Solution>, String>> {
    let end = u.end.as_ref()?;
    let value = match end {
        Ok(v) => v,
        Err(e) => return Some(Err(program_error(e))),
    };
    if let Some(gpu) = value.get("gpu") {
        let at = |k: &str| gpu.get(k).and_then(Value::as_number).unwrap_or(0.0);
        if at("finished") > 0.0 {
            eprintln!("fab force: the GPU engine reached its f32 floor on {} of the block's loads (the last at residual {:.1e}); the f64 engine finished them", at("finished"), at("floor").max(0.0).sqrt());
        }
    }
    if let Some(remade) = value.get("remade").and_then(Value::as_number).filter(|&n| n > 0.0) {
        eprintln!("fab force: the f64 finish made {remade} coarse levels' f64 matrices again (freed once the GPU engine packed its own)");
    }
    if let Some(factor) = value.get("factor") {
        let at = |k: &str| factor.get(k).and_then(Value::as_number).unwrap_or(0.0);
        if at("dropped") > 0.0 {
            eprintln!("fab force: the coarsest level's factor dropped {} of its {} pivots (solved as zero there)", at("dropped"), at("size"));
        }
    }
    let Some(Value::Array(solves)) = value.get("solves").filter(|s| matches!(s, Value::Array(a) if !a.is_empty())) else {
        return Some(Err(format!("the program ended with {value:?}")));
    };
    let mut all = Vec::with_capacity(solves.len());
    for (k, solve) in solves.iter().enumerate() {
        let number = |key: &str| solve.get(key).and_then(Value::as_number);
        let (Some(its), Some(rr), Some(bb)) = (number("its"), number("rr"), number("bb")) else {
            return Some(Err(format!("the program ended with {value:?}")));
        };
        let Some(OutputData::Words(words)) = u.output(&format!("u{k}")).map(|o| &o.data) else {
            return Some(Err(format!("the program published no displacement for load {k}")));
        };
        let mut disp = vec![[0.0; 3]; words.len() / 6];
        let n = 6 * disp.len();
        bytemuck::cast_slice_mut::<V3, u32>(&mut disp).copy_from_slice(&words[..n]);
        let residual = if bb > 0.0 { (rr / bb).sqrt() } else { 0.0 };
        all.push(Solution { u: disp, iterations: its as usize, residual });
    }
    Some(Ok(all))
}

/// Once the program ended: whether its GPU engine solved (else its CPU engine), and the GPU memory
/// its GPU route needs in all and in its largest buffer (bytes: what it compared with the
/// device's limits to pick its engine).
pub fn engine(u: &Updates) -> Option<(bool, f64, f64)> {
    let Ok(value) = u.end.as_ref()? else {
        return None;
    };
    let gpu = value.get("gpu").is_some_and(|g| g.get("runs").is_some());
    let at = |k: &str| value.get(k).and_then(Value::as_number).unwrap_or(0.0);
    Some((gpu, at("gpu_need"), at("gpu_largest")))
}

fn program_error(e: &ProgramError) -> String {
    format!("{e}")
}

/// The limits a solve runs under in this process: counted work and bytes, never a cap that a
/// large part would meet before the machine does.
pub fn limits() -> Limits {
    Limits { fuel: u64::MAX, work: u64::MAX, buffer_bytes: u64::MAX, ..Limits::default() }
}

/// The solve's progress from the program's updates: its iterations and the residual's share of
/// the load, from the start on.
pub fn progress(u: &Updates) -> Option<(usize, f64)> {
    let cg = u.value("cg")?;
    let number = |k: &str| cg.get(k).and_then(Value::as_number);
    let (its, rr, bb) = (number("its")?, number("rr")?, number("bb")?);
    Some((its as usize, if bb > 0.0 { (rr / bb).sqrt() } else { 0.0 }))
}

/// [`crate::solve::solve_until`] (never stopped) on the hierarchy of `fine` as the program, in
/// this process.
pub fn solve(fine: &Level, f: &[V3], guess: Option<&[V3]>, opts: &Options) -> Result<Solution, String> {
    solve_on(fine, f, guess, opts, None)
}

/// [`solve`] with a GPU device: the program's GPU engine where it holds the solve.
pub fn solve_on(fine: &Level, f: &[V3], guess: Option<&[V3]>, opts: &Options, gpu: Option<GpuLink>) -> Result<Solution, String> {
    let (params, inputs) = job(fine, f, guess, opts)?;
    let session = Session::open(&program(), SessionConfig { params, inputs, limits: limits(), gpu, ..SessionConfig::default() });
    let mut all = Updates::default();
    loop {
        all.merge(session.wait());
        if let Some(done) = answer(&all) {
            return done;
        }
    }
}
