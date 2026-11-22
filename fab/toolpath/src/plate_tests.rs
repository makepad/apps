//! Several parts on one [`stages::Plate`]: each plans as it does alone, at any thread
//! count, whether its traces are cut by the plate's tasks or put before the plate is made.

use crate::stages::{PartPlan, Plate, PlateCtl, PlatePart, PlateRun, Stage, Task};
use crate::*;
use makepad_fab_slicer::sdf::SdfSource;
use std::sync::{Mutex, MutexGuard};

/// Held by a test that sets the thread count or needs its workers at once: `par::threads()`
/// is the process's, and it bounds the helpers of every plate.
pub(crate) fn threads_held() -> MutexGuard<'static, ()> {
    static THREADS: Mutex<()> = Mutex::new(());
    THREADS.lock().unwrap_or_else(|e| e.into_inner())
}

fn box3(x: f64, y: f64, z: f64, c: [f64; 3], r: [f64; 3]) -> f64 {
    ((x - c[0]).abs() - r[0]).max((y - c[1]).abs() - r[1]).max((z - c[2]).abs() - r[2])
}

/// A 20 x 14 block with a round hole, its top 3 mm high left of x = 104 and
/// 2 mm right of it (a top step), centred at (100, 100); a ledge from its
/// step out over a 4 x 4 pillar beside it (a bridge over the gap above the
/// pillar, which is an island of its own up to 1 mm).
pub(crate) fn part(x: f64, y: f64, z: f64) -> f64 {
    let left = box3(x, y, z, [97.0, 100.0, 1.5], [7.0, 7.0, 1.5]);
    let right = box3(x, y, z, [107.0, 100.0, 1.0], [3.0, 7.0, 1.0]);
    let hole = 2.5 - ((x - 97.0).powi(2) + (y - 100.0).powi(2)).sqrt();
    let ledge = box3(x, y, z, [112.0, 100.0, 1.7], [4.0, 2.0, 0.3]);
    let pillar = box3(x, y, z, [114.0, 100.0, 0.5], [2.0, 2.0, 0.5]);
    left.min(right).max(hole).min(ledge).min(pillar)
}
pub(crate) const PART: ([f64; 3], [f64; 3]) = ([90.0, 93.0, 0.0], [116.0, 107.0, 3.0]);

pub(crate) fn job(generator: &str) -> Job {
    let mut cfg = makepad_fab_settings::config::Config::new();
    for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
        cfg.set_str(key, "zig-zag");
    }
    for (key, value) in [("nozzle_diameter", "0.4"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "3"), ("top_shell_layers", "3"), ("bottom_shell_layers", "2"), ("sparse_infill_density", "15%"), ("wall_generator", generator), ("detect_narrow_internal_solid_infill", "1"), ("detect_floating_vertical_shell", "1")] {
        cfg.set_str(key, value);
    }
    cfg.set_vec("filament_map", vec!["1".into(), "1".into()]);
    Job::new(cfg)
}

pub(crate) fn input(source: Box<dyn ContourSource + Send + Sync>) -> ObjectInput {
    ObjectInput { overrides: Vec::new(), name: "step".into(), source, filament: 0 }
}

/// The test part, and a copy 30 mm to the right turned a quarter (another shape on the plate).
fn parts() -> Vec<ObjectInput> {
    let turned = |x: f64, y: f64, z: f64| part(y, 230.0 - x, z);
    let bounds = ([123.0, 90.0, PART.0[2]], [137.0, 116.0, PART.1[2]]);
    vec![input(Box::new(SdfSource::new(Arc::new(part), PART))), input(Box::new(SdfSource::new(Arc::new(turned), bounds)))]
}

