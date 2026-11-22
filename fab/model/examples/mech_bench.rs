//! Headless Mechanics comparison of the two engines (Box3D's MechWorld and the dynamics engine's
//! DynWorld) on five source demos: joint drift (max and p95), hinge axes and limits, lost weave
//! interlocks, kinetic energy at rest, final linear/angular speed, and wall time of each step.
//! Run through devhub on the comparison's fixed Linux node with CARGO_INCREMENTAL=0:
//! `cargo run --release -p fab-model --example mech_bench [box3d|dynamics|both] [weave substeps]` (both when not
//! named; the weave assembly's own substeps when not given).
//! Writes the same results to `$DEVHUB_KEEP/mech_bench.tsv` when that directory is set.
//!
//! Each scenario starts in a fresh world. Pull starts at t=0 (the printed rest configuration),
//! moves for 1 s, holds for 2 s, then releases without an extra step. Weave drag waits 1 s,
//! selects the current link centre farthest from its nearest anchor, moves along frame.x at
//! 1000 mm/s for 0.5 s, then holds for 1 s; drag300 is the same at 300 mm/s (a normal hand speed
//! in Mechanics) for 1 s, then holds for 1 s. Ties select the last body in assembly order. Hang runs 5 s,
//! drop 3 s, jointed rest 5 s. No display/plate hold transform is applied to the sim frame.
//! Metrics include the initial state and every completed step; interlocks every 24 steps and
//! at the end. Timing covers only step(), including sleeping steps, with no warmup excluded.
//! Median averages the middle two samples; p95 uses nearest rank. Inapplicable metrics are 0.
//! Kinetic energy at rest is the largest over the last second of hang and rest, from the poses of
//! consecutive steps (the same for both engines), in nJ (1 nJ = 1 g mm^2/s^2).

use fab_model::{eval::Evaluator, FabModel};
use fab_plate::machine::Machine;
use makepad_csg_mech::{
    geom::{add, dot, len, madd, norm, scale, sub, P3},
    runner::{HAND_HZ, HZ},
    dyn_world::DynWorld,
    sim::{solver_workers, AnchorKind, Assembly, JointKind, MechWorld, Pose, GRAVITY},
    weave::{self, Weave, WeaveProfile, WeaveScene},
    Profile,
};
use std::{fs::File, io::Write, path::PathBuf, time::Instant};

/// What the bench needs of an engine's world.
trait Sim: Sized {
    fn build(asm: &Assembly, gravity: P3, floor: Option<f64>) -> Result<Self, String>;
    fn rest_com(&self, i: usize) -> P3;
    fn poses(&self) -> Vec<Pose>;
    fn angles(&mut self, asm: &Assembly) -> Vec<f64>;
    fn grab(&mut self, body: usize, point: P3, hz: f64);
    fn drag(&mut self, point: P3, dt: f64);
    fn release(&mut self);
    fn step(&mut self, dt: f64);
    fn max_speed(&self) -> f64;
    fn max_angular_speed(&self) -> f64;
}

macro_rules! sim {
    ($world:ty, $build:expr) => {
        impl Sim for $world {
            fn build(asm: &Assembly, gravity: P3, floor: Option<f64>) -> Result<Self, String> { $build(asm, gravity, floor) }
            fn rest_com(&self, i: usize) -> P3 { <$world>::rest_com(self, i) }
            fn poses(&self) -> Vec<Pose> { <$world>::poses(self) }
            fn angles(&mut self, asm: &Assembly) -> Vec<f64> { <$world>::angles(self, asm) }
            fn grab(&mut self, body: usize, point: P3, hz: f64) { <$world>::grab(self, body, point, hz) }
            fn drag(&mut self, point: P3, dt: f64) { <$world>::drag(self, point, dt) }
            fn release(&mut self) { <$world>::release(self) }
            fn step(&mut self, dt: f64) { <$world>::step(self, dt) }
            fn max_speed(&self) -> f64 { <$world>::max_speed(self) }
            fn max_angular_speed(&self) -> f64 { <$world>::max_angular_speed(self) }
        }
    };
}
sim!(MechWorld, |asm, gravity, floor| Ok(MechWorld::new(asm, gravity, floor)));
sim!(DynWorld, DynWorld::new);

