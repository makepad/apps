//! Rounds (ESIMRT S5): a run cut into blocks that step apart and meet at every round's end, on
//! helper cores or not.

use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimDevice, CoSimPin, CoState, StepTo, Watch};
use fab_elec_sim::live::{Latest, LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{run_live, LiveSim, LiveSpec, Replan, Snapshots};
use fab_elec_sim::circuit::{Boundary, BoundaryKind, CtlKind};
use fab_elec_sim::rounds::{end_shift, seed_from, Block, Coupling, EndRecord, EndShift, Helpers, HolderPort, Lag, Planner, Post, RoundPlan, Warm, SHADOW_ACT, SHADOW_FIGURES};
use fab_elec_sim::analysis::partition::{Land, PartOpts};
use fab_elec_sim::analysis::round::{plan_blocks, plan_blocks_kept, warm_plan, BlockPlan, BlockPlanner, RoundMap};
use fab_elec_sim::analysis::states::StateCache;
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_sim::{GpioParams, HoldKind, ModelKind, PinMode, PinPull, Record, SimCircuit, SimDiag, Sink, SinkState, Solver, Source, Wave};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

const SAMPLE: f64 = 1.0 / 96e3;
const R: [f64; 4] = [390e3, 300e3, 240e3, 200e3];
const PARAMS: GpioParams = GpioParams { r_on_h: 25.0, r_on_l: 25.0, r_pull: 40e3, r_leak: 1e12, t_edge: 5e-9, vih: 0.55, vil: 0.45 };

/// A Schmitt-trigger RC oscillator (two hysteretic switches reading its input, R onto 10 nF) on a
/// supply of its own: it shares only ground with the others.
fn oscillator(c: &mut SimCircuit, k: usize) {
    let (i, o, v) = (format!("i{k}"), format!("o{k}"), format!("v{k}"));
    c.v(&format!("vs{k}"), &v, "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.0, tr: 10e-6, tf: 10e-6, pw: 1.0, per: 2.0 }));
    c.sw(&format!("slo{k}"), &o, "0", &i, "0", "lo", false);
    c.sw(&format!("shi{k}"), &v, &o, "0", &i, "hi", true);
    c.r(&format!("rf{k}"), &o, &i, R[k]);
    c.c(&format!("ct{k}"), &i, "0", 10e-9);
    c.c(&format!("co{k}"), &o, "0", 10e-12);
}

/// Block `k`'s circuit: block 0 a toggled GPIO pad and oscillator 0, block k oscillator k.
fn block_circuit(k: usize) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("lo", ModelKind::Sw, &[("vt", 2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    c.model("hi", ModelKind::Sw, &[("vt", -2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    if k == 0 {
        c.v("supply", "vdd", "0", Source::dc(3.3));
        // lowered as an input, as a processor holds its pads at reset: its first write drives it
        c.gpio("pa0", "pad", "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
        c.r("rload", "pad", "0", 10e3);
        c.c("cpad", "pad", "0", 1e-9);
    }
    oscillator(&mut c, k);
    c
}

/// The four blocks as one circuit.
fn whole() -> SimCircuit {
    let mut c = block_circuit(0);
    for k in 1..R.len() {
        oscillator(&mut c, k);
    }
    c
}

/// Blocks from their own circuits, with the run's options. Each solver is seeded from the run's
/// whole circuit by name; with `from`, its values and device states then come from there (the
/// four-block run's start), so a block run alone starts where it starts among the others.
#[derive(Clone)]
struct Split {
    blocks: Vec<usize>,
    from: Option<Solver>,
}

impl Planner for Split {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, _t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        for (n, &k) in self.blocks.iter().enumerate() {
            let mut c = block_circuit(k);
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let own = seed_from(&mut s, whole);
            if let Some(from) = &self.from {
                seed_from(&mut s, from);
            }
            plan.blocks.push(Block { name: format!("block {k}"), stage: u32::from(n > 0), own });
            solvers.push(s);
        }
        Ok(Some((plan, solvers)))
    }
}

/// Toggles pin 0 every 50 us from high at t = 0. With `leap`, its fourth run asks the analog
/// 1 ms ahead (`Bus::advance`) before it returns, past several round ends.
#[derive(Clone)]
struct Toggle {
    k: u64,
    leap: bool,
}

impl CoSim for Toggle {
    fn next(&self) -> f64 {
        self.k as f64 * 50e-6
    }
    // a process that forks, as firmware does: a run reading late can go back
    fn fork(&self) -> Option<Box<dyn CoSim>> {
        Some(Box::new(self.clone()))
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        assert!((bus.supply(0) - 3.3).abs() < 1e-9, "the pin's supply reads in the control block: {}", bus.supply(0));
        assert!(bus.write(0, if self.k % 2 == 0 { PinMode::OutHigh } else { PinMode::OutLow }), "the toggle writes its pin");
        if self.leap && self.k == 3 {
            let to = bus.t() + 1e-3;
            bus.advance(to)?;
            assert_eq!(bus.t(), to, "a nested advance reaches exactly its time");
        }
        self.k += 1;
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    fn arm(&mut self, _t: f64, _to: Option<StepTo>) -> bool {
        false
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

fn toggle(leap: bool) -> CoSimDevice {
    let pin = CoSimPin { name: "PA0".into(), gpio: "pa0".into(), pad: "pad".into(), vss: "0".into(), vdd: "vdd".into(), vih: PARAMS.vih, vil: PARAMS.vil };
    CoSimDevice { dev: Box::new(Toggle { k: 0, leap }), pins: vec![pin] }
}

/// Long-lived helper threads fed by a channel, as the host's task pool is.
fn pool(threads: usize) -> Post {
    let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..threads {
        let rx = rx.clone();
        thread::spawn(move || loop {
            let job = rx.lock().unwrap().recv();
            match job {
                Ok(job) => job(),
                Err(_) => return,
            }
        });
    }
    let tx = Mutex::new(tx);
    Arc::new(move |job| {
        let _ = tx.lock().unwrap().send(job);
    })
}

const EDIT: f64 = 4e-3;
const STOP: f64 = 16e-3;

/// An unpaced run's spec in plain rounds: steps of at most `max_step`, time resolution `res` (0
/// Exact, else Fast).
fn spec(max_step: f64, res: f64) -> LiveSpec {
    // plain rounds: a read a round late is one round length late (the pipeline's lags: pipeline.rs)
    LiveSpec { rate: f64::INFINITY, playing: false, max_step, res, trace: false, pipeline: false, ..LiveSpec::default() }
}

/// A run of `circuit` in the rounds of `planner` (or whole without one), with `co` attached, its
/// first plan made on the worker and `helpers` running the packs from there; halfway oscillator
/// `edit`'s rf (when there) halves. Returns the record and the run.
fn run(circuit: SimCircuit, planner: Option<Box<dyn Planner>>, co: Option<CoSimDevice>, edit: usize, spec: LiveSpec, helpers: Option<Helpers>, shuffle: Option<u64>) -> (Record, LiveSim) {
    let mut sim = LiveSim::new(circuit, ValuePlan::default(), 1, spec).expect("the operating point converges");
    if co.is_some() {
        sim.attach(co);
    }
    sim.set_planner(planner).expect("the plan installs");
    sim.set_helpers(helpers);
    sim.set_pack_shuffle(shuffle);
    sim.values(1.0);
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    for (stop, edit_now) in [(EDIT, true), (STOP, false)] {
        while sim.t() < stop {
            sim.advance(stop, None, &mut record).unwrap_or_else(|d| panic!("the blocks step: {d}"));
        }
        let device = format!("rf{edit}");
        if edit_now && sim.solver().find_device(&device).is_some() {
            assert!(sim.apply(LiveCmd::SetParam { generation: 1, device, key: "value".into(), value: R[edit] / 2.0 }), "the edit keeps the run");
        }
    }
    (record, sim)
}

/// `name`'s mean period over its rising crossings of `level` after `from`.
fn period(record: &Record, name: &str, level: f64, from: f64) -> f64 {
    let o = record.v(name);
    let ups: Vec<f64> = (1..record.at.len())
        .filter(|&n| record.at[n] > from && o[n - 1] < level && o[n] >= level)
        .map(|n| record.at[n - 1] + (record.at[n] - record.at[n - 1]) * (level - o[n - 1]) / (o[n] - o[n - 1]))
        .collect();
    assert!(ups.len() >= 3, "{name} swings after {from:e} s: rising edges at {ups:?}");
    (ups[ups.len() - 1] - ups[0]) / (ups.len() - 1) as f64
}

/// Oscillator `k`'s period for its rf at `r`: R C (ln(3.15/2.15) + ln(2.85/1.85)), its timing
/// node charging toward 5 V and discharging toward 0 between the switches' 1.85 V and 2.85 V.
fn rc_period(r: f64) -> f64 {
    r * 10e-9 * ((3.15f64 / 2.15).ln() + (2.85f64 / 1.85).ln())
}

/// Four blocks that share only ground: a toggled GPIO pad with an oscillator as the control
/// block, three more oscillators in the next stage, an edit to one of them halfway.
/// - The run is the same, bit for bit, on the worker alone, with one helper core, with two, and
///   with its stages packed at random: packing and cores never change a value in rounds.
/// - With helper cores from the start and the attached process yet to move its pad from its
///   lowered mode, the first plan waits for that, the run stepping whole: it is made on the worker
///   and goes in a `Replan::pins` after the pad's first write, sooner than the lead. Rounds asked
///   of a run stepping whole after its pad moved (the planner kept aside) plan at once.
/// - Each block steps exactly as its circuit alone does from the same state with the same round
///   ends: no block disturbs another, the merge carries each block's own points as they are, and
///   the edit reaches the block that has the resistor (its oscillator's period follows R C).
#[test]
fn blocks_in_rounds_step_alone_whatever_core_runs_them() {
    let start = LiveSim::new(whole(), ValuePlan::default(), 1, LiveSpec { max_step: SAMPLE, res: SAMPLE, ..LiveSpec::default() })
        .expect("the operating point converges")
        .solver()
        .clone();
    let all = || Some(Box::new(Split { blocks: (0..R.len()).collect(), from: None }) as Box<dyn Planner>);
    let (base, sim) = run(whole(), all(), Some(toggle(false)), 1, spec(SAMPLE, SAMPLE), None, None);
    assert_eq!(sim.blocks(), R.len(), "the run steps four blocks");
    let rounds = sim.stats().rounds;
    assert!(rounds as f64 >= STOP / (32.0 * SAMPLE), "{rounds} rounds of 32 samples over {STOP} s");
    for (name, helpers, shuffle) in [("one helper", 1, None), ("two helpers", 2, None), ("two helpers, packed at random", 2, Some(7))] {
        let (record, sim) = run(whole(), all(), Some(toggle(false)), 1, spec(SAMPLE, SAMPLE), Some(Helpers { post: pool(helpers), count: helpers }), shuffle);
        let st = sim.stats();
        eprintln!("{name}: {} rounds, {} packs on a helper, {} taken back by the worker", st.rounds, st.packs_helped, st.packs_claimed);
        let posted = st.packs_helped + st.packs_claimed;
        if shuffle.is_some() {
            assert_eq!(posted, rounds * helpers as u64, "{name}: every round posts a pack to each helper");
        } else {
            // by cost, a stage goes in no more packs than its cost over its largest block's
            assert!(posted <= rounds * helpers as u64 && st.packs_helped > 0, "{name}: every round posts at most a pack to each helper, and they step some ({posted} over {rounds} rounds)");
        }
        assert_eq!(record.at, base.at, "{name}: the same points as on the worker alone");
        assert!(record.x == base.x, "{name}: the same values as on the worker alone");
    }
    let lead = 2e-3;
    let inline: Post = Arc::new(|job| job());
    let mut sim = LiveSim::new(whole(), ValuePlan::default(), 1, LiveSpec { replan: Replan { lead, ..Replan::default() }, ..spec(SAMPLE, SAMPLE) }).expect("the operating point converges");
    sim.attach(Some(toggle(false)));
    sim.set_helpers(Some(Helpers { post: inline, count: 1 }));
    sim.set_planner(all()).expect("the first plan waits");
    assert_eq!((sim.blocks(), sim.stats().plans), (1, 0), "the first plan waits for the pad, the run on one stepper");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    while sim.t() < 2.0 * lead {
        sim.advance(2.0 * lead, None, &mut record).unwrap_or_else(|d| panic!("the run steps: {d}"));
    }
    let st = sim.stats();
    // the toggle's first write, high over the pad's lowered low, is at 0
    let due = Replan::default().pins;
    assert!(sim.blocks() == R.len() && (st.plans, st.plans_helped) == (1, 0) && st.last_plan >= due && st.last_plan < due + 2.0 * SAMPLE,
        "the first plan went in a pins after the pad's first write, made on the worker: {} blocks, {} plans, {} on a helper, in at {:e} s", sim.blocks(), st.plans, st.plans_helped, st.last_plan);
    // rounds asked of a run stepping whole, its planner kept aside (as the host asks them of a run
    // it measured behind): the planner saw the pad move, so the plan is made and in at once
    let mut sim = LiveSim::new(whole(), ValuePlan::default(), 1, LiveSpec { replan: Replan { lead, ..Replan::default() }, ..spec(SAMPLE, SAMPLE) }).expect("the operating point converges");
    sim.attach(Some(toggle(false)));
    sim.set_helpers(Some(Helpers { post: Arc::new(|job| job()), count: 1 }));
    sim.set_rounds(false).expect("rounds off");
    sim.set_planner(all()).expect("the planner is kept aside");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    advance(&mut sim, lead, &mut record);
    assert_eq!((sim.blocks(), sim.stats().plans), (1, 0), "no plan while rounds are off");
    sim.set_rounds(true).expect("the plan installs");
    assert_eq!((sim.blocks(), sim.stats().plans), (R.len(), 1), "rounds asked after the pad moved: the plan is in at once");
    for k in 0..R.len() {
        let (alone, _) = run(block_circuit(k), Some(Box::new(Split { blocks: vec![k], from: Some(start.clone()) })), (k == 0).then(|| toggle(false)), 1, spec(SAMPLE, SAMPLE), None, None);
        let mut checked = 0;
        for (n, &t) in alone.at.iter().enumerate() {
            let m = base.at.binary_search_by(|a| a.total_cmp(&t)).unwrap_or_else(|_| panic!("block {k}: its point at {t:e} s is in the merged stream"));
            for (u, name) in alone.names.iter().enumerate().skip(1) {
                let (a, b) = (alone.x[n][u], base.x[m][base.index(name)]);
                assert_eq!(a.to_bits(), b.to_bits(), "block {k}: {name} at {t:e} s is {b} among the blocks, {a} alone");
                checked += 1;
            }
        }
        assert!(checked > 1000, "block {k}: {checked} values compared");
    }
    for k in 0..R.len() {
        let want = rc_period(if k == 1 { R[1] / 2.0 } else { R[k] });
        let got = period(&base, &format!("o{k}"), 2.5, EDIT + 2e-3);
        assert!((got / want - 1.0).abs() < 1e-2, "oscillator {k}: period {got:e} s after the edit, R C (ln(3.15/2.15) + ln(2.85/1.85)) {want:e} s");
    }
}

/// A listener at `out`: a VCCS reading `src` (without loading it) into 10 k and 1 nF, unity gain
/// behind 10 us.
fn listener(c: &mut SimCircuit, name: &str, src: &str, out: &str) {
    c.vctl(&format!("g{name}"), CtlKind::Vccs, "0", out, src, "0", 1e-4);
    c.r(&format!("r{name}"), out, "0", 10e3);
    c.c(&format!("c{name}"), out, "0", 1e-9);
}

/// The pad and oscillator 0, a listener `l` on its timing node and one `p` on the pad, an echo
/// `e` listening to `l`, and oscillator 1 apart with a 1 mA load on the pad's supply.
fn coupled() -> SimCircuit {
    let mut c = block_circuit(0);
    oscillator(&mut c, 1);
    c.r("rvdd", "vdd", "0", 3.3e3);
    listener(&mut c, "l", "i0", "l");
    listener(&mut c, "p", "pad", "p");
    listener(&mut c, "e", "l", "e");
    c
}

/// `coupled` in three blocks, in the S4 form (block 0 takes the other blocks' currents a round
/// late; they read its voltages in the same round): block 0 the pad, oscillator 0 and the echo's
/// load, taking the echo's drive current a round late; block 1 the listeners and the echo's
/// drive, cut at block 0's node `e`, reading i0, the pad and `e` through boundary sources in the
/// same round; block 2 oscillator 1 and the supply's load, with its own copy of the supply. A block's boundary node is the exporter's to show; the supply's current is the sum of
/// its copies'.
#[derive(Clone)]
struct Cut;

impl Planner for Cut {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, _t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let boundary = |c: &mut SimCircuit, name: &str, node: &str| {
            let v0 = whole.x[whole.unknown(node).expect("the cut node is in the whole circuit") as usize];
            let node = c.node(node);
            c.devices.boundary.push(Boundary { name: name.into(), node, kind: BoundaryKind::Voltage, v0 });
        };
        let mut b0 = block_circuit(0);
        b0.r("re", "e", "0", 10e3);
        b0.c("ce", "e", "0", 1e-9);
        let drive = 1e-4 * whole.x[whole.unknown("l").expect("the listener's node") as usize];
        let node = b0.node("e");
        b0.devices.boundary.push(Boundary { name: "ie".into(), node, kind: BoundaryKind::Current, v0: drive });
        let mut b1 = SimCircuit::new();
        boundary(&mut b1, "bi", "i0");
        listener(&mut b1, "l", "i0", "l");
        boundary(&mut b1, "bp", "pad");
        listener(&mut b1, "p", "pad", "p");
        boundary(&mut b1, "be", "e");
        b1.vctl("ge", CtlKind::Vccs, "0", "e", "l", "0", 1e-4);
        let mut b2 = block_circuit(1);
        b2.v("supply", "vdd", "0", Source::dc(3.3));
        b2.r("rvdd", "vdd", "0", 3.3e3);
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        for (k, (mut c, cut)) in [(b0, &[][..]), (b1, &["i0", "pad", "e"][..]), (b2, &["vdd", "supply#branch"][..])].into_iter().enumerate() {
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let mut own = seed_from(&mut s, whole);
            own.retain(|&(l, _)| !cut.contains(&s.unknowns[l as usize].name.as_str()));
            plan.blocks.push(Block { name: format!("block {k}"), stage: u32::from(k > 0), own });
            solvers.push(s);
        }
        let i0 = solvers[0].unknown("i0").expect("block 0 solves i0");
        let pad = solvers[0].unknown("pad").expect("block 0 solves the pad");
        let e = solvers[0].unknown("e").expect("block 0 solves e");
        let drive = solvers[1].unknown("be#branch").expect("block 1 holds e through its boundary");
        plan.couplings.push(Coupling { from: (0, i0), to: vec![(1, "bi".into())], lag: Lag::Same, cuts: Vec::new(), rho: 0.0 });
        plan.couplings.push(Coupling { from: (0, pad), to: vec![(1, "bp".into())], lag: Lag::Same, cuts: Vec::new(), rho: 0.0 });
        plan.couplings.push(Coupling { from: (0, e), to: vec![(1, "be".into())], lag: Lag::Same, cuts: Vec::new(), rho: 0.0 });
        plan.couplings.push(Coupling { from: (1, drive), to: vec![(0, "ie".into())], lag: Lag::Round, cuts: Vec::new(), rho: 0.0 });
        let share = |k: usize| solvers[k].unknown("supply#branch").expect("each copy of the supply has a branch");
        plan.sums.push((whole.unknown("supply#branch").expect("the supply has a branch"), vec![(0, share(0)), (2, share(2))]));
        Ok(Some((plan, solvers)))
    }
}

/// `v` over the times `at`, linear between them, at `t`.
fn lerp(at: &[f64], v: &[f64], t: f64) -> f64 {
    let n = at.partition_point(|&a| a < t).clamp(1, at.len() - 1);
    v[n - 1] + (v[n] - v[n - 1]) * (t - at[n - 1]) / (at[n] - at[n - 1])
}

/// The largest difference of `name` in `run` from `name` in `truth` `lag` earlier, both read
/// linearly between their points on a grid of an eighth of a sample from `from` to the stop: what
/// a reader of either stream sees. `truth` is dense: its own reading is far finer than `run`'s.
fn deviation(run: &Record, truth: &Record, name: &str, lag: f64, from: f64) -> f64 {
    let (r, w) = (run.v(name), truth.v(name));
    let n = ((STOP - from) / (SAMPLE / 8.0)) as usize;
    (0..=n).map(|k| from + k as f64 * SAMPLE / 8.0).map(|t| (lerp(&run.at, &r, t) - lerp(&truth.at, &w, t - lag)).abs()).fold(0.0, f64::max)
}

/// Blocks fed through boundary sources, in the S4 form: listeners read oscillator 0 and the GPIO
/// pad in the same round (exact edges: a VCCS does not load what it reads), the control block
/// takes the echo's drive current from the listeners' block one round late, and a supply is
/// copied into the block that loads it; the firmware leaps 1 ms inside one of its runs.
/// - The run is the same, bit for bit, on the worker alone, with one helper core, with two, and
///   packed at random: the tables a block is fed are fixed before its stage runs.
/// - In Exact, and in Fast at the live settings, the listeners follow a fine run of the whole
///   circuit as closely as the whole circuit does at the blocks' settings (within twice its
///   error in Exact, three times in Fast), the pad's edges included; the echo follows it one
///   round length later, right in size and not in step with it: a lagged read is exactly one
///   round length back. The supply's current is the sum of its copies' shares.
/// - Halving oscillator 0's resistor in block 0 is heard across both cuts: the listener and the
///   echo swing at the new R C period.
/// - An event crosses a cut once: block 1 lands on block 0's edges, writes and crossings, and
///   none comes back through the round-late drive (corner landings at most twice the whole's
///   corners and changes, plus the crossings).
/// - Rounds turn off (one stepper, a planner given meanwhile waiting) and on again (its blocks)
///   while the run goes on.
/// - Rounds never pass the round length, the leap included (the run asserts it).
#[test]
fn boundary_feeds_carry_values_across_blocks_in_rounds() {
    let round = 32.0 * SAMPLE;
    let cut = || Some(Box::new(Cut) as Box<dyn Planner>);
    let (base, mut sim) = run(coupled(), cut(), Some(toggle(true)), 0, spec(SAMPLE, SAMPLE), None, None);
    assert_eq!(sim.blocks(), 3, "the run steps three blocks");
    let rounds = sim.stats().rounds;
    assert!(rounds as f64 >= STOP / round, "{rounds} rounds of 32 samples over {STOP} s");
    // Rounds off steps the whole circuit on, a planner given meanwhile waits, and rounds on plans
    // with it again; the run goes on through both.
    let mut sink = Record::default();
    sink.begin(&sim.solver().unknowns);
    sim.apply(LiveCmd::SetRounds(false));
    assert_eq!(sim.blocks(), 1, "rounds off: one stepper");
    sim.set_planner(cut()).expect("a planner waits while rounds are off");
    let at = sim.t() + 4.0 * round;
    while sim.t() < at {
        sim.advance(at, None, &mut sink).unwrap_or_else(|d| panic!("the whole circuit steps: {d}"));
    }
    assert_eq!((sim.blocks(), sim.stats().rounds), (1, rounds), "no rounds while they are off");
    sim.apply(LiveCmd::SetRounds(true));
    assert_eq!(sim.blocks(), 3, "rounds on: the planner's three blocks");
    let at = sim.t() + 4.0 * round;
    while sim.t() < at {
        sim.advance(at, None, &mut sink).unwrap_or_else(|d| panic!("the blocks step: {d}"));
    }
    assert!(sim.stats().rounds > rounds, "rounds again once on");
    for (name, helpers, shuffle) in [("one helper", 1, None), ("two helpers", 2, None), ("two helpers, packed at random", 2, Some(7))] {
        let (record, _) = run(coupled(), cut(), Some(toggle(true)), 0, spec(SAMPLE, SAMPLE), Some(Helpers { post: pool(helpers), count: helpers }), shuffle);
        assert_eq!(record.at, base.at, "{name}: the same points as on the worker alone");
        assert!(record.x == base.x, "{name}: the same values as on the worker alone");
    }
    // Against a fine run of the whole circuit, beside the whole circuit at the blocks' own
    // settings, in Exact and in Fast at the live settings: splitting may cost at most twice the
    // whole circuit's own error in Exact (and 0.1 mV), a round-late read included. In Fast each
    // block resolves its events to the sample on its own, so a value read across a cut carries
    // the exporter's error and the importer's: up to twice the whole's, three times allowed.
    let at = |reltol: f64| {
        let mut c = coupled();
        c.opts.reltol = reltol;
        c
    };
    let (truth, _) = run(at(1e-5), None, Some(toggle(true)), 0, spec(SAMPLE / 16.0, 0.0), None, None);
    for (mode, res, reltol, times) in [("Exact", 0.0, 1e-4, 2.0), ("Fast", SAMPLE, coupled().opts.reltol, 3.0)] {
        let (whole, whole_sim) = run(at(reltol), None, Some(toggle(true)), 0, spec(SAMPLE, res), None, None);
        let (split, split_sim) = run(at(reltol), cut(), Some(toggle(true)), 0, spec(SAMPLE, res), None, None);
        // Each of the pad's corners and writes (the whole's corner and change restarts) is landed
        // on at most twice, by block 0 and by block 1, and each of block 0's crossings once, by
        // block 1: a landing on an imported event is not sent back, or every edge would return
        // through the round-late drive a round later, every round.
        let (w, s) = (whole_sim.restarts(), split_sim.restarts());
        eprintln!("{mode}: {} corner landings among the blocks; whole {} corners, {} changes; {} crossings among the blocks", s.corner, w.corner, w.change, s.crossing);
        assert!(s.corner <= 2 * (w.corner + w.change) + s.crossing,
            "{mode}: {} corner landings among the blocks against the whole's {} corners and {} changes and {} crossings: an imported event came back", s.corner, w.corner, w.change, s.crossing);
        for (name, what, lag) in [("l", "the listener on oscillator 0", 0.0), ("p", "the listener on the pad", 0.0), ("e", "the echo, one round late", round)] {
            let (got, own) = (deviation(&split, &truth, name, lag, lag), deviation(&whole, &truth, name, 0.0, 0.0));
            eprintln!("{mode}: {what}: {got:e} V from the fine run among the blocks, {own:e} V whole");
            assert!(got <= times * own + 1e-4, "{mode}: {what} follows the fine run within {times} times the whole circuit's error (and 0.1 mV): {got:e} V, whole {own:e} V");
        }
        let unlagged = deviation(&split, &truth, "e", 0.0, round);
        assert!(unlagged > 0.1, "{mode}: the echo is not in step with the fine run's: {unlagged:e} V");
        let (s, w) = (split.v("supply#branch"), truth.v("supply#branch"));
        // a sample past the pad's edges: in Fast the firmware's writes land on the next sample
        let rail = split.at.iter().zip(&s).filter(|&(&t, _)| (20e-6..45e-6).contains(&(t % 50e-6))).map(|(&t, &v)| (v - lerp(&truth.at, &w, t)).abs()).fold(0.0, f64::max);
        eprintln!("{mode}: the supply's current {rail:e} A from the fine run's between the pad's edges");
        assert!(rail < 1e-5, "{mode}: the supply's current is the sum of its copies' shares, block 2's 1 mA load and block 0's pad (between the pad's edges): {rail:e} A off");
    }
    let want = rc_period(R[0] / 2.0);
    for name in ["o0", "l", "e"] {
        let got = period(&base, name, if name == "o0" { 2.5 } else { 2.35 }, EDIT + 2e-3);
        assert!((got / want - 1.0).abs() < 1e-2, "{name}: period {got:e} s after oscillator 0's edit, its R C period {want:e} s");
    }
}

/// Two more pads on block 0's supply, `pb` and `pc`, each with a load.
fn pads(c: &mut SimCircuit) {
    for p in ["pb", "pc"] {
        c.gpio(&format!("{p}0"), p, "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
        c.r(&format!("rl{p}"), p, "0", 10e3);
        c.c(&format!("cl{p}"), p, "0", 1e-9);
    }
}

/// Firmware's start: its three pads set high 20 us apart from t = 0, then nothing.
struct Setup {
    k: u32,
}

impl CoSim for Setup {
    fn next(&self) -> f64 {
        if self.k < 3 { f64::from(self.k) * 20e-6 } else { f64::INFINITY }
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        assert!(bus.write(self.k, PinMode::OutHigh), "the firmware sets its pad");
        self.k += 1;
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    fn arm(&mut self, _t: f64, _to: Option<StepTo>) -> bool {
        false
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

fn setup() -> CoSimDevice {
    let pin = |name: &str, gpio: &str, pad: &str| CoSimPin { name: name.into(), gpio: gpio.into(), pad: pad.into(), vss: "0".into(), vdd: "vdd".into(), vih: PARAMS.vih, vil: PARAMS.vil };
    CoSimDevice { dev: Box::new(Setup { k: 0 }), pins: vec![pin("PA0", "pa0", "pad"), pin("PB0", "pb0", "pb"), pin("PC0", "pc0", "pc")] }
}

/// One stepper at the start (an MCU's pads are as lowered), then the pads with oscillator 0 as
/// block 0 and each other oscillator a block; with `rename`, the blocks of a re-plan after that
/// named anew: the same blocks, not the same cut (`RoundPlan::same_cut`).
#[derive(Clone)]
struct Later {
    calls: u32,
    rename: bool,
}

impl Planner for Later {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, _t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        self.calls += 1;
        if self.calls == 1 {
            return Ok(None);
        }
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        for k in 0..R.len() {
            let mut c = block_circuit(k);
            if k == 0 {
                pads(&mut c);
            }
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let own = seed_from(&mut s, whole);
            let name = if self.rename && self.calls > 2 { format!("kept {k}") } else { format!("block {k}") };
            plan.blocks.push(Block { name, stage: u32::from(k > 0), own });
            solvers.push(s);
        }
        Ok(Some((plan, solvers)))
    }
}

fn advance(sim: &mut LiveSim, to: f64, record: &mut Record) {
    while sim.t() < to {
        sim.advance(to, None, record).unwrap_or_else(|d| panic!("the run steps: {d}"));
    }
}

/// The first warm reports its simulated time and stays out until the proof sees the worker hold.
/// The planning call then cuts with Later, keeps its one stepper or returns the given error.
#[derive(Clone)]
struct StartPlanner {
    posted: mpsc::Sender<f64>,
    ready: Arc<AtomicBool>,
    planner: Result<Later, SimDiag>,
}

impl Planner for StartPlanner {
    fn warm(&self, _ckt: &Arc<SimCircuit>, _whole: &Solver, t: f64) -> Option<Warm> {
        self.posted.send(t).expect("the proof receives the warm's time");
        Some(Warm::new(self.ready.clone(), Arc::new(AtomicBool::new(false))))
    }
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        match &mut self.planner {
            Ok(planner) => planner.plan(ckt, whole, t),
            Err(d) => Err(d.clone()),
        }
    }
}

/// A sink that would hold until playback starts. Its last point says where it heard each start.
#[derive(Default)]
struct StartSink {
    t: f64,
    starts: Vec<f64>,
}

impl Sink for StartSink {
    fn point(&mut self, at: f64, _outer: Option<f64>, _x: &[f64]) { self.t = at; }
    fn holds(&mut self) -> bool { self.starts.is_empty() }
    fn started(&mut self) { self.starts.push(self.t); }
}

/// A run with a planner plans again once the firmware has set its pads and once a storm of edits
/// is over: each burst gives one plan, made from the run where it fell due and going in a fixed
/// simulated time later. On one stepper (the pads' writes) it falls due after the burst's first
/// write; in rounds (the storm), after its last edit.
/// - Exact: the run is the same, bit for bit, whether its plans were made on the worker, posted
///   (by a post that runs each job as it is posted), or on helper cores (whichever core starts a
///   posted plan: one no helper has started by its time the worker makes itself).
/// - The storm's last edit is heard at once and through the new plan: oscillator 1's period
///   follows its last rf.
/// - The storm's plan cuts the run as it already runs (the same four blocks): the blocks run on,
///   so the run is the same, bit for bit, as one that never makes that plan.
/// - The storm's plan with its blocks named anew (not the cut the run runs, so no whole keep): each
///   of its blocks is a running one and runs on, the control block with the attached process, so
///   the run is the same, bit for bit, as the one that keeps the cut whole.
/// - A heard Fast run measures its set-up whole. It starts in rounds if slow or asked, holding
///   at the warm's point through the planning call even when the lead expires with no pad moved.
///   With headroom it starts whole while the warm is out. A planner keeping one stepper or failing
///   also releases playback there. Each start is said once. An unheard run never prepares.
#[test]
fn a_burst_of_pad_writes_or_edits_gives_one_plan() {
    let replan = Replan { edit: 1e-3, pins: 0.5e-3, lead: 0.5e-3, slow: 0.0 };
    let storm = [2.0e-3, 2.2e-3, 2.4e-3, 2.6e-3, 2.8e-3, 3.0e-3];
    let (stop, round) = (8e-3, 32.0 * SAMPLE);
    let mut runs = Vec::new();
    let inline: Post = Arc::new(|job| job());
    let never = f64::INFINITY;
    let renamed = "with the storm's blocks named anew";
    for (name, slow, post, edit) in [("on the worker", never, pool(2), replan.edit), ("posted", -1.0, inline, replan.edit), ("on helper cores", -1.0, pool(2), replan.edit), ("with no plan after the storm", never, pool(2), never), (renamed, never, pool(2), replan.edit)] {
        let mut c = whole();
        pads(&mut c);
        let spec = LiveSpec { replan: Replan { slow, edit, ..replan.clone() }, ..spec(SAMPLE, 0.0) };
        let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the operating point converges");
        sim.attach(Some(setup()));
        // its plans and checks on the same cores (`set_checks`), as a host gives its background jobs
        let helpers = Helpers { post, count: 2 };
        sim.set_helpers(Some(helpers.clone()));
        sim.set_checks(Some(helpers), false);
        sim.set_planner(Some(Box::new(Later { calls: 0, rename: name == renamed }))).expect("the plan installs");
        sim.values(1.0);
        assert_eq!(sim.blocks(), 1, "{name}: one stepper while the pads are as lowered");
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        advance(&mut sim, storm[0], &mut record);
        let st = sim.stats();
        assert_eq!((st.plans, sim.blocks()), (2, R.len()), "{name}: the pads' three writes give one plan, of four blocks");
        // on one stepper the first write sets when: the last (40 us) does not push it on
        let due = replan.pins + replan.lead;
        assert!(st.last_plan >= due && st.last_plan < 40e-6 + due, "{name}: the pads' plan went in at {:e} s, {due:e} s after the first write", st.last_plan);
        for (n, &at) in storm.iter().enumerate() {
            advance(&mut sim, at, &mut record);
            let value = if n % 2 == 1 { R[1] / 2.0 } else { R[1] };
            assert!(sim.apply(LiveCmd::SetParam { generation: 1, device: "rf1".into(), key: "value".into(), value }), "the edit keeps the run");
        }
        let last = sim.t();
        advance(&mut sim, stop, &mut record);
        let st = sim.stats();
        eprintln!("{name}: {} plans, {} on a helper, the last in at {:e} s", st.plans, st.plans_helped, st.last_plan);
        assert!(st.plan_error.is_none(), "{name}: {:?}", st.plan_error);
        if edit == never {
            assert_eq!((st.plans, st.plans_kept), (2, 0), "{name}: no plan after the pads'");
        } else {
            assert_eq!((st.plans, sim.blocks()), (3, R.len()), "{name}: the storm of six edits gives one plan");
            if name == renamed {
                assert_eq!((st.plans_kept, st.blocks_kept), (0, R.len() as u64), "{name}: the storm's plan is a new cut of the running blocks: each runs on");
            } else {
                assert_eq!((st.plans_kept, st.blocks_kept), (1, 0), "{name}: the storm's plan cuts the run as it runs: its blocks run on");
            }
            let due = last + replan.edit + replan.lead;
            assert!(st.last_plan >= due && st.last_plan <= due + 2.0 * round, "{name}: the storm's plan was kept at {:e} s, {due:e} s after its last edit", st.last_plan);
        }
        match name {
            "on the worker" => assert_eq!(st.plans_helped, 0, "{name}: every plan was made on the worker"),
            "posted" => assert_eq!(st.plans_helped, 2, "{name}: both plans went the helper's way"),
            _ => {}
        }
        let (got, want) = (period(&record, "o1", 2.5, last + 0.2e-3), rc_period(R[1] / 2.0));
        assert!((got / want - 1.0).abs() < 1e-2, "{name}: oscillator 1's period {got:e} s through the plan, R C (ln(3.15/2.15) + ln(2.85/1.85)) {want:e} s");
        runs.push(record);
    }
    for (name, record) in ["posted", "on helper cores", "with no plan after the storm", renamed].iter().zip(&runs[1..]) {
        assert_eq!(record.at, runs[0].at, "{name}: the same points as the run with its plans made on the worker");
        assert!(record.x == runs[0].x, "{name}: the same values as the run with its plans made on the worker");
    }

    let (pins, lead) = (50e-6, 120e-6);
    let (large, tiny) = (1e9, 1e-6);
    let mut quiet = setup();
    quiet.dev = Box::new(Setup { k: 3 }); // attached, with lowered pins, and no write due
    // Later with one call behind it cuts four blocks; on its first call it chooses one stepper.
    for (name, rate, co, due, asked, calls, heard) in [
        ("slow whole after pad writes", large, Some(setup()), pins, false, Ok(1), true),
        ("whole with headroom", tiny, Some(setup()), pins, false, Ok(1), true),
        ("rounds asked, no pad moved by the lead", tiny, Some(quiet), lead, true, Ok(1), true),
        ("planner keeps one stepper", large, Some(setup()), pins, false, Ok(0), true),
        ("planner fails, no attached process", large, None, pins, false, Err("first plan failed"), true),
        ("unheard", large, Some(setup()), pins, false, Ok(1), false),
    ] {
        let mut c = whole();
        pads(&mut c);
        let until = 3.0 * lead;
        let spec = LiveSpec { heard, rate, playing: true, until, replan: Replan { pins, lead, ..replan.clone() }, ..spec(SAMPLE, SAMPLE) };
        let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the pads' operating point converges");
        sim.attach(co);
        let helpers = Helpers { post: Arc::new(|job| job()), count: 2 };
        sim.set_helpers(Some(helpers.clone()));
        sim.set_checks(Some(helpers), true);
        sim.set_rounds(false).expect("the planner's slot is parked");
        let (posted, warm) = mpsc::channel();
        let ready = Arc::new(AtomicBool::new(false));
        let planner = calls.map(|calls| Later { calls, rename: false }).map_err(|msg| SimDiag::setup(msg.into()));
        sim.set_planner(Some(Box::new(StartPlanner { posted, ready: ready.clone(), planner }))).expect("the planner waits in its slot");
        assert_eq!(sim.preparing(), heard, "{name}: only the heard run prepares at its operating point");
        let mut sink = StartSink::default();
        if !heard {
            while sim.t() < lead {
                sim.advance(lead, None, &mut sink).expect("the unheard run steps across the warm");
            }
            assert!(!sim.preparing() && sink.starts.is_empty() && sim.stats().plans == 0, "{name}: the warm never starts preparation or calls the parked planner");
            continue;
        }
        let (tx, cmds) = mpsc::channel();
        if asked { tx.send(LiveCmd::SetRounds(true)).unwrap(); }
        let values = Latest::new();
        values.keep_all(true);
        let (mut traces, _) = mpsc::sync_channel(0);
        let mut sim = run_live(sim, &cmds, &values, &mut traces, &mut sink, &|| {
            let Some((_, v)) = values.latest() else { return };
            if v.started || !v.running || v.held == Some(HoldKind::Other) {
                tx.send(LiveCmd::Stop).unwrap();
            } else if v.held == Some(HoldKind::Plan) {
                ready.store(true, Ordering::Release);
                tx.send(LiveCmd::Sinks).unwrap(); // wake the hold without waiting on the wall
            }
        }).expect("the worker returns the run at its start");
        let at = warm.try_recv().expect("the parked planner posted its warm");
        assert!(warm.try_recv().is_err() && at >= due && at < due + lead, "{name}: the warm posts once after {due:e}, without waiting a second lead, got {at:e}");
        if due == pins {
            assert!(at < lead, "{name}: the short measurement posts the warm before the fallback lead");
        }
        let mut publications = Vec::new();
        values.take_kept(&mut publications);
        let holds: Vec<_> = publications.iter().filter(|v| !v.started).filter_map(|v| v.held.map(|kind| (kind, v.t))).collect();
        let plan = rate == large || asked;
        let expected_hold = plan.then_some((HoldKind::Plan, at));
        assert_eq!(holds.as_slice(), expected_hold.as_slice(), "{name}: only a requested or needed plan holds, at the warm's point");
        assert_eq!(ready.load(Ordering::Acquire), plan, "{name}: a run starting whole with headroom leaves its warm out");
        assert_eq!(publications.iter().filter(|v| v.started).map(|v| v.t).collect::<Vec<_>>(), [at], "{name}: values say started once where the warm was posted");
        assert!(!sim.preparing() && sim.t() == at, "{name}: set-up and planning have ended without stepping past the warm");
        assert_eq!(sim.stats().plans, u64::from(plan), "{name}: the planning call was made exactly when requested or needed");
        if plan {
            assert_eq!(sim.stats().last_plan, at, "{name}: the first planning call finished at the held time, without a second lead");
        }
        assert_eq!(sim.stats().plan_error.as_ref().map(|d| d.message.as_str()), calls.err(), "{name}: only a failed planning call reports its error");
        assert_eq!(sim.blocks(), if plan && calls == Ok(1) { R.len() } else { 1 }, "{name}: a cut starts in rounds, headroom, one stepper by choice and a failure start whole");
        // Step past a second lead: a first_due left behind would announce the start again there.
        sim.apply(LiveCmd::SetRate(f64::INFINITY));
        while sim.t() < until {
            sim.advance(until, None, &mut sink).expect("playback steps past a second lead");
        }
        assert_eq!(sink.starts, [at], "{name}: the sink hears started once at the warm's point, even past a second lead");
        assert!(!sim.values(0.0).started, "{name}: stepping past a second lead never publishes another start");
    }
}

/// Four blocks at first, then each re-plan the other cut: oscillators 0 and 1 in one block, or
/// all four apart, so no re-plan is kept whole; oscillators 2 and 3 are blocks of both cuts, and
/// run on. With `apart`, every plan has all four apart: each re-plan the cut the run runs. A
/// re-plan says it has started on `started`, waits for the test to let it go on `gate`, takes
/// `late` of wall time, as a helper core's plan on a cold start does, and says it is done on
/// `done`.
#[derive(Clone)]
struct Turns {
    calls: u32,
    apart: bool,
    late: Duration,
    started: Option<mpsc::Sender<()>>,
    gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
    done: Option<mpsc::Sender<()>>,
}

impl Planner for Turns {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, _t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        self.calls += 1;
        if self.calls > 1 {
            if let Some(started) = &self.started {
                let _ = started.send(());
            }
            if let Some(gate) = &self.gate {
                let _ = gate.lock().unwrap().recv();
            }
            thread::sleep(self.late);
        }
        let groups: Vec<Vec<usize>> = if self.calls % 2 == 0 && !self.apart { vec![vec![0, 1], vec![2], vec![3]] } else { (0..R.len()).map(|k| vec![k]).collect() };
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        for (n, group) in groups.iter().enumerate() {
            let mut c = block_circuit(group[0]);
            for &k in &group[1..] {
                oscillator(&mut c, k);
            }
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let own = seed_from(&mut s, whole);
            plan.blocks.push(Block { name: format!("block {n}"), stage: u32::from(n > 0), own });
            solvers.push(s);
        }
        if let Some(done) = self.done.as_ref().filter(|_| self.calls > 1) {
            let _ = done.send(());
        }
        Ok(Some((plan, solvers)))
    }
}

/// In a run nobody hears, a re-plan goes in at the first round start at or after its time in
/// Fast as in Exact, whatever core makes it and however long it takes: a helper's plan not back
/// by then is waited for (`LiveStats::plan_waits`), so a take's bits follow from its inputs alone.
/// Two edits each give a re-plan of the other cut. Made on the worker, on helper cores, and on
/// helper cores that start each at once and take 50 ms of wall time for it (far past the 0.5 ms
/// lead the run steps meanwhile), the run is the same, bit for bit, and every re-plan went in at
/// its time. Oscillators 2 and 3 are blocks of both cuts: through each re-cut they run on (their
/// histories whole), the same, bit for bit, as in a run whose re-plans keep the cut it runs.
#[test]
fn a_late_plan_goes_in_at_its_time_in_fast_too() {
    let replan = Replan { edit: 0.5e-3, pins: 0.5e-3, lead: 0.5e-3, slow: -1.0 };
    let (edits, stop) = ([1.5e-3, 3.5e-3], 6e-3);
    let mut runs = Vec::new();
    let mut apart_run = None;
    for (name, helpers, late, apart) in [("on the worker", false, 0, false), ("on helper cores", true, 0, false), ("on helper cores, late", true, 50, false), ("never cut anew", false, 0, true)] {
        let mut sim = LiveSim::new(whole(), ValuePlan::default(), 1, LiveSpec { replan: replan.clone(), ..spec(SAMPLE, SAMPLE) }).expect("the operating point converges");
        // the late helpers' post returns once a helper has started the plan: the worker meets
        // the plan's time with it in the making (they make plans only: a pack would never say so)
        let (started, on_helper) = mpsc::channel();
        let on_helper = Mutex::new(on_helper);
        let cores = pool(2);
        let post: Post = if late > 0 {
            Arc::new(move |job| {
                cores(job);
                on_helper.lock().unwrap().recv().expect("a helper starts the plan");
            })
        } else {
            cores
        };
        // its plans and checks on the same cores (`set_checks`), as a host gives its background jobs
        let given = helpers.then(|| Helpers { post, count: if late > 0 { 0 } else { 2 } });
        sim.set_helpers(given.clone());
        sim.set_checks(given, false);
        sim.set_planner(Some(Box::new(Turns { calls: 0, apart, late: Duration::from_millis(late), started: (late > 0).then_some(started), gate: None, done: None }))).expect("the first plan installs");
        let round = sim.block_work().expect("the first plan is in").1;
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        let mut went_in = Vec::new();
        for (n, &at) in edits.iter().enumerate() {
            advance(&mut sim, at, &mut record);
            let at = sim.t();
            assert!(sim.apply(LiveCmd::SetParam { generation: 1, device: "rf1".into(), key: "value".into(), value: R[1] / (n + 2) as f64 }), "{name}: the edit keeps the run");
            let due = at + replan.edit + replan.lead;
            advance(&mut sim, due + 2.0 * round, &mut record);
            let t = sim.stats().last_plan;
            assert!(t >= due && t <= due + 2.0 * round, "{name}: re-plan {n} went in at {t:e} s, within two rounds of {due:e} s");
            assert_eq!(sim.blocks(), if n == 0 && !apart { 3 } else { 4 }, "{name}: re-plan {n} cut the run its own way");
            went_in.push(t);
        }
        advance(&mut sim, stop, &mut record);
        let st = sim.stats();
        eprintln!("{name}: {} plans, {} on a helper, {} waited for ({:?} in all)", st.plans, st.plans_helped, st.plan_waits, st.plan_wait_wall);
        assert!(st.plan_error.is_none(), "{name}: {:?}", st.plan_error);
        if apart {
            assert_eq!((st.plans, st.plans_kept, st.blocks_kept), (3, 2, 0), "{name}: the first plan and one re-plan an edit, each the cut the run runs");
            apart_run = Some(record);
            continue;
        }
        assert_eq!((st.plans, st.plans_kept, st.blocks_kept), (3, 0, 4), "{name}: the first plan and one re-plan an edit, each going in and keeping oscillators 2 and 3's blocks");
        match (helpers, late) {
            (false, _) => assert_eq!((st.plans_helped, st.plan_waits), (0, 0), "{name}: every plan was made on the worker"),
            (true, 0) => {}
            _ => assert_eq!((st.plans_helped, st.plan_waits), (2, 2), "{name}: both re-plans were made on a helper, late, and waited for"),
        }
        runs.push((name, went_in, record));
    }
    let (_, went_in, first) = &runs[0];
    for (name, at, record) in &runs[1..] {
        assert_eq!(at, went_in, "{name}: the re-plans went in where they did with the plans made on the worker");
        assert_eq!(record.at, first.at, "{name}: the same points as with the plans made on the worker");
        assert!(record.x == first.x, "{name}: the same values as with the plans made on the worker");
    }
    let apart = apart_run.expect("the run never cut anew");
    let mut checked = 0;
    for (n, &t) in apart.at.iter().enumerate() {
        let Ok(m) = first.at.binary_search_by(|a| a.total_cmp(&t)) else { continue };
        for (u, name) in apart.names.iter().enumerate().filter(|(_, name)| name.trim_end_matches("#branch").ends_with(['2', '3'])) {
            let (a, b) = (apart.x[n][u], first.x[m][first.index(name)]);
            assert_eq!(a.to_bits(), b.to_bits(), "{name} at {t:e} s is {b} through the re-cuts, {a} in the run never cut anew");
            checked += 1;
        }
    }
    assert!(checked > 1000, "oscillators 2 and 3: {checked} values compared");
}

/// R1. A heard run (`LiveSpec::heard`) never waits for a plan: one its helper is late with goes in
/// at the first round start where it is back, and that round is written (`LiveSim::go_ins`); one
/// an edit calls back first never goes in, and that is written too. Made again from its timeline
/// (`LiveSim::follow`: the edits and the written rounds), with its plans made on the worker, on
/// helper cores, and on helper cores 50 ms late, the run puts each plan in where the heard run did
/// and is the same, bit for bit, waiting where it must: the bits follow from the inputs and the
/// written rounds, never from when a plan came back. Four edits each give a re-plan, held back
/// past its time by 2, 4 and 1 rounds of the run, and one past the next edit.
#[test]
fn a_heard_run_takes_a_late_plan_where_it_is_back_and_its_timeline_makes_it_again() {
    let replan = Replan { edit: 0.5e-3, pins: 0.5e-3, lead: 0.5e-3, slow: -1.0 };
    let (edits, stop) = ([1.5e-3, 4.5e-3, 7.5e-3, 10.5e-3], 13e-3);
    let hold: [Option<u32>; 4] = [Some(2), Some(4), None, Some(1)];
    let edit = |n: usize| LiveCmd::SetParam { generation: 1, device: "rf1".into(), key: "value".into(), value: R[1] / (n + 2) as f64 };
    let new = |heard: bool| LiveSim::new(whole(), ValuePlan::default(), 1, LiveSpec { replan: replan.clone(), heard, ..spec(SAMPLE, SAMPLE) }).expect("the operating point converges");

    // heard: each re-plan is made on a helper the test lets go once the run is the hold's rounds
    // past the plan's time, and is back before the run steps on (its planner says it is done, and
    // the hand-off is given time), so it goes in at the next round start
    let mut heard = new(true);
    let (open, gate) = mpsc::channel();
    let (done, made) = mpsc::channel();
    let let_go = || {
        open.send(()).expect("the helper waits at the gate");
        made.recv().expect("the helper makes the plan");
        thread::sleep(Duration::from_millis(20));
    };
    // its plans and checks on the pool's cores (`set_checks`), as a host gives its background jobs
    let cores = pool(2);
    heard.set_helpers(Some(Helpers { post: cores.clone(), count: 0 }));
    heard.set_checks(Some(Helpers { post: cores, count: 2 }), false);
    heard.set_planner(Some(Box::new(Turns { calls: 0, apart: false, late: Duration::ZERO, started: None, gate: Some(Arc::new(Mutex::new(gate))), done: Some(done) }))).expect("the first plan installs");
    let round = heard.block_work().expect("the first plan is in").1;
    let mut first = Record::default();
    first.begin(&heard.solver().unknowns);
    let mut called_back = false;
    for (n, &at) in edits.iter().enumerate() {
        advance(&mut heard, at, &mut first);
        // the plan the edit calls back is let go first: back, it still goes nowhere
        if std::mem::take(&mut called_back) {
            let_go();
        }
        let at = heard.t();
        assert!(heard.apply(LiveCmd::Edit(vec![(at, edit(n))])), "the edit keeps the run");
        let Some(rounds) = hold[n] else {
            called_back = true;
            continue;
        };
        // the re-plan starts at a round start (`edit` after the edit), due `lead` after that
        let plans = heard.stats().plans;
        let from = loop {
            let t = heard.t();
            advance(&mut heard, t + round / 2.0, &mut first);
            if heard.stats().plans > plans {
                break t;
            }
        };
        let due = from + replan.lead;
        advance(&mut heard, due + f64::from(rounds) * round, &mut first);
        let back = heard.t();
        let_go();
        advance(&mut heard, back + round / 2.0, &mut first);
        assert_eq!(heard.stats().last_plan, back, "re-plan {n} went in at the round start where it was back, {:e} s past its time", back - due);
    }
    advance(&mut heard, stop, &mut first);
    let st = heard.stats();
    eprintln!("heard: {} plans, {} in late (by up to {:e} s), {} waits, go-ins {:?}", st.plans, st.plans_late, st.plan_late_max, st.plan_waits, heard.go_ins());
    assert!(st.plan_error.is_none(), "heard: {:?}", st.plan_error);
    assert_eq!((st.plans, st.plans_late, st.plan_waits), (5, 3, 0), "heard: the first plan and four re-plans, three in late, none waited for");
    let (timeline, go_ins) = (heard.timeline().to_vec(), heard.go_ins().to_vec());
    assert_eq!(go_ins.len(), 4, "heard: a round written for each re-plan: {go_ins:?}");
    assert!(go_ins[2].1.is_infinite() && go_ins.iter().filter(|g| g.1.is_infinite()).count() == 1, "heard: the plan the next edit called back never went in: {go_ins:?}");

    for (name, helpers, late) in [("on the worker", false, 0), ("on helper cores", true, 0), ("on helper cores, 50 ms late", true, 50)] {
        let mut sim = new(false);
        // its plans and checks on the same cores (`set_checks`), as a host gives its background jobs
        let given = helpers.then(|| Helpers { post: pool(2), count: 2 });
        sim.set_helpers(given.clone());
        sim.set_checks(given, false);
        sim.set_planner(Some(Box::new(Turns { calls: 0, apart: false, late: Duration::from_millis(late), started: None, gate: None, done: None }))).expect("the first plan installs");
        sim.follow(&timeline, &go_ins, &[], &[], None);
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        advance(&mut sim, stop, &mut record);
        let st = sim.stats();
        eprintln!("{name}: {} plans, {} on a helper, {} waited for ({:?} in all)", st.plans, st.plans_helped, st.plan_waits, st.plan_wait_wall);
        assert!(st.plan_error.is_none(), "{name}: {:?}", st.plan_error);
        assert_eq!((st.plans, st.plans_late), (5, 0), "{name}: the same plans, none taken late");
        if late > 0 {
            assert!(st.plan_waits > 0, "{name}: a plan not made yet was waited for at its written round");
        }
        assert_eq!(sim.go_ins(), &go_ins[..], "{name}: each plan went in at the round written for it");
        assert_eq!(record.at, first.at, "{name}: the heard run's points");
        assert!(record.x == first.x, "{name}: the heard run's values");
    }
}

/// A 5 V supply behind 1 ohm with 47 uF on its rail; an aggressor sinking 1 mA pulses from it
/// through 2.2 k (weakly enough for a cut: the rail's 0.5 ohm over 2.2 k), and a victim amplifying
/// the rail's ripple 100 times (an amplifier referenced to the supply: 0.1 V from 1 mV).
fn rail() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("vs", "vs", "0", Source::dc(5.0));
    c.r("rs", "vs", "vcc", 1.0);
    c.c("cvcc", "vcc", "0", 47e-6);
    c.r("ra", "vcc", "va", 2.2e3);
    c.i("ia", "va", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 1e-3, td: 0.5e-3, tr: 20e-6, tf: 20e-6, pw: 1e-3, per: 2e-3 }));
    c.vctl("amp", CtlKind::Vcvs, "out", "0", "vcc", "vs", 100.0);
    c.r("ro", "out", "0", 10e3);
    c
}

const RAIL_OPTS: PartOpts = PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: 1e-3, hub_tol: 0.1, fmin: 10.0, fmax: 48e3, band: f64::INFINITY, per_octave: 4, min_island: 1, control: Vec::new(), supplies: Vec::new(), heard: Vec::new(), round: 32.0 / 96e3 };

/// The block planner's plan (`plan_blocks`).
#[derive(Clone)]
struct Auto;

impl Planner for Auto {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        Ok(plan_blocks(ckt, whole, t, &RAIL_OPTS)?.map(|b| (b.plan, b.solvers)))
    }
}

/// Crosstalk through a shared rail, cut by the block planner: the supply, the aggressor and the
/// victim each a block. The aggressor's draw reaches the victim through the supply's block with
/// exactly one read a round late on the way: the cut's late direction, the supply reading the
/// aggressor or the rail read onward.
/// - In Exact and in Fast at the live settings, the victim hears the crosstalk right in size and
///   one round late: against a fine run of the whole circuit read a round length back, within
///   the bounds of the feed proof above (twice the whole circuit's own error in Exact, three times
///   in Fast, and 0.1 mV); read in step it is off by most of its 0.1 V swing. The rail follows
///   the fine run as late as the supply reads the aggressor.
#[test]
fn rail_crosstalk_between_blocks_is_right_in_size_and_one_round_late() {
    let round = 32.0 * SAMPLE;
    let auto = || Some(Box::new(Auto) as Box<dyn Planner>);
    let mut whole = Solver::new(&rail()).expect("the rail circuit builds");
    fab_elec_sim::analysis::op::operating_point(&mut whole, false).expect("the rail circuit has an operating point");
    let plan = plan_blocks(&rail(), &whole, 0.0, &RAIL_OPTS).expect("the rail circuit plans").expect("the rail circuit splits");
    let report = &plan.report;
    eprintln!("{report}");
    assert_eq!(plan.plan.blocks.len(), 3, "the supply, the aggressor and the victim: {report}");
    let lag_of = |node: &str| plan.plan.couplings.iter().find(|c| c.to.iter().any(|t| t.1 == format!("{node}#in"))).unwrap_or_else(|| panic!("a block reads {node}: {report}")).lag;
    let lags = [lag_of("va"), lag_of("vcc")];
    assert_eq!(lags.iter().filter(|&&l| l == Lag::Round).count(), 1, "one of the supply's read of the aggressor and the rail's read onward is late: {lags:?}, {report}");
    // the rail itself is late when the supply reads the aggressor late
    let rail_lag = if lags[0] == Lag::Round { round } else { 0.0 };
    let at = |reltol: f64| {
        let mut c = rail();
        c.opts.reltol = reltol;
        c
    };
    let (truth, _) = run(at(1e-5), None, None, 0, spec(SAMPLE / 16.0, 0.0), None, None);
    let out = truth.v("out");
    let ripple = |f: fn(f64, f64) -> f64, from: f64| truth.at.iter().zip(&out).filter(|&(&t, _)| t > 0.4e-3).fold(from, |m, (_, &v)| f(m, v));
    let swing = ripple(f64::max, f64::MIN) - ripple(f64::min, f64::MAX);
    assert!(swing > 0.08, "the victim hears about 0.1 V of crosstalk: {swing:e} V");
    for (mode, res, reltol, times) in [("Exact", 0.0, 1e-4, 2.0), ("Fast", SAMPLE, rail().opts.reltol, 3.0)] {
        let (whole, _) = run(at(reltol), None, None, 0, spec(SAMPLE, res), None, None);
        let (split, sim) = run(at(reltol), auto(), None, 0, spec(SAMPLE, res), None, None);
        assert_eq!(sim.blocks(), 3, "{mode}: the run steps three blocks");
        for (name, what, lag) in [("out", "the victim", round), ("vcc", "the rail", rail_lag)] {
            let (got, own) = (deviation(&split, &truth, name, lag, lag), deviation(&whole, &truth, name, 0.0, 0.0));
            eprintln!("{mode}: {what}: {got:e} V from the fine run {lag:e} s back among the blocks, {own:e} V whole");
            assert!(got <= times * own + 1e-4, "{mode}: {what} follows the fine run {lag:e} s back within {times} times the whole circuit's error (and 0.1 mV): {got:e} V, whole {own:e} V");
        }
        let unlagged = deviation(&split, &truth, "out", 0.0, round);
        eprintln!("{mode}: the victim read in step: {unlagged:e} V off, of a {swing:e} V swing");
        assert!(unlagged > 0.5 * swing, "{mode}: the victim's crosstalk is a round late, not in step: {unlagged:e} V off, of {swing:e} V");
    }
}

/// Three voices on one supply: 5 V behind 10 ohm, declared a supply. With `bulk` the rail holds
/// 22 uF and a bulk 100 uF behind 4.7 ohm (two RC sections as the holder stands, at about 64 us and
/// 1.6 ms); without, the rail is flat, 10 ohm at every frequency. Each voice is a Schmitt-trigger
/// RC oscillator (rf onto 10 nF, its timing node starting empty) and a second section buffering
/// the oscillator's output into 10 kohm; both sections' switches draw from the rail.
fn shared_rail_voices(bulk: bool) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("lo", ModelKind::Sw, &[("vt", 2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    c.model("hi", ModelKind::Sw, &[("vt", -2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    c.v("vs", "vs", "0", Source::dc(5.0));
    c.r("rs", "vs", "rail", 10.0);
    if bulk {
        c.c("crail", "rail", "0", 22e-6);
        c.r("rbulk", "rail", "bulk", 4.7);
        c.c("cbulk", "bulk", "0", 100e-6);
    }
    c.declare_supply("rail");
    for k in 0..3 {
        let n = |s: &str| format!("{s}{k}");
        c.sw(&n("slo"), &n("o"), "0", &n("i"), "0", "lo", false);
        c.sw(&n("shi"), "rail", &n("o"), "0", &n("i"), "hi", true);
        c.r(&n("rf"), &n("o"), &n("i"), R[k]);
        c.c(&n("ct"), &n("i"), "0", 10e-9);
        c.c(&n("co"), &n("o"), "0", 10e-12);
        c.set_ic(&n("i"), 0.0);
        c.sw(&n("blo"), &n("b"), "0", &n("o"), "0", "lo", true);
        c.sw(&n("bhi"), "rail", &n("b"), "0", &n("o"), "hi", false);
        c.c(&n("cb"), &n("b"), "0", 10e-12);
        c.r(&n("rl"), &n("b"), "0", 10e3);
    }
    c
}

/// The block planner at the host's listening settings: a band to 24 kHz, islands under 8 unknowns
/// joining their single reader.
const LISTEN_OPTS: PartOpts = PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: 1e-3, hub_tol: 0.1, fmin: 10.0, fmax: 24e3, band: f64::INFINITY, per_octave: 4, min_island: 8, control: Vec::new(), supplies: Vec::new(), heard: Vec::new(), round: 32.0 / 48e3 };

#[derive(Clone)]
struct Listening;

impl Planner for Listening {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        Ok(plan_blocks(ckt, whole, t, &LISTEN_OPTS)?.map(|b| (b.plan, b.solvers)))
    }
}

/// Voices whose switches draw from a shared supply each run in a block of their own: what a
/// buffer's switch draws from the rail follows the oscillator output it reads, and that is the
/// buffer's block's draw, returned a round late, not a read of the oscillator by the rail's block.
/// - The plan: the rail alone in block 0, and each voice (its oscillator and its buffer) a block
///   apart from the others, with nothing merged by the round map.
/// - The rail's holder stands beside each voice as two RC sections (`HolderPort::second`, a
///   declared supply), leaving at most 0.5 % of its impedance.
/// - Each port is the holder's 10 ohm at DC within 1 %, and a pair's time constants are no
///   slower than the band's lowest point. So too on a flat rail (no capacitors, 10 ohm at every
///   frequency), which stays in block 0: there the pair has nothing to fit, and the band's lowest
///   point is all that bounds its search.
/// - In Fast at the live settings, each oscillator's period in rounds is the whole run's within
///   0.1 %, and the run names the block each net is solved in (`LiveSim::block_of`) as the plan.
#[test]
fn voices_whose_switches_draw_from_a_shared_supply_each_run_in_a_block_of_their_own() {
    let mut whole = Solver::new(&shared_rail_voices(true)).expect("the voices build");
    fab_elec_sim::analysis::op::operating_point(&mut whole, false).expect("the voices have an operating point");
    let plan = plan_blocks(&shared_rail_voices(true), &whole, 0.0, &LISTEN_OPTS).expect("the voices plan").expect("the voices split");
    let report = &plan.report;
    let block_of = |name: &str| plan.plan.blocks.iter().position(|b| b.own.iter().any(|&(_, g)| whole.unknowns[g as usize].name == name))
        .unwrap_or_else(|| panic!("a block solves {name}: {report}"));
    let voices: Vec<usize> = (0..3).map(|k| block_of(&format!("o{k}"))).collect();
    assert_eq!(block_of("rail"), 0, "the rail is block 0: {report}");
    for (k, &b) in voices.iter().enumerate() {
        assert!(b != 0 && block_of(&format!("i{k}")) == b && block_of(&format!("b{k}")) == b,
            "voice {k}'s oscillator and buffer are one block, not the rail's: {report}");
    }
    assert!(voices[0] != voices[1] && voices[1] != voices[2] && voices[0] != voices[2] && plan.plan.blocks.len() == 4 && plan.map.merges == 0,
        "each voice is a block of its own, with nothing merged: {voices:?}, {report}");
    let ports: Vec<&HolderPort> = plan.plan.ports.iter().filter(|p| p.name.starts_with("rail#port")).collect();
    assert!(ports.len() == 3 && ports.iter().all(|p| p.second && p.rb > 0.0 && p.rb2 > 0.0 && p.tau < p.tau2),
        "each voice stands beside the rail's holder as two RC sections: {:?}, {report}", ports.iter().map(|p| (p.r, p.rb, p.tau, p.rb2, p.tau2, p.l)).collect::<Vec<_>>());
    let left: f64 = report.lines().find(|l| l.contains("the holder of rail")).and_then(|l| l.split("leaving at most ").nth(1)).and_then(|l| l.split(" %").next())
        .and_then(|v| v.trim().parse().ok()).unwrap_or_else(|| panic!("the report says what the rail's port leaves: {report}"));
    assert!(left <= 0.5, "the rail's two-section port leaves at most 0.5 % of its holder's impedance: {left} %, {report}");
    // at DC the capacitors are open and the holder is the supply's 10 ohm; a pair's sections are
    // no slower than the band's lowest point
    let slowest = 1.0 / (2.0 * std::f64::consts::PI * LISTEN_OPTS.fmin);
    let pinned = |ports: &[&HolderPort], rail: &str, report: &str| {
        for p in ports {
            assert!(((p.r + p.rb + p.rb2) / 10.0 - 1.0).abs() < 0.01 && (p.tau2 == 0.0 || (p.tau <= slowest && p.tau2 <= slowest)),
                "the {rail} rail's port is its holder's 10 ohm at DC within 1 %, a pair's time constants up to {slowest:.3e} s: {:?}, {report}",
                (p.r, p.rb, p.tau, p.rb2, p.tau2, p.l));
        }
    };
    pinned(&ports, "bulk", report);
    let mut flat = Solver::new(&shared_rail_voices(false)).expect("the flat rail's voices build");
    fab_elec_sim::analysis::op::operating_point(&mut flat, false).expect("the flat rail's voices have an operating point");
    let plan = plan_blocks(&shared_rail_voices(false), &flat, 0.0, &LISTEN_OPTS).expect("the flat rail's voices plan").expect("the flat rail's voices split");
    let report = &plan.report;
    let rail = plan.plan.blocks.iter().position(|b| b.own.iter().any(|&(_, g)| flat.unknowns[g as usize].name == "rail"));
    let ports: Vec<&HolderPort> = plan.plan.ports.iter().filter(|p| p.name.starts_with("rail#port")).collect();
    assert!(rail == Some(0) && ports.len() == 3, "the flat rail is block 0, with a port beside each voice: {rail:?}, {report}");
    pinned(&ports, "flat", report);
    let (whole_run, one) = run(shared_rail_voices(true), None, None, 9, spec(SAMPLE, SAMPLE), None, None);
    let (split, sim) = run(shared_rail_voices(true), Some(Box::new(Listening)), None, 9, spec(SAMPLE, SAMPLE), None, None);
    assert_eq!(sim.blocks(), 4, "the run steps the rail and the three voices apart");
    // the run says where it solves each net (`LiveSim::block_of`) as the plan does
    assert_eq!((one.block_of("rail"), sim.block_of("rail"), sim.block_of("no such net")), (None, Some(0), None),
        "on one stepper no net is in a block; in rounds the rail is block 0, and a name the circuit lacks in none");
    let running: Vec<Option<usize>> = (0..3).map(|k| sim.block_of(&format!("o{k}"))).collect();
    for (k, &b) in running.iter().enumerate() {
        assert!(b.is_some_and(|b| b != 0) && sim.block_of(&format!("i{k}")) == b && running.iter().filter(|&&o| o == b).count() == 1,
            "the run solves voice {k} in a block of its own: {running:?}");
    }
    for k in 0..3 {
        let name = format!("o{k}");
        let (got, want) = (period(&split, &name, 2.5, 2e-3), period(&whole_run, &name, 2.5, 2e-3));
        eprintln!("voice {k}: {got:e} s in rounds, {want:e} s whole");
        assert!((got / want - 1.0).abs() < 1e-3, "voice {k}'s period in rounds is the whole run's within 0.1 %: {got:e} s, {want:e} s");
    }
}

/// A key between two pads, as in a keyboard matrix: the row pad on its pull-up, through the key's
/// switch and a diode into the column pad, driven low; a listener on the row through 1 M, and a
/// chopper on the listener: a switch the circuit's own clock drives, not a person.
fn keypad() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(3.3));
    c.gpio("row0", "row", "vdd", "0", PARAMS, PinMode::Input(PinPull::Up));
    c.gpio("col0", "col", "vdd", "0", PARAMS, PinMode::OutLow);
    c.c("crow", "row", "0", 10e-12);
    c.model("button", ModelKind::Sw, &[("ron", 0.1), ("roff", 1e9), ("vt", 0.5), ("vh", 0.0), ("vband", 0.1)]);
    c.model("dk", ModelKind::Diode, &[]);
    c.v("press", "ctl", "0", Source::dc(0.0));
    c.sw("key", "row", "m", "ctl", "0", "button", false);
    c.d("dkey", "m", "col", "dk");
    c.r("rlis", "row", "lis", 1e6);
    c.c("clis", "lis", "0", 1e-9);
    c.v("clock", "clk", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 1.0, td: 0.0, tr: 1e-6, tf: 1e-6, pw: 0.5e-3, per: 1e-3 }));
    c.sw("chop", "lis", "lo", "clk", "0", "button", false);
    c.r("rlo", "lo", "0", 1e6);
    c
}

/// A key pressed in rounds. The block planner cuts with the key pressed, so the key's cell stays
/// with the pads it joins (pressed, it is a near short from the row's pull-up into the driven
/// column, whose current a cut would return a round late, growing every round), and it proves the
/// plan with the key in either position. The listener is a block of its own, its current returned
/// to the row a round late, so the pads' block reads across the cut and exports: the key is on a
/// loop, its control is not.
/// - The plan proves the key in both positions, and not the chopper, which keeps the landed rule.
/// - The press is heard at once, and while the key is held: the row falls to the diode's drop
///   above the column. The release is heard. The run stays in rounds throughout: no leave, no
///   time on one stepper, and the plans after the press and the release cut it as it runs.
#[test]
fn a_key_pressed_in_rounds_is_heard_at_once_and_the_run_stays_in_rounds() {
    let replan = Replan { edit: 1e-3, pins: 1e-3, lead: 0.5e-3, slow: f64::INFINITY };
    let spec = LiveSpec { replan, ..spec(SAMPLE, SAMPLE) };
    let mut sim = LiveSim::new(keypad(), ValuePlan::default(), 1, spec).expect("the keypad's operating point converges");
    let plan = plan_blocks(&keypad(), sim.solver(), 0.0, &RAIL_OPTS).expect("the keypad plans").expect("the keypad splits");
    eprintln!("{}", plan.report);
    assert_eq!((plan.plan.blocks.len(), &plan.plan.switches[..]), (2, &["key".to_string()][..]), "the pads with the key's cell, and the listener; the key proved in both positions: {}", plan.report);
    sim.set_planner(Some(Box::new(Auto))).expect("the plan installs");
    assert_eq!(sim.blocks(), 2, "the run goes in rounds");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    let key = |v: f64| LiveCmd::SetParam { generation: 1, device: "press".into(), key: "dc".into(), value: v };
    let row = |record: &Record| record.v("row").last().copied().expect("the run has points");
    advance(&mut sim, 1e-3, &mut record);
    assert!(row(&record) > 3.0, "the row rests on its pull-up: {} V", row(&record));
    assert!(sim.apply(key(1.0)), "the press keeps the run");
    for (to, what) in [(1.2e-3, "at once"), (4e-3, "held, past the plan made with it held")] {
        advance(&mut sim, to, &mut record);
        assert!(row(&record) < 1.0, "the row reads the press {what}: {} V", row(&record));
    }
    assert!(sim.apply(key(0.0)), "the release keeps the run");
    advance(&mut sim, 4.5e-3, &mut record);
    assert!(row(&record) > 3.0, "the row reads the release: {} V", row(&record));
    advance(&mut sim, 8e-3, &mut record);
    let st = sim.stats();
    assert_eq!((st.edit_leaves, st.off_rounds, sim.blocks()), (0, 0.0, 2), "the run stays in rounds through the press and the release");
    assert!((st.plans, st.plans_kept) == (3, 2) && st.plan_error.is_none(), "the plans after the press and the release cut the run as it runs: {} plans, {} kept, {:?}", st.plans, st.plans_kept, st.plan_error);
}

/// A planner keeps its other states and the tests made in them by what the board is set to
/// (`plan_blocks_kept`). Warmed by a plan at one state of the keypad's run (the chopper closed,
/// its listener's load on it), it gives the plan a cold planner makes at another (the chopper open),
/// its states, some tests and its round maps taken from before and the rest made on two cores, the cold one's on
/// the calling thread alone: what is kept changes the time a plan takes, never the plan. Made ahead
/// as a board opens (`warm_plan` at the chopper closed), the plan at the chopper open is as warm.
#[test]
fn a_warm_planner_gives_the_cold_ones_plan() {
    let mut run = LiveSim::new(keypad(), ValuePlan::default(), 1, spec(SAMPLE, SAMPLE)).expect("the keypad's operating point converges");
    let mut record = Record::default();
    record.begin(&run.solver().unknowns);
    let mut at = |to: f64| {
        advance(&mut run, to, &mut record);
        (run.solver().clone(), run.t())
    };
    let (closed, open) = (at(0.25e-3), at(0.75e-3));
    let (cache, cores) = (StateCache::default(), Helpers { post: pool(2), count: 2 });
    let plan = |(s, t): &(Solver, f64), cache: &StateCache, cores: Option<&Helpers>| {
        plan_blocks_kept(&keypad(), s, *t, &RAIL_OPTS, cache, cores, None).expect("the keypad plans").expect("the keypad splits")
    };
    let first = plan(&closed, &cache, Some(&cores));
    eprintln!("{}", first.report);
    let (warm, cold) = (plan(&open, &cache, Some(&cores)), plan(&open, &StateCache::default(), None));
    eprintln!("{}", warm.report);
    // "<taken> of their <tests> tests taken from before"
    let taken = warm.report.split(" of their ").next().and_then(|a| a.rsplit(' ').next()).and_then(|n| n.parse::<usize>().ok());
    assert!(warm.report.contains(" kept in ") && taken.is_some_and(|n| n > 0), "the warm plan takes its states and tests from before: {}", warm.report);
    // the round map in the key's states, kept by the plan and the state: a cold planner keeps none,
    // a warm one of the same key and the same plan keeps every map the cold one made
    assert!(cold.map.key_maps > 0 && cold.map.kept == 0 && (warm.map.key_maps, warm.map.kept) == (0, cold.map.key_maps),
        "a cold planner keeps no map and a warm one keeps them all: cold {} made, {} kept; warm {} made, {} kept", cold.map.key_maps, cold.map.kept, warm.map.key_maps, warm.map.kept);
    // the plan, its round map past its time, the report past its timings and counts, and the held
    // search's islands, merges and cuts with their figures in the other states, the states named
    let what = |p: &BlockPlan| {
        let h = &p.held;
        (format!("{:?}\n{:?}", p.plan, RoundMap { ms: 0.0, key_maps: 0, kept: 0, ..p.map.clone() }), p.report.lines().skip(2).collect::<Vec<_>>().join("\n"), format!("{:?}\n{:?}\n{:?}", h.islands, h.merges, h.nodes))
    };
    assert_eq!(what(&warm), what(&cold), "a warm planner gives the cold one's plan");
    // made ahead on a cache of its own, the plan of the key keeps its maps and is the same
    let ahead = StateCache::default();
    warm_plan(&keypad(), &closed.0, closed.1, &RAIL_OPTS, &ahead, Some(&cores));
    let after = plan(&open, &ahead, Some(&cores));
    assert!((after.map.key_maps, after.map.kept) == (0, cold.map.key_maps) && what(&after) == what(&cold),
        "a plan made ahead as a board opens keeps every map the plan after it needs: {} made, {} kept", after.map.key_maps, after.map.kept);
}

/// A pad and its listener: the pad on its pull-up, the listener on it through `rlis` into 1 nF.
/// The listener is a block of its own whose current returns to the pad a round late (the pad's
/// hub form): the loop's gain a round is about the pad's impedance over `rlis`. The circuit is
/// linear, so it linearises alike at every state.
fn listened_pad(rlis: f64) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(3.3));
    c.gpio("row0", "row", "vdd", "0", PARAMS, PinMode::Input(PinPull::Up));
    c.c("crow", "row", "0", 10e-12);
    c.r("rlis", "row", "lis", rlis);
    c.c("clis", "lis", "0", 1e-9);
    c
}

/// A plan made after an edit proves the circuit as edited, not as lowered: the listener's
/// resistor turned from 1 M to 2 M halves the loop gain the plan proves, to what a plan of the
/// circuit lowered at 2 M proves.
#[test]
fn a_plan_made_after_an_edit_proves_the_edited_value() {
    let gain = |sim: &LiveSim| plan_blocks(&listened_pad(1e6), sim.solver(), sim.t(), &RAIL_OPTS).expect("the pad plans").expect("the listener splits off").map.gain.0;
    let new = |rlis: f64| LiveSim::new(listened_pad(rlis), ValuePlan::default(), 1, spec(SAMPLE, SAMPLE)).expect("the listener's operating point converges");
    let mut sim = new(1e6);
    let before = gain(&sim);
    assert!(sim.apply(LiveCmd::SetParam { generation: 1, device: "rlis".into(), key: "value".into(), value: 2e6 }), "the edit keeps the run");
    let after = gain(&sim);
    let lowered = plan_blocks(&listened_pad(2e6), new(2e6).solver(), 0.0, &RAIL_OPTS).expect("the pad plans").expect("the listener splits off").map.gain.0;
    assert!((after - lowered).abs() <= 1e-9 * lowered && (after / before - 0.5).abs() < 0.01, "the plan after the edit proves 2 M: {after:e}, lowered at 2 M {lowered:e}, at 1 M {before:e}");
}

/// Two pads, each into a listener of its own: `ra` into 10 p, and 47 k into 10 p. Each listener
/// is a block that reads its pad, an import that can jump (the pad's writes); unalike, neither
/// takes the other's test.
fn two_listeners(ra: f64) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(3.3));
    c.gpio("pa", "a", "vdd", "0", PARAMS, PinMode::OutLow);
    c.gpio("pb", "b", "vdd", "0", PARAMS, PinMode::OutLow);
    c.r("ra", "a", "la", ra);
    c.c("cla", "la", "0", 10e-12);
    c.r("rb", "b", "lb", 47e3);
    c.c("clb", "lb", "0", 10e-12);
    c
}

