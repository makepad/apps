//! The cells, parts and jobs the lab runs, and a session: one job stepping through the planner
//! and the virtual cell, as far as it is asked to go at a time.

use crate::fdm::{fit_to_rating, park_over, FdmCell, FdmPlan, Part, FDM_WORK_HEIGHT};
use fab_arms::design::Design;
use fab_arms::fdm::Fdm;
use fab_arms::path::Motion;
use fab_arms::planner::{LoopMode, Opts, Planner, Report};
use fab_arms::route::{DedTrim, DedTrimOpts, Route};
use fab_cell_sim::{Errors, SimCell, SimConfig};
use fab_robot::arm::Q;
use fab_robot::cell::CellSetup;
use fab_robot::field::Shape;
use fab_robot::io::CellLink;
use fab_robot::math::*;
use fab_robot::presets::*;

/// A cell the lab builds in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Cell {
    /// Two desktop arms (754 mm, quasi-direct drives): one deposits, one cuts.
    #[default]
    DesktopPair,
    /// One 850 on its controller with the dual head (weld and cut behind one source and a
    /// beam switch) and a line scanner, over a tilt and turn positioner.
    Single850,
    /// One arm printing plastic.
    Fdm(FdmCell),
}

impl Cell {
    /// The metal cells (the plastic ones: every arm with every head, `FdmCell`).
    pub const ALL: [Cell; 2] = [Cell::DesktopPair, Cell::Single850];

    pub fn label(self) -> String {
        match self {
            Cell::DesktopPair => "Two desktop arms (B601)".into(),
            Cell::Single850 => "One 850, dual head".into(),
            Cell::Fdm(c) => c.label(),
        }
    }

    /// The pair facing over a 40 mm stub the part stands on, the arms' bases beside the plate
    /// level with its top; the one 850 over the stub on its positioner's table.
    pub fn setup(self) -> CellSetup {
        match self {
            Cell::DesktopPair => {
                let mut c = cell_dual_b601();
                c.substrate = c.substrate.on_stub(7.0, 40.0);
                for a in c.arms.iter_mut() {
                    a.base.t[2] = -40.0;
                }
                // Where the arms hold their work just over the stub within their ratings, the
                // preloads with gravity there.
                fit_to_rating(&mut c, self.work_height());
                c
            }
            Cell::Single850 => {
                // The part on the pair's 40 mm stub, on the table: the beams and the heads pass
                // under its lower faces (on the bare table the wrist would meet it).
                let mut c = cell_single_850_dual_head();
                c.substrate = c.substrate.on_stub(7.0, 40.0);
                c.plate = c.plate.mul(&Pose::at([0.0, 0.0, 40.0]));
                fit_to_rating(&mut c, self.work_height());
                c
            }
            Cell::Fdm(c) => c.setup(),
        }
    }

    /// How high over the part (mm) the arms' work pose holds the tool: where the cell is laid
    /// out to hold within the ratings.
    pub fn work_height(self) -> f64 {
        match self {
            Cell::DesktopPair => 20.0,
            Cell::Single850 => 80.0,
            Cell::Fdm(_) => FDM_WORK_HEIGHT,
        }
    }

    /// Where each arm waits while the other works: up and back, behind its base; a lone arm
    /// over the part.
    pub fn park(self) -> Q {
        match self {
            Cell::DesktopPair => [0.0, 0.8, 2.2, 0.0, -1.2, 0.0],
            Cell::Single850 => park_over(&self.setup(), 0, self.work_height()),
            Cell::Fdm(c) => c.park(),
        }
    }

    /// How the planner moves this cell's paths, where it is the cell's own.
    pub fn motion(self) -> Option<Motion> {
        match self {
            Cell::DesktopPair | Cell::Single850 => None,
            Cell::Fdm(c) => Some(c.motion()),
        }
    }
}

/// A ring wall 2.2 mm thick round the stub (outer radius 7.1), `height` tall.
pub fn ring(height: f64) -> Design {
    Design::ring([0.0, 0.0], 4.9, 7.1, height)
}

/// A job: the part, the route, and what already stands in the cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Job {
    /// A 2 mm ring wall laid with the margin, scanned, cut back to the design, scanned.
    #[default]
    BuildAndTrim,
    /// The same wall laid without the margin or the cut.
    Deposit,
    /// The wall already laid with the margin: the cut alone.
    Trim,
}