/// Kinetic energy (g mm^2/s^2) of the bodies between two consecutive poses `dt` apart: each
/// body's mass at its centre's speed plus its rest inertia, turned with it, at its turn rate.
fn kinetic(asm: &Assembly, before: &[Pose], after: &[Pose], dt: f64) -> f64 {
    let mut energy = 0.0;
    for (i, b) in asm.bodies.iter().enumerate() {
        let v = scale(sub(after[i].com, before[i].com), 1.0 / dt);
        energy += 0.5 * b.mass.mass * dot(v, v);
        // the turn from before to after (quaternions x, y, z, w), as a rotation vector
        let (p, q) = (before[i].rot, after[i].rot);
        let r = [
            q[3] * -p[0] + q[0] * p[3] + q[1] * -p[2] - q[2] * -p[1],
            q[3] * -p[1] - q[0] * -p[2] + q[1] * p[3] + q[2] * -p[0],
            q[3] * -p[2] + q[0] * -p[1] - q[1] * -p[0] + q[2] * p[3],
            q[3] * p[3] - q[0] * -p[0] - q[1] * -p[1] - q[2] * -p[2],
        ];
        let s = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
        if s < 1e-15 {
            continue;
        }
        let angle = 2.0 * s.atan2(r[3].abs());
        let sign = if r[3] < 0.0 { -1.0 } else { 1.0 };
        let w = scale([r[0], r[1], r[2]], sign * angle / (s * dt));
        // the inertia (model frame at rest) turned to now: I_now = R I R^T, so w.I_now.w = (R^T w).I.(R^T w)
        let local = after[i].unrotate(w);
        let inertia = &b.mass.inertia;
        let iw = [
            inertia[0][0] * local[0] + inertia[0][1] * local[1] + inertia[0][2] * local[2],
            inertia[1][0] * local[0] + inertia[1][1] * local[1] + inertia[1][2] * local[2],
            inertia[2][0] * local[0] + inertia[2][1] * local[1] + inertia[2][2] * local[2],
        ];
        energy += 0.5 * dot(local, iw);
    }
    energy
}

/// Engine-independent geometric measurements; rest centres are supplied alongside the poses.
/// The end separation is the largest separation among joints in this snapshot.
fn joint_metrics(asm: &Assembly, rest: &[P3], poses: &[Pose], angles: &[f64]) -> (f64, f64, f64) {
    let (mut separation, mut axis_error, mut overshoot) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (i, j) in asm.joints.iter().enumerate() {
        if j.kind.holds() {
            continue;
        }
        let a = poses[j.a].apply(rest[j.a], j.pivot);
        let b = poses[j.b].apply(rest[j.b], j.pivot);
        separation = separation.max(len(sub(a, b)));
        if let JointKind::Revolute { lower, upper } = j.kind {
            let tip = add(j.pivot, j.axis);
            let axis_a = norm(sub(poses[j.a].apply(rest[j.a], tip), a));
            let axis_b = norm(sub(poses[j.b].apply(rest[j.b], tip), b));
            axis_error = axis_error.max(dot(axis_a, axis_b).clamp(-1.0, 1.0).acos().to_degrees());
            if !j.kind.turns_freely() {
                overshoot = overshoot.max((lower - angles[i]).max(angles[i] - upper).max(0.0).to_degrees());
            }
        }
    }
    (separation, axis_error, overshoot)
}

/// Pose each closed centreline exactly as the hanging-mail test does, independent of the solver.
fn unlinked(woven: &Weave, rest: &[P3], poses: &[Pose]) -> usize {
    let posed: Vec<Vec<P3>> = woven.links.iter().enumerate()
        .map(|(i, link)| link.pts.iter().map(|p| poses[i].apply(rest[i], *p)).collect()).collect();
    woven.pairs.iter().filter(|(a, b)| weave::linking_number(&posed[*a as usize], &posed[*b as usize]) == 0).count()
}

#[derive(Default)]
struct Metrics {
    sep_max: f64,
    sep_end: f64,
    axis_max: f64,
    over_max: f64,
    unlinked_max: usize,
    unlinked_end: usize,
    /// Every joint-separation sample, for the p95.
    seps: Vec<f64>,
    ke_rest: f64,
    speed_end: f64,
    angular_end: f64,
    step_ms: Vec<f64>,
}