/// A reader's import test is kept by everything it reads, its matrices in every state among them
/// (`round::charge_hold`): a plan again in the same states takes every reader's test from before;
/// after a value edit inside one reader, the next plan makes that reader's test again, only that
/// one's, and gives what a cold planner gives.
#[test]
fn an_import_test_is_kept_by_its_readers_matrices() {
    let mut sim = LiveSim::new(two_listeners(100e3), ValuePlan::default(), 1, spec(SAMPLE, SAMPLE)).expect("the listeners' operating point converges");
    let plan = |sim: &LiveSim, cache: &StateCache| {
        plan_blocks_kept(&two_listeners(100e3), sim.solver(), sim.t(), &RAIL_OPTS, cache, None, None).expect("the pads plan").expect("the listeners split off")
    };
    // per tested import, by whether its reader holds the edited listener: whether the plan made
    // its reader's test, and its figure's bits
    let tests = |p: &BlockPlan| {
        let mut out: Vec<(bool, bool, u64)> = p.tested.iter().filter(|x| x.jumps).map(|x| (p.solvers[x.reader].unknown("la").is_some(), x.made, x.figure.to_bits())).collect();
        out.sort_unstable();
        out
    };
    let cache = StateCache::default();
    let first = plan(&sim, &cache);
    eprintln!("{}", first.report);
    let before = tests(&first);
    assert!(before.len() == 2 && before.iter().all(|x| x.1) && before[0].0 != before[1].0, "each listener's pad is tested in its own reader, both tests made: {before:?}");
    let again = tests(&plan(&sim, &cache));
    assert!(again.iter().all(|x| !x.1) && again.iter().map(|x| x.2).eq(before.iter().map(|x| x.2)), "a plan again in the same states takes both tests from before: {again:?}");
    assert!(sim.apply(LiveCmd::SetParam { generation: 1, device: "ra".into(), key: "value".into(), value: 200e3 }), "the edit keeps the run");
    let edited = plan(&sim, &cache);
    eprintln!("{}", edited.report);
    let (after, cold) = (tests(&edited), tests(&plan(&sim, &StateCache::default())));
    assert!(after.iter().all(|x| x.1 == x.0), "after the edit only the edited listener's test is made, the other's taken from before: {after:?}");
    assert!(after.iter().map(|x| (x.0, x.2)).eq(cold.iter().map(|x| (x.0, x.2))) && after.iter().zip(&before).any(|(a, b)| a.0 && a.2 != b.2),
        "the edited listener's figure moves with its resistor, as a cold planner finds it: before {before:?}, after {after:?}, cold {cold:?}");
}

