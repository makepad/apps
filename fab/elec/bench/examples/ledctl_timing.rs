//! Native transient timing baseline for the LED controller board.

use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::{lower, Lowered, ModelLibs};
use fab_elec_core::ids::BoardId;
use fab_elec_sim::{run, Analysis, Control, Record, SimCircuit, SimDiag, Sink, Solver, Summary, TranSpec};
use std::hint::black_box;
use std::time::{Duration, Instant};

const REPEATS: usize = 5;

#[derive(Default)]
struct Count {
    points: usize,
}

impl Sink for Count {
    fn point(&mut self, _at: f64, _outer: Option<f64>, _x: &[f64]) {
        self.points += 1;
    }
}

#[derive(Clone, Copy)]
struct Case<'a> {
    name: &'static str,
    lowered: &'a Lowered,
    reltol: f64,
    tstop: f64,
    max_step: Option<f64>,
    record: bool,
}

struct Sample {
    wall: Duration,
    summary: Summary,
}

fn timed_run(circuit: &SimCircuit, analysis: &Analysis, recording: bool) -> Result<Sample, SimDiag> {
    let control = Control::default();
    let mut count = Count::default();
    let mut record = Record::default();
    let sink: &mut dyn Sink = if recording { &mut record } else { &mut count };

    let start = Instant::now();
    let result = run(circuit, analysis, sink, &control);
    let wall = start.elapsed();

    // Keep sink results observable; creation, inspection and disposal are not timed.
    black_box(count.points);
    black_box(&record);
    result.map(|summary| Sample { wall, summary })
}