impl Metrics {
    fn joints(&mut self, asm: &Assembly, rest: &[P3], poses: &[Pose], angles: &[f64]) {
        let (sep, axis, over) = joint_metrics(asm, rest, poses, angles);
        self.sep_end = sep;
        self.sep_max = self.sep_max.max(sep);
        self.seps.push(sep);
        self.axis_max = self.axis_max.max(axis);
        self.over_max = self.over_max.max(over);
    }

    fn links(&mut self, woven: &Weave, rest: &[P3], poses: &[Pose]) {
        self.unlinked_end = unlinked(woven, rest, poses);
        self.unlinked_max = self.unlinked_max.max(self.unlinked_end);
    }

    fn step<W: Sim>(&mut self, world: &mut W) {
        let start = Instant::now();
        world.step(1.0 / HZ);
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        self.step_ms.push(ms);
    }

    fn finish<W: Sim>(&mut self, world: &W) {
        self.speed_end = world.max_speed();
        self.angular_end = world.max_angular_speed();
    }

    fn report(&mut self, engine: &str, demo: &str, scenario: &str, asm: &Assembly, tsv: &mut Option<File>) -> std::io::Result<()> {
        self.step_ms.sort_by(f64::total_cmp);
        self.seps.sort_by(f64::total_cmp);
        let sep_p95 = if self.seps.is_empty() { 0.0 } else { self.seps[(95 * self.seps.len()).div_ceil(100) - 1] };
        let n = self.step_ms.len();
        let med = (self.step_ms[(n - 1) / 2] + self.step_ms[n / 2]) * 0.5;
        let p95 = self.step_ms[(95 * n).div_ceil(100) - 1];
        let max = self.step_ms[n - 1];
        let (bodies, joints) = (asm.bodies.len(), asm.joints.len());
        println!("{engine} {demo} {scenario} {bodies} {joints} | sep {:.6} {:.6} p95 {sep_p95:.6} mm | ke rest {:.6} nJ | axis {:.6} deg | over {:.6} deg | unlinked {} {} | speed {:.6} mm/s ang {:.6} rad/s | step ms {med:.6} {p95:.6} {max:.6}",
            self.sep_max, self.sep_end, self.ke_rest, self.axis_max, self.over_max, self.unlinked_max, self.unlinked_end, self.speed_end, self.angular_end);
        if let Some(file) = tsv {
            writeln!(file, "{engine}\t{demo}\t{scenario}\t{bodies}\t{joints}\t{:.6}\t{:.6}\t{sep_p95:.6}\t{:.6}\t{:.6}\t{:.6}\t{}\t{}\t{:.6}\t{:.6}\t{med:.6}\t{p95:.6}\t{max:.6}",
                self.sep_max, self.sep_end, self.ke_rest, self.axis_max, self.over_max, self.unlinked_max, self.unlinked_end, self.speed_end, self.angular_end)?;
            file.flush()?;
        }
        Ok(())
    }
}

/// The simulation-frame gravity and floor from Mechanics::jointed. Its hold matrix only
/// transforms the displayed scene; hung assemblies still simulate in their model frame.
fn jointed_environment(asm: &Assembly) -> (P3, Option<f64>) {
    let up = norm(asm.up);
    let on_bed = asm.anchors.first().is_none_or(|a| a.kind == AnchorKind::Fixed);
    if on_bed {
        let low = asm.bodies.iter().flat_map(|b| b.hulls.iter().flatten())
            .map(|p| -dot(*p, up)).fold(f64::NEG_INFINITY, f64::max);
        (scale(up, -GRAVITY), Some(-(low + 0.05)))
    } else {
        (scale(asm.hang_gravity(), GRAVITY), None)
    }
}