/// A pad driven high into two listeners, each through 10 k into 10 p with a load `rg{k}` (1 M) to
/// ground. Under the rail options each listener is a block of its own that reads the pad and
/// returns its draw to it a round late: two holder ports at the row, the pad's hub.
fn pad_two_listeners() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(3.3));
    c.gpio("row0", "row", "vdd", "0", PARAMS, PinMode::OutHigh);
    c.c("crow", "row", "0", 10e-12);
    for k in 0..2 {
        c.r(&format!("rl{k}"), "row", &format!("l{k}"), 10e3);
        c.c(&format!("cl{k}"), &format!("l{k}"), "0", 10e-12);
        c.r(&format!("rg{k}"), &format!("l{k}"), "0", 1e6);
    }
    c
}

/// The hub reads screened (`LiveSpec::hub_screen`, counted in `RoundSplit::hub_onsets`). A
/// listener's late term is how far its own draw moves the pad in the round before the pad's block
/// hears it: a step `di` in its load's current is `di` times the pad's 25 R for that one round.
/// The screen is 1e-3 of the circuit's largest source, 3.3 mV.
/// - At rest, through the start and the plans after each edit, no port round passes the screen.
/// - A step whose `di * 25 R` is 0.85 of the screen stays under it; one at 1.15 passes it in one
///   port round: the term is recorded at its size, and the other listener, which hears only the
///   pad's move a round later, stays under.
/// - Both loads stepped at once pass it in two port rounds, one a port.
/// - Off (the default), no hub read is screened.
#[test]
fn a_hub_readers_late_term_is_its_draws_step_through_the_pad() {
    const SCREEN: f64 = 3.3e-3;
    let draw = |rg: f64| 3.3 / (10e3 + rg);
    // the load that steps the listener's draw from `rg`'s by `screens` of the screen
    let load = |rg: f64, screens: f64| 3.3 / (draw(rg) + screens * SCREEN / 25.0) - 10e3;
    let set = |device: &str, value: f64| LiveCmd::SetParam { generation: 1, device: device.into(), key: "value".into(), value };
    let replan = Replan { edit: 1e-3, pins: 1e-3, lead: 0.5e-3, slow: f64::INFINITY };
    let run = |hub_screen: bool, steps: &[(f64, &[(&str, f64)])], end: f64| {
        let mut sim = LiveSim::new(pad_two_listeners(), ValuePlan::default(), 1, LiveSpec { replan: replan.clone(), hub_screen, ..spec(SAMPLE, SAMPLE) })
            .expect("the listeners' operating point converges");
        sim.set_planner(Some(Box::new(Auto))).expect("the plan installs");
        assert_eq!(sim.blocks(), 3, "the pad, and each listener a block of its own");
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        let mut onsets = Vec::new();
        for &(at, edits) in steps {
            advance(&mut sim, at, &mut record);
            onsets.push(sim.stats().round_split.hub_onsets);
            for &(device, value) in edits {
                assert!(sim.apply(set(device, value)), "the edit keeps the run");
            }
        }
        advance(&mut sim, end, &mut record);
        onsets.push(sim.stats().round_split.hub_onsets);
        let st = sim.stats();
        assert!(st.edit_leaves == 0 && sim.blocks() == 3 && st.plans > steps.len() as u64, "the run stays in rounds, planned again after each edit: {} plans", st.plans);
        onsets
    };
    let under = load(1e6, 0.85);
    let over = load(under, 1.15);
    let onsets = run(true, &[(2e-3, &[("rg0", under)]), (3.5e-3, &[("rg0", over)]), (5e-3, &[("rg0", 1e6), ("rg1", load(1e6, 2.0))])], 6.5e-3);
    assert_eq!(onsets, [0, 0, 1, 3], "port rounds past the screen: none at rest, none for 0.85 of it, one for 1.15, two for both loads at once (cumulative)");
    assert_eq!(run(false, &[(2e-3, &[("rg0", 100.0), ("rg1", 100.0)])], 3.5e-3), [0, 0], "off, no hub read is screened");
}

