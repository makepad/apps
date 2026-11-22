//! Grow's speed on this machine's GPU over a bracket's envelope: per level, ms and CG iterations
//! per design iteration and ns per cell per CG iteration, the best of 3 after a warm-up run.
//!
//! fab-growcheck [finest mm] [tolerance] [coarsest sweeps]: defaults 1, the job's, the job's.
//!
//! fab-growcheck live [its at 4 mm] [its at 2 mm] [sharpest] [warm its] [warm step]: a held grow of
//! that envelope, how soon its shape shows and how it moves: from the session's opening, then after
//! a 5% load nudge and after the same load sent again (what it moves anyway).

use fab_optimise::grow::{cantilever, config, latest, program, run, settled, shape, Edit, Grown, Job, Level};
use makepad_compute_program::gpu::GpuLink;
use makepad_compute_program::{Session, Value};
use std::time::Instant;

const ENVELOPE: [f64; 3] = [64.0, 46.0, 52.0];
const DESIGN_ITERATIONS: usize = 20;

fn calls(g: &Grown) -> (f64, f64) {
    let (mut dispatches, mut waits) = (0.0, 0.0);
    for row in g.stats.get("kernels").and_then(Value::as_array).unwrap_or_default() {
        let n = |k: &str| row.get(k).and_then(Value::as_number).unwrap_or(0.0);
        match row.get("label") {
            Some(Value::String(l)) if l.to_string() == "(waits for the GPU)" => waits = n("ms"),
            Some(Value::String(l)) if l.to_string().starts_with("grow ") => dispatches += n("calls"),
            _ => {}
        }
    }
    (dispatches, waits)
}

/// The share of `a`'s solid that is on the other side of the iso in `b`.
fn moved(a: &[f32], b: &[f32]) -> f64 {
    let solid = a.iter().filter(|x| **x > 0.5).count();
    a.iter().zip(b).filter(|(x, y)| (**x > 0.5) != (**y > 0.5)).count() as f64 / solid.max(1) as f64
}

/// Reads a held session up to `rev`'s settling: the shapes it showed, in ms from `t`, and the compliance.
fn watch(session: &Session, level: usize, rev: usize, t: Instant) -> (Vec<(f64, usize, Vec<f32>)>, f64) {
    let mut shown = Vec::new();
    loop {
        let u = session.wait();
        if let Some(end) = &u.end {
            panic!("the held grow ended: {end:?}");
        }
        let ms = 1000.0 * t.elapsed().as_secs_f64();
        for l in 0..=level {
            if let Some((r, x)) = shape(&u, l) {
                if r == rev {
                    shown.push((ms, l, x));
                }
            }
        }
        if let Some(c) = settled(&u, rev) {
            return (shown, c);
        }
    }
}

/// How the shapes shown came to the last one: the first's time, and the first one within `near` of it.
fn report(what: &str, shown: &[(f64, usize, Vec<f32>)], compliance: f64) {
    let (end, level, last) = shown.last().map(|(t, l, x)| (*t, *l, x)).unwrap();
    let at = |near: f64| shown.iter().find(|(_, l, x)| *l == level && moved(last, x) <= near).map_or(f64::NAN, |(t, ..)| *t);
    println!("{what}: first shape {:.1} ms, within 20% of its end {:.1} ms, 10% {:.1} ms, 5% {:.1} ms; settled {end:.1} ms over {} shapes, compliance {compliance:.3}", shown[0].0, at(0.2), at(0.1), at(0.05), shown.len());
}

fn live(gpu: GpuLink) {
    let arg = |i: usize, default: f64| std::env::args().nth(i).and_then(|a| a.parse::<f64>().ok()).unwrap_or(default);
    let (its0, its1, sharpest, warm_its, warm_step) = (arg(2, 20.0) as usize, arg(3, 30.0) as usize, arg(4, 16.0), arg(5, 8.0) as usize, arg(6, 0.05));
    let n0 = ENVELOPE.map(|l| (l / 4.0).round() as usize);
    let n1 = n0.map(|c| 2 * c);
    let middle = (sharpest).sqrt();
    let levels: Vec<Level> = vec![cantilever(n0, 4.0, its0, 6.0, [1.0, middle], 100.0), cantilever(n1, 2.0, its1, 3.0, [middle, sharpest], 100.0)];
    let mut job = Job::new(levels, 0.3);
    job.hold = true;
    job.warm_its = warm_its;
    job.warm_step = warm_step;
    println!("{n0:?} then {n1:?} cells, {its0} and {its1} iterations to sharpness {sharpest}; an edit: {warm_its} at step {warm_step}");
    turns(&job, &gpu);
    for _ in 0..2 {
        let t = Instant::now();
        let session = Session::open(&program(), config(&job, Some(gpu.clone())));
        let (cold, c) = watch(&session, 1, 0, t);
        report("from nothing", &cold, c);
        // the 4 mm level's own way to its end
        let coarse: Vec<_> = cold.iter().filter(|(_, l, _)| *l == 0).cloned().collect();
        report("  its 4 mm level", &coarse, c);
        let before = cold.last().unwrap().2.clone();

        let load = |side: f32| -> Vec<Vec<[f32; 4]>> { job.levels.iter().map(|l| l.load.iter().map(|f| [f[0], side * f[2].abs(), f[2], 0.0]).collect()).collect() };
        let t = Instant::now();
        let rev = session.send("edit", Edit { volume: job.volume, load: load(0.05) }.value()) as usize;
        let (warm, c) = watch(&session, 1, rev, t);
        report("a 5% nudge", &warm, c);
        let nudged = warm.last().unwrap().2.clone();
        println!("  it moved {:.2}% of the solid, its first shape {:.2}%", 100.0 * moved(&before, &nudged), 100.0 * moved(&before, &warm[0].2));

        let t = Instant::now();
        let rev = session.send("edit", Edit { volume: job.volume, load: load(0.05) }.value()) as usize;
        let (same, c) = watch(&session, 1, rev, t);
        report("the same load again", &same, c);
        println!("  it moved {:.2}% of the solid", 100.0 * moved(&nudged, &same.last().unwrap().2));

        session.cancel();
    }
}

