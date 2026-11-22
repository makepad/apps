//! Plastic on an arm. The part is sliced by Fab's own toolpath planner (the one every printer
//! job uses) with the printer's process and the arm's extruder in place of the printer's
//! nozzle: its nozzle, layer height, line width and melt flow. Those layers are the arm's
//! route; the planner times them with the arm's own motion limits. The same part sliced for
//! the printer (the built-in H2D, `printer`) gives Fab's own print time to set the arm's
//! against.

use fab_arms::design::{Design, Sdf};
use fab_arms::fdm::{FdmLayer, FdmOpts, FdmPath};
use fab_arms::path::Motion;
use fab_arms::route::{IK, W_MIN};
use fab_robot::arm::*;
use fab_robot::cell::CellSetup;
use fab_robot::dynamics::{aim_preload, first_mode, torque_use, Wrench};
use fab_robot::kin::{frames, ik, manipulability, IkGoal};
use fab_robot::math::*;
use fab_robot::presets::{fdm_cell, FDM_ARMS, HEADS};
use makepad_fab_settings::machine::Job;
use makepad_fab_settings::profiles::JobSetup;
use makepad_fab_slicer::sdf::SdfSource;
use makepad_fab_toolpath::{generate, ObjectInput, Toolpaths};
use std::sync::Arc;

/// A part to print.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Part {
    /// 8 × 8 × 1.2 mm: two layers at 0.6 mm (the tests' part).
    #[default]
    Tile,
    /// A 20 mm calibration cube.
    Cube,
    /// A 60 × 40 × 25 mm block.
    Block,
    /// A cup: 60 mm across, 50 mm tall, 2.4 mm wall and floor.
    Cup,
}

impl Part {
    pub const ALL: [Part; 4] = [Part::Tile, Part::Cube, Part::Block, Part::Cup];

    pub fn label(self) -> &'static str {
        match self {
            Part::Tile => "Tile 8 × 8 × 1.2 mm",
            Part::Cube => "Cube 20 mm",
            Part::Block => "Block 60 × 40 × 25 mm",
            Part::Cup => "Cup ⌀60 × 50 mm",
        }
    }

    /// The part on the plate (part frame: centred, its base at z 0).
    pub fn design(self) -> Design {
        match self {
            Part::Tile => block([8.0, 8.0, 1.2]),
            Part::Cube => block([20.0; 3]),
            Part::Block => block([60.0, 40.0, 25.0]),
            Part::Cup => cup(30.0, 50.0, 2.4),
        }
    }
}

fn block(size: V3) -> Design {
    let half = scale(size, 0.5);
    let sdf: Sdf = Arc::new(move |p: V3| {
        let q = [p[0].abs() - half[0], p[1].abs() - half[1], (p[2] - half[2]).abs() - half[2]];
        let out = len([q[0].max(0.0), q[1].max(0.0), q[2].max(0.0)]);
        out + q[0].max(q[1]).max(q[2]).min(0.0)
    });
    Design::new(sdf, [-half[0], -half[1], 0.0], [half[0], half[1], size[2]])
}

fn cup(radius: f64, height: f64, wall: f64) -> Design {
    let cylinder = |p: V3, r: f64, z0: f64, z1: f64| {
        let d = [(p[0] * p[0] + p[1] * p[1]).sqrt() - r, (p[2] - 0.5 * (z0 + z1)).abs() - 0.5 * (z1 - z0)];
        d[0].max(d[1]).min(0.0) + d[0].max(0.0).hypot(d[1].max(0.0))
    };
    let sdf: Sdf = Arc::new(move |p: V3| cylinder(p, radius, 0.0, height).max(-cylinder(p, radius - wall, wall, height + 1.0)));
    Design::new(sdf, [-radius, -radius, 0.0], [radius, radius, height])
}

/// A one-arm print cell: an arm from [`FDM_ARMS`], a head from [`HEADS`], the nozzle in
/// tenths of a mm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FdmCell {
    pub arm: u8,
    pub head: u8,
    pub nozzle: u8,
}

impl Default for FdmCell {
    fn default() -> Self {
        FdmCell { arm: 0, head: 0, nozzle: 10 }
    }
}

impl FdmCell {
    pub fn nozzle_mm(self) -> f64 {
        self.nozzle as f64 / 10.0
    }

    pub fn tool(self) -> Tool {
        (HEADS[self.head as usize].make)(self.nozzle_mm())
    }

    pub fn extruder(self) -> Extruder {
        self.tool().extruder().expect("an FDM head extrudes")
    }

    /// The cell, the arm where it holds its print pose within its ratings (its preloads, if
    /// any, pointed with gravity there).
    pub fn setup(self) -> CellSetup {
        let mut c = fdm_cell(&FDM_ARMS[self.arm as usize], self.tool());
        fit_to_rating(&mut c, FDM_WORK_HEIGHT);
        c
    }