#[derive(Clone, Copy, PartialEq)]
enum RelaySource { Pad, Switched, WeakSwitch }

/// A pad `pa` (5 V) rising at `at` and high for a millisecond in two, read by `reader`: a diode
/// into a capacitor charged within a tenth of a sample and held (10 M), or a resistor into a
/// capacitor as fast. `buffered`: through 10 k into a unity buffer whose output the reader reads,
/// and the planner cuts at the source and at the buffer's input, three blocks; else straight from
/// the source. The pad's rise is a corner. With the switch, the pad ramps for a millisecond and
/// the jump is born on `swout` as the pad crosses the switch's threshold.
fn relayed_pad(reader: &str, source: RelaySource, buffered: bool, at: f64) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(5.0));
    let params = if source == RelaySource::Pad { PARAMS } else { GpioParams { t_edge: 1e-3, ..PARAMS } };
    c.gpio("pa", "a", "vdd", "0", params, PinMode::Pwm { period: 2e-3, high: 1e-3, origin: at });
    let export = if source != RelaySource::Pad {
        // The weak switch drives the same 25 ohm load through 25 M: at 5 V its output is at most 5 uV.
        let ron = if source == RelaySource::WeakSwitch { 25e6 } else { 1.0 };
        c.model("hi", ModelKind::Sw, &[("vt", 2.0), ("vh", 0.25), ("ron", ron), ("roff", 1e9)]);
        c.sw("sdrive", "vdd", "swout", "a", "0", "hi", false);
        c.r("rpull", "swout", "0", 25.0);
        "swout"
    } else { "a" };
    let from = if buffered {
        c.r("rin", export, "x", 10e3);
        c.vctl("buf", CtlKind::Vcvs, "bo", "0", "x", "0", 1.0);
        "bo"
    } else {
        export
    };
    if reader == "diode" {
        c.model("dr", ModelKind::Diode, &[]);
        c.r("rr", from, "z", 1e3);
        c.d("dd", "z", "n", "dr");
        c.c("cn", "n", "0", 0.1 * SAMPLE48 / 1e3);
        c.r("rh", "n", "0", 10e6);
    } else {
        c.r("rr", from, "n", 10e3);
        c.c("cn", "n", "0", 0.1 * SAMPLE48 / 10e3);
    }
    c
}

