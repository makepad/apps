//! How the simulator scales with circuit size: a timed series over generated circuits of about 50,
//! 500 and 5000 nodes, each an RC ladder driven by a pulse with a diode and BJT mix along it.
//!
//! Per size: the unknowns and devices, the operating point's wall time and Newton iterations, and
//! a transient's accepted and rejected steps, Newton iterations per step, wall time and steps per
//! second (medians over `REPS` runs with profiling off, into a sink that keeps nothing), then one
//! profiled run's split of that time: device evaluation (load), full factorisations,
//! refactorisations, triangular solves and the rest (step control, prediction, history).
//!
//! Run it native and alone on a quiet box (`cargo run --release -p fab-elec-sim --example scale`);
//! an argument lists the target node counts instead of `50 500 5000`.

use fab_elec_sim::analysis::Sink;
use fab_elec_sim::{run, Analysis, Control, ModelKind, SimCircuit, Source, Summary, TranSpec, Wave};
use std::time::Instant;

/// Timed runs per measurement; the median is reported.
const REPS: usize = 5;
/// The drive: 0 to 5 V pulses of 2 us every 10 us with 100 ns edges; the transient covers four.
const PERIOD: f64 = 10e-6;
const TSTOP: f64 = 4.0 * PERIOD;

/// A sink that keeps nothing: the timings are the simulator's, not a recorder's.
struct Discard;

impl Sink for Discard {}

/// About `nodes` circuit nodes in groups of eight: four RC ladder sections (1 kohm, 100 pF), and
/// at the group's last section an NPN stage (10 kohm base resistor, 100 ohm emitter resistor,
/// 4.7 kohm collector load to 5 V) whose collector drives a diode into a 10 pF, 100 kohm hold.
/// Every fourth group's diode instead clamps its ladder node to the rail. Returns the circuit and
/// its node count (ground excluded, the devices' internal nodes not counted).
fn circuit(nodes: usize) -> (SimCircuit, usize) {
    let mut c = SimCircuit::new();
    c.model("dm", ModelKind::Diode, &[("is", 1e-14), ("rs", 1.0), ("cjo", 2e-12), ("tt", 5e-9)]);
    c.model("qm", ModelKind::Npn, &[
        ("is", 1e-15), ("bf", 100.0), ("rb", 50.0), ("rc", 10.0), ("re", 1.0),
        ("cje", 2e-12), ("cjc", 1e-12), ("tf", 3e-10), ("tr", 8e-9),
    ]);
    c.v("vcc", "vcc", "0", Source::dc(5.0));
    c.v("vin", "l0", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.0, tr: 100e-9, tf: 100e-9, pw: 2e-6, per: PERIOD }));
    let groups = (nodes / 8).max(1);
    let mut prev = "l0".to_owned();
    let mut count = 2;
    for g in 0..groups {
        for s in 0..4 {
            let k = 4 * g + s + 1;
            let node = format!("l{k}");
            c.r(&format!("r{k}"), &prev, &node, 1e3);
            c.c(&format!("c{k}"), &node, "0", 100e-12);
            prev = node;
            count += 1;
        }
        let (b, e, col, o) = (format!("b{g}"), format!("e{g}"), format!("k{g}"), format!("o{g}"));
        c.r(&format!("rb{g}"), &prev, &b, 10e3);
        c.q(&format!("q{g}"), &col, &b, &e, "qm");
        c.r(&format!("re{g}"), &e, "0", 100.0);
        c.r(&format!("rc{g}"), "vcc", &col, 4.7e3);
        if g % 4 == 3 {
            c.d(&format!("d{g}"), &prev, "vcc", "dm");
            c.r(&format!("ro{g}"), &col, &o, 1e3);
        } else {
            c.d(&format!("d{g}"), &col, &o, "dm");
        }
        c.c(&format!("co{g}"), &o, "0", 10e-12);
        c.r(&format!("rh{g}"), &o, "0", 100e3);
        count += 4;
    }
    (c, count)
}

fn devices(c: &SimCircuit) -> usize {
    let d = &c.devices;
    d.r.len() + d.c.len() + d.v.len() + d.d.len() + d.q.len()
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(f64::total_cmp);
    xs[xs.len() / 2]
}

/// `REPS` runs of `a`; the median wall seconds and the last run's summary.
fn timed(c: &SimCircuit, a: &Analysis) -> (f64, Summary) {
    let mut walls = Vec::with_capacity(REPS);
    let mut last = None;
    for _ in 0..REPS {
        let start = Instant::now();
        let summary = run(c, a, &mut Discard, &Control::default()).unwrap_or_else(|d| panic!("the run converges: {d}"));
        walls.push(start.elapsed().as_secs_f64());
        last = Some(summary);
    }
    (median(walls), last.expect("REPS > 0"))
}

fn main() {
    let sizes: Vec<usize> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let sizes = if sizes.is_empty() { vec![50, 500, 5000] } else { sizes };
    let tran = Analysis::Tran(TranSpec { tstop: TSTOP, max_step: None, uic: false });
    println!("scale: {REPS} timed runs per measurement (median), tran to {:.0} us", TSTOP * 1e6);
    for nodes in sizes {
        let (mut c, count) = circuit(nodes);
        c.opts.profile = false;
        let (op_wall, op) = timed(&c, &Analysis::Op);
        let (tr_wall, tr) = timed(&c, &tran);
        let s = &tr.stats;
        println!(
            "nodes {count} unknowns {} devices {} {} | op {:.3} ms, {} iterations | tran {} steps ({} rejected: newton {}, crossing {}, provisional {}, error {}), {:.2} iterations/step, {:.1} ms, {:.0} steps/s, {:.1} us/step, factors {} refactors {}",
            tr.unknowns, devices(&c), if tr.dense { "dense" } else { "sparse" },
            op_wall * 1e3, op.stats.iterations,
            s.steps, s.rejected, s.rejected_newton, s.rejected_crossing, s.rejected_provisional, s.rejected_error,
            s.iterations as f64 / s.steps.max(1) as f64, tr_wall * 1e3, s.steps as f64 / tr_wall, tr_wall * 1e6 / s.steps.max(1) as f64,
            tr.lu.0, tr.lu.1,
        );
        // one profiled run for the split (its clock reads add a little to every phase)
        c.opts.profile = true;
        let start = Instant::now();
        let profiled = run(&c, &tran, &mut Discard, &Control::default()).unwrap_or_else(|d| panic!("the profiled run converges: {d}"));
        let wall_ns = start.elapsed().as_nanos() as f64;
        let p = profiled.profile.expect("a profiled run has its profile");
        let share = |ns: u64| 100.0 * ns as f64 / wall_ns;
        let rest = wall_ns - (p.load_ns + p.factor_ns + p.refactor_ns + p.solve_ns) as f64;
        println!(
            "    profiled {:.1} ms: load {:.1} %, factor {:.1} % ({} at {:.1} us), refactor {:.1} % ({} at {:.1} us), solve {:.1} % ({:.2} us each over {} iterations), rest {:.1} %",
            wall_ns / 1e6,
            share(p.load_ns),
            share(p.factor_ns), p.factors, p.factor_ns as f64 / 1e3 / p.factors.max(1) as f64,
            share(p.refactor_ns), p.refactors, p.refactor_ns as f64 / 1e3 / p.refactors.max(1) as f64,
            share(p.solve_ns), p.solve_ns as f64 / 1e3 / profiled.stats.iterations.max(1) as f64, profiled.stats.iterations,
            100.0 * rest / wall_ns,
        );
    }
}
