//! The live loop against the virtual cell, end to end: two desktop arms in a softer, heavier
//! cell than their models say (`Errors::TYPICAL`) build and trim a small ring. Each check runs
//! well under a second; the tests assert them and the Arm workspace lists them.

use crate::fdm::{plan, printer_time, work_over, FdmCell, Part};
use crate::printer::{h2d, H2D_LABEL};
use crate::lab::{ring, Cell, Job, Session, Setup};
use crate::realtime::RtPath;
use fab_arms::design::Design;
use fab_arms::planner::{LoopMode, Report};
use fab_arms::route::{DedTrim, DedTrimOpts, BEAM_BLOCKED, CUT_UNREACHED};
use fab_cell_sim::SimCell;
pub use fab_robot::checks::{Check, Named, PHYSICS};
use fab_robot::field::BuildField;
use fab_robot::arm::{ArmId, GRAVITY, Q};
use fab_robot::dynamics::{torque_use, Wrench};
use fab_robot::kin::frames;
use fab_robot::cell::CellSetup;
use fab_robot::io::CellLink;
use fab_robot::math::*;
use fab_robot::presets::ARCMIN;

/// The cell's end-to-end checks, in the order the workspace lists them.
pub const CELL: &[Named] = &[
    ("Live loop lays the bead on the track", live_deposit_holds_the_track),
    ("Rigid open loop misses the track", rigid_deposit_misses_the_track),
    ("Unpreloaded, the lost motion strays the wall", unpreloaded_lost_motion_strays),
    ("The desktop pair holds within its ratings", pair_holds_within_rating),
    ("Same seed, same bits", same_seed_same_bits),
    ("Armed, the loop thread meets no compile", armed_loop_meets_no_compile),
    ("Live loop trims the margin to the design", live_cut_trims_the_margin),
    ("Rigid open loop cuts beside the part", rigid_cut_misses_the_margin),
    ("Plastic: the plan's time is the cell's", fdm_plan_time_is_the_cells),
    ("Plastic: the printed tile holds its size", fdm_tile_holds_its_size),
    ("One 850: the live loop welds on the track", single_850_welds_on_the_track),
    ("One 850: the live loop trims what it reaches", single_850_trims_what_it_reaches),
];

/// Every check: the dynamics', then the cell's.
pub fn all() -> impl Iterator<Item = Named> {
    PHYSICS.iter().chain(CELL.iter()).copied()
}

/// Run `route` for `design` in `mode` until it is done or `max_time` passes, `laid` already
/// standing in the cell.
fn run(mode: LoopMode, seed: u64, design: &Design, route: DedTrim, laid: Option<&Design>, max_time: f64) -> (Report, SimCell) {
    let setup = Setup { max_time, ..Setup::new(mode, seed) };
    run_in(setup, &setup.cell.setup(), design, route, laid)
}

/// As [`run`], in `cell`.
fn run_in(setup: Setup, cell: &CellSetup, design: &Design, route: DedTrim, laid: Option<&Design>) -> (Report, SimCell) {
    let mut s = Session::in_cell(setup, cell, design, Box::new(route), laid);
    let report = s.run().clone();
    (report, s.sim)
}

/// The wall's outer face at bearing `a` (rad) and height `z`: out from the axis to the
/// material, then through it.
fn outer(f: &BuildField, a: f64, z: f64) -> Option<f64> {
    let at = |r: f64| f.distance([r * a.cos(), r * a.sin(), z]);
    let mut r = 2.0;
    while r < 14.0 && at(r) > 0.0 {
        r += 0.02;
    }
    while r < 14.0 && at(r) <= 0.0 {
        r += 0.02;
    }
    (r < 14.0).then_some(r)
}

/// How far the true beads' middles lie off the track (radius `track`): (median, p95), none
/// if fewer than 20 were laid.
fn bead_error(sim: &SimCell, track: f64) -> Option<(f64, f64)> {
    let mut e: Vec<f64> = sim
        .log()
        .beads
        .iter()
        .map(|(_, b)| {
            let m = lerp(b.a, b.b, 0.5);
            ((m[0] * m[0] + m[1] * m[1]).sqrt() - track).abs()
        })
        .collect();
    if e.len() <= 20 {
        return None;
    }
    e.sort_by(f64::total_cmp);
    Some((e[e.len() / 2], e[e.len() * 95 / 100]))
}

/// One layer, no margin: the bead goes down the middle of the wall (radius 6).
fn one_layer() -> (Design, DedTrim) {
    let design = ring(1.0);
    let route = DedTrim::new(&design, DedTrimOpts { trim: false, ..DedTrimOpts::default() });
    (design, route)
}