fn jointed<W: Sim>(asm: &Assembly, pull: bool) -> Result<Metrics, String> {
    let (gravity, floor) = jointed_environment(asm);
    let mut world = W::build(asm, gravity, floor)?;
    let rest: Vec<P3> = (0..asm.bodies.len()).map(|i| world.rest_com(i)).collect();
    let hand = if pull {
        let centroid = asm.centroid();
        let body = (0..asm.bodies.len()).max_by(|a, b| {
            len(sub(rest[*a], centroid)).total_cmp(&len(sub(rest[*b], centroid)))
        }).ok_or("empty jointed assembly")?;
        let point = asm.bodies[body].hulls.iter().flatten().copied()
            .max_by(|a, b| len(sub(*a, centroid)).total_cmp(&len(sub(*b, centroid))))
            .ok_or("pull body has no hull points")?;
        world.grab(body, point, HAND_HZ);
        Some((point, norm(sub(point, centroid))))
    } else {
        None
    };
    let steps = ((if pull { 3.0 } else { 5.0 }) * HZ) as usize;
    let mut metrics = Metrics { step_ms: Vec::with_capacity(steps), ..Default::default() };
    let mut before = world.poses();
    let angles = world.angles(asm);
    metrics.joints(asm, &rest, &before, &angles);
    for step in 1..=steps {
        if let Some((point, direction)) = hand {
            world.drag(madd(point, direction, 300.0 * (step as f64 / HZ).min(1.0)), 1.0 / HZ);
        }
        metrics.step(&mut world);
        let poses = world.poses();
        let angles = world.angles(asm);
        metrics.joints(asm, &rest, &poses, &angles);
        if !pull && step as f64 > steps as f64 - HZ {
            metrics.ke_rest = metrics.ke_rest.max(kinetic(asm, &before, &poses, 1.0 / HZ));
        }
        before = poses;
    }
    if pull {
        world.release();
    }
    metrics.finish(&world);
    Ok(metrics)
}

fn woven_scenario<W: Sim>(woven: &Weave, scenario: &str, substeps: Option<u32>) -> Result<(Assembly, Metrics), String> {
    let which = if scenario == "drop" { WeaveScene::Drop } else { WeaveScene::Hang };
    let mut setup = weave::scene(woven, fab_model::weave::DENSITY, which);
    setup.asm.workers = solver_workers(setup.asm.bodies.len());
    if let Some(n) = substeps {
        setup.asm.substeps = n;
    }
    let asm = setup.asm;
    let mut world = W::build(&asm, setup.gravity, setup.floor)?;
    let rest: Vec<P3> = (0..asm.bodies.len()).map(|i| world.rest_com(i)).collect();
    // the hand's speed (mm/s) and how long it moves, for the drags
    let (speed, moves) = if scenario == "drag300" { (300.0, 1.0) } else { (1000.0, 0.5) };
    let seconds = match scenario { "hang" => 5.0, "drag" | "drag300" => 1.0 + moves + 1.0, _ => 3.0 };
    let steps = (seconds * HZ) as usize;
    let mut metrics = Metrics { step_ms: Vec::with_capacity(steps), ..Default::default() };
    let mut before = world.poses();
    metrics.links(woven, &rest, &before);
    let mut hand = None;
    for step in 0..steps {
        if scenario.starts_with("drag") && step == HZ as usize {
            if asm.anchors.is_empty() {
                return Err("hanging weave has no anchors".into());
            }
            let poses = world.poses();
            let centres: Vec<P3> = woven.links.iter().enumerate()
                .map(|(i, link)| poses[i].apply(rest[i], link.center())).collect();
            let distance = |i: usize| asm.anchors.iter()
                .map(|a| len(sub(centres[i], a.pivot))).fold(f64::INFINITY, f64::min);
            let body = (0..centres.len()).max_by(|a, b| distance(*a).total_cmp(&distance(*b)))
                .ok_or("empty weave")?;
            world.grab(body, centres[body], HAND_HZ);
            hand = Some(centres[body]);
        }
        if let Some(point) = hand {
            let elapsed = ((step + 1) as f64 / HZ - 1.0).min(moves);
            world.drag(madd(point, woven.frame.x, speed * elapsed), 1.0 / HZ);
        }
        metrics.step(&mut world);
        let poses = world.poses();
        if (step + 1) % 24 == 0 || step + 1 == steps {
            metrics.links(woven, &rest, &poses);
        }
        if scenario == "hang" && (step + 1) as f64 > steps as f64 - HZ {
            metrics.ke_rest = metrics.ke_rest.max(kinetic(&asm, &before, &poses, 1.0 / HZ));
        }
        before = poses;
    }
    metrics.finish(&world);
    Ok((asm, metrics))
}