/// How far an edit's first look tells what it comes to: the load turned by each angle on a settled
/// grow, the compliance of its first iteration and where it settles, against a grow from nothing.
fn turns(job: &Job, gpu: &GpuLink) {
    for degrees in [3.0f32, 10.0, 20.0, 30.0, 45.0, 60.0, 90.0] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let turned: Vec<Vec<[f32; 4]>> = job.levels.iter().map(|l| l.load.iter().map(|f| [0.0, sin * f[2].abs(), cos * f[2], 0.0]).collect()).collect();
        let session = Session::open(&program(), config(job, Some(gpu.clone())));
        let (_, settled0) = watch(&session, 1, 0, Instant::now());
        let rev = session.send("edit", Edit { volume: job.volume, load: turned.clone() }.value()) as usize;
        let mut first = None;
        let c = loop {
            let u = session.wait();
            if let Some(end) = &u.end {
                panic!("the held grow ended: {end:?}");
            }
            if let (None, Some(i)) = (first, latest(&u).filter(|i| i.rev == rev)) {
                first = Some(i.compliance);
            }
            if let Some(c) = settled(&u, rev) {
                break c;
            }
        };
        session.cancel();
        let mut fresh = job.clone();
        fresh.hold = false;
        for (l, load) in fresh.levels.iter_mut().zip(&turned) {
            l.load = load.clone();
        }
        let cold = run(&fresh, gpu.clone()).unwrap_or_else(|e| panic!("{e}")).iterations.last().unwrap().compliance;
        let first = first.unwrap_or(f64::NAN);
        println!("turn {degrees}: settled before {settled0:.3}, first look {first:.3} ({:.2}x), settled {c:.3}, from nothing {cold:.3} (warm over fresh {:.3})", first / settled0, c / cold);
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("live") {
        return live(makepad_compute_program_gpu::spawn().expect("a GPU device"));
    }
    let arg = |i: usize| std::env::args().nth(i).and_then(|a| a.parse::<f64>().ok());
    let finest = arg(1).unwrap_or(1.0);
    let gpu = makepad_compute_program_gpu::spawn().expect("a GPU device");
    let mut h = 4.0;
    while h >= finest {
        let n = ENVELOPE.map(|l| (l / h).round() as usize);
        let cells = n[0] * n[1] * n[2];
        let mut job = Job::new(vec![cantilever(n, h, DESIGN_ITERATIONS, 1.5 * h, [1.0, 4.0], 100.0)], 0.3);
        job.tol = arg(2).unwrap_or(job.tol);
        job.coarsest_sweeps = arg(3).map_or(job.coarsest_sweeps, |s| s as usize);
        run(&job, gpu.clone()).unwrap_or_else(|e| panic!("{e}"));
        let mut best = f64::MAX;
        let mut grown = None;
        for _ in 0..3 {
            let t = Instant::now();
            let g = run(&job, gpu.clone()).unwrap_or_else(|e| panic!("{e}"));
            best = best.min(1000.0 * t.elapsed().as_secs_f64());
            grown = Some(g);
        }
        let g = grown.unwrap();
        let cg: usize = g.iterations.iter().map(|i| i.pcg).sum();
        let last = g.iterations.last().unwrap();
        let (dispatches, waits) = calls(&g);
        println!(
            "{h} mm, {n:?} = {cells} cells: {:.1} ms a design iteration, {:.1} CG, {:.0} dispatches, {:.1} ns per cell per CG iteration; GPU waits {:.0} of {best:.0} ms; compliance {:.4}, volume {:.2} of {:.2}",
            best / DESIGN_ITERATIONS as f64,
            cg as f64 / DESIGN_ITERATIONS as f64,
            dispatches / DESIGN_ITERATIONS as f64,
            1e6 * best / (cells * cg.max(1)) as f64,
            waits,
            last.compliance,
            last.volume,
            last.target,
        );
        h /= 2.0;
    }
}