const SAMPLE48: f64 = 1.0 / 48e3;

fn relayed_opts() -> PartOpts {
    PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: fab_elec_sim::analysis::round::CUT_TOL, hub_tol: 0.1, fmin: 10.0, fmax: 0.5 / SAMPLE48, band: f64::INFINITY, per_octave: 4, min_island: 1, control: Vec::new(),
        supplies: Vec::new(), heard: Vec::new(), round: 8.0 * SAMPLE48 }
}

/// A jump relayed through a block lands along its whole path, and passes on as an event (the relay
/// answer; `round::charge_hold`, `Run::step`'s `RELAYED`):
/// - planned: the pad's import into the buffer's block charges nothing, but the buffer's export
///   into the diode's block would be held, so both land; the buffer's test is kept by its matrices,
///   and with a linear reader after it instead, the next plan takes that test from before and
///   lands nothing by its charge: a verdict is made again every plan, never kept with a test. The
///   buffer's pair still lands there, by the band figure alone (`round::band_figure`): its
///   transfer to its export prices a split at the band's top over the bar. The final linear
///   reader's import lands by band only when its own node is declared heard. Probing it during
///   a run changes that landing when the plan made behind the sound goes in;
/// - run in rounds, the pad rising at two places in a sample: the diode's block lands on the jump
///   the buffer passes on, and its node's peak is the one it takes straight from the pad, within a
///   millivolt (split, or landed with no event passed on, it overshoots by up to 2.7 V more);
/// - and straight from the pad that peak is Exact's within a millivolt: the pad's edge, a 5 ns ramp
///   the resolution steps over with no corner, is passed on with its end along the landed couplings,
///   and each reader takes its first step to there and doubles on (`Accepted::edge_end`); without
///   it the peak overshoots by 0.08 and 0.7 V at these two places, as whole Fast's does;
/// - the buffer relays the pad's corner as a plain event. When the pad drives a switch, naming
///   `swout` to `x` lands the diode block's import and it restarts at the relayed instant as at
///   an edge. Naming `swout` to itself counts one born pair by name. The diode's import then
///   lands only if its own weighted charge or band figure exceeds its bar. A weak switch still
///   crosses, but its relayed step under the diode reader's step bar causes no edge restart.
#[test]
fn a_jump_relayed_through_a_block_lands_along_its_path() {
    use RelaySource::{Pad, Switched, WeakSwitch};
    let cache = StateCache::default();
    let plan = |reader: &str, heard: bool| {
        let mut c = relayed_pad(reader, Pad, true, 10.0 * SAMPLE48);
        if heard {
            c.declare_heard("n");
        }
        let sim = LiveSim::new(c.clone(), ValuePlan::default(), 1, spec(SAMPLE48, SAMPLE48)).expect("the rig's operating point converges");
        plan_blocks_kept(&c, sim.solver(), sim.t(), &relayed_opts(), &cache, None, None).expect("the rig plans").expect("the rig splits")
    };
    let diode = plan("diode", false);
    let reader = (0..diode.solvers.len()).find(|&k| diode.solvers[k].unknown("n").is_some()).expect("a block holds the diode's node");
    let born: Vec<_> = diode.tested.iter().filter(|x| x.jumps).collect();
    assert!(diode.plan.blocks.len() == 3 && born.len() == 1 && born[0].reader != reader, "three blocks, the pad born into the buffer's block: {}", diode.report);
    assert!(born[0].lands && born[0].figure < born[0].bar && diode.tested.iter().any(|x| x.reader == reader && !x.jumps && x.lands && x.amp * x.figure > x.bar) && diode.plan.lands.len() == 2,
        "the relayed pair lands on its own figure, the born one for it: {}", diode.report);
    let linear = plan("linear", false);
    let born: Vec<_> = linear.tested.iter().filter(|x| x.jumps).collect();
    assert!(born.len() == 1 && !born[0].made, "the buffer's test is taken from before, its matrices the same: {}", linear.report);
    assert!(linear.tested.iter().all(|x| !x.lands || x.by_band), "with a linear reader after the buffer nothing lands by its charge: the verdict is made again, not kept with the test: {}", linear.report);
    assert!(born[0].lands && born[0].by_band, "with a linear reader after the buffer, the pad's jump lands in the buffer by the band figure alone: {}", linear.report);
    let heard_linear = plan("linear", true);
    for (p, heard) in [(&linear, false), (&heard_linear, true)] {
        let import = p.tested.iter().find(|x| !x.jumps && x.amp > 0.0 && p.solvers[x.reader].unknown("n").is_some())
            .expect("the relayed jump reaches the linear reader that owns n");
        assert_eq!((import.lands, import.by_band), (heard, heard),
            "the final linear reader should land by band exactly when n is declared heard (heard={heard}): {}", p.report);
    }
    // Keep the probe's plan queued while the old cut plays. Returning the job makes the next
    // round start deterministic without tying the proof to the speed of a helper.
    let mut c = relayed_pad("linear", Pad, true, 10.0 * SAMPLE48);
    c.declare_heard("vdd");
    let signals = ValuePlan { nets: vec!["0".into(), "vdd".into(), "n".into()], pins: vec![Vec::new()], pin_net: vec![None], ..ValuePlan::default() };
    let node = signals.nets.iter().position(|name| name == "n").expect("the linear reader's node has a net signal");
    let mut rows = vec![true; signals.nets.len() + signals.pins.len()];
    rows[node] = false;
    let live_spec = LiveSpec { heard: true, playing: true, sample_rate: 48e3, round: 8,
        replan: Replan { edit: SAMPLE48, lead: SAMPLE48, slow: f64::INFINITY, ..Replan::default() }, ..spec(SAMPLE48, SAMPLE48) };
    let mut live = LiveSim::new(c, signals, 1, live_spec).expect("the live linear rig starts");
    let (queue, queued) = mpsc::channel();
    live.set_checks(Some(Helpers { count: 1, post: Arc::new(move |job| {
        if fab_elec_sim::rounds::behind() { queue.send(job).expect("the proof keeps the behind queue open"); } else { job(); }
    }) }), false);
    live.set_planner(Some(Box::new(BlockPlanner { opts: relayed_opts(), states: StateCache::default(), pool: None }))).expect("the live linear rig plans");
    let lands = |sim: &LiveSim| {
        let plan = sim.round_plan().expect("the linear rig stays in rounds");
        let reader = sim.block_of("n").expect("a live block owns the linear reader's node") as u32;
        let q = plan.couplings.iter().position(|c| c.from.0 != 0 && c.to.iter().any(|(r, _)| *r == reader))
            .expect("the live linear reader imports the buffer's export");
        plan.lands.binary_search(&(q as u32, reader)).is_ok()
    };
    let mut record = Record::default();
    record.begin(&live.solver().unknowns);
    let round = live.round_len();
    let first = live.stats().plans;
    live.apply(LiveCmd::SetWhole(rows.clone().into()));
    advance(&mut live, 2.0 * round, &mut record);
    assert_eq!(live.stats().plans, first, "probing ground, a declared output and a pin row must leave the plan in force");
    assert!(!lands(&live), "the unprobed linear reader must start with its relayed import split");
    rows[node] = true;
    live.apply(LiveCmd::SetWhole(rows.into()));
    advance(&mut live, 6.0 * round, &mut record);
    assert!(!lands(&live), "the old cut must keep playing while the probe's plan is queued");
    let job = queued.try_recv().expect("the live probe must post its plan behind the sound even after a cheap plan");
    job();
    advance(&mut live, 7.0 * round, &mut record);
    assert!(lands(&live), "the installed probe plan must land the relayed import into the linear reader");
    assert_eq!(live.stats().plans_helped, 1, "the checker must make the probe plan");
    live.apply(LiveCmd::SetWhole(Arc::from([])));
    assert!(live.circuit().nodes.iter().any(|n| n.name == "vdd" && n.heard), "clearing the views must keep the design's declared output heard");
    advance(&mut live, 10.0 * round, &mut record);
    let job = queued.try_recv().expect("clearing the probe must re-judge the linear reader behind the sound");
    job();
    advance(&mut live, 11.0 * round, &mut record);
    assert!(!lands(&live), "the live linear reader must split again after its probe is cleared");

    // the node's peak in rounds, and how its block restarted
    let run = |source: RelaySource, buffered: bool, at: f64| {
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: SAMPLE48, res: SAMPLE48, trace: false, pipeline: false, edge_restarts: true, ..LiveSpec::default() };
        let mut c = relayed_pad("diode", source, buffered, at);
        if source == WeakSwitch {
            c.declare_heard("bo");
        }
        let mut sim = LiveSim::new(c.clone(), ValuePlan::default(), 1, spec).expect("the operating point converges");
        let mut planner = BlockPlanner { opts: relayed_opts(), states: Default::default(), pool: None };
        let mut weak_bar = None;
        if source != Pad {
            // Keep the switch output with its pad. The diode block imports x and solves bo locally.
            planner.opts.control.push("swout".into());
            // The run uses the last target: the weak switch names only its born pair.
            let targets: &[&str] = if source == WeakSwitch { &["swout"] } else { &["swout", "x"] };
            for &to in targets {
                planner.opts.land = vec![Land { born: "swout".into(), to: to.into() }];
                let p = plan_blocks_kept(&c, sim.solver(), sim.t(), &planner.opts, &planner.states, None, None).expect("the switch rig plans").expect("the switch rig splits");
                assert!(p.solvers.len() == 3 && p.solvers[0].find_device("sdrive").is_some(),
                    "the switched rig must split into three blocks with the switch in block 0: {}", p.report);
                let import = |boundary: &str| p.tested.iter().find(|x| p.plan.couplings[x.coupling].to.iter()
                    .any(|(reader, name)| *reader as usize == x.reader && name == boundary))
                    .unwrap_or_else(|| panic!("the switched rig must have a tested {boundary} import: {}", p.report));
                let born = import("swout#in");
                let relayed = import("x#in");
                assert!(born.jumps && born.lands && p.relay.by_name.contains(&(born.coupling, born.reader)),
                    "the switch's born pair must land by name for either target: {}", p.report);
                assert!(!relayed.jumps && p.solvers[relayed.reader].find_device("dd").is_some(),
                    "the diode's block must read x as the buffer's relayed export: {}", p.report);
                let named = p.relay.by_name.contains(&(relayed.coupling, relayed.reader));
                if to == "x" {
                    assert!(relayed.lands && named, "swout to x must land the diode's relayed pair by name: {}", p.report);
                } else {
                    assert!(!named, "swout to itself must stop before the diode's relayed pair: {}", p.report);
                    assert_eq!(relayed.lands, relayed.amp * relayed.figure > relayed.bar || relayed.amp * relayed.band > relayed.bar,
                        "with only swout named, the diode's relayed pair must land exactly when its own charge or band figure is over its bar: {}", p.report);
                    assert!(p.report.lines().any(|line| line.starts_with("fast charge, slow hold:") && line.contains(", 1 by name)")),
                        "the plan line must count the born swout pair once by name: {}", p.report);
                    if source == WeakSwitch {
                        assert!(relayed.lands, "the heard buffer output must make the diode's import a landing read: {}", p.report);
                        weak_bar = Some(relayed.step);
                    }
                }
            }
        }
        sim.set_planner(Some(Box::new(planner))).expect("the plan installs");
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        // a round's points reach the record a round late
        advance(&mut sim, at + 32.0 * SAMPLE48, &mut record);
        if let Some(bar) = weak_bar {
            let x = record.v("x");
            let span = x.iter().copied().fold(f64::NEG_INFINITY, f64::max) - x.iter().copied().fold(f64::INFINITY, f64::min);
            assert!(0.0 < span && span < bar, "the relayed export must move within the diode reader's step bar: {span:e} V against {bar:e} V");
        }
        let restarts = sim.block_work().map(|(_, _, work)| {
            if source != Pad {
                assert!(work[0].restarts.crossing > 0, "the switch makes a crossing in the source block: {:?}", work[0].restarts);
            }
            if source == WeakSwitch {
                let buffer = sim.block_of("x").expect("a block exports the buffer input");
                assert!(work[buffer].restarts.corner_edge > 0, "the named born pair must land the weak crossing before the buffer passes it on");
            }
            work[sim.block_of("n").expect("a block solves the diode's node")].restarts
        });
        (record.v("n").iter().copied().fold(f64::MIN, f64::max), restarts)
    };
    let exact = |at: f64| {
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: SAMPLE48, res: 0.0, reltol: Some(1e-5), trace: false, pipeline: false, ..LiveSpec::default() };
        let mut sim = LiveSim::new(relayed_pad("diode", Pad, false, at), ValuePlan::default(), 1, spec).expect("the operating point converges");
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        advance(&mut sim, at + 32.0 * SAMPLE48, &mut record);
        record.v("n").iter().copied().fold(f64::MIN, f64::max)
    };
    for phase in [0.25, 0.75] {
        let at = (10.0 + phase) * SAMPLE48;
        let ((relayed, r), (straight, _), exact) = (run(Pad, true, at), run(Pad, false, at), exact(at));
        let r = r.expect("the buffered pad runs in rounds");
        assert!(r.corner > 0 && r.corner_edge == 0, "at phase {phase} the diode lands the relayed pad corner as a plain corner: {r:?}");
        assert!((relayed - straight).abs() < 1e-3, "at phase {phase} the diode's peak through the buffer is the one it takes straight from the pad: {relayed:.4} V against {straight:.4} V");
        assert!((straight - exact).abs() < 1e-3, "at phase {phase} the diode's peak straight from the pad is Exact's: the pad's edge passed on with its end, {straight:.4} V against {exact:.4} V");
    }
    // One 32-sample round contains the switch's crossing, before the pad ramp ends.
    let (_, r) = run(Switched, true, 0.0);
    let r = r.expect("the buffered switch runs in rounds");
    assert!(r.corner > 0 && r.corner_edge == r.corner, "the diode lands every relayed switching instant as an edge: {r:?}");
    let (_, r) = run(WeakSwitch, true, 0.0);
    let r = r.expect("the buffered weak switch runs in rounds");
    assert_eq!(r.corner_edge, 0, "the diode must take a relayed switching step under its bar as data: {r:?}");
}

/// The block planner, the gains each plan's round map proved recorded.
#[derive(Clone)]
struct Mapped(Arc<Mutex<Vec<(f64, f64)>>>);

impl Planner for Mapped {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let got = plan_blocks(ckt, whole, t, &RAIL_OPTS)?;
        if let Some(b) = &got {
            self.0.lock().unwrap().push((b.map.gain.0, b.map.gain_ordinary.0));
        }
        Ok(got.map(|b| (b.plan, b.solvers)))
    }
}

/// `listened_pad` with a diode across the listener, its node light (10 p): an edit of the
/// listener's resistor moves the diode's operating point, and the listener block's linearisation
/// with it, within the edit's round.
fn clamped_pad(rlis: f64) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("dl", ModelKind::Diode, &[]);
    c.v("supply", "vdd", "0", Source::dc(3.3));
    c.gpio("row0", "row", "vdd", "0", PARAMS, PinMode::Input(PinPull::Up));
    c.c("crow", "row", "0", 10e-12);
    c.r("rlis", "row", "lis", rlis);
    c.c("clis", "lis", "0", 10e-12);
    c.d("dlis", "lis", "0", "dl");
    c
}