#[test]
fn a_plate_plans_each_part_as_alone_at_any_thread_count() {
    let job = job("classic");
    let parts = parts();
    let heights: Vec<Vec<(f64, f64)>> = parts.iter().map(|o| plan_heights(o, &job)).collect();
    let _threads = threads_held();
    let before = par::threads();
    par::set_threads(16);
    let alone: Vec<String> = parts.iter().enumerate().map(|(i, o)| format!("{:?}", plan_one(par::Ctl::default(), i, o, &job).unwrap().layers)).collect();
    for threads in [16, 4, 1] {
        par::set_threads(threads);
        let plans: Vec<PartPlan> = parts.iter().enumerate().map(|(i, o)| PartPlan::new(i, o, &job, heights[i].clone())).collect();
        // Part 0 cut by the plate's tasks; part 1's even layers put before the plate is made (as
        // the live engine puts the layers it has), its odd ones cut.
        let p = Params::from_job(&job, 0);
        for (k, &(z, h)) in heights[1].iter().enumerate().step_by(2) {
            plans[1].put_trace(k, parts[1].source.contours(&[cut_z(&p, z, h)]).pop().unwrap(), None);
        }
        let plate = Plate::new(plans.iter().map(|plan| PlatePart { plan, targets: vec![Task::part(Stage::Finish)], urgency: 0, focus: Some(3) }).collect());
        assert_eq!(plate.run(PlateCtl::default()), PlateRun::Finished);
        drop(plate);
        for (i, plan) in plans.iter().enumerate() {
            assert_eq!(format!("{:?}", plan.take_finished().layers), alone[i], "part {i} at {threads} threads");
        }
    }
    par::set_threads(before);
}

#[test]
fn a_plate_names_its_spans_by_the_engine_part() {
    // As the live engine's plate when part 0 is closed: part 1 is its only member.
    let job = job("classic");
    let parts = parts();
    let plan = PartPlan::new(1, &parts[1], &job, plan_heights(&parts[1], &job));
    let plate = Plate::new(vec![PlatePart { plan: &plan, targets: vec![Task::finished_layer(2)], urgency: 0, focus: None }]);
    // spans are recorded process-wide: this run's revision is one no other test names
    const REVISION: u64 = 0x5ba7_0001;
    par::record_spans(true);
    assert_eq!(plate.run(PlateCtl { revision: REVISION, ..Default::default() }), PlateRun::Finished);
    let spans: Vec<par::Span> = par::take_spans().into_iter().filter(|s| s.revision == REVISION).collect();
    par::record_spans(false);
    assert!(spans.iter().any(|s| s.stage == "trace"), "the plate's tasks were recorded");
    assert!(spans.iter().all(|s| s.part == 1), "parts named {:?}", spans.iter().map(|s| s.part).collect::<std::collections::BTreeSet<_>>());
}

/// A source whose cuts wait at a gate once it is armed.
struct Gated {
    inner: SdfSource,
    gate: Arc<Gate>,
}

#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    cv: std::sync::Condvar,
}

#[derive(Default)]
struct GateState {
    armed: bool,
    entered: bool,
    open: bool,
    timed_out: bool,
}

impl Gate {
    fn pass(&self) {
        let mut s = self.state.lock().unwrap();
        if !s.armed {
            return;
        }
        s.entered = true;
        self.cv.notify_all();
        // A run that never wakes would hold it shut: fail instead of hanging.
        let (mut s, wait) = self.cv.wait_timeout_while(s, std::time::Duration::from_secs(30), |s| !s.open).unwrap();
        s.timed_out = wait.timed_out();
    }
}

impl ContourSource for Gated {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.inner.bounds()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        self.gate.pass();
        self.inner.contours(zs)
    }
}

