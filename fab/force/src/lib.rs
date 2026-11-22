//! Fab Force: linear elastic stress analysis of a part as it will print.
//!
//! - [`material`]: the printed-part material model (walls, skins and infill from the slicer's
//!   settings; its sources are in the module's documentation) and the strength check;
//! - [`grid`]: the voxel grid over the part's distance field, partial cells at the surface;
//! - [`mesh`]: a part given as triangles, sampled as the distance field the grid takes;
//! - [`element`]: the trilinear hexahedron, evaluated on the fly;
//! - [`solve`]: the reference solver (f64), multigrid-preconditioned CG;
//! - [`results`]: stress, safety factor and their extremes;
//! - [`buckle`]: linear buckling under a solved load: the geometric stiffness and the modes;
//! - [`surface`]: the surface the Force render mode draws and picks fixtures and loads on;
//! - [`bc`]: fixtures and loads on picked regions, as node holds and forces;
//! - [`pieces`]: the part's pieces, and those nothing holds (left out of the solve);
//! - [`study`]: a part at one cell size after another, its loads kept as the points picked;
//! - [`sliced`]: the part's walls and skins as the slicer printed them, into the grid's cells;
//! - [`thin`]: how thick the part is under its surface, against the cells that read it;
//! - [`display`]: the surface as drawn, with its fields, and a ray's hit on it;
//! - [`opt`]: stress-driven lightening: where a part may be cut, the cuts, their search;
//! - [`program`]: the solve as a compute program (Splash kernels, f64; on a GPU device f32 runs
//!   corrected in f64), in-process or on a node;
//! - `hub` (feature `node`): studies and batches solved on the AI hub (this machine's, and the
//!   fleet's with networked AI hub on), the program on the device the hub picks: Fab's one
//!   route for its solves.
//!
//! Units: mm, N, MPa (N/mm^2), printer frame (Z the build direction).

pub mod bc;
pub mod bench;
pub mod buckle;
pub mod display;
pub mod element;
#[cfg(feature = "node")]
pub mod hub;
pub mod grid;
pub mod material;
pub mod mesh;
pub mod opt;
pub mod pieces;
pub mod program;
pub mod results;
pub mod sliced;
pub mod solve;
pub mod study;
pub mod thin;
pub mod surface;

#[cfg(test)]
mod tests;

pub use element::V3;
use std::sync::Arc;

/// One analysis at one resolution: the grid, the surface its lattice gives, the solve and
/// what it means.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// Shared with the model and the posed study it is of.
    pub grid: Arc<grid::Grid>,
    pub held: Vec<u8>,
    pub solution: solve::Solution,
    pub results: results::Results,
    /// The pieces nothing holds, left out of the solve.
    pub loose: pieces::Loose,
}

/// A study as the solvers take it: the grid without the pieces nothing holds, its holds and
/// forces per node, the cells near its fixtures, and what it left out.
#[derive(Clone, Debug)]
pub struct Posed {
    /// The model's own until a loose piece is left out of it (then a copy without it).
    pub grid: Arc<grid::Grid>,
    pub held: Vec<u8>,
    pub force: Vec<V3>,
    /// Per cell: near a fixture ([`bc::near`], or touching a held node), its safety factor
    /// read apart.
    pub near: Vec<bool>,
    pub loose: pieces::Loose,
    /// The surface its stress is recovered on. None gives cell means only.
    pub surface: Option<Arc<surface::Surface>>,
}

/// Leaves the pieces nothing in `held` holds out of `grid` ([`pieces::drop_loose`]); an error
/// when a load is on one: nothing could carry it. `near` (per cell, [`bc::near`]) takes the
/// cells touching a held node too.
pub fn pose(grid: impl Into<Arc<grid::Grid>>, held: Vec<u8>, mut force: Vec<V3>, mut near: Vec<bool>) -> Result<Posed, String> {
    let mut grid: Arc<grid::Grid> = grid.into();
    let (loose, share) = pieces::drop_loose(&mut grid, &held, &mut force);
    if share > 0.0 {
        return Err(format!("{:.0} % of the load is on {}, which nothing holds: no load path carries it. Hold that piece too, or load the part where a fixture holds it.", 100.0 * share, loose.describe()));
    }
    for (c, near) in near.iter_mut().enumerate() {
        *near = *near || grid.cell_nodes(c).iter().any(|&n| held[n] != 0);
    }
    Ok(Posed { grid, held, force, near, loose, surface: None })
}

/// Solves `grid` under `conditions` (regions of `surface`) with `materials`.
pub fn analyse(grid: grid::Grid, surface: &surface::Surface, materials: &material::Materials, conditions: &[bc::Condition], opts: &solve::Options) -> Result<Outcome, String> {
    let (held, force) = bc::nodes(&grid, surface, conditions);
    let near = bc::near(&grid, surface, conditions);
    let mut posed = pose(grid, held, force, near)?;
    posed.surface = Some(Arc::new(surface.clone()));
    Ok(analyse_posed(posed, materials, None, opts, &mut |_, _| false))
}

/// [`analyse`] with the holds and forces given per node (the cells touching a held node near
/// the fixtures).
pub fn analyse_nodes(grid: grid::Grid, held: Vec<u8>, force: Vec<V3>, materials: &material::Materials, opts: &solve::Options) -> Result<Outcome, String> {
    let near = vec![false; grid.cells()];
    Ok(analyse_posed(pose(grid, held, force, near)?, materials, None, opts, &mut |_, _| false))
}

/// Solves a posed study on the CPU from `guess`, `guess` and `stop` as
/// [`solve::solve_until`]'s.
pub fn analyse_posed(posed: Posed, materials: &material::Materials, guess: Option<&[V3]>, opts: &solve::Options, stop: &mut dyn FnMut(usize, f64) -> bool) -> Outcome {
    let h = solve::Hierarchy::new(posed.grid.clone(), &posed.held, materials, opts);
    analyse_on(&h, posed, materials, guess, opts, stop)
}

/// [`analyse_posed`] on `h`, a hierarchy built for `posed`'s grid and held nodes with these
/// materials: a study whose loads alone changed solves on the one it has.
pub fn analyse_on(h: &solve::Hierarchy, posed: Posed, materials: &material::Materials, guess: Option<&[V3]>, opts: &solve::Options, stop: &mut dyn FnMut(usize, f64) -> bool) -> Outcome {
    let solution = solve::solve_until(h, &posed.force, guess, opts, stop);
    outcome(posed.grid, posed.held, &posed.near, solution, materials, posed.loose, posed.surface.as_deref())
}

/// What a solve on `grid` means: stress, safety factor and their extremes (`near`: per cell,
/// near a fixture). Whichever engine solved.
pub fn outcome(grid: impl Into<Arc<grid::Grid>>, held: Vec<u8>, near: &[bool], solution: solve::Solution, materials: &material::Materials, loose: pieces::Loose, surface: Option<&surface::Surface>) -> Outcome {
    let grid: Arc<grid::Grid> = grid.into();
    let results = results::evaluate(&grid, materials, &solution.u, near, surface);
    Outcome { grid, held, solution, results, loose }
}