fn set_rlis(ohms: f64) -> LiveCmd {
    LiveCmd::SetParam { generation: 1, device: "rlis".into(), key: "value".into(), value: ohms }
}

/// `circuit` in the rounds of the block planner, with `helpers`, the listener's resistor doubled
/// at 1 ms; run to 3 ms, it stays in rounds. The run, its record, the gains the re-proof proved
/// and those of the plans made.
fn doubled(circuit: SimCircuit, pipeline: bool, helpers: usize) -> (LiveSim, Record, (f64, f64), Vec<(f64, f64)>) {
    let replan = Replan { edit: 1e-3, pins: 1e-3, lead: 0.5e-3, slow: f64::INFINITY };
    let spec = LiveSpec { replan, pipeline, ..spec(SAMPLE, SAMPLE) };
    let maps = Arc::new(Mutex::new(Vec::new()));
    let mut sim = LiveSim::new(circuit, ValuePlan::default(), 1, spec).expect("the listener's operating point converges");
    sim.set_planner(Some(Box::new(Mapped(maps.clone())))).expect("the plan installs");
    if helpers > 0 {
        sim.set_helpers(Some(Helpers { post: pool(helpers), count: helpers }));
    }
    assert_eq!(sim.blocks(), 2, "the pad and the listener go in rounds");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    advance(&mut sim, 1e-3, &mut record);
    assert!(sim.apply(set_rlis(2e6)), "the edit keeps the run");
    advance(&mut sim, 3e-3, &mut record);
    let st = sim.stats();
    let proved = match &st.last_reproof {
        Some(Ok(map)) => (map.gain.0, map.gain_ordinary.0),
        other => panic!("the doubled resistor is proved and holds: {other:?}"),
    };
    let fresh = maps.lock().unwrap().clone();
    assert!((st.reproofs, st.edit_leaves, sim.blocks()) == (1, 0, 2) && (st.plans, st.plans_kept) == (2, 1), "the run stays in rounds, and the plan after the edit cuts it as it runs: {} proofs, {} leaves, {} blocks, {} plans, {} kept", st.reproofs, st.edit_leaves, sim.blocks(), st.plans, st.plans_kept);
    (sim, record, proved, fresh)
}

/// An edit on a loop of the plan's round map is proved at the end of its round: the edited block
/// linearised where it stands, the others as the plan's proof keeps them.
/// - The listener's resistor doubled: the proof holds and the run stays in rounds. It proves the
///   gains a fresh plan of the edited circuit proves, under the first plan's, and that plan cuts
///   the run as it runs.
/// - The resistor at 1 k, the loop's gain past the pad's bar: the proof fails at the round's end,
///   and the run leaves rounds there.
/// - In the pipeline (`LiveSpec::pipeline`), the listener's stage running behind block 0 and its
///   points shared out with three helpers, the proof gives the same gains to the bit as on the
///   worker alone: the stage finishes the edit's round first. The listener clamped by a diode
///   (`clamped_pad`), its block's linearisation moves within that round.
#[test]
fn an_edit_on_a_loop_stays_in_rounds_while_its_proof_holds() {
    let (mut sim, mut record, proved, fresh) = doubled(listened_pad(1e6), false, 0);
    let near = |a: f64, b: f64| (a - b).abs() <= 1e-9 * a.abs().max(b.abs());
    assert!(near(proved.0, fresh[1].0) && near(proved.1, fresh[1].1) && proved.0 < fresh[0].0, "the proof's gains are the fresh plan's at 2 M, under the first plan's: proved {proved:?}, the plans' {fresh:?}");
    let edited = sim.t();
    assert!(sim.apply(set_rlis(1e3)), "the edit keeps the run");
    advance(&mut sim, edited + 2.5 * 32.0 * SAMPLE, &mut record);
    let st = sim.stats();
    assert!(matches!(&st.last_reproof, Some(Err(_))) && (st.reproofs, st.edit_leaves, sim.blocks()) == (2, 1, 1), "at 1 k the proof fails and the run leaves rounds at the edit's round end: {} proofs, {} leaves, {} blocks, {:?}", st.reproofs, st.edit_leaves, sim.blocks(), st.last_reproof);
    let alone = doubled(clamped_pad(1e6), true, 0).2;
    let helped = doubled(clamped_pad(1e6), true, 3).2;
    assert_eq!(alone, helped, "the proof in the pipeline on the worker alone and with three helpers");
}

/// The jumps of `split_jumps_keep_their_time`, alternately up and down by 1 V, each 1 ps wide and
/// centred on its time: a train 61.7 us apart from 110 us, its times on no sample grid and its
/// places between samples spread over the sample whatever its length, and one 1.04 us after the
/// end of the 32nd sample at 96 kHz, a round's end at each sample length of the proof.
fn jump_times() -> Vec<f64> {
    let mut at: Vec<f64> = (0..16).map(|k| 110e-6 + k as f64 * 61.7e-6).collect();
    at.push(32.1 * SAMPLE);
    at.sort_by(f64::total_cmp);
    at
}
/// The reader's R C: 4.7 k onto 10 nF.
const JUMP_TAU: f64 = 4.7e3 * 10e-9;
const JUMP_STOP: f64 = 1.2e-3;

/// The jumps from 0 V on their load: block 0 of `Jumps`.
fn pulse(c: &mut SimCircuit) {
    let mut points = vec![(0.0, 0.0)];
    for (k, at) in jump_times().into_iter().enumerate() {
        let level = (k % 2) as f64;
        points.push((at - 0.5e-12, level));
        points.push((at + 0.5e-12, 1.0 - level));
    }
    c.v("vp", "p", "0", Source::wave(Wave::Pwl(points)));
    c.r("rp", "p", "0", 1e3);
}

/// The reader: R C from the jumps' node.
fn reader(c: &mut SimCircuit) {
    c.r("ry", "p", "y", 4.7e3);
    c.c("cy", "y", "0", 10e-9);
}

/// The jumps in block 0, their reader in block 1 reading their node in the same round.
#[derive(Clone)]
struct Jumps;

impl Planner for Jumps {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, _t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let mut b0 = SimCircuit::new();
        pulse(&mut b0);
        let mut b1 = SimCircuit::new();
        let v0 = whole.x[whole.unknown("p").expect("the jumps' node") as usize];
        let node = b1.node("p");
        b1.devices.boundary.push(Boundary { name: "bp".into(), node, kind: BoundaryKind::Voltage, v0 });
        reader(&mut b1);
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        for (k, (mut c, cut)) in [(b0, &[][..]), (b1, &["p"][..])].into_iter().enumerate() {
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let mut own = seed_from(&mut s, whole);
            own.retain(|&(l, _)| !cut.contains(&s.unknowns[l as usize].name.as_str()));
            plan.blocks.push(Block { name: format!("block {k}"), stage: k as u32, own });
            solvers.push(s);
        }
        let p = solvers[0].unknown("p").expect("block 0 solves the jumps' node");
        plan.couplings.push(Coupling { from: (0, p), to: vec![(1, "bp".into())], lag: Lag::Same, cuts: Vec::new(), rho: 0.0 });
        Ok(Some((plan, solvers)))
    }
}

/// `Jumps` in Fast on the sample period `h`, its jumps split onto the reader's grid or landed on:
/// the record, and the reader's history restarts.
fn jump_run(h: f64, split: bool) -> (Record, fab_elec_sim::analysis::tran::Restarts) {
    let mut c = SimCircuit::new();
    pulse(&mut c);
    reader(&mut c);
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: h, res: h, trace: false, pipeline: false, edge_restarts: split, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the operating point converges");
    sim.set_planner(Some(Box::new(Jumps))).expect("the plan installs");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    while sim.t() < JUMP_STOP {
        sim.advance(JUMP_STOP, None, &mut record).unwrap_or_else(|d| panic!("the blocks step: {d}"));
    }
    let (_, _, work) = sim.block_work().expect("the run is in rounds");
    (record, work[1].restarts)
}

/// The reader's largest difference from its exact response on its grid points a sample or more
/// from every jump, and the time of that largest.
fn jump_error(record: &Record, h: f64) -> (f64, f64) {
    let (at, y) = (jump_times(), record.v("y"));
    let exact = |t: f64| at.iter().enumerate().filter(|&(_, &a)| t > a).map(|(k, &a)| if k % 2 == 0 { 1.0 } else { -1.0 } * (1.0 - (-(t - a) / JUMP_TAU).exp())).sum::<f64>();
    let (from, to) = ((at[0] / h).ceil() as i64 + 1, (JUMP_STOP / h).floor() as i64);
    (from..=to).map(|k| k as f64 * h).filter(|&t| at.iter().all(|&a| (t - a).abs() >= h)).map(|t| ((lerp(&record.at, &y, t) - exact(t)).abs(), t)).fold((0.0, 0.0), |m, e| if e.0 > m.0 { e } else { m })
}

/// A jump another block makes reaches its reader as a ramp a sample wide centred on the jump's
/// time (`LiveSpec::edge_restarts`, A-K1's falsifier 1): 17 jumps at times on no grid, one just past
/// a round's end, read in the same round by an R C through a boundary source, in Fast on the
/// sample period.
/// - The reader never lands on a jump: no history restart, where landing on them (edge restarts
///   off) restarts it at each.
/// - Its error against the exact response, on the grid a sample or more from every jump, falls
///   about 4 times a halving of the sample: the ramp keeps each jump's area and its time to second
///   order, wherever the jump falls between samples, the one past the round's end included.
/// - With half a sample of lookahead, both rounds read within the exported range and in jump order.
/// - The second round keeps the exact bits the first read at their shared knot.
/// - The trapezoids keep the jumps' area across both rounds, up to the last knot's half weight.
#[test]
fn split_jumps_keep_their_time() {
    let mut last = f64::NAN;
    for h in [SAMPLE, SAMPLE / 2.0, SAMPLE / 4.0] {
        let (split, restarts) = jump_run(h, true);
        let (landed, landed_restarts) = jump_run(h, false);
        let ((got, worst), (own, _)) = (jump_error(&split, h), jump_error(&landed, h));
        eprintln!("h {h:e}: split {got:e} V at {worst:e} s ({} reader restarts), landed {own:e} V ({} reader restarts), {} times the last", restarts.total(), landed_restarts.total(), last / got);
        assert_eq!(restarts.total(), 0, "at {h:e} s the reader never restarts on a split jump: {restarts:?}");
        assert!(landed_restarts.corner >= 17, "at {h:e} s landing on the jumps restarts the reader at each: {landed_restarts:?}");
        assert!(!(last / got < 3.0), "a halving of the sample to {h:e} s cuts the error about 4 times: {last:e} V to {got:e} V, at {worst:e} s");
        last = got;
    }

    use fab_elec_sim::dev::boundary::Table;
    const H: f64 = 1.0 / 1024.0;
    const N: i32 = 8;
    const K0: i32 = 48;
    let check = |offsets: &[f64], jump: f64, delay: f64| {
        let pre: f64 = 0.5;
        let post = pre + jump * offsets.len() as f64;
        let at: Vec<_> = offsets.iter().map(|&e| (f64::from(K0) + e) * H - delay).collect();
        let mut times: Vec<_> = (4 * (K0 - N - 2)..=4 * (K0 + N) + 2)
            .map(|q| f64::from(q) * (H / 4.0)).collect();
        for &tj in &at {
            times.extend([tj, tj.next_up()]);
        }
        times.sort_by(f64::total_cmp);
        let read = |start: i32, taken: Option<f64>| {
            let (t0, end) = (f64::from(start) * H, f64::from(start + N) * H);
            let known = end + H / 2.0;
            // Keep the last point at or before the lookback, through the final exporter point.
            let first = times.partition_point(|&t| t <= t0 - delay - H).saturating_sub(1);
            let past = times.partition_point(|&t| t < known - delay);
            let mut t = times[first..past].to_vec();
            t.push(known - delay);
            let v = t.iter().map(|&t| pre + jump * at.iter().filter(|&&tj| tj < t).count() as f64).collect();
            let edges: Vec<_> = t.iter().enumerate().filter(|(_, t)| at.contains(t)).map(|(i, _)| i as u32).collect();
            let table = Table { t, v, events: edges.clone(), edges };
            let fed = table.split(H, delay, t0, taken, end, known).map(|split| split.0).unwrap_or(table);
            (start..=start + N).map(|k| fed.value(f64::from(k) * H - delay)).collect::<Vec<_>>()
        };
        let left = read(K0 - N, None);
        let taken = left[N as usize];
        let right = read(K0, Some(taken));
        let case = format!("offsets {offsets:?}, jump {jump}, delay/H {}", delay / H);
        assert_eq!(right[0].to_bits(), taken.to_bits(), "taken at t0 changed from {taken} to {} for {case}", right[0]);
        let range = pre.min(post)..=pre.max(post);
        let mut area = 0.0;
        for round in [&left, &right] {
            for &v in round {
                assert!(range.contains(&v), "read {v} outside the exported range {range:?} for {case}");
            }
            for pair in round.windows(2) {
                assert!((pair[1] - pair[0]) * jump >= 0.0, "reads {pair:?} reverse jump order for {case}");
                area += (0.5 * (pair[0] + pair[1]) - pre) * H;
            }
        }
        let span = f64::from(N);
        let exact: f64 = offsets.iter().map(|&e| jump * (span - e) * H).sum();
        // For a jump d steps before the last knot the trapezoid counts (0.5 + d)/2 steps
        // of the jump there. The exporter held it for d steps.
        let endpoint: f64 = offsets.iter().map(|&e| span - e).filter(|&d| d < 0.5)
            .map(|d| 0.5 * (0.5 - d) * jump * H).sum();
        assert!((area - exact - endpoint).abs() <= 1e-9 * exact.abs(),
            "area {area} differs from exact {exact} plus endpoint term {endpoint} for {case}");
    };
    for jump in [-1.0, 1.0] {
        for delay in [0.0, 0.3 * H] {
            for i in 0..64 {
                let e = (f64::from(i) + 0.5) / 32.0 - 1.0;
                check(&[e], jump, delay);
                check(&[e, e + 1.0 / 128.0], jump, delay);
            }
            check(&[f64::from(N) - 3.0 / 16.0], jump, delay);
        }
    }
}

/// `a = s - 2 b` with `s` a 1 kHz sine of 1 V, block 0's part of `run_away`.
fn run_away_a(c: &mut SimCircuit) {
    c.v("drive", "s", "0", Source::wave(Wave::Sin { vo: 0.0, va: 1.0, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }));
    c.vctl("e1", CtlKind::Vcvs, "a", "s", "b", "0", -2.0);
    c.r("ra", "a", "0", 1e3);
}

/// `b = a`, block 1's part of `run_away`.
fn run_away_b(c: &mut SimCircuit) {
    c.vctl("e2", CtlKind::Vcvs, "b", "0", "a", "0", 1.0);
    c.r("rb", "b", "0", 1e3);
}

/// `a = 1000 s`, block 0's part of `ONE_WAY`: an ideal amplifier, far past the growth rule's bar.
fn one_way_a(c: &mut SimCircuit) {
    c.v("drive", "s", "0", Source::wave(Wave::Sin { vo: 0.0, va: 1.0, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }));
    c.vctl("e1", CtlKind::Vcvs, "a", "0", "s", "0", 1e3);
    c.r("ra", "a", "0", 1e3);
}

/// Two blocks: block 0 `a`'s part, block 1 `b`'s, reading a in the same round; with `loops`,
/// block 0 reads b a round late. The planner learns no refusals: the run declines its plan once it
/// refused the cut.
#[derive(Clone)]
struct TwoBlocks {
    a: fn(&mut SimCircuit),
    b: fn(&mut SimCircuit),
    loops: bool,
}

/// `run_away_a` and `run_away_b`, cut at b. Solved whole a is a third of the drive; read a round
/// late, `a = s - 2 b` makes `a` twice last round's, sign turned, plus the drive: it doubles each
/// round.
const RUN_AWAY: TwoBlocks = TwoBlocks { a: run_away_a, b: run_away_b, loops: true };

/// `one_way_a` and `run_away_b` (`b = a`), cut at a and read in the same round: a plan that reads
/// nothing late.
const ONE_WAY: TwoBlocks = TwoBlocks { a: one_way_a, b: run_away_b, loops: false };

impl Planner for TwoBlocks {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, _t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let boundary = |c: &mut SimCircuit, name: &str, node: &str| {
            let v0 = whole.x[whole.unknown(node).expect("the cut node is in the whole circuit") as usize];
            let node = c.node(node);
            c.devices.boundary.push(Boundary { name: name.into(), node, kind: BoundaryKind::Voltage, v0 });
        };
        let (mut b0, mut b1) = (SimCircuit::new(), SimCircuit::new());
        (self.a)(&mut b0);
        if self.loops {
            boundary(&mut b0, "bb", "b");
        }
        (self.b)(&mut b1);
        boundary(&mut b1, "ba", "a");
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        for (k, (mut c, cut)) in [(b0, "b"), (b1, "a")].into_iter().enumerate() {
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let mut own = seed_from(&mut s, whole);
            own.retain(|&(l, _)| s.unknowns[l as usize].name != cut);
            plan.blocks.push(Block { name: format!("block {k}"), stage: k as u32, own });
            solvers.push(s);
        }
        let b = solvers[1].unknown("b").expect("block 1 solves b");
        let a = solvers[0].unknown("a").expect("block 0 solves a");
        if self.loops {
            plan.couplings.push(Coupling { from: (1, b), to: vec![(0, "bb".into())], lag: Lag::Round, cuts: Vec::new(), rho: 0.0 });
        }
        plan.couplings.push(Coupling { from: (0, a), to: vec![(1, "ba".into())], lag: Lag::Same, cuts: Vec::new(), rho: 0.0 });
        Ok(Some((plan, solvers)))
    }
}

/// `c = 10 a`: block 3 of `UPSTREAM`, reading a in the same round: past the growth rule's bar
/// while a is a tenth of it.
fn tenfold_c(c: &mut SimCircuit) {
    c.vctl("e3", CtlKind::Vcvs, "c", "0", "a", "0", 10.0);
    c.r("rc", "c", "0", 1e3);
}

/// `d = c / 1000`: block 2 of `UPSTREAM`, reading c a round late, far within what it can hold.
fn reader_d(c: &mut SimCircuit) {
    c.vctl("e4", CtlKind::Vcvs, "d", "0", "c", "0", 1e-3);
    c.r("rd", "d", "0", 1e3);
}

/// `e = s / 2`: block 2 of `HUB`, reading the drive in the same round.
fn half_s(c: &mut SimCircuit) {
    c.vctl("e5", CtlKind::Vcvs, "e", "0", "s", "0", 0.5);
    c.r("re", "e", "0", 1e3);
}

/// `q = e`: in block 0 of `HUB`, what it reads of block 2, a round late; nothing else in it reads q.
fn copy_e(c: &mut SimCircuit) {
    c.vctl("e6", CtlKind::Vcvs, "q", "0", "e", "0", 1.0);
    c.r("rq", "q", "0", 1e3);
}

