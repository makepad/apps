//! The counted bench run over a committed lowering, from its operating point.
//! Every accepted transient point is recorded and judged against the expectations.

use crate::expect::{CheckState, Expect, Expectations, Sample};
use crate::Lowered;
use fab_elec_core::diag::Diag;
use fab_elec_sim::cosim::CoSimDevice;
use fab_elec_sim::live::LiveCmd;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::{Rescue, Sink};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The wall time between two looks at a run's `stop` flag: a soft end of the stepper's, which
/// moves no point of the run.
const SLICE: Duration = Duration::from_millis(50);

/// The counted check's result.
#[derive(Clone, Debug, PartialEq)]
pub struct BenchReport {
    /// Per expectation, in order (`Expectations::states`).
    pub states: Vec<CheckState>,
    /// Probes the lowering does not have (from `Expectations::new`).
    pub diags: Vec<Diag>,
    /// Accepted points judged, and the simulated time reached.
    pub points: usize,
    pub t: f64,
    /// The solver's failure, when the run stopped early (its message).
    pub error: Option<String>,
    /// The steps the run took only through the stepper's gmin ladder (`Rescue`): a run that got
    /// through only because of it says so here.
    pub rescues: Vec<Rescue>,
    /// Stopped by its caller before `tstop` (the states are as far as it got).
    pub stopped: bool,
}

struct NoopSink;

impl Sink for NoopSink {}

/// The live spec a bench run steps at: Exact as the app runs it (`LiveSpec::default()`, whose
/// tolerance in Exact is `Options::default().reltol`, `LiveSpec::reltol_at`), unpaced, every
/// point kept with its pin currents. The app's Exact is this one (a test holds them together).
pub fn bench_spec() -> LiveSpec {
    LiveSpec { rate: f64::INFINITY, playing: false, trace: true, trace_pins: true, record: f64::INFINITY, ..LiveSpec::default() }
}

/// Runs `lowered` from its operating point to `tstop` in Exact at the bench's spec (`bench_spec`),
/// with `co` (the bench's firmware, as its maker makes one for a run) attached from the start.
/// Records every accepted point, including pin currents, and feeds it to `Expectations`.
/// Component power is the sum of each connected pin's net voltage times its current.
/// The three sample buffers are reused throughout; an invalid stop time returns an error.
/// Each state's `outcome()` is the counted run's final verdict at `tstop`;
/// a windowed span ending after `tstop` is `NotJudged`. `stop` set ends the run within a slice
/// of wall time, judged as far as it got (`BenchReport::stopped`). `progress` hears the time the
/// run has reached after each slice.
pub fn bench_run(lowered: Lowered, expects: Vec<Expect>, tstop: f64, co: Option<CoSimDevice>, stop: &AtomicBool, progress: &dyn Fn(f64)) -> BenchReport {
    let Lowered { circuit, plan, index, .. } = lowered;
    let (mut expectations, diags) = Expectations::new(expects, &index);
    let mut report = BenchReport {
        states: expectations.states().to_vec(),
        diags,
        points: 0,
        t: 0.0,
        error: None,
        rescues: Vec::new(),
        stopped: false,
    };
    if !tstop.is_finite() || tstop < 0.0 {
        report.error = Some("bench stop time must be finite and nonnegative".to_owned());
        return report;
    }

    let (nets, pins, parts) = (plan.nets.len(), plan.pins.len(), plan.part_pins.len());
    let pin_net = plan.pin_net.clone();
    let part_pins = plan.part_pins.clone();
    let mut sim = match LiveSim::new(circuit, plan, index.generation, bench_spec()) {
        Ok(sim) => sim,
        Err(error) => {
            report.error = Some(error.message);
            return report;
        }
    };
    if co.is_some() {
        sim.attach(co);
    }
    sim.apply(LiveCmd::Step(tstop));
    let mut sink = NoopSink;
    // Advance can return early when LiveSim has values ready to publish, and returns after a
    // slice of wall time; the step ends paused at `tstop` (a call that moves no time ends the
    // run, so roundoff at the stop cannot spin).
    while sim.t() < tstop {
        if stop.load(Ordering::Relaxed) {
            report.stopped = true;
            break;
        }
        let before = sim.t();
        if let Err(error) = sim.advance(f64::INFINITY, Some(SLICE), &mut sink) {
            report.error = Some(error.message);
            break;
        }
        if sim.t() <= before {
            break;
        }
        progress(sim.t());
    }
    // a run that stood still short of its stop says so, never that it got there
    if !report.stopped && report.error.is_none() && sim.t() < tstop * (1.0 - 1e-9) {
        report.error = Some(format!("the run stood still at {} s, short of its stop at {tstop} s", sim.t()));
    }

    let mut net_v = vec![0.0; nets];
    let mut pin_i = vec![0.0; pins];
    let mut part_p = vec![0.0; parts];
    let store = sim.store();
    for point in 0..store.len() {
        let mut signals = store.signals.iter();
        for value in net_v.iter_mut().chain(pin_i.iter_mut()) {
            *value = signals.next().map_or(f64::NAN, |signal| f64::from(signal.at(point)));
        }
        for (power, &(first, count)) in part_p.iter_mut().zip(&part_pins) {
            *power = pin_i.iter().zip(&pin_net).skip(first as usize).take(count as usize)
                .filter_map(|(&current, &net)| {
                    net.map(|net| net_v.get(net as usize).copied().unwrap_or(f64::NAN) * current)
                })
                .sum();
        }
        expectations.step(&Sample { t: store.time.at(point), net_v: &net_v, pin_i: &pin_i, part_p: &part_p });
    }
    report.states = expectations.states().to_vec();
    report.points = store.len();
    report.t = sim.t();
    report.rescues = sim.stats().rescues.clone();
    report
}