/// A helper with no ready task leaves, even while another helper's task runs: a plate holds no
/// core it does not use (in the app, no pool worker or CPU permit other jobs need). Part 0's one
/// task is a cut held at a gate; part 1 plans whole meanwhile, and then only the held helper is
/// left.
#[test]
fn an_idle_helper_leaves_while_another_task_runs() {
    let job = job("classic");
    let gate = Arc::new(Gate::default());
    let cut = input(Box::new(Gated { inner: SdfSource::new(Arc::new(part), PART), gate: gate.clone() }));
    let parts = parts();
    let other = &parts[1];
    let heights = [plan_heights(&cut, &job), plan_heights(other, &job)];
    let n = heights[1].len();
    let plans = [PartPlan::new(0, &cut, &job, heights[0].clone()), PartPlan::new(1, other, &job, heights[1].clone())];
    gate.state.lock().unwrap().armed = true;
    let plate = Plate::new(vec![
        PlatePart { plan: &plans[0], targets: vec![Task::trace(0)], urgency: 0, focus: None },
        PlatePart { plan: &plans[1], targets: plans[1].layer_targets(0..n), urgency: 1, focus: None },
    ]);
    let _threads = threads_held();
    let before = par::threads();
    par::set_threads(4);
    let (run, alone) = std::thread::scope(|s| {
        let watch = s.spawn(|| {
            drop(gate.cv.wait_while(gate.state.lock().unwrap(), |s| !s.entered).unwrap());
            let start = std::time::Instant::now();
            // part 1 plans on the other helpers; once it is done they have nothing to do
            let alone = loop {
                if plate.finished(1) && plate.helpers() == 1 {
                    break true;
                }
                if start.elapsed().as_secs() >= 30 {
                    break false;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            };
            gate.state.lock().unwrap().open = true;
            gate.cv.notify_all();
            alone
        });
        let run = plate.run(PlateCtl::default());
        (run, watch.join().unwrap())
    });
    par::set_threads(before);
    assert!(alone, "idle helpers stayed while part 0's cut was held ({} helpers)", plate.helpers());
    assert!(!gate.state.lock().unwrap().timed_out);
    assert_eq!(run, PlateRun::Finished);
    assert_eq!(plate.helpers(), 0, "every helper left once the plate was done");
}

/// A crew whose helpers can't start before its lead returns (in the app, a pool whose every
/// worker is busy, perhaps with other crews' leads) still plans the plate, as alone: the lead
/// runs the tasks itself.
#[test]
fn a_plate_finishes_when_no_helper_can_start() {
    // A helper recruited waits until the lead returns, as a pool's scope runs its queued children
    // at its end: a lead that waits for its helpers meanwhile waits for ever.
    struct Unstarted<'a>(&'a std::sync::atomic::AtomicUsize);
    impl par::Crew for Unstarted<'_> {
        fn recruit(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn wait(&self, _: &mut dyn FnMut()) {
            panic!("the lead waited for helpers that can't start before it returns");
        }
        fn leave(&self) -> bool {
            false
        }
    }
    struct NoWorkerFree;
    impl par::FanOut for NoWorkerFree {
        fn run(&self, len: usize, f: &(dyn Fn(usize) + Sync)) {
            (0..len).for_each(f);
        }
        fn crew(&self, lead: &(dyn Fn(&dyn par::Crew) + Sync), helper: &(dyn Fn(&dyn par::Crew) + Sync)) {
            let queued = std::sync::atomic::AtomicUsize::new(0);
            lead(&Unstarted(&queued));
            while queued.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                queued.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                helper(&Unstarted(&queued));
            }
        }
    }
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            par::set_fan_out(Arc::new(par::ScopedThreads));
            par::set_threads(self.0);
        }
    }
    let job = job("classic");
    let parts = parts();
    let _threads = threads_held();
    let alone: Vec<String> = parts.iter().enumerate().map(|(i, o)| format!("{:?}", plan_one(par::Ctl::default(), i, o, &job).unwrap().layers)).collect();
    let _restore = Restore(par::threads());
    par::set_threads(4);
    par::set_fan_out(Arc::new(NoWorkerFree));
    let plans: Vec<PartPlan> = parts.iter().enumerate().map(|(i, o)| PartPlan::new(i, o, &job, plan_heights(o, &job))).collect();
    let plate = Plate::new(plans.iter().map(|plan| PlatePart { plan, targets: vec![Task::part(Stage::Finish)], urgency: 0, focus: Some(3) }).collect());
    assert_eq!(plate.run(PlateCtl::default()), PlateRun::Finished);
    assert_eq!(plate.helpers(), 0, "every helper left once the plate was done");
    drop(plate);
    for (i, plan) in plans.iter().enumerate() {
        assert_eq!(format!("{:?}", plan.take_finished().layers), alone[i], "part {i}");
    }
}