fn measure(case: Case<'_>) -> Result<Sample, String> {
    if !case.lowered.diags.is_empty() {
        return Err(case.lowered.diags.iter().map(|diag| diag.message.as_str()).collect::<Vec<_>>().join("; "));
    }
    let mut circuit = case.lowered.circuit.clone();
    circuit.opts.reltol = case.reltol;
    let analysis = Analysis::Tran(TranSpec {
        tstop: case.tstop,
        max_step: case.max_step,
        uic: false,
    });
    let mut samples = Vec::with_capacity(REPEATS);
    let mut errors = Vec::new();
    for repeat in 1..=REPEATS {
        match timed_run(&circuit, &analysis, case.record) {
            Ok(sample) => samples.push(sample),
            Err(diag) => errors.push(format!("run {repeat}/{REPEATS}: {diag}")),
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    // Keep the solver counters from the same run as the median wall time.
    samples.sort_by_key(|sample| sample.wall);
    Ok(samples.swap_remove(REPEATS / 2))
}

fn main() {
    let build = if cfg!(debug_assertions) { "debug" } else { "release" };
    let parallelism = std::thread::available_parallelism()
        .map(|count| count.to_string())
        .unwrap_or_else(|_| "unavailable".to_owned());
    println!("build={build} available_parallelism={parallelism}");

    let design = fab_elec_fixtures::ledctl();
    let libs = ModelLibs::generic();
    let bench = ledctl_bench(10e3, [0.3; 8]);
    let start = Instant::now();
    let pwm10k = lower(&design, BoardId(0), &bench, &libs);
    let lowering = start.elapsed();
    // Query actual solver layout outside both timers; do not infer it from node count.
    match Solver::new(&pwm10k.circuit) {
        Ok(solver) => println!(
            "lowering pwm10k: {:.3} ms unknowns={} solver={}",
            lowering.as_secs_f64() * 1e3,
            solver.n,
            if solver.layout.is_dense() { "dense" } else { "sparse" },
        ),
        Err(diag) => println!(
            "lowering pwm10k: {:.3} ms unknowns=unavailable solver=unavailable ERROR: {diag}",
            lowering.as_secs_f64() * 1e3,
        ),
    }

    // Lower each additional stimulus once, outside the transient timing loop.
    let pwm1k = lower(&design, BoardId(0), &ledctl_bench(1e3, [0.3; 8]), &libs);
    let staggered = lower(
        &design,
        BoardId(0),
        &ledctl_bench(10e3, [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8]),
        &libs,
    );
    let run_case = Case {
        name: "pwm10k-run",
        lowered: &pwm10k,
        reltol: 1e-3,
        tstop: 20e-3,
        max_step: None,
        record: false,
    };
    let live_case = Case { name: "pwm10k-live", reltol: 1e-2, ..run_case };
    let cases = [
        run_case,
        live_case,
        Case { name: "pwm10k-live-dt", max_step: Some(1e-5), ..live_case },
        Case { name: "pwm1k-live", lowered: &pwm1k, tstop: 0.2, ..live_case },
        Case { name: "staggered-live", lowered: &staggered, ..live_case },
        Case { name: "pwm10k-record", record: true, ..live_case },
    ];

    println!(
        "{:<16} {:>8} {:>10} {:>9} {:>10} {:>9} {:>12} {:>10} {:>12} {:>11} {:>12} {:>10}",
        "case", "reltol", "max_step", "sim_ms", "steps", "rejected", "newton/step",
        "lu_full", "lu_refactor", "wall_ms", "steps/s", "sim/wall",
    );
    let mut fastest: Option<(&str, f64, f64)> = None;
    for case in cases {
        let max_step = case.max_step.map(|step| format!("{step:.0e}")).unwrap_or_else(|| "None".to_owned());
        let prefix = format!(
            "{:<16} {:>8.0e} {:>10} {:>9.3}",
            case.name, case.reltol, max_step, case.tstop * 1e3,
        );
        match measure(case) {
            Ok(sample) => {
                let wall_seconds = sample.wall.as_secs_f64();
                let stats = sample.summary.stats;
                let newton_per_step = stats.iterations as f64 / stats.steps as f64;
                let steps_per_second = stats.steps as f64 / wall_seconds;
                let sim_per_wall = case.tstop / wall_seconds;
                println!(
                    "{prefix} {:>10} {:>9} {:>12.3} {:>10} {:>12} {:>11.3} {:>12.1} {:>10.3}",
                    stats.steps, stats.rejected, newton_per_step,
                    sample.summary.lu.0, sample.summary.lu.1,
                    wall_seconds * 1e3, steps_per_second, sim_per_wall,
                );
                // Compare simulated-time throughput: the 1 kHz case runs ten times longer.
                if fastest.as_ref().map_or(true, |best| sim_per_wall > best.2) {
                    fastest = Some((case.name, steps_per_second, sim_per_wall));
                }
            }
            Err(diag) => println!(
                "{prefix} {:>10} {:>9} {:>12} {:>10} {:>12} {:>11} {:>12} {:>10} ERROR: {}",
                "-", "-", "-", "-", "-", "-", "-", "-",
                diag.split_whitespace().collect::<Vec<_>>().join(" "),
            ),
        }
    }
    match fastest {
        Some((name, steps_per_second, sim_per_wall)) => println!(
            "fastest (sim/wall): {name} steps/s={steps_per_second:.1} sim/wall={sim_per_wall:.3}",
        ),
        None => println!("fastest: unavailable (no case completed all {REPEATS} runs)"),
    }
    for case in [run_case, live_case, Case { name: "staggered-live", lowered: &staggered, ..live_case }] {
        profile(case);
    }
    live_overhead(&pwm10k, 20e-3);
    latency(&design, &libs);
}

/// One profiled run (`Options::profile`) of a case: where an iteration's time goes, how many
/// steps a corner costs, and why steps were rejected (cause, steps since the restart, time since
/// the latest source corner).
fn profile(case: Case<'_>) {
    let mut circuit = case.lowered.circuit.clone();
    circuit.opts.reltol = case.reltol;
    circuit.opts.profile = true;
    let analysis = Analysis::Tran(TranSpec { tstop: case.tstop, max_step: case.max_step, uic: false });
    let Ok(sample) = timed_run(&circuit, &analysis, false) else {
        println!("profile {}: the run failed", case.name);
        return;
    };
    let Some(p) = sample.summary.profile.as_deref() else {
        println!("profile {}: no profile", case.name);
        return;
    };
    // the source corners (every breakpoint) up to the stop time
    let mut corners = Vec::new();
    if let Ok(solver) = Solver::new(&circuit) {
        let mut t = 0.0;
        loop {
            let next = solver.banks.iter().map(|b| b.next_break(t)).fold(f64::INFINITY, f64::min);
            if !(next < case.tstop) || next <= t {
                break;
            }
            corners.push(next);
            t = next;
        }
    }
    let st = sample.summary.stats;
    let wall = sample.wall.as_nanos() as f64;
    let accepted = st.steps - st.rejected;
    let pct = |ns: u64| 100.0 * ns as f64 / wall;
    let rest = wall - (p.load_ns + p.factor_ns + p.refactor_ns + p.solve_ns) as f64;
    println!(
        "profile {}: wall {:.2} ms, {} steps ({} accepted, {} rejected {:.1} %), {:.2} iterations per accepted step, {} corners ({:.1} accepted steps a corner)",
        case.name, wall / 1e6, st.steps, accepted, st.rejected, 100.0 * st.rejected as f64 / st.steps.max(1) as f64,
        st.iterations as f64 / accepted.max(1) as f64, corners.len(), accepted as f64 / corners.len().max(1) as f64,
    );
    println!(
        "  split: load {:.1} %, factor {:.1} % ({} x {:.0} ns), refactor {:.1} % ({} x {:.0} ns), solve {:.1} % ({:.0} ns each), rest {:.1} %; {:.0} ns an iteration",
        pct(p.load_ns), pct(p.factor_ns), p.factors, p.factor_ns as f64 / p.factors.max(1) as f64,
        pct(p.refactor_ns), p.refactors, p.refactor_ns as f64 / p.refactors.max(1) as f64,
        pct(p.solve_ns), p.solve_ns as f64 / st.solves.max(1) as f64, 100.0 * rest / wall, wall / st.iterations.max(1) as f64,
    );
    println!(
        "  rejected by cause: newton {}, crossing {}, provisional {}, error {}",
        st.rejected_newton, st.rejected_crossing, st.rejected_provisional, st.rejected_error,
    );
    let mut since = [0u64; 7];
    let mut after = [0u64; 5];
    for r in &p.rejections {
        since[match r.since_restart { 0 => 0, 1 => 1, 2 => 2, 3 => 3, 4..=7 => 4, 8..=15 => 5, _ => 6 }] += 1;
        let k = corners.partition_point(|&c| c <= r.t);
        let dt = if k == 0 { r.t } else { r.t - corners[k - 1] };
        after[match dt { d if d < 1e-8 => 0, d if d < 1e-7 => 1, d if d < 1e-6 => 2, d if d < 1e-5 => 3, _ => 4 }] += 1;
    }
    println!("  rejected by steps since the restart: 0: {} 1: {} 2: {} 3: {} 4-7: {} 8-15: {} 16+: {}", since[0], since[1], since[2], since[3], since[4], since[5], since[6]);
    println!("  rejected by time since the latest corner: <10 ns {} <100 ns {} <1 us {} <10 us {} later {}", after[0], after[1], after[2], after[3], after[4]);
}

/// The live loop's own cost per step (trace push, interval means, taps) against the plain run:
/// the same circuit stepped by `LiveSim` with the trace and pin currents on.
fn live_overhead(lowered: &Lowered, tstop: f64) {
    use fab_elec_sim::live::LiveCmd;
    use fab_elec_sim::livesim::{LiveSim, LiveSpec};
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, trace: true, trace_pins: true, record: f64::INFINITY, ..LiveSpec::default() };
    let reltol = spec.reltol_at(spec.res);
    let Ok(mut sim) = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec) else {
        println!("live overhead: the live bench does not start");
        return;
    };
    sim.apply(LiveCmd::Step(tstop));
    let mut sink = Count::default();
    let start = Instant::now();
    while sim.t() < tstop {
        let before = sim.t();
        if sim.advance(f64::INFINITY, None, &mut sink).is_err() || sim.t() <= before {
            break;
        }
    }
    let wall = start.elapsed().as_nanos() as f64;
    let live_steps = sim.solver().stats.steps;
    let mut circuit = lowered.circuit.clone();
    circuit.opts.reltol = reltol;
    let analysis = Analysis::Tran(TranSpec { tstop, max_step: None, uic: false });
    match timed_run(&circuit, &analysis, false) {
        Ok(run) => println!(
            "live overhead (reltol {reltol:e}, trace and pins on): live {:.0} ns a step ({live_steps} steps), run {:.0} ns a step ({} steps)",
            wall / live_steps.max(1) as f64, run.wall.as_nanos() as f64 / run.summary.stats.steps.max(1) as f64, run.summary.stats.steps,
        ),
        Err(_) => println!("live overhead: the run failed"),
    }
}