fn demo(engines: &[&str], name: &str, file: &str, source: &str, is_weave: bool, substeps: Option<u32>, tsv: &mut Option<File>) -> Result<(), String> {
    eprintln!("mech_bench: evaluating {name}");
    let model = FabModel::with_script(name, file, source);
    let mut evaluator = Evaluator::new(Machine::dual_nozzle_350());
    let (_, ev) = model.evaluate(&mut evaluator, None).map_err(|e| e.to_string())?;
    if is_weave {
        let w = ev.weaves.first().ok_or("demo has no weave")?;
        let wp = WeaveProfile::new(&Profile::default(), Some(w.links.radius));
        let woven = w.weave(&wp);
        for engine in engines {
            for scenario in ["hang", "drag", "drag300", "drop"] {
                let (asm, mut metrics) = if *engine == "box3d" { woven_scenario::<MechWorld>(&woven, scenario, substeps)? } else { woven_scenario::<DynWorld>(&woven, scenario, substeps)? };
                metrics.report(engine, name, scenario, &asm, tsv).map_err(|e| e.to_string())?;
            }
        }
    } else {
        let mech = ev.mech.first().ok_or("demo has no jointed piece")?;
        let asm = mech.assembly(&[], 1.0).map_err(|e| e.to_string())?;
        for engine in engines {
            for scenario in ["rest", "pull"] {
                let pull = scenario == "pull";
                let result = if *engine == "box3d" { jointed::<MechWorld>(&asm, pull) } else { jointed::<DynWorld>(&asm, pull) };
                match result {
                    Ok(mut metrics) => metrics.report(engine, name, scenario, &asm, tsv).map_err(|e| e.to_string())?,
                    Err(error) => println!("{engine} {name} {scenario}: not run: {error}"),
                }
            }
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut tsv = std::env::var_os("DEVHUB_KEEP")
        .map(|dir| File::create(PathBuf::from(dir).join("mech_bench.tsv"))).transpose()?;
    if let Some(file) = &mut tsv {
        writeln!(file, "engine\tdemo\tscenario\tbodies\tjoints\tsep_max_mm\tsep_end_mm\tsep_p95_mm\tke_rest_nj\taxis_max_deg\tover_max_deg\tunlinked_max\tunlinked_end\tspeed_end_mm_s\tangular_end_rad_s\tstep_median_ms\tstep_p95_ms\tstep_max_ms")?;
    }
    let demos = [
        ("chain", "chain.cad.splash", include_str!("../../app/resources/chain.cad.splash"), true),
        ("mail", "chainmail_sheet.cad.splash", include_str!("../../app/resources/demos/chainmail_sheet/chainmail_sheet.cad.splash"), true),
        ("dragon", "articulated_dragon.cad.splash", include_str!("../../app/resources/demos/articulated_dragon/articulated_dragon.cad.splash"), false),
        ("robot", "poseable_robot.cad.splash", include_str!("../../app/resources/demos/poseable_robot/poseable_robot.cad.splash"), false),
        ("athlete", "poseable_athlete.cad.splash", include_str!("../../app/resources/demos/poseable_athlete/poseable_athlete.cad.splash"), false),
    ];
    let named = std::env::args().nth(1).filter(|a| a != "both");
    // Weave substeps for this run (default: the assembly's own).
    let substeps: Option<u32> = match std::env::args().nth(2) {
        Some(n) => Some(n.parse().map_err(|_| format!("substeps must be a number: {n}"))?),
        None => None,
    };
    let engines: Vec<&str> = match named.as_deref() {
        Some(e @ ("box3d" | "dynamics")) => vec![e],
        None => vec!["box3d", "dynamics"],
        Some(other) => return Err(format!("unknown engine {other}: box3d or dynamics").into()),
    };
    let mut failed = 0;
    for (name, file, source, is_weave) in demos {
        if let Err(error) = demo(&engines, name, file, source, is_weave, substeps, &mut tsv) {
            eprintln!("mech_bench: {name} failed: {error}");
            failed += 1;
        }
    }
    if failed != 0 {
        return Err(format!("{failed} demo(s) failed").into());
    }
    Ok(())
}