/// A block of a `Wired` plan: its parts, its stage, and its boundaries (name, node).
type WiredBlock = (&'static [fn(&mut SimCircuit)], u32, &'static [(&'static str, &'static str)]);
/// A coupling of a `Wired` plan: the exporting block and node, its readers (block, boundary), its lag.
type Wire = (u32, &'static str, &'static [(u32, &'static str)], Lag);

/// `RUN_AWAY` and two more blocks: d (stage 1) reading c a round late, and c (stage 2) reading a
/// in the same round. The plan reads late at b (block 0's read) and at c (d's); what reaches c
/// within a round is a, and through a block 0's read of b. The loop through c is none: d reads it
/// late and reaches nothing.
const UPSTREAM: (&[WiredBlock], &[Wire]) = (
    &[(&[run_away_a], 0, &[("bb", "b")]), (&[run_away_b], 1, &[("ba", "a")]), (&[reader_d], 1, &[("dc", "c")]), (&[tenfold_c], 2, &[("ca", "a")])],
    &[(1, "b", &[(0, "bb")], Lag::Round), (0, "a", &[(1, "ba"), (3, "ca")], Lag::Same), (3, "c", &[(2, "dc")], Lag::Round)],
);

/// `RUN_AWAY` with a hub: block 0 also gives the drive to block 2 (`e = s / 2`) in the same round
/// and reads e back a round late (`q = e`), a loop that holds. Block 0 reads late at b and at e;
/// the loop that runs away goes through a and b only.
const HUB: (&[WiredBlock], &[Wire]) = (
    &[(&[run_away_a, copy_e], 0, &[("bb", "b"), ("qe", "e")]), (&[run_away_b], 1, &[("ba", "a")]), (&[half_s], 1, &[("es", "s")])],
    &[(1, "b", &[(0, "bb")], Lag::Round), (0, "a", &[(1, "ba")], Lag::Same), (0, "s", &[(2, "es")], Lag::Same), (2, "e", &[(0, "qe")], Lag::Round)],
);

/// A plan as wired by hand: each block's parts with voltage boundaries at the nodes it reads, and
/// the couplings between them. The planner learns no refusals.
#[derive(Clone)]
struct Wired(&'static [WiredBlock], &'static [Wire]);

impl Planner for Wired {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, _t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        for (k, &(parts, stage, reads)) in self.0.iter().enumerate() {
            let mut c = SimCircuit::new();
            for part in parts {
                part(&mut c);
            }
            for &(name, node) in reads {
                let v0 = whole.x[whole.unknown(node).expect("the cut node is in the whole circuit") as usize];
                let at = c.node(node);
                c.devices.boundary.push(Boundary { name: name.into(), node: at, kind: BoundaryKind::Voltage, v0 });
            }
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let mut own = seed_from(&mut s, whole);
            own.retain(|&(l, _)| !reads.iter().any(|r| s.unknowns[l as usize].name == r.1));
            plan.blocks.push(Block { name: format!("block {k}"), stage, own });
            solvers.push(s);
        }
        plan.couplings = self.1.iter().map(|&(from, node, to, lag)| Coupling {
            from: (from, solvers[from as usize].unknown(node).expect("the block solves its node")),
            to: to.iter().map(|&(k, name)| (k, name.into())).collect(), lag, cuts: Vec::new(), rho: 0.0,
        }).collect();
        Ok(Some((plan, solvers)))
    }
}

/// A `Record` that drops the points a run going back superseded (`Sink::rewind`), with the times
/// it went back to.
#[derive(Default)]
struct Rewound(Record, Vec<f64>);

impl Sink for Rewound {
    fn begin(&mut self, u: &[UnknownInfo]) {
        self.0.begin(u);
    }
    fn point(&mut self, at: f64, outer: Option<f64>, x: &[f64]) {
        self.0.point(at, outer, x);
    }
    fn rewind(&mut self, at: f64, _saved: Option<&dyn SinkState>, _unknowns: &[UnknownInfo]) {
        let n = self.0.at.partition_point(|&t| t <= at);
        self.0.at.truncate(n);
        self.0.outer.truncate(n);
        self.0.x.truncate(n);
        self.1.push(at);
    }
}

/// The growth rule (`livesim::HOLD`): a loop that reading a value a round late sets running away
/// (`RUN_AWAY`, its bar 100 V from its 1 V drive) stops in the round that passes the bar. A run in
/// those rounds, in Fast and playing (a paused run keeps no snapshots), with snapshots kept at its
/// start and 0.3 ms in, and one keeping none:
/// - goes back once, to the newest restore point before the round that broke the bar: the snapshot
///   at 0.3 ms, or, keeping none, the anchor where its rounds began (`LiveSim::take_anchor`: taken
///   only by a run that keeps no snapshot), at its start; its sinks with it; and refuses the cut
///   read late at b there;
/// - declines the plan, made again (going in half a millisecond on), that reads late at b, and
///   steps on whole;
/// - publishes nothing past the bar: from the restore point to the end, `a` is a third of the
///   drive at every point, to a nanovolt;
/// - made again from its timeline (`LiveSim::follow`, the refusal with it), keeping no snapshots,
///   is the same run, bit for bit, and never breaches.
#[test]
fn a_loop_that_runs_away_in_rounds_goes_back_and_refuses_its_cut() {
    const END: f64 = 3e-3;
    // a re-plan goes in half a millisecond after it starts, made on the worker
    let replan = Replan { lead: 0.5e-3, slow: f64::INFINITY, ..Replan::default() };
    let new = |span: f64| {
        let mut c = SimCircuit::new();
        run_away_a(&mut c);
        run_away_b(&mut c);
        let snapshots = Snapshots { span, every: Duration::ZERO, share: f64::INFINITY, bytes: u64::MAX };
        let mut sim = LiveSim::new(c, ValuePlan::default(), 1, LiveSpec { round: 16, snapshots, playing: true, replan: replan.clone(), ..spec(SAMPLE, SAMPLE) }).expect("the operating point converges");
        sim.set_planner(Some(Box::new(RUN_AWAY))).expect("the plan installs");
        let mut sink = Rewound::default();
        sink.begin(&sim.solver().unknowns);
        (sim, sink)
    };
    let advance = |sim: &mut LiveSim, sink: &mut Rewound, to: f64| {
        while sim.t() < to {
            sim.advance(to, None, sink).unwrap_or_else(|d| panic!("the run steps: {d}"));
        }
    };
    for (name, span) in [("snapshots kept", 1.0), ("no snapshots", 0.0)] {
        let (mut sim, mut sink) = new(span);
        sim.keep_snapshot(&sink);
        advance(&mut sim, &mut sink, 0.3e-3);
        let kept = if span > 0.0 { sim.t() } else { 0.0 };
        sim.keep_snapshot(&sink);
        let st = sim.stats();
        assert_eq!((st.snapshots_kept, st.breaches, st.anchors), if span > 0.0 { (2, 0, 0) } else { (0, 0, 1) }, "{name}: the snapshots kept and the anchors taken, the loop still within its bar at {:e} s", sim.t());
        advance(&mut sim, &mut sink, END);
        let st = sim.stats();
        let why = st.last_breach.clone().unwrap_or_default();
        assert!(st.breaches == 1 && st.rewinds == 1 && sink.1 == [kept] && why.contains("late at b refused"),
            "{name}: the loop past its bar takes the run back once, to {kept:e} s, the cut at b refused: {} breaches, {} rewinds, back to {:?}: {why}", st.breaches, st.rewinds, sink.1);
        let declined = st.plan_error.as_ref().map(|d| d.message.clone()).unwrap_or_default();
        assert!(declined.contains("refused"), "{name}: the plan made again reads late at b and is declined: {declined:?}");
        let (a, s) = (sink.0.v("a"), sink.0.v("s"));
        let off = sink.0.at.iter().zip(a.iter().zip(&s)).filter(|p| *p.0 > kept).map(|(&t, (a, s))| ((a - s / 3.0).abs(), t)).fold((0.0, 0.0), |w, p| if p.0 > w.0 { p } else { w });
        assert!(sink.0.at.last().is_some_and(|&t| t >= END) && off.0 < 1e-9, "{name}: from {kept:e} s to the end a is a third of the drive: off by {:e} V at {:e} s", off.0, off.1);
        let refusals = sim.refusals().to_vec();
        assert!(refusals.len() == 1 && refusals[0].0 == kept && refusals[0].1 == ["b"], "{name}: the refusal of b at {kept:e} s is on the timeline: {refusals:?}");
        let (mut again, mut got) = new(0.0);
        again.follow(sim.timeline(), &sim.go_ins(), &refusals, sim.landings(), None);
        advance(&mut again, &mut got, END);
        assert_eq!(again.stats().breaches, 0, "{name}: made again with the refusal, the run never breaches");
        assert!(got.0.at == sink.0.at && got.0.x == sink.0.x, "{name}: made again from its timeline, the same run: {} points against {}", got.0.at.len(), sink.0.at.len());
    }
}

/// The growth rule's blame (`Rounds::blame`): a breach refuses the late reads that close a loop
/// through the value that broke within a round, and with none, those upstream of it within the
/// round, not every one. In `HUB`, block 0 reads b and e late and the loop through a and b runs
/// away: the run refuses b alone, not e. In `UPSTREAM`, c (ten times a, read in the same round)
/// passes the bar while a is a tenth of it, and no loop goes through c: the run refuses b, block
/// 0's late read that reaches c through a, and not d's read of c, downstream of it. Each goes back
/// once, and made again from its timeline it is the same run, bit for bit.
#[test]
fn a_breach_refuses_the_late_reads_on_its_loop_or_upstream_of_it() {
    const END: f64 = 3e-3;
    let replan = Replan { lead: 0.5e-3, slow: f64::INFINITY, ..Replan::default() };
    for (name, (blocks, wires), first) in [("the hub", HUB, &["a = ", "b = "][..]), ("upstream", UPSTREAM, &["c = "][..])] {
        let new = || {
            let mut c = SimCircuit::new();
            for part in blocks.iter().flat_map(|b| b.0) {
                part(&mut c);
            }
            let mut sim = LiveSim::new(c, ValuePlan::default(), 1, LiveSpec { round: 16, playing: true, replan: replan.clone(), ..spec(SAMPLE, SAMPLE) }).expect("the operating point converges");
            sim.set_planner(Some(Box::new(Wired(blocks, wires)))).expect("the plan installs");
            let mut sink = Rewound::default();
            sink.begin(&sim.solver().unknowns);
            (sim, sink)
        };
        let advance = |sim: &mut LiveSim, sink: &mut Rewound| {
            while sim.t() < END {
                sim.advance(END, None, sink).unwrap_or_else(|d| panic!("{name}: the run steps: {d}"));
            }
        };
        let (mut sim, mut sink) = new();
        assert_eq!(sim.blocks(), blocks.len(), "{name}: the plan's blocks go in");
        advance(&mut sim, &mut sink);
        let st = sim.stats();
        let why = st.last_breach.clone().unwrap_or_default();
        let refusals = sim.refusals().to_vec();
        assert!(st.breaches == 1 && first.iter().any(|f| why.starts_with(f)) && refusals.len() == 1 && refusals[0].1 == ["b"],
            "{name}: past the bar at {first:?}, the run refuses b alone: {} breaches, {refusals:?}: {why}", st.breaches);
        let (mut again, mut got) = new();
        again.follow(sim.timeline(), &sim.go_ins(), &refusals, sim.landings(), None);
        advance(&mut again, &mut got);
        assert!(again.stats().breaches == 0 && got.0.at == sink.0.at && got.0.x == sink.0.x,
            "{name}: made again from its timeline, the same run: {} breaches, {} points against {}", again.stats().breaches, got.0.at.len(), sink.0.at.len());
    }
}

/// `a = s - b / 2`, block 0's part of a loop that holds read late but is far from what it is solved
/// whole: what reading b a round late changes, the shadow rounds see.
fn half_a(c: &mut SimCircuit) {
    c.v("drive", "s", "0", Source::wave(Wave::Sin { vo: 0.0, va: 1.0, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }));
    c.vctl("e1", CtlKind::Vcvs, "a", "s", "b", "0", -0.5);
    c.r("ra", "a", "0", 1e3);
}

/// `a = s - b / 10^4`: the same loop, so light that reading b late changes a by little.
fn light_a(c: &mut SimCircuit) {
    c.v("drive", "s", "0", Source::wave(Wave::Sin { vo: 0.0, va: 1.0, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }));
    c.vctl("e1", CtlKind::Vcvs, "a", "s", "b", "0", -1e-4);
    c.r("ra", "a", "0", 1e3);
}

/// The shadow rounds (`Rounds::shadows_post`): a late-reading block's rounds run again with what it
/// read late taken on time. Block 0 of `a = s - g b`, `b = a`, reads b a round late; taken again on
/// time, a ends at `s - g b(end)`, and b on time there is the a the round ran to (block 1 reads a in
/// the same round), so where it ends differs by `|(1 + g) a(end) - s(end)|` (its own end figure,
/// against the 1 V drive). Every round taken runs again, none failing.
/// The rig's plans promise what the block planner's do for an ordinary value read late
/// (`RoundPlan::late_tol`, `Promising`).
/// - g 10^-4: the largest stays under that promise, and the run is in its two blocks to the end,
///   refusing nothing.
/// - g a half: the largest is that, to a nanovolt, at the round it names, past the bar where a block
///   ends is held to (`SHADOW_ACT` times the promise). Within the burst after its plan went in, at a
///   round start after that round, the run refuses the cut at b (`LiveSim::shadow_taken`), on its
///   timeline, and declines the plan made again (the planner learns no refusals), stepping whole:
///   from there a is the drive over 1.5 at every point, to a nanovolt. Made again from its timeline
///   (`LiveSim::follow`), it is the same run, bit for bit, its own shadow rounds refusing nothing.
/// - Every round run again has its end's shift counted (`end_shift`). On records made by hand (a
///   round of 10 us held to 3.2e-2 of 5 V; a ramp of 0.1 V a us and a square): a square that rises
///   1 us earlier again ends a whole swing apart but is shifted by that, as ran behind, within a
///   step bound of 2 us, and past it under one of 0.5 us, where its crossing is no longer the one
///   at the end; a level a quarter volt apart is past the bar however shifted, and so is a ramp two
///   microseconds ahead with no crossing near the end (a shift is credited to an edge only); a
///   crossing more away from the end, the counts disagree; and a ramp half a microsecond ahead again
///   ends within the bar, shifted by that along its last step.
#[test]
fn a_round_run_again_with_its_late_values_on_time_sees_what_lateness_did_and_past_its_bar_refuses_the_cut() {
    const END: f64 = 20e-3;
    // the block planner's promise for an ordinary value read late (`PartOpts::cut_tol`)
    const BAR: f64 = 1e-3;
    // a re-plan goes in half a millisecond after it starts, made on the worker
    let replan = Replan { lead: 0.5e-3, slow: f64::INFINITY, ..Replan::default() };
    // the rounds after a plan goes in that every reader's every round runs again (`SHADOW_BURST`)
    const BURST: f64 = 32.0;
    let round = 16.0 * SAMPLE;
    let new = |a: fn(&mut SimCircuit)| {
        let mut c = SimCircuit::new();
        a(&mut c);
        run_away_b(&mut c);
        let mut sim = LiveSim::new(c, ValuePlan::default(), 1, LiveSpec { round: 16, playing: true, replan: replan.clone(), ..spec(SAMPLE, SAMPLE) }).expect("the operating point converges");
        sim.set_planner(Some(Box::new(Promising(TwoBlocks { a, b: run_away_b, loops: true }, BAR)))).expect("the plan installs");
        let mut sink = Record::default();
        sink.begin(&sim.solver().unknowns);
        (sim, sink)
    };
    let advance = |sim: &mut LiveSim, sink: &mut Record| {
        while sim.t() < END {
            sim.advance(END, None, sink).unwrap_or_else(|d| panic!("the run steps: {d}"));
        }
    };
    let end = SHADOW_FIGURES.iter().position(|&f| f == "own end").expect("a figure of where a block ends");

    let (mut light, mut light_sink) = new(light_a);
    advance(&mut light, &mut light_sink);
    let st = light.stats().clone();
    let sp = &st.round_split;
    assert!(sp.shadows > 0 && sp.shadow_failed == 0 && sp.shadow_worst[end] < BAR && sp.shadow_bar == SHADOW_ACT * BAR,
        "g 1e-4: rounds run again, none failing, where block 0 ends under the plan's bar, held to {SHADOW_ACT} times it: {} run, {} failed, {:e}, held to {:e}", sp.shadows, sp.shadow_failed, sp.shadow_worst[end], sp.shadow_bar);
    assert_eq!((light.blocks(), st.breaches, st.shadow_acts), (2, 0, 0), "g 1e-4: the loop runs in its two blocks to the end, refusing nothing");
    assert_eq!(sp.shift_plain + sp.shift_still + sp.shift_found + sp.shift_over, sp.shadows, "g 1e-4: every round run again has its end's shift counted");

    let (mut half, mut sink) = new(half_a);
    advance(&mut half, &mut sink);
    let st = half.stats().clone();
    let sp = &st.round_split;
    assert!(sp.shadows > 0 && sp.shadow_failed == 0, "g a half: rounds run again, none failing: {} run, {} failed", sp.shadows, sp.shadow_failed);
    // the round the largest names, and what lateness did there, from the run's own record
    let at: f64 = sp.shadow_at[end].rsplit(" at ").next().and_then(|t| t.trim_end_matches(" s").parse().ok()).expect("the largest names its round's end");
    let k = sink.at.iter().enumerate().min_by(|x, y| (x.1 - at).abs().total_cmp(&(y.1 - at).abs())).map(|(k, _)| k).expect("the run has points");
    let did = (1.5 * sink.v("a")[k] - sink.v("s")[k]).abs();
    assert!((sp.shadow_worst[end] - did).abs() < 1e-9 && did > sp.shadow_bar,
        "g a half: the largest end figure is what reading b late did to a at {at:.6} s, |1.5 a(end) - s(end)| = {did:.9} V, past {:e}: {:.9}", sp.shadow_bar, sp.shadow_worst[end]);
    let refusals = half.refusals().to_vec();
    let why = st.last_shadow_act.clone().unwrap_or_default();
    assert!(st.shadow_acts == 1 && st.breaches == 0 && refusals.len() == 1 && refusals[0].1 == ["b"] && refusals[0].0 > at && refusals[0].0 < BURST * round && why.contains("late at b refused"),
        "g a half: past its bar, the cut at b refused once, on the timeline, after the round and within the burst after the plan went in: {} acts, {} breaches, {refusals:?}: {why}", st.shadow_acts, st.breaches);
    let declined = st.plan_error.as_ref().map(|d| d.message.clone()).unwrap_or_default();
    assert!(half.blocks() == 1 && declined.contains("refused"), "g a half: the plan made again reads late at b and is declined, the run whole: {} blocks, {declined:?}", half.blocks());
    let (a, s, from) = (sink.v("a"), sink.v("s"), refusals[0].0);
    let off = sink.at.iter().zip(a.iter().zip(&s)).filter(|p| *p.0 > from).map(|(&t, (a, s))| ((1.5 * a - s).abs(), t)).fold((0.0, 0.0), |w, p| if p.0 > w.0 { p } else { w });
    assert!(sink.at.last().is_some_and(|&t| t >= END) && off.0 < 1e-9, "g a half: from {from:e} s to the end a is the drive over 1.5: off by {:e} V at {:e} s", off.0, off.1);
    let (mut again, mut got) = new(half_a);
    again.follow(half.timeline(), &half.go_ins(), &refusals, half.landings(), None);
    advance(&mut again, &mut got);
    assert!(again.stats().shadow_acts == 0 && got.at == sink.at && got.x == sink.x,
        "g a half: made again from its timeline, the same run, its own shadow rounds refusing nothing: {} acts, {} points against {}", again.stats().shadow_acts, got.at.len(), sink.at.len());

    // records made by hand: points (us, ramp, square), crossings (how many, the last in us)
    type Hand = (Vec<f64>, Vec<f64>, u32, f64);
    fn hand(points: &[(f64, f64, f64)], crossings: u32, last: f64) -> Hand {
        (points.iter().map(|p| p.0 * 1e-6).collect(), points.iter().flat_map(|p| [p.1, p.2]).collect(), crossings, last * 1e-6)
    }
    fn rec(h: &Hand) -> EndRecord<'_> {
        EndRecord { t: &h.0, vals: &h.1, crossings: h.2, last_crossing: h.3 }
    }
    fn shift(ran: &Hand, again: &Hand, tau_bar: f64) -> (EndShift, bool) {
        end_shift(0.0, 10e-6, &[1.0, 0.0], &[true, true], rec(ran), rec(again), 5.0, 3.2e-2, tau_bar)
    }
    let ramp = |t: f64| 1.0 + 0.1 * t;
    let low = |ts: &[f64], lead: f64, square: f64| ts.iter().map(|&t| (t, ramp(t + lead), square)).collect::<Vec<_>>();
    let ran = hand(&low(&[2.0, 4.0, 6.0, 8.0, 10.0], 0.0, 0.0), 1, 3.0);
    let mut rises = low(&[2.0, 4.0, 6.0, 8.0, 9.0], 0.0, 0.0);
    rises.extend([(9.01, ramp(9.01), 5.0), (10.0, ramp(10.0), 5.0)]);
    let rises = hand(&rises, 2, 9.01);
    let moved = shift(&ran, &rises, 2e-6);
    assert!(matches!(moved, (EndShift::Shifted(tau, _), true) if (-1e-6..-0.999e-6).contains(&tau)),
        "by hand: a square risen 1 us earlier again is shifted by that, as ran behind, its crossing the one at the end: {moved:?}");
    assert!(matches!(moved, (EndShift::Shifted(_, figure), _) if (0.0199..=3.2e-2 + 1e-12).contains(&figure)),
        "by hand: once shifted the ends are within the bar, no nearer than the ramp's 0.1 V of 5 V leaves: {moved:?}");
    assert_eq!(shift(&ran, &rises, 0.5e-6), (EndShift::Over, false), "by hand: under a step bound of 0.5 us that shift is past the bar, its crossing too far from the end");
    assert_eq!(shift(&ran, &hand(&low(&[2.0, 4.0, 6.0, 8.0, 10.0], 0.0, 0.25), 1, 3.0), 2e-6), (EndShift::Over, true), "by hand: a level a quarter volt apart is past the bar however shifted");
    assert_eq!(shift(&ran, &hand(&low(&[2.0, 4.0, 6.0, 8.0, 10.0], 2.0, 0.0), 1, 3.0), 2e-6), (EndShift::Over, true), "by hand: a ramp 2 us ahead, its crossing far from the end, is past the bar however shifted");
    assert!(!shift(&ran, &hand(&low(&[2.0, 4.0, 6.0, 8.0, 10.0], 0.0, 0.0), 2, 5.0), 2e-6).1, "by hand: a crossing more away from the end, the counts disagree");
    let ahead = shift(&ran, &hand(&low(&[2.0, 4.0, 6.0, 8.0, 10.0], 0.5, 0.0), 1, 3.0), 2e-6);
    assert!(matches!(ahead, (EndShift::Plain(Some(tau)), true) if (tau + 0.5e-6).abs() < 1e-12),
        "by hand: a ramp half a microsecond ahead again ends within the bar, as ran behind by that along its last step: {ahead:?}");
}

/// A Schmitt-trigger RC oscillator (`oscillator`'s, rf 39 kohm onto 10 nF: about 3.1 kHz) with a
/// second timing resistor `rx` onto its timing node from x, which another block buffers from its
/// output; its nodes and devices named with `k` after them.
fn timed(c: &mut SimCircuit, k: &str, rx: f64) {
    c.model("lo", ModelKind::Sw, &[("vt", 2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    c.model("hi", ModelKind::Sw, &[("vt", -2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    let (v, o, i, x) = (format!("v{k}"), format!("o{k}"), format!("i{k}"), format!("x{k}"));
    c.v(&format!("vs{k}"), &v, "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.0, tr: 10e-6, tf: 10e-6, pw: 1.0, per: 2.0 }));
    c.sw(&format!("slo{k}"), &o, "0", &i, "0", "lo", false);
    c.sw(&format!("shi{k}"), &v, &o, "0", &i, "hi", true);
    c.r(&format!("rf{k}"), &o, &i, 39e3);
    c.r(&format!("rx{k}"), &x, &i, rx);
    c.c(&format!("ct{k}"), &i, "0", 10e-9);
    c.c(&format!("co{k}"), &o, "0", 10e-12);
}

/// `timed` with rx 300 times rf.
fn timed_near(c: &mut SimCircuit) {
    timed(c, "", 300.0 * 39e3);
}

/// `timed` with rx 30000 times rf.
fn timed_far(c: &mut SimCircuit) {
    timed(c, "", 3e4 * 39e3);
}

/// `timed_far`, its nodes and devices named with 2 after them.
fn timed_far2(c: &mut SimCircuit) {
    timed(c, "2", 3e4 * 39e3);
}

/// `x = o`: block 1, the oscillator's output buffered.
fn buffer_x(c: &mut SimCircuit) {
    c.vctl("ex", CtlKind::Vcvs, "x", "0", "o", "0", 1.0);
    c.r("rl", "x", "0", 10e3);
}

/// `x2 = o2`: `timed_far2`'s output buffered.
fn buffer_x2(c: &mut SimCircuit) {
    c.vctl("ex2", CtlKind::Vcvs, "x2", "0", "o2", "0", 1.0);
    c.r("rl2", "x2", "0", 10e3);
}

/// The oscillator (`timed_near`) in block 0, reading x a round late; its output buffered in block
/// 1, reading o in the same round.
const TIMED_NEAR: (&[WiredBlock], &[Wire]) = (
    &[(&[timed_near], 0, &[("bx", "x")]), (&[buffer_x], 1, &[("bo", "o")])],
    &[(1, "x", &[(0, "bx")], Lag::Round), (0, "o", &[(1, "bo")], Lag::Same)],
);

/// `TIMED_NEAR` with `timed_far`.
const TIMED_FAR: (&[WiredBlock], &[Wire]) = (
    &[(&[timed_far], 0, &[("bx", "x")]), (&[buffer_x], 1, &[("bo", "o")])],
    &[(1, "x", &[(0, "bx")], Lag::Round), (0, "o", &[(1, "bo")], Lag::Same)],
);

/// An oscillator whose timing resistor's far end is read a round late (`timed`: block 0 reads x,
/// its output buffered in block 1): each edge, the far end keeps its old level for a round, so the
/// edge after comes late, every time. Each round's end, read on time, is within its bar (the
/// shadow rounds see no round past it however shifted, none with crossings apart), but its edges
/// move one way: its shifts (`ShiftSums`), taken in its rounds with own crossings, drift. The
/// rig's plans promise what the block planner's do for an ordinary value read late
/// (`Promising`), and drift and smear are held to that promise (`Rounds::shift_bar`).
/// - rx 300 times rf: the drift, clear of its scatter, passes the promise, and the run refuses the
///   cut at x by it within the burst after its plan went in (`LiveSim::shadow_taken`), and declines
///   the plan made again, stepping whole.
/// - rx 30000 times rf: the drift is clear of its scatter, but about a hundredth of the near one
///   (it goes as the far end's pull), under the promise: the run stays in its two blocks to the
///   end, refusing nothing.
#[test]
fn edges_that_a_late_read_moves_one_way_refuse_the_cut_by_their_drift() {
    const END: f64 = 20e-3;
    const BAR: f64 = 1e-3;
    const BURST: f64 = 32.0;
    let round = 16.0 * SAMPLE;
    let replan = Replan { lead: 0.5e-3, slow: f64::INFINITY, ..Replan::default() };
    let mut drifts = Vec::new();
    for (name, (blocks, wires), refused) in [("near", TIMED_NEAR, true), ("far", TIMED_FAR, false)] {
        let mut c = SimCircuit::new();
        for part in blocks.iter().flat_map(|b| b.0) {
            part(&mut c);
        }
        let mut sim = LiveSim::new(c, ValuePlan::default(), 1, LiveSpec { round: 16, playing: true, replan: replan.clone(), ..spec(SAMPLE, SAMPLE) }).expect("the operating point converges");
        sim.set_planner(Some(Box::new(Promising(Wired(blocks, wires), BAR)))).expect("the plan installs");
        assert_eq!(sim.blocks(), 2, "{name}: the plan's blocks go in");
        let mut sink = Record::default();
        sink.begin(&sim.solver().unknowns);
        while sim.t() < END {
            sim.advance(END, None, &mut sink).unwrap_or_else(|d| panic!("{name}: the run steps: {d}"));
        }
        let st = sim.stats().clone();
        let sp = &st.round_split;
        let sums = sp.shift_by.get("block 0").copied().unwrap_or_default();
        assert!(sp.shadows > 0 && sp.shadow_failed == 0 && sp.shift_over == 0 && sp.shift_parity == 0,
            "{name}: rounds run again, none failing, none past its bar however shifted, none with its crossings apart: {} run, {} failed, {} over, {} apart", sp.shadows, sp.shadow_failed, sp.shift_over, sp.shift_parity);
        assert!(sums.drifts() && sums.sum < 0.0, "{name}: the oscillator's edges come late, one way, clear of their scatter: {sums:?}");
        drifts.push(sums.drift());
        let refusals = sim.refusals().to_vec();
        let why = st.last_shadow_act.clone().unwrap_or_default();
        if refused {
            assert!(st.shadow_acts == 1 && st.breaches == 0 && refusals.len() == 1 && refusals[0].1 == ["x"] && refusals[0].0 < BURST * round && why.contains(" drift ") && why.contains("late at x refused"),
                "{name}: the drift past the promise refuses the cut at x once, within the burst after the plan went in: {} acts, {} breaches, {refusals:?}: {why}", st.shadow_acts, st.breaches);
            let declined = st.plan_error.as_ref().map(|d| d.message.clone()).unwrap_or_default();
            assert!(sim.blocks() == 1 && declined.contains("refused"), "{name}: the plan made again reads late at x and is declined, the run whole: {} blocks, {declined:?}", sim.blocks());
        } else {
            assert!(st.shadow_acts == 0 && refusals.is_empty() && sim.blocks() == 2 && sums.drift() < BAR,
                "{name}: the drift under the promise, the run in its two blocks to the end, refusing nothing: {} acts, {refusals:?}, {} blocks, drift {:e}", st.shadow_acts, sim.blocks(), sums.drift());
        }
    }
    assert!(drifts[0] > BAR && (50.0..200.0).contains(&(drifts[0] / drifts[1])), "the drift goes as the far end's pull: the far one about a hundredth of the near one's, which passes the promise: {drifts:?}");
}

/// A divider on a source of its own, sharing only ground with the rest: on no loop between
/// blocks, so an edit of it keeps the run in its rounds.
fn divider(c: &mut SimCircuit) {
    c.v("vd", "w", "0", Source::dc(1.0));
    c.r("rd1", "w", "d", 10e3);
    c.r("rd2", "d", "0", 10e3);
}

/// `TIMED_NEAR` with a divider in its first block (`divider`) and, apart from it, `TIMED_FAR`
/// named with 2 (`timed_far2`, `buffer_x2`): four blocks, the oscillators each reading their far
/// end late.
const PAIRED: (&[WiredBlock], &[Wire]) = (
    &[(&[timed_near, divider], 0, &[("bx", "x")]), (&[buffer_x], 1, &[("bo", "o")]), (&[timed_far2], 1, &[("bx2", "x2")]), (&[buffer_x2], 2, &[("bo2", "o2")])],
    &[(1, "x", &[(0, "bx")], Lag::Round), (0, "o", &[(1, "bo")], Lag::Same), (3, "x2", &[(2, "bx2")], Lag::Round), (2, "o2", &[(3, "bo2")], Lag::Same)],
);

/// `PAIRED` with the cut at x merged: the near oscillator and its buffer one block, nothing read
/// late at x; the far pair as before, three blocks.
const PAIRED_MERGED: (&[WiredBlock], &[Wire]) = (
    &[(&[timed_near, buffer_x, divider], 0, &[]), (&[timed_far2], 1, &[("bx2", "x2")]), (&[buffer_x2], 2, &[("bo2", "o2")])],
    &[(2, "x2", &[(1, "bx2")], Lag::Round), (1, "o2", &[(2, "bo2")], Lag::Same)],
);

/// A planner of `PAIRED` that learns a refusal of the cut at x and then plans `PAIRED_MERGED`, its
/// plans promising what the block planner's do (`Promising`).
#[derive(Clone, Default)]
struct Merging {
    refused: bool,
}

impl Planner for Merging {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let (blocks, wires) = if self.refused { PAIRED_MERGED } else { PAIRED };
        Promising(Wired(blocks, wires), 1e-3).plan(ckt, whole, t)
    }
    fn refuse_cut(&mut self, node: &str) {
        self.refused |= node == "x";
    }
}

/// What a run refused, the host's next run of the circuit starts with (`LiveSim::set_memory`,
/// Fable's A-next step 5), and a plan on its way at a refusal is made again with it
/// (`remake_plan_on_way`). `PAIRED`, its planner learning refusals (`Merging`), its re-plans going
/// in 5 ms after they start:
/// - the first run, an edit of the divider 1 ms in keeping it in its rounds and starting a
///   re-plan, refuses the cut at x by the near oscillator's drift while that plan is on its way:
///   the plan is dropped (it never goes in) and made again from the refusal, the merged plan,
///   which goes in a lead later; no plan is declined;
/// - a second run of the circuit at the first's part values (the edit's value set at its start)
///   has the refusal on its timeline at its start and starts on the merged plan; it never leaves
///   it, no plan going in after, and the shadow rounds judge it from its first rounds as any plan
///   (every round of the far oscillator's for the burst after it went in). Made again from its
///   timeline, it is the same run, bit for bit;
/// - a run of the circuit at its own part values (no edit) is another key: it recalls nothing and
///   starts on the four blocks.
#[test]
fn a_refusal_is_remembered_for_the_circuit_and_a_plan_on_its_way_is_made_again_with_it() {
    const END: f64 = 20e-3;
    const BURST: usize = 32;
    let replan = Replan { edit: 0.2e-3, lead: 5e-3, slow: f64::INFINITY, ..Replan::default() };
    let spec = LiveSpec { round: 16, playing: true, replan: replan.clone(), ..spec(SAMPLE, SAMPLE) };
    let whole = || {
        let mut c = SimCircuit::new();
        for part in PAIRED.0.iter().flat_map(|b| b.0) {
            part(&mut c);
        }
        c
    };
    let memory = StateCache::default();
    let edited = [("rd2".to_string(), "value".to_string(), 20e3)];
    let run = |tuned: &[(String, String, f64)], follow: Option<&LiveSim>| {
        let mut sim = LiveSim::new_tuned(whole(), ValuePlan::default(), 1, spec.clone(), None, tuned).expect("the operating point converges");
        match follow {
            Some(first) => sim.follow(first.timeline(), &first.go_ins(), first.refusals(), first.landings(), first.carried_cut()),
            None => sim.set_memory(Some(memory.clone())),
        }
        sim.set_planner(Some(Box::new(Merging::default()))).expect("the plan installs");
        sim
    };
    let advance = |sim: &mut LiveSim, to: f64, sink: &mut Record| {
        while sim.t() < to {
            sim.advance(to, None, sink).unwrap_or_else(|d| panic!("the run steps: {d}"));
        }
    };

    let mut first = run(&[], None);
    assert_eq!(first.blocks(), 4, "first: the four blocks");
    let mut sink = Record::default();
    sink.begin(&first.solver().unknowns);
    advance(&mut first, 1e-3, &mut sink);
    assert!(first.apply(LiveCmd::SetParam { generation: 1, device: "rd2".into(), key: "value".into(), value: edited[0].2 }), "first: the edit keeps the run");
    assert_eq!(first.blocks(), 4, "first: the edit, on no loop between blocks, keeps the run in its rounds");
    advance(&mut first, END, &mut sink);
    let st = first.stats().clone();
    let (refusals, go_ins) = (first.refusals().to_vec(), first.go_ins());
    let why = st.last_shadow_act.clone().unwrap_or_default();
    assert!(st.shadow_acts == 1 && refusals.len() == 1 && refusals[0].1 == ["x"] && why.contains(" drift "),
        "first: the near oscillator's drift refuses the cut at x: {} acts, {refusals:?}: {why}", st.shadow_acts);
    let at = refusals[0].0;
    let dropped = go_ins.iter().filter(|g| g.1.is_infinite()).count();
    let remade = go_ins.iter().find(|g| g.1.is_finite() && g.0 >= at);
    assert!(go_ins.iter().any(|g| g.0 < at && g.1.is_infinite()) && dropped == 1 && remade.is_some_and(|g| g.0 < at + 16.0 * SAMPLE && (g.1 - g.0 - replan.lead).abs() < 16.0 * SAMPLE),
        "first: the edit's plan, on its way at the refusal at {at:e} s, never goes in, and the merged plan is made from the next round start, a lead before it goes in: {go_ins:?}");
    assert!(st.plan_error.is_none() && first.blocks() == 3, "first: no plan declined, the run on the merged plan: {:?}, {} blocks", st.plan_error, first.blocks());

    let mut second = run(&edited, None);
    assert!(second.blocks() == 3 && second.refusals() == [(0.0, vec!["x".to_string()])],
        "second: the refusal on its timeline at its start, on the merged plan: {} blocks, {:?}", second.blocks(), second.refusals());
    let mut sink = Record::default();
    sink.begin(&second.solver().unknowns);
    advance(&mut second, END, &mut sink);
    let st = second.stats().clone();
    let sp = &st.round_split;
    let far = sp.shift_by.get("block 1").copied().unwrap_or_default();
    assert!(second.blocks() == 3 && second.go_ins().is_empty() && st.shadow_acts == 0 && st.breaches == 0 && sp.shadows as usize >= BURST && far.span >= BURST as f64 * 16.0 * SAMPLE * (1.0 - 1e-9),
        "second: on the merged plan to the end, judged from its first rounds: {} blocks, {:?} in, {} acts, {} breaches, {} shadow rounds, the far oscillator's {far:?}", second.blocks(), second.go_ins(), st.shadow_acts, st.breaches, sp.shadows);
    let mut again = run(&edited, Some(&second));
    let mut got = Record::default();
    got.begin(&again.solver().unknowns);
    advance(&mut again, END, &mut got);
    assert!(got.at == sink.at && got.x == sink.x, "second, made again from its timeline: the same run, {} points against {}", got.at.len(), sink.at.len());

    let own = run(&[], None);
    assert!(own.blocks() == 4 && own.refusals().is_empty(), "at the circuit's own values: nothing recalled, the four blocks: {} blocks, {:?}", own.blocks(), own.refusals());
}

/// A planner whose plans promise what each value read late gets wrong at most
/// (`RoundPlan::late_tol`, `hub_tol`), as the block planner's do: their shadow rounds are held to it.
/// The block planner's bar for a return in the hub form (`PartOpts::hub_tol`): no shadow bar is
/// looser.
const HUB_TOL: f64 = 0.1;

#[derive(Clone)]
struct Promising<P>(P, f64);

impl<P: Planner + Clone + 'static> Planner for Promising<P> {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        Ok(self.0.plan(ckt, whole, t)?.map(|(plan, solvers)| (RoundPlan { late_tol: vec![self.1; plan.couplings.len()], hub_tol: Some(HUB_TOL), ..plan }, solvers)))
    }
}

/// A process that never runs and cannot be taken back (`CoSim::fork`: none).
struct Fixed;

impl CoSim for Fixed {
    fn next(&self) -> f64 {
        f64::INFINITY
    }
    fn run(&mut self, _bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    fn arm(&mut self, _t: f64, _to: Option<StepTo>) -> bool {
        false
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

/// A run whose attached process cannot be taken back (`Fixed`) has no restore point for the growth
/// rule, so it reads nothing late (`LiveSim::install`, `attach`):
/// - attached first, `RUN_AWAY`'s plan, which reads b late, is declined, and the run steps whole;
/// - attached to a run in those rounds, the run leaves them there and steps whole;
/// - attached first, `ONE_WAY`'s plan, which reads nothing late, goes in: no breach is possible
///   there, so its values go unchecked (`Rounds::hold`), its ideal amplifier's `a = 1000 s` far past
///   the bar, and it steps in rounds to the end, every point after its first round (where the
///   first steps grow from the operating point) a sample instant: with no event no block halves
///   its last step before a round's end, the end a rounding past it included.
///
/// Whole, `RUN_AWAY`'s a is a third of the drive; never past the bar, no run goes back.
#[test]
fn a_run_whose_process_cannot_go_back_reads_nothing_late() {
    const END: f64 = 3e-3;
    let fixed = || Some(CoSimDevice { dev: Box::new(Fixed), pins: Vec::new() });
    for (name, plan, first) in [("attached first", RUN_AWAY, true), ("attached in rounds", RUN_AWAY, false), ("reading nothing late", ONE_WAY, true)] {
        let mut c = SimCircuit::new();
        (plan.a)(&mut c);
        (plan.b)(&mut c);
        let mut sim = LiveSim::new(c, ValuePlan::default(), 1, LiveSpec { round: 16, ..spec(SAMPLE, SAMPLE) }).expect("the operating point converges");
        let mut sink = Record::default();
        sink.begin(&sim.solver().unknowns);
        if first {
            sim.attach(fixed());
        }
        let loops = plan.loops;
        let planned = sim.set_planner(Some(Box::new(plan)));
        let mut from = 0.0;
        if first && loops {
            let why = planned.err().map(|d| d.message).unwrap_or_default();
            assert!(why.contains("late at b, and the attached process cannot be taken back"), "{name}: the plan reading b late is declined: {why:?}");
        } else {
            planned.unwrap_or_else(|d| panic!("{name}: the plan installs: {d}"));
        }
        if !first {
            sim.advance(0.3e-3, None, &mut sink).unwrap_or_else(|d| panic!("{name}: the run steps: {d}"));
            assert_eq!(sim.blocks(), 2, "{name}: in rounds at {:e} s", sim.t());
            sim.attach(fixed());
            from = sim.t();
        }
        let blocks = if loops { 1 } else { 2 };
        while sim.t() < END {
            assert_eq!(sim.blocks(), blocks, "{name}: the blocks the run steps at {:e} s", sim.t());
            sim.advance(END, None, &mut sink).unwrap_or_else(|d| panic!("{name}: the run steps: {d}"));
        }
        assert_eq!((sim.stats().breaches, sim.stats().rewinds), (0, 0), "{name}: no breach, no going back");
        let (out, s) = (sink.v(if loops { "a" } else { "b" }), sink.v("s"));
        let gain = if loops { 1.0 / 3.0 } else { 1e3 };
        let off = sink.at.iter().zip(out.iter().zip(&s)).filter(|p| *p.0 > from).map(|(&t, (v, s))| ((v - gain * s).abs(), t)).fold((0.0, 0.0), |w, p| if p.0 > w.0 { p } else { w });
        assert!(sink.at.last().is_some_and(|&t| t >= END) && off.0 < 1e-6 * gain.max(1.0), "{name}: from {from:e} s to the end the output is {gain} times the drive, to a millionth of the drive's volt: off by {:e} V at {:e} s", off.0, off.1);
        if !loops {
            let first = sim.round_len();
            let halved: Vec<f64> = sink.at.iter().copied().filter(|&t| t > first && (t / SAMPLE - (t / SAMPLE).round()).abs() > 1e-6).collect();
            assert!(halved.is_empty(), "{name}: with no event every point after the first round is a sample instant; {} of {} are not: {:?}", halved.len(), sink.at.len(), &halved[..halved.len().min(8)]);
        }
    }
}