/// Live, the tracker's loop lays one layer's bead on its track (p95 under 0.1 mm) and the wall
/// stands within 0.25 mm of the design all round.
pub fn live_deposit_holds_the_track() -> Check {
    let (design, route) = one_layer();
    let (report, sim) = run(LoopMode::Live, 11, &design, route, None, 20.0);
    let Some((median, p95)) = bead_error(&sim, 6.0) else {
        return Check { pass: false, figures: format!("{} beads laid in {:.1} s", sim.log().beads.len(), report.time) };
    };
    let (walls, worst) = wall_error(&sim);
    Check {
        pass: report.done && report.faults.is_empty() && p95 < 0.1 && walls == 24 && worst < 0.25,
        figures: format!("beads off the track median {median:.3}, p95 {p95:.3} mm; outer face worst {worst:.3} mm ({walls} of 24 bearings); {:.1} s{}", report.time, faults(&report)),
    }
}

/// The one-layer ring's outer face at 24 bearings: how many it stands at, and the worst off
/// the design (radius 7.1).
fn wall_error(sim: &SimCell) -> (usize, f64) {
    let walls: Vec<f64> = (0..24).filter_map(|k| outer(sim.truth_field(), k as f64 * std::f64::consts::TAU / 24.0, 0.5)).collect();
    (walls.len(), walls.iter().fold(0.0f64, |m, r| m.max((r - 7.1).abs())))
}

/// The desktop arms' 8 arcmin of lost motion is real: without the preload that takes it up,
/// the same live loop lays the wall more than 0.25 mm off the design where the loads reverse.
pub fn unpreloaded_lost_motion_strays() -> Check {
    let (design, route) = one_layer();
    let setup = Setup { max_time: 20.0, ..Setup::new(LoopMode::Live, 11) };
    let mut cell = setup.cell.setup();
    let lost = cell.arms[0].joints[1].drive.lost_motion / ARCMIN;
    for j in cell.arms.iter_mut().flat_map(|a| a.joints.iter_mut()) {
        j.drive.preload = 0.0;
        j.drive.preload_friction = 0.0;
    }
    let (report, sim) = run_in(setup, &cell, &design, route, None);
    let (walls, worst) = wall_error(&sim);
    let track = bead_error(&sim, 6.0).map_or("too few beads".to_string(), |(_, p95)| format!("beads off the track p95 {p95:.3} mm"));
    Check {
        pass: lost > 0.0 && walls > 0 && worst > 0.25,
        figures: format!("{lost:.0} arcmin, no preload: outer face worst {worst:.3} mm ({walls} of 24 bearings), {track}; {:.1} s", report.time),
    }
}

/// A rigid slicer's plan on the same soft arm lays the bead more than 1 mm off its track.
pub fn rigid_deposit_misses_the_track() -> Check {
    let (design, route) = one_layer();
    let (report, sim) = run(LoopMode::Rigid, 11, &design, route, None, 20.0);
    let Some((median, p95)) = bead_error(&sim, 6.0) else {
        return Check { pass: false, figures: format!("{} beads laid in {:.1} s", sim.log().beads.len(), report.time) };
    };
    Check { pass: median > 1.0, figures: format!("beads off the track median {median:.3}, p95 {p95:.3} mm; {:.1} s", report.time) }
}

/// The same seed builds the same bits; another seed, another cell.
pub fn same_seed_same_bits() -> Check {
    let go = |seed: u64| {
        let (design, route) = one_layer();
        let (report, sim) = run(LoopMode::Live, seed, &design, route, None, 3.0);
        (sim.log().beads.clone(), sim.true_q(ArmId::W), report.time)
    };
    let a = go(4);
    let same = a == go(4);
    let other = a.1 != go(5).1;
    Check {
        pass: !a.0.is_empty() && same && other,
        figures: format!("{} beads in 3 s; seed 4 twice {}; seed 5 {}", a.0.len(), if same { "identical" } else { "DIFFERS" }, if other { "differs" } else { "IDENTICAL" }),
    }
}

/// The worst of `uses` (per arm, each joint's share of its rating): (arm, joint, share).
fn worst_use(uses: &[Q]) -> (usize, usize, f64) {
    let mut w = (0, 0, f64::MIN);
    for (i, u) in uses.iter().enumerate() {
        for (j, u) in u.iter().enumerate() {
            if *u > w.2 {
                w = (i, j, *u);
            }
        }
    }
    w
}