/// Publication latency of a value change: a `SetParam` on lamp 0's resistor sent to the running
/// live worker (rate 1, every LED on) until the first `LiveValues` whose lamp current shows it.
fn latency(design: &fab_elec_design::Design, libs: &ModelLibs) {
    use fab_elec_bench::SimCmd;
    use fab_elec_core::ids::{CompRef, PinIx, PinRef};
    use fab_elec_sim::live::{Latest, LiveValues};
    use fab_elec_sim::livesim::{run_live, LiveSim, LiveSpec};
    use std::sync::{mpsc, Arc};

    let lowered = lower(design, BoardId(0), &ledctl_bench(10e3, [0.0; 8]), libs);
    let comp = design.unit(BoardId(0)).and_then(|u| u.circuit.comp("lamps[0].r")).map(|c| c.id);
    let Some(comp) = comp else {
        println!("latency: unavailable (no lamps[0].r)");
        return;
    };
    let r = CompRef { board: BoardId(0), comp };
    let Some(pin) = lowered.index.pin(PinRef { board: r.board, comp: r.comp, pin: PinIx(1) }) else {
        println!("latency: unavailable (no pin index)");
        return;
    };
    let sim = match LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, LiveSpec::default()) {
        Ok(sim) => sim,
        Err(diag) => {
            println!("latency: unavailable ({diag})");
            return;
        }
    };
    let (cmd_tx, cmd_rx) = mpsc::sync_channel(64);
    let (mut trace_tx, trace_rx) = mpsc::sync_channel(4);
    let values: Arc<Latest<LiveValues>> = Arc::new(Latest::new());
    let worker = {
        let values = values.clone();
        std::thread::spawn(move || {
            let mut sink = Count::default();
            run_live(sim, &cmd_rx, &values, &mut trace_tx, &mut sink, &|| {}).map(|sim| sim.stats().points)
        })
    };
    let wait_for = |pred: &dyn Fn(&LiveValues) -> bool, since: u64| -> Option<(u64, Instant)> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            while trace_rx.try_recv().is_ok() {}
            if let Some((n, lv)) = values.latest() {
                if n > since && pred(&lv) {
                    return Some((n, Instant::now()));
                }
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        None
    };
    let Some((mut seen, _)) = wait_for(&|lv| lv.t > 0.05, 0) else {
        println!("latency: unavailable (the worker published nothing)");
        return;
    };
    let mut samples = Vec::new();
    for k in 0..20 {
        let ohms = if k % 2 == 0 { 540.0 } else { 270.0 };
        let before = values.latest().map(|(_, lv)| lv.branch_i[pin].abs()).unwrap_or(0.0);
        let Some(cmd) = lowered.index.command(&SimCmd::SetParam { comp: r, value: ohms }).pop() else { break };
        let sent = Instant::now();
        if cmd_tx.try_send(cmd).is_err() {
            break;
        }
        let changed = move |lv: &LiveValues| (lv.branch_i[pin].abs() - before).abs() > 0.3 * before;
        match wait_for(&changed, seen) {
            Some((n, at)) => {
                seen = n;
                samples.push(at - sent);
            }
            None => break,
        }
    }
    let _ = cmd_tx.try_send(fab_elec_sim::live::LiveCmd::Stop);
    let points = worker.join().ok().and_then(|r| r.ok()).unwrap_or(0);
    samples.sort();
    match (samples.first(), samples.get(samples.len() / 2), samples.last()) {
        (Some(lo), Some(mid), Some(hi)) => println!(
            "latency SetParam -> LiveValues (rate 1, {} changes, {points} points): min {:.2} ms median {:.2} ms max {:.2} ms",
            samples.len(), lo.as_secs_f64() * 1e3, mid.as_secs_f64() * 1e3, hi.as_secs_f64() * 1e3,
        ),
        _ => println!("latency: no change observed"),
    }
}