    pub fn label(self) -> String {
        format!("{}, {} {:.1} mm", FDM_ARMS[self.arm as usize].label, HEADS[self.head as usize].label, self.extruder().nozzle)
    }

    /// Where the arm waits: tool down 60 mm over the bed's centre.
    pub fn park(self) -> Q {
        park_over(&self.setup(), 0, FDM_WORK_HEIGHT)
    }

    /// How the arm prints: [`print_accel`] on its first mode at park, that acceleration times
    /// the mode as its jerk, corners rounded by 0.05 mm, no rest at starts.
    pub fn motion(self) -> Motion {
        let c = self.setup();
        let f1 = first_mode(&c.arms[0], &self.park());
        let accel = print_accel(f1);
        Motion { accel, jerk: accel * f1, deviation: 0.05, turn: 0.6, lever: 0.0, rest: false }
    }
}

/// How high over the bed (mm) the plastic cell's arm waits and is laid out to hold its pose.
pub const FDM_WORK_HEIGHT: f64 = 60.0;

/// The cells' gravity.
const DOWN: V3 = [0.0, 0.0, -GRAVITY];

/// Arm `m`'s holding torque at `q` with its preloads pointed with gravity there, as a share of
/// each joint's continuous rating: the worst joint's.
fn worst_hold(m: &ArmModel, q: &Q) -> f64 {
    let mut m = m.clone();
    aim_preload(&mut m, q, DOWN);
    torque_use(&m, &frames(&m, q), DOWN, &Wrench::NONE).iter().fold(0.0, |a, u| a.max(*u))
}

/// Arm `arm` of cell `c` with its tool along `z` (part frame) `height` mm over the part frame's
/// origin, a positioner level (the pool flat): of the poses there, one clear of a singularity
/// (`W_MIN`) where any is, then one its joints hold within their continuous rating, then the
/// furthest from a singularity (of the clear ones past their rating, the nearest to it). With
/// whether it is clear and within its rating, the worst joint's share and its manipulability;
/// none when no pose reaches there.
fn pose_over(c: &CellSetup, arm: usize, height: f64, z: V3) -> Option<(Q, (bool, bool), f64, f64)> {
    let m = &c.arms[arm];
    let part = c.part_frame([0.0; 2]);
    let goal = IkGoal { p: part.apply([0.0, 0.0, height]), z: part.rotate(z), x: None };
    let mut best: Option<((bool, bool), f64, f64, Q)> = None;
    for q2 in [-0.6, 0.0, 0.6] {
        for q3 in [-1.2, -0.6, 0.0, 0.6, 1.2] {
            for q5 in [-1.2, 0.0, 1.2] {
                let seed = [0.0, q2, q3, 0.0, q5, 0.0];
                if let Ok(q) = ik(m, &goal, &seed, &IK) {
                    let w = manipulability(m, &q);
                    let hold = worst_hold(m, &q);
                    let class = (w >= W_MIN, hold <= 1.0);
                    let better = best.is_none_or(|b| match class {
                        _ if class != b.0 => class > b.0,
                        (true, false) => hold < b.2,
                        _ => w > b.1,
                    });
                    if better {
                        best = Some((class, w, hold, q));
                    }
                }
            }
        }
    }
    best.map(|b| (b.3, b.0, b.2, b.1))
}

/// Arm `arm`'s work pose in cell `c`: its tool down `height` mm over the part ([`pose_over`]),
/// with the worst joint's share of its rating there.
fn work_pose(c: &CellSetup, arm: usize, height: f64) -> Option<(Q, f64)> {
    pose_over(c, arm, height, [0.0, 0.0, -1.0]).map(|b| (b.0, b.2))
}

/// Arm `arm`'s work pose in cell `c` ([`work_pose`]). A cell whose arm cannot reach over its
/// part at all is no cell (every preset's is checked).
pub fn work_over(c: &CellSetup, arm: usize, height: f64) -> Q {
    work_pose(c, arm, height).map(|b| b.0).unwrap_or_else(|| panic!("{}: arm {arm} reaches no pose {height} mm over its part", c.label))
}

