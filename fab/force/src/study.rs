//! A study of one part: its field (or its triangles) sampled at a cell size (the model), the
//! loads on it (kept as the points clicked, or as the test a named region makes of each surface
//! quad, so they pick their regions again on a finer model or an edited part), solved at one
//! cell size after another. A fixture on a hole holds what a bolt through it clamps
//! ([`Surface::clamp`]) unless it is `rigid`.

use crate::bc::{Condition, Kind};
use crate::element::V3;
use crate::grid::{Grid, Lattice};
use crate::material::Materials;
use crate::mesh::Mesh;
use crate::surface::{field_normals, Pick, Region, Surface};
use std::sync::Arc;

/// A fixture or load: what it is and where on the part.
#[derive(Clone, Debug, PartialEq)]
pub struct Load {
    pub kind: Kind,
    pub spot: Spot,
    /// A fixture on a hole holds the hole's wall itself, rigid, not a bolt's clamp.
    pub rigid: bool,
}

impl Load {
    /// A fixture or load at `spot`, a fixture on a hole a bolt's clamp.
    pub fn new(kind: Kind, spot: Spot) -> Load {
        Load { kind, spot, rigid: false }
    }
}

/// Where a load's region is, kept so it picks its region again on every model (a finer one,
/// the part edited).
#[derive(Clone, Debug, PartialEq)]
pub enum Spot {
    /// A point on the surface (printer mm, undeformed): the quad nearest it, grown as picked.
    Seed { pick: Pick, at: V3 },
    /// The quads a named region keeps ([`Within`]).
    Within(Within),
}

/// A region the design names (a feature, a region solid, all of the part), as a test on each
/// surface quad: its centre and outward normal (printer mm) and the model's cell size.
#[derive(Clone)]
pub struct Within {
    /// The region's identity: equal keys keep the same quads (the test is a closure).
    pub key: u64,
    pub keep: Arc<dyn Fn(V3, V3, f64) -> bool + Send + Sync>,
}

impl PartialEq for Within {
    fn eq(&self, o: &Within) -> bool {
        self.key == o.key
    }
}

impl std::fmt::Debug for Within {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Within({:016x})", self.key)
    }
}

/// The part at one cell size: the sampled field, the surface it gives, its cells' materials.
#[derive(Clone, Debug)]
pub struct Model {
    pub h: f64,
    /// The samples the grid was graded from: None once shed ([`Model::shed`]), for a model
    /// solved once that nothing regrades (it is sampled again if something does).
    pub lattice: Option<Arc<Lattice>>,
    pub surface: Arc<Surface>,
    /// Shared with the posed study, the analysed level and the answer solved on it.
    pub grid: Arc<Grid>,
}

impl Model {
    /// Samples the part's field (`distance`: printer-frame points in, distances out) over
    /// `lo..hi` at cells of `h`.
    pub fn new(lo: V3, hi: V3, h: f64, distance: &mut dyn FnMut(&[V3], &mut [f64]), m: &Materials) -> Model {
        let (grid, lattice) = Grid::from_field(lo, hi, h, distance, m);
        // the field's gradient a thousandth of a lattice step across
        let surface = Surface::contour(&lattice, &mut field_normals(distance, 1e-3 * lattice.spacing));
        Model::sampled(h, grid, lattice, surface)
    }

    /// [`Model::new`] for a part given as triangles (printer mm).
    pub fn of_mesh(lo: V3, hi: V3, h: f64, mesh: &Mesh, m: &Materials) -> Model {
        let (grid, lattice) = Grid::from_samples(lo, hi, h, &mut |l| mesh.sample(l), m);
        let surface = Surface::contour(&lattice, &mut |ps, out| mesh.normals(ps, out));
        Model::sampled(h, grid, lattice, surface)
    }

    fn sampled(h: f64, grid: Grid, lattice: Lattice, surface: Surface) -> Model {
        Model { h, lattice: Some(Arc::new(lattice)), surface: Arc::new(surface), grid: Arc::new(grid) }
    }

    /// The same model without its samples: a size solved once frees them before its solve (the
    /// lattice is as large as the grid), its surface and cells kept. It no longer regrades.
    pub fn shed(&self) -> Model {
        Model { lattice: None, ..self.clone() }
    }

    /// The samples' spacing (mm): half a cell, kept or shed.
    pub fn spacing(&self) -> f64 {
        self.lattice.as_ref().map_or(0.5 * self.h, |l| l.spacing)
    }

    /// Whether [`Model::with_materials`] and [`Model::as_sliced`] serve `m`: its samples read
    /// the part's depth as deep as `m`'s walls and skins reach ([`crate::grid::deepest`]),
    /// else the part is sampled again for it (FORCE A15). A shed model does not.
    pub fn regrades(&self, m: &Materials) -> bool {
        self.lattice.as_ref().is_some_and(|l| l.band >= crate::grid::deepest(m, l.spacing))
    }