impl Job {
    pub const ALL: [Job; 3] = [Job::BuildAndTrim, Job::Deposit, Job::Trim];

    pub fn label(self) -> &'static str {
        match self {
            Job::BuildAndTrim => "Ring wall: deposit and trim",
            Job::Deposit => "Ring wall: deposit only",
            Job::Trim => "Trim a laid ring",
        }
    }

    pub fn design(self) -> Design {
        ring(2.0)
    }

    pub fn route(self, design: &Design) -> DedTrim {
        match self {
            Job::BuildAndTrim => DedTrim::new(design, DedTrimOpts::default()),
            Job::Deposit => DedTrim::new(design, DedTrimOpts { trim: false, ..DedTrimOpts::default() }),
            Job::Trim => DedTrim::new(design, DedTrimOpts::default()).from_cut(1),
        }
    }

    /// What stands in the cell at the start.
    pub fn laid(self) -> Option<Design> {
        match self {
            Job::Trim => Some(Design::ring([0.0, 0.0], 4.9, 7.1 + DedTrimOpts::default().margin, 2.0)),
            _ => None,
        }
    }
}

/// A design as a solid to place in the cell.
pub struct Solid<'a>(pub &'a Design);

impl Shape for Solid<'_> {
    fn distance(&self, p: V3) -> f64 {
        self.0.distance(p)
    }
    fn reach(&self) -> (V3, V3) {
        (self.0.lo, self.0.hi)
    }
}

/// How a session is set up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Setup {
    pub cell: Cell,
    pub mode: LoopMode,
    pub errors: Errors,
    pub seed: u64,
    /// Stop after this much cell time (s).
    pub max_time: f64,
}

impl Setup {
    pub fn new(mode: LoopMode, seed: u64) -> Setup {
        Setup { cell: Cell::DesktopPair, mode, errors: Errors::TYPICAL, seed, max_time: 600.0 }
    }
}

/// One job in the virtual cell: the planner driving it, a step at a time.
pub struct Session {
    pub sim: SimCell,
    pub planner: Planner,
    pub setup: Setup,
}

impl Session {
    /// `route` building `design` in the cell `setup` names, `laid` already standing there.
    pub fn new(setup: Setup, design: &Design, route: Box<dyn Route>, laid: Option<&Design>) -> Session {
        Session::in_cell(setup, &setup.cell.setup(), design, route, laid)
    }

    /// As [`Session::new`], in `cell`: `setup.cell`'s with something changed.
    pub fn in_cell(setup: Setup, cell: &CellSetup, design: &Design, route: Box<dyn Route>, laid: Option<&Design>) -> Session {
        let park = setup.cell.park();
        let arms = cell.arms.len();
        let mut sim = SimCell::new(cell, SimConfig::new(setup.seed, setup.errors, vec![park; arms]));
        if let Some(l) = laid {
            sim.place(&Solid(l));
        }
        let mut opts = Opts::new(setup.mode, vec![park; arms]);
        opts.max_time = setup.max_time;
        if let Some(m) = setup.cell.motion() {
            opts.motion = m;
        }
        let planner = Planner::new(sim.info().clone(), design.clone(), route, opts, &vec![park; arms]);
        Session { sim, planner, setup }
    }

    pub fn job(setup: Setup, job: Job) -> Session {
        let design = job.design();
        let route = job.route(&design);
        Session::new(setup, &design, Box::new(route), job.laid().as_ref())
    }

    /// `part` printed in the plastic cell `setup` names, as `plan` slices it.
    pub fn fdm(setup: Setup, part: Part, plan: FdmPlan) -> Session {
        Session::new(setup, &part.design(), Box::new(Fdm::new(plan.layers, plan.opts)), None)
    }

    /// The job's time as the planner plans it, without running the cell (see
    /// [`Planner::dry_run`]); the session is spent.
    pub fn dry_run(&mut self) -> f64 {
        self.planner.dry_run()
    }

    /// Run to cell time `until` (or the job's end); true once the job has ended.
    pub fn run_until(&mut self, until: f64) -> bool {
        self.planner.run_until(&mut self.sim, until)
    }

    /// Run the job to its end.
    pub fn run(&mut self) -> &Report {
        self.planner.run(&mut self.sim)
    }

    pub fn time(&self) -> f64 {
        self.sim.now()
    }
}