/// Where arm `arm` of cell `c` waits, `height` mm over the part: its work pose where that is
/// clear of a singularity and within its ratings, else the tool tipped toward or away from
/// the arm's base, 15° at a time to 45°, the least tip that is (a wrist over the part tool
/// down can be straight); where none is, the one furthest from a singularity, which the
/// planner lists in its report. A cell whose arm cannot reach over its part at all is no cell
/// (every preset's is checked).
pub fn park_over(c: &CellSetup, arm: usize, height: f64) -> Q {
    let part = c.part_frame([0.0; 2]);
    let toward = part.inv().rotate(sub(part.t, c.arms[arm].base.t));
    // Any level axis tips it when the base is right under the part.
    let level = [toward[0], toward[1], 0.0];
    let axis = if len(level) > 1e-6 { norm(cross([0.0, 0.0, 1.0], level)) } else { [1.0, 0.0, 0.0] };
    let mut best: Option<((bool, bool), f64, Q)> = None;
    for deg in [0.0f64, 15.0, -15.0, 30.0, -30.0, 45.0, -45.0] {
        let z = mv(&rot_axis(axis, deg.to_radians()), [0.0, 0.0, -1.0]);
        let Some((q, class, _, w)) = pose_over(c, arm, height, z) else { continue };
        if class == (true, true) {
            return q;
        }
        if best.as_ref().is_none_or(|b| class > b.0 || (class == b.0 && w > b.1)) {
            best = Some((class, w, q));
        }
    }
    best.map(|b| b.2).unwrap_or_else(|| panic!("{}: arm {arm} reaches no pose {height} mm over its part", c.label))
}

/// How far in toward the part a base may come to hold its work pose within its ratings, and
/// the step it comes in by (mm, along the floor).
const FIT_MIN: f64 = 120.0;
const FIT_STEP: f64 = 10.0;

/// Place each arm where it holds its work pose `height` mm over the part within every joint's
/// continuous rating: the base where the preset puts it, else brought in toward the part along
/// the floor, the first place that holds (an arm no place holds stays where it was, over its
/// rating, which its runs flag). Then point its preloads with gravity at that pose
/// ([`fab_robot::dynamics::aim_preload`]).
pub fn fit_to_rating(c: &mut CellSetup, height: f64) {
    let centre = c.part_frame([0.0; 2]).t;
    for i in 0..c.arms.len() {
        let home = c.arms[i].base.t;
        let away = [home[0] - centre[0], home[1] - centre[1], 0.0];
        let out = len(away);
        let mut r = out;
        let mut placed = false;
        while r >= FIT_MIN.min(out) {
            let k = r / out.max(1e-9);
            c.arms[i].base.t = [centre[0] + away[0] * k, centre[1] + away[1] * k, home[2]];
            if work_pose(c, i, height).is_some_and(|w| w.1 <= 1.0) {
                placed = true;
                break;
            }
            r -= FIT_STEP;
        }
        if !placed {
            c.arms[i].base.t = home;
        }
        if c.arms[i].joints.iter().any(|j| j.drive.preload != 0.0) {
            let q = work_over(c, i, height);
            aim_preload(&mut c.arms[i], &q, DOWN);
        }
    }
}

/// The acceleration (mm/s²) a machine whose lowest mode is `f1` Hz prints at: the ring a speed
/// change leaves stays near 0.05 mm (a jerk ramp one period long leaves about a quarter of the
/// a/ω² a step would, est.). The arms and the printer they are timed against share it.
pub fn print_accel(f1: f64) -> f64 {
    let w = std::f64::consts::TAU * f1;
    0.05 * w * w / 0.25
}

/// The printer job with the arm's extruder in its nozzle's place: nozzle, layers and lines, no
/// flow cap (the route's); no skirt, brim or prime tower (the arm prints the part alone).
pub fn arm_job(job: &Job, e: &Extruder) -> Result<Job, String> {
    let mut cfg = job.cfg.clone();
    let (n, h, w) = (e.nozzle, e.layer(), e.line());
    let num = |v: f64| format!("{v:.3}");
    let mut put = |k: &str, v: String| {
        let len = cfg.vec_len(k);
        if len > 1 {
            cfg.set_vec(k, vec![v; len]);
        } else {
            cfg.set_str(k, v);
        }
    };
    put("nozzle_diameter", num(n));
    put("layer_height", num(h));
    put("initial_layer_print_height", num(h));
    put("min_layer_height", num(0.2 * n));
    put("max_layer_height", num(0.8 * n));
    for k in ["line_width", "outer_wall_line_width", "inner_wall_line_width", "sparse_infill_line_width", "internal_solid_infill_line_width", "top_surface_line_width", "initial_layer_line_width", "support_line_width"] {
        put(k, num(w));
    }
    // The plan keeps each feature's own speed: the arm's route caps it by the extruder's melt
    // flow per segment (so the cap can be weighed by lifting it).
    put("filament_max_volumetric_speed", "0".into());
    put("enable_prime_tower", "0".into());
    put("skirt_loops", "0".into());
    put("brim_type", "no_brim".into());
    let arm = Job { cfg, fil_ext: job.fil_ext.clone() };
    makepad_fab_toolpath::check(&arm)?;
    Ok(arm)
}