    /// The same samples with other materials (another filament or other print settings).
    pub fn with_materials(&self, m: &Materials) -> Model {
        self.regraded(m, None)
    }

    /// The same samples with the walls and skins the slicer printed ([`crate::sliced`]), the
    /// rest of each cell `m`'s infill.
    pub fn as_sliced(&self, m: &Materials, printed: &crate::sliced::Printed) -> Model {
        self.regraded(m, Some(printed))
    }

    fn regraded(&self, m: &Materials, printed: Option<&crate::sliced::Printed>) -> Model {
        let lattice = self.lattice.as_ref().expect("a shed model is sampled again, not regraded ([`Model::regrades`])");
        let grid = Grid::from_lattice(lattice, self.grid.n, self.grid.origin, self.h, m, printed);
        Model { h: self.h, lattice: self.lattice.clone(), surface: self.surface.clone(), grid: Arc::new(grid) }
    }

    /// Each load's region on this model's surface, one per load: the quad nearest its seed,
    /// grown as it was picked, or the quads its named region keeps (nothing on a model with no
    /// surface); a fixture on a hole the bolt's clamp, unless rigid.
    pub fn conditions(&self, loads: &[Load]) -> Vec<Condition> {
        loads.iter().map(|l| {
            let region = match &l.spot {
                Spot::Seed { pick, at } => self.surface.nearest(*at).map_or_else(Region::default, |q| self.surface.grow(q, *pick)),
                Spot::Within(w) => self.surface.select(|c, n| (w.keep)(c, n, self.h)),
            };
            let region = match (l.kind, l.rigid) {
                (Kind::Fixed, false) => self.surface.clamp(&region, self.h).unwrap_or(region),
                _ => region,
            };
            Condition { kind: l.kind, region }
        }).collect()
    }

    /// The study is posed: something holds the part and something loads it.
    pub fn posed(loads: &[Load]) -> bool {
        loads.iter().any(|l| l.kind == Kind::Fixed) && loads.iter().any(|l| l.kind != Kind::Fixed)
    }

    /// Solves this model under `conditions` (its [`Model::conditions`]); `stop` as
    /// [`crate::solve::solve_until`]'s.
    pub fn solve(&self, m: &Materials, conditions: &[Condition], opts: &crate::solve::Options, stop: &mut dyn FnMut(usize, f64) -> bool) -> Result<crate::Outcome, String> {
        Ok(crate::analyse_posed(self.pose(conditions)?, m, None, opts, stop))
    }

    /// The study as the solvers take it ([`crate::pose`]): the pieces nothing holds left out,
    /// the cells near the fixtures marked.
    pub fn pose(&self, conditions: &[Condition]) -> Result<crate::Posed, String> {
        let (held, force) = crate::bc::nodes(&self.grid, &self.surface, conditions);
        let near = crate::bc::near(&self.grid, &self.surface, conditions);
        let mut posed = crate::pose(self.grid.clone(), held, force, near)?;
        posed.surface = Some(self.surface.clone());
        Ok(posed)
    }
}

/// Cells over the part's box at a study's first step.
pub const FIRST_CELLS: f64 = 16_000.0;

/// Where the finest step's grid is solved a second time, so the verdict sees how much an
/// extreme moves with where the cells cut the part ([`crate::results::Verdict`]): its box
/// moved by this (half a cell of `h` along every axis), the cells' corners where their
/// centres were.
pub fn shift(h: f64) -> V3 {
    [0.5 * h; 3]
}

/// The first cell size gives about [`FIRST_CELLS`] over the box. A small part needs its
/// last own size for the verdict just as a large part does, so leave room for every halving.
pub fn first(lo: V3, hi: V3, own: usize, finest: f64) -> f64 {
    let volume: f64 = (0..3).map(|a| (hi[a] - lo[a]).max(1e-3)).product();
    (volume / FIRST_CELLS).cbrt().max(finest * 2.0_f64.powf(own.saturating_sub(1) as f64))
}

/// Up to `levels` sizes from `first`, each half the last, none finer than `finest`.
pub fn halvings(first: f64, levels: usize, finest: f64) -> Vec<f64> {
    let mut h = first.max(finest);
    let mut out = vec![h];
    while out.len() < levels && h * 0.5 >= finest {
        h *= 0.5;
        out.push(h);
    }
    out
}

/// The cell sizes a study steps through: the first gives about [`FIRST_CELLS`] over the
/// part's box (a first answer in a fraction of the time), each next one half the last,
/// `levels` of them, none finer than `finest`.
pub fn steps(lo: V3, hi: V3, levels: usize, finest: f64) -> Vec<f64> {
    halvings(first(lo, hi, 1, finest), levels, finest)
}