/// The desktop pair holds its work within every joint's continuous rating, preloads on: each
/// arm's holding torque at its work pose over the stub and at its park, and the RMS torque of
/// the build-and-trim's plan (its dry run), each at most the rating.
pub fn pair_holds_within_rating() -> Check {
    let cell = Cell::DesktopPair;
    let c = cell.setup();
    let gravity = [0.0, 0.0, -GRAVITY];
    let hold = |q: &dyn Fn(usize) -> Q| -> Vec<Q> { (0..c.arms.len()).map(|i| torque_use(&c.arms[i], &frames(&c.arms[i], &q(i)), gravity, &Wrench::NONE)).collect() };
    let work = worst_use(&hold(&|i| work_over(&c, i, cell.work_height())));
    let park = worst_use(&hold(&|_| cell.park()));
    let mut s = Session::job(Setup::new(LoopMode::Live, 11), Job::BuildAndTrim);
    s.dry_run();
    let r = &s.planner.report;
    let rms = worst_use(&r.rms_use);
    let at = |w: (usize, usize, f64)| format!("J{} arm {} {:.2}", w.1 + 1, w.0, w.2);
    let out: Vec<String> = c.arms.iter().map(|a| format!("{:.0}", len([a.base.t[0], a.base.t[1], 0.0]))).collect();
    Check {
        pass: r.rms_use.len() == 2 && r.done && work.2 <= 1.0 && park.2 <= 1.0 && rms.2 <= 1.0,
        figures: format!("of its rating, the worst: work pose {}, park {}, the build and trim's RMS {}; bases {} mm out", at(work), at(park), at(rms), out.join(" / ")),
    }
}