/// The bed's centre in the printer's coordinates.
fn bed_centre(job: &Job) -> [f64; 2] {
    let pts = job.cfg.points("printable_area");
    if pts.is_empty() {
        return [100.0, 100.0];
    }
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in &pts {
        lo = [lo[0].min(p.0), lo[1].min(p.1)];
        hi = [hi[0].max(p.0), hi[1].max(p.1)];
    }
    [0.5 * (lo[0] + hi[0]), 0.5 * (lo[1] + hi[1])]
}

/// The part as Fab's slicer reads it: its field moved onto the bed's centre.
fn input(design: &Design, at: [f64; 2]) -> (ObjectInput, ([f64; 3], [f64; 3])) {
    let sdf = design.sdf.clone();
    let field = Arc::new(move |x: f64, y: f64, z: f64| sdf([x - at[0], y - at[1], z]));
    let bounds = ([design.lo[0] + at[0], design.lo[1] + at[1], design.lo[2]], [design.hi[0] + at[0], design.hi[1] + at[1], design.hi[2]]);
    let source = SdfSource::new(field, bounds);
    (ObjectInput { name: "part".into(), source: Box::new(source), filament: 0, overrides: Vec::new() }, bounds)
}

/// Fab's toolpaths as the arm's layers (part frame), in the order the printer prints them:
/// each island's walls, then its fills.
pub fn layers(tp: &Toolpaths, at: [f64; 2]) -> Vec<FdmLayer> {
    tp.layers
        .iter()
        .map(|l| {
            let mut paths = Vec::new();
            for ol in l.objects.iter().filter(|o| o.object == 0) {
                for island in &ol.islands {
                    for ep in island.walls.iter().chain(&island.fills) {
                        let pts = ep.pts.iter().enumerate().map(|(i, p)| [p.x_mm() - at[0], p.y_mm() - at[1], l.print_z + ep.z.get(i).copied().unwrap_or(0.0)]).collect::<Vec<V3>>();
                        let segs = pts.len().saturating_sub(1);
                        let flow = if ep.flow.is_empty() && ep.dry.is_empty() {
                            Vec::new()
                        } else {
                            (0..segs).map(|i| if ep.dry.get(i).copied().unwrap_or(false) { 0.0 } else { ep.flow.get(i).copied().unwrap_or(1.0) }).collect()
                        };
                        let width = ep.width;
                        // Fab's area is the bead's cross-section with its flow: the arm's bead
                        // the width that holds it at this height.
                        let height = ep.height;
                        let width = if ep.area > 0.0 { bead_width(ep.area, height).max(0.5 * width) } else { width };
                        paths.push(FdmPath { pts, width, height, speed: ep.speed, flow });
                    }
                }
            }
            FdmLayer { z: l.print_z, height: l.height, paths }
        })
        .collect()
}

/// The arm's print of `design`: Fab's plan of it for the arm's extruder, as layers and route
/// settings.
#[derive(Clone)]
pub struct FdmPlan {
    pub layers: Vec<FdmLayer>,
    pub opts: FdmOpts,
}

pub fn plan(setup: &JobSetup, design: &Design, e: &Extruder) -> Result<FdmPlan, String> {
    let job = makepad_fab_slice::prepare_job(setup)?;
    let arm = arm_job(&job, e)?;
    let at = bed_centre(&arm);
    let (inp, _) = input(design, at);
    let tp = generate(&[inp], &arm);
    let h = e.layer();
    let opts = FdmOpts {
        travel: arm.cfg.f64("travel_speed").max(50.0),
        speed_max: arm.cfg.f64("travel_speed").max(50.0),
        hop: 2.0 * h,
        hop_after: 5.0 * e.nozzle,
        approach: 5.0,
        flow_max: e.flow_max,
        min_layer_time: e.min_layer_time(h),
        min_speed: arm.cfg.f64("slow_down_min_speed").max(1.0),
    };
    Ok(FdmPlan { layers: layers(&tp, at), opts })
}

/// Fab's own estimate for `design` on the printer `setup` describes, as its slice reports it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrinterTime {
    /// Printing (s), and with the printer's start sequence.
    pub model: f64,
    pub total: f64,
    pub layers: usize,
    pub grams: f64,
}

pub fn printer_time(setup: &JobSetup, design: &Design) -> Result<PrinterTime, String> {
    let job = makepad_fab_slice::prepare_job(setup)?;
    let (inp, bounds) = input(design, bed_centre(&job));
    let r = makepad_fab_slice::slice_inputs(setup, vec![inp], vec![bounds])?;
    Ok(PrinterTime { model: r.stats.model_time, total: r.stats.total_time, layers: r.stats.layers, grams: r.stats.filament_g.iter().sum() })
}