/// Armed, the cell's loop thread (real-time for good) runs 3 s of the live ring and meets no
/// lazy compile: all it reaches was compiled ahead, and Relax found no hole in that on the way
/// (realtime::holes). A native build compiles nothing lazily.
pub fn armed_loop_meets_no_compile() -> Check {
    let (design, route) = one_layer();
    let setup = Setup { max_time: 3.0, ..Setup::new(LoopMode::Live, 4) };
    let rt = RtPath::mark(None);
    let session = Box::new(Session::new(setup, &design, Box::new(route), None));
    // The compile runs in the background; the session's trait objects are new code for it.
    while !rt.compiled() {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let (Some(armed), Some(cell_loop)) = (rt.arm(), rt.loop_thread()) else {
        return Check { pass: false, figures: format!("compiled, yet not armed: {}", rt.line()) };
    };
    let run = cell_loop.run(session, 3.0);
    drop(armed);
    match run {
        Err(e) => Check { pass: false, figures: e },
        Ok((s, _, met)) => {
            let beads = s.sim.log().beads.len();
            let holes = crate::realtime::holes();
            let build = if cfg!(relax) { "" } else { " (a native build)" };
            Check { pass: beads > 0 && met == 0 && holes == 0, figures: format!("{beads} beads in 3 s on the loop thread, armed; {met} lazy compiles met, {holes} holes in the real-time set{build}") }
        }
    }
}

/// The 2 mm ring laid with the margin, then the cut: the outer faces at three heights and 36
/// bearings, as cut.
fn trim(mode: LoopMode) -> (Vec<f64>, Report) {
    let job = Job::Trim;
    let design = job.design();
    let (report, sim) = run(mode, 11, &design, job.route(&design), job.laid().as_ref(), 14.0);
    let mut faces = Vec::new();
    for z in [0.8, 1.2, 1.6] {
        for k in 0..36 {
            faces.extend(outer(sim.truth_field(), k as f64 * std::f64::consts::TAU / 36.0, z));
        }
    }
    (faces, report)
}

/// Live, the loop trims the faces it reaches in 14 s (a quarter at least) to the design (80 %
/// of them within 0.15 mm) and gouges none (nothing under 6.85 mm).
pub fn live_cut_trims_the_margin() -> Check {
    let (faces, report) = trim(LoopMode::Live);
    let cut: Vec<f64> = faces.iter().copied().filter(|r| *r < 7.35).collect();
    let on = cut.iter().filter(|r| (*r - 7.1).abs() < 0.15).count();
    let deepest = faces.iter().fold(f64::MAX, |m, r| m.min(*r));
    Check {
        pass: report.faults.is_empty() && cut.len() >= faces.len() / 4 && on * 10 >= cut.len() * 8 && deepest > 6.85,
        figures: format!("{} of {} faces cut, {on} of them within 0.15 mm; deepest r {deepest:.3} (design 7.1); {} tasks, {:.1} s{}", cut.len(), faces.len(), report.tasks, report.time, faults(&report)),
    }
}

/// A rigid plan cuts beside the part: under a tenth of the faces end on the design.
pub fn rigid_cut_misses_the_margin() -> Check {
    let (faces, report) = trim(LoopMode::Rigid);
    let on = faces.iter().filter(|r| (*r - 7.1).abs() < 0.2).count();
    Check { pass: on < faces.len() / 10, figures: format!("{on} of {} faces on the design; {} tasks, {:.1} s", faces.len(), report.tasks, report.time) }
}

/// The plastic tile printed live by the IRB 1100 with its 1 mm nozzle, in a mis-modelled cell:
/// the session after the run, and the planner's dry-run time for the same plan.
fn fdm_tile() -> Result<(Session, f64), String> {
    let cell = FdmCell::default();
    let setup = Setup { cell: Cell::Fdm(cell), max_time: 120.0, ..Setup::new(LoopMode::Live, 3) };
    let part = Part::Tile;
    let make = || plan(&h2d(), &part.design(), &cell.extruder()).map(|p| Session::fdm(setup, part, p));
    let planned = make()?.dry_run();
    let mut s = make()?;
    s.run();
    Ok((s, planned))
}

/// The time the planner plans for a print (its dry run: the arm's own acceleration, jerk,
/// corners, setpoint ticks, travels and layer changes) is the time the cell takes to the
/// setpoint tick; set against Fab's own estimate for the H2D.
pub fn fdm_plan_time_is_the_cells() -> Check {
    let (s, planned) = match fdm_tile() {
        Ok(r) => r,
        Err(e) => return Check { pass: false, figures: e },
    };
    let r = &s.planner.report;
    let tick = 1.0 / s.sim.info().arms[0].controller.setpoint_hz;
    let gap = (r.time - planned).abs();
    let printer = printer_time(&h2d(), &Part::Tile.design());
    let versus = match &printer {
        Ok(p) => format!("; Fab's estimate on the {H2D_LABEL} {:.1} s, the arm {:.1}×", p.model, r.time / p.model.max(1e-9)),
        Err(e) => format!("; no printer estimate: {e}"),
    };
    Check {
        pass: r.done && r.faults.is_empty() && gap <= 2.0 * tick * r.tasks.max(1) as f64 && printer.is_ok(),
        figures: format!("cell {:.3} s, planned {planned:.3} s ({} layers, {:.1} s printing){versus}{}", r.time, r.tasks, r.deposit_time, faults(r)),
    }
}

/// Live, the soft arm prints the tile to size: the footprint within 0.5 mm of 8 mm both ways,
/// the top over its middle 4 mm at 1.2 within 0.1 mm (the median) with at most 1 in 20 points
/// short of a whole layer (a 1.15 mm line's fill leaves corners of so small a part open),
/// every strand bonded, the plastic laid within 10 % of the solid (its flow is 3 % short).
pub fn fdm_tile_holds_its_size() -> Check {
    let (s, _) = match fdm_tile() {
        Ok(r) => r,
        Err(e) => return Check { pass: false, figures: e },
    };
    let f = s.sim.truth_field();
    let edge = |dir: V3, z: f64| {
        let mut r = 8.0;
        while r > 0.0 && f.distance(add(scale(dir, r), [0.0, 0.0, z])) > 0.0 {
            r -= 0.02;
        }
        r
    };
    let mut worst = 0.0f64;
    for z in [0.3, 0.9] {
        let x = edge([1.0, 0.0, 0.0], z) + edge([-1.0, 0.0, 0.0], z);
        let y = edge([0.0, 1.0, 0.0], z) + edge([0.0, -1.0, 0.0], z);
        worst = worst.max((x - 8.0).abs()).max((y - 8.0).abs());
    }
    let tops: Vec<f64> = (0..81).map(|k| [-2.0 + 0.5 * (k % 9) as f64, -2.0 + 0.5 * (k / 9) as f64]).map(|p| f.top_below([p[0], p[1], 4.0], 8.0).unwrap_or(0.0)).collect();
    let mut sorted = tops.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let short = tops.iter().filter(|t| **t < 0.6).count();
    let log = s.sim.log();
    let volume = 8.0 * 8.0 * 1.2;
    let r = &s.planner.report;
    Check {
        pass: r.done && worst < 0.5 && (median - 1.2).abs() < 0.1 && short * 20 <= tops.len() && log.lack_of_fusion == 0 && (log.deposited / volume - 1.0).abs() < 0.1,
        figures: format!("footprint worst {worst:.2} mm off 8; top median {median:.3} mm (1.2), {short} of {} points short of a layer; {:.1} mm³ laid ({volume:.1} solid); {} strands unbonded, {} pieces of ooze; {:.1} s", tops.len(), log.deposited, log.lack_of_fusion, log.strings, r.time),
    }
}

fn faults(r: &Report) -> String {
    if r.faults.is_empty() {
        String::new()
    } else {
        format!("; faults: {}", r.faults.join(", "))
    }
}

/// The one 850 over its positioner, live: one layer of the ring's bead on its track (p95 under
/// 0.25 mm) and the wall within 0.3 mm of the design all round (the note's §5b expects ±0.3-0.5
/// mm (2σ) welding on the fly), within its ratings. The part takes 20 % more heat than the
/// thermal model says: held to the plate's thermocouple, the planner's warp of the wall's top
/// stays within a quarter of the true warp off it, the plate within 3 K.
pub fn single_850_welds_on_the_track() -> Check {
    let (design, route) = one_layer();
    let setup = Setup { cell: Cell::Single850, max_time: 20.0, ..Setup::new(LoopMode::Live, 11) };
    let mut s = Session::new(setup, &design, Box::new(route), None);
    let report = s.run().clone();
    let sim = &s.sim;
    let Some((median, p95)) = bead_error(sim, 6.0) else {
        return Check { pass: false, figures: format!("{} beads laid in {:.1} s", sim.log().beads.len(), report.time) };
    };
    let (walls, worst) = wall_error(sim);
    let rating = report.over_rating_line().unwrap_or_else(|| "within its ratings".into());
    // The wall's top at its outer face, as the heat truly warps it and as the planner holds it.
    let (truth, held, th) = (sim.heat(), s.planner.heat(), sim.thermal());
    let top = [7.1, 0.0, 1.0];
    let (warp, held_warp) = (dist(th.warp(&truth, top), top), dist(th.warp(&held, top), top));
    let off = dist(th.warp(&truth, top), th.warp(&held, top));
    Check {
        pass: report.done && report.faults.is_empty() && report.over_rating().is_empty() && p95 < 0.25 && walls == 24 && worst < 0.3 && warp > 0.0 && off < 0.25 * warp && (truth.bulk - held.bulk).abs() < 3.0,
        figures: format!(
            "beads off the track median {median:.3}, p95 {p95:.3} mm; outer face worst {worst:.3} mm ({walls} of 24 bearings); {rating}; the wall's top warped {:.1} µm, held at {:.1} ({:.1} off), the plate {:.1} K over ambient, held at {:.1}; {:.1} s{}",
            warp * 1e3,
            held_warp * 1e3,
            off * 1e3,
            truth.bulk,
            held.bulk,
            report.time,
            faults(&report)
        ),
    }
}

/// The one 850, live, cutting the laid ring's whole margin, the positioner turning the part to
/// face it (between arcs, or under them as they are cut): every face cut (nine in ten at least)
/// on the design (80 % within 0.15 mm), none gouged (nothing under 6.85 mm), nothing left out of
/// reach, the table turned.
pub fn single_850_trims_what_it_reaches() -> Check {
    let job = Job::Trim;
    let design = job.design();
    let setup = Setup { cell: Cell::Single850, max_time: 120.0, ..Setup::new(LoopMode::Live, 11) };
    let (report, sim) = run_in(setup, &setup.cell.setup(), &design, job.route(&design), job.laid().as_ref());
    let mut faces = Vec::new();
    for z in [0.8, 1.2, 1.6] {
        for k in 0..36 {
            faces.extend(outer(sim.truth_field(), k as f64 * std::f64::consts::TAU / 36.0, z));
        }
    }
    let cut: Vec<f64> = faces.iter().copied().filter(|r| *r < 7.35).collect();
    let on = cut.iter().filter(|r| (*r - 7.1).abs() < 0.15).count();
    let deepest = faces.iter().fold(f64::MAX, |m, r| m.min(*r));
    let unreached = report.flags.iter().filter(|f| f.1 == CUT_UNREACHED || f.1 == BEAM_BLOCKED).count();
    let skipped = report.skipped.len();
    Check {
        pass: report.done && report.faults.is_empty() && skipped == 0 && unreached == 0 && report.table_moves + report.coordinated > 0 && cut.len() * 10 >= faces.len() * 9 && on * 10 >= cut.len() * 8 && deepest > 6.85,
        figures: format!(
            "{} of {} faces cut, {on} of them within 0.15 mm; deepest r {deepest:.3} (design 7.1); {unreached} cut points out of its reach, {skipped} tasks skipped; the table {} moves, {:.1} s, turning under {} cuts; {} tasks, {:.1} s{}",
            cut.len(),
            faces.len(),
            report.table_moves,
            report.table_time,
            report.coordinated,
            report.tasks,
            report.time,
            faults(&report)
        ),
    }
}
