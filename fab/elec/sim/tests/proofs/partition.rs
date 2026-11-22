//! The two-voice board keeps each voice's mix network and cuts at the summer's virtual ground; a
//! pad's consumers read it in the same round and return their current a round late, as a declared
//! supply's do (block 0's other nodes are ordinary), and the round map merges a loop no single cut
//! sees.

use fab_elec_sim::analysis::held::held;
use fab_elec_sim::analysis::op::operating_point;
use fab_elec_sim::analysis::partition::{partition, PartOpts};
use fab_elec_sim::analysis::round::{circuit_opts, plan_blocks, BlockPlanner};
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec, Replan};
use fab_elec_sim::rounds::{Lag, Planner, RoundPlan};
use fab_elec_sim::stamp::UnkKind;
use fab_elec_sim::{CtlKind, ModelKind, Record, SimCircuit, SimDiag, Sink, Solver, Source, Wave};
use std::sync::{Arc, Mutex};

fn two_voices() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("qn", ModelKind::Npn, &[("is", 1e-14), ("bf", 200.0), ("vaf", 100.0), ("cje", 8e-12), ("cjc", 4e-12), ("tf", 4e-10), ("tr", 5e-8)]);
    c.model("dsig", ModelKind::Diode, &[("is", 2.5e-9), ("n", 1.75), ("rs", 0.6), ("cjo", 4e-12), ("tt", 6e-9)]);
    c.v("vcc", "vcc", "0", Source::dc(9.0));
    c.v("vref", "ref", "0", Source::dc(4.5));
    for (k, cap) in [15e-9, 47e-9].into_iter().enumerate() {
        let n = |s: &str| format!("{s}{k}");
        c.v(&n("vtrig"), &n("trig"), "0", Source::wave(Wave::Pulse {
            v1: 0.0, v2: 5.0, td: 1e-3 + k as f64 * 0.25 / 4.0,
            tr: 10e-6, tf: 10e-6, pw: 1e-3, per: 0.25,
        }));
        c.r(&n("rbt"), &n("trig"), &n("bt"), 10e3);
        c.q(&n("qt"), &n("ct"), &n("bt"), "0", "qn");
        c.r(&n("rct"), "vcc", &n("ct"), 10e3);
        c.c(&n("cin"), &n("ct"), &n("kin"), 100e-9);
        c.r(&n("rin"), &n("kin"), &n("inv"), 100e3);
        c.vctl(&n("amp"), CtlKind::Vcvs, &n("out"), "ref", "ref", &n("inv"), 1e5);
        c.r(&n("r1"), &n("out"), &n("inv"), 1e6);
        c.c(&n("c1"), &n("out"), &n("mid"), cap);
        c.c(&n("c2"), &n("mid"), &n("inv"), cap);
        c.r(&n("r2"), &n("mid"), "ref", 4.7e3);
        c.r(&n("rclip"), &n("out"), &n("clip"), 10e3);
        c.c(&n("cdc"), &n("clip"), &n("ac"), 1e-6);
        c.d(&n("dp"), &n("ac"), "0", "dsig");
        c.d(&n("dn"), "0", &n("ac"), "dsig");
        c.r(&n("rmix"), &n("ac"), &n("mix"), 10e3);
        c.c(&n("cmix"), &n("mix"), "0", 1e-9);
        c.r(&n("rload"), &n("mix"), "0", 100e3);
        c.r(&n("rsum"), &n("mix"), "sum", 22e3);
    }
    c.vctl("summer", CtlKind::Vcvs, "master", "ref", "ref", "sum", 1e5);
    c.r("rf", "master", "sum", 22e3);
    c.c("cf", "master", "sum", 100e-12);
    c
}

#[test]
fn two_voice_partition() {
    let c = two_voices();
    let opts = PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: 1e-3, hub_tol: 0.1, fmin: 10.0, fmax: 48e3, band: f64::INFINITY, per_octave: 4, min_island: 8, control: vec![], supplies: Vec::new(), round: 32.0 / 96e3 };
    let p = partition(&c, &opts).unwrap_or_else(|d| panic!("two-voice partition must succeed: {d}"));
    let again = partition(&c, &opts).unwrap_or_else(|d| panic!("repeated two-voice partition must succeed: {d}"));
    assert_eq!(p.report("two voices"), again.report("two voices"), "partition reports must be deterministic");
    assert_eq!(p.islands.len(), 3, "two voices and their summer must form exactly three islands: {}", p.report("two voices"));
    let unknown = |name: &str| p.names.iter().position(|n| n == name)
        .unwrap_or_else(|| panic!("partition must retain the unknown name {name}")) as u32;
    let island = |name: &str| p.islands.iter().position(|i| i.unknowns.contains(&unknown(name)))
        .unwrap_or_else(|| panic!("{name} must belong to an island"));
    for k in 0..2 {
        assert_eq!(island(&format!("out{k}")), island(&format!("mix{k}")), "voice {k} must retain its mix network");
    }
    assert_ne!(island("out0"), island("out1"), "the two voices must be in different islands");
    assert_eq!(island("sum"), island("master"), "the summer must retain its virtual ground and output");
    for name in ["out0", "out1"] {
        assert_ne!(island(name), island("sum"), "{name} must be separated from the summer");
    }
    assert_eq!(p.cuts.len(), 1, "the board must have exactly one weak cut");
    let cut = &p.cuts[0];
    assert_eq!(cut.node, unknown("sum"), "the weak cut must be at the summer's virtual ground");
    assert!(cut.rho.iter().all(|&r| r < 1e-3), "the sum cut's rho must stay below 1e-3 across the band");
    assert!(!cut.stiff_first, "the voice islands must run before the summer");
    assert_eq!(cut.stiff, island("sum"), "the summer must be the stiff island");
    for name in ["vcc", "ref"] {
        assert!(p.sources.contains(&unknown(name)), "ideal source {name} must be classified as a source");
    }
}

/// The two voices run as three blocks: each voice before the summer, which reads their mix nodes
/// in the same round through ghosts of its input resistors, while the voices read the virtual
/// ground one round late. Each block's operating point, with its boundaries at the whole
/// circuit's values, is the whole circuit's on the unknowns it solves.
#[test]
fn two_voice_blocks() {
    let c = two_voices();
    let opts = PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: 1e-3, hub_tol: 0.1, fmin: 10.0, fmax: 48e3, band: f64::INFINITY, per_octave: 4, min_island: 8, control: vec![], supplies: Vec::new(), round: 32.0 / 96e3 };
    let mut whole = Solver::new(&c).expect("the two voices build");
    operating_point(&mut whole, false).expect("the two voices have an operating point");
    let plan = plan_blocks(&c, &whole, 0.0, &opts).expect("the two voices plan").expect("the two voices split");
    let blocks = &plan.plan.blocks;
    assert_eq!(blocks.len(), 3, "two voices and the summer: {}", plan.report);
    assert_eq!(blocks[0].stage, 0, "block 0 is alone at stage 0: {}", plan.report);
    let summer = blocks.iter().position(|b| b.own.iter().any(|&(_, g)| whole.unknowns[g as usize].name == "master"))
        .expect("a block solves the summer");
    assert_eq!(summer, 2, "the summer runs after both voices: {}", plan.report);
    let mut solved = vec![0; whole.unknowns.len()];
    for b in blocks {
        for &(_, g) in &b.own {
            solved[g as usize] += 1;
        }
    }
    assert!(solved[1..].iter().all(|&n| n == 1), "every unknown is solved by exactly one block: {}", plan.report);
    let ghosts = &plan.plan.ghosts;
    assert_eq!(ghosts.len(), 2, "each voice's summing resistor has a ghost in the summer: {ghosts:?}");
    assert!(ghosts.values().all(|g| g.len() == 1 && g[0].0 as usize == summer), "the ghosts sit in the summer: {ghosts:?}");
    for k in 0..2 {
        let mix = plan.plan.couplings.iter().find(|l| l.to.iter().any(|t| t.1 == format!("mix{k}#in"))).expect("the summer reads each mix node");
        assert!(mix.lag == Lag::Same && mix.to == vec![(summer as u32, format!("mix{k}#in"))], "the summer reads mix{k} in the same round: {mix:?}");
    }
    let sum = plan.plan.couplings.iter().find(|l| l.to.iter().any(|t| t.1 == "sum#in")).expect("the voices read the virtual ground");
    assert!(sum.lag == Lag::Round && sum.to.len() == 2 && sum.from.0 as usize == summer && sum.to.iter().all(|t| sum.cut_of(t.0).is_some()) && sum.rho < 1e-3,
        "the voices read the virtual ground one round late, each through a cut: {sum:?}");
    let mut solvers = plan.solvers;
    for (b, s) in blocks.iter().zip(solvers.iter_mut()) {
        operating_point(s, false).unwrap_or_else(|d| panic!("block {} has an operating point: {d}", b.name));
    }
    // both are Newton solutions: agreement to well inside Newton's own tolerances (1e-3 relative)
    let close = |v: f64, w: f64, current: bool| (v - w).abs() <= 1e-5 * w.abs() + if current { 1e-10 } else { 1e-6 };
    let current = |g: u32| whole.unknowns[g as usize].kind == UnkKind::Current;
    for (b, s) in blocks.iter().zip(&solvers) {
        for &(l, g) in &b.own {
            if plan.plan.sums.iter().any(|&(u, _)| u == g) {
                continue;
            }
            let (v, w) = (s.x[l as usize], whole.x[g as usize]);
            assert!(close(v, w, current(g)), "block {}: {} at {v} against the whole circuit's {w}", b.name, whole.unknowns[g as usize].name);
        }
    }
    assert!(plan.plan.sums.iter().any(|(u, _)| whole.unknowns[*u as usize].name == "vref#branch"), "the reference source is copied: {:?}", plan.plan.sums);
    for (u, shares) in &plan.plan.sums {
        let total: f64 = shares.iter().map(|&(k, l)| solvers[k as usize].x[l as usize]).sum();
        let w = whole.x[*u as usize];
        assert!(close(total, w, true), "{}: the copies' shares sum to {total} against the whole circuit's {w}", whole.unknowns[*u as usize].name);
    }
}

/// The two voices' options: each voice and the summer a block (`two_voice_blocks`).
const VOICE_OPTS: PartOpts = PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: 1e-3, hub_tol: 0.1, fmin: 10.0, fmax: 48e3, band: f64::INFINITY, per_octave: 4, min_island: 8, control: Vec::new(), supplies: Vec::new(), round: 32.0 / 96e3 };

/// The host's planner (`BlockPlanner`) with the two voices' options, keeping where each plan it
/// makes holds `rsum0`'s ghost.
#[derive(Clone)]
struct Voices(Arc<Mutex<Vec<Option<Vec<(u32, String)>>>>>);

impl Planner for Voices {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let got = BlockPlanner { opts: VOICE_OPTS, states: Default::default(), pool: None }.plan(ckt, whole, t)?;
        if let Some((plan, _)) = &got {
            self.0.lock().unwrap().push(plan.ghosts.get("rsum0").cloned());
        }
        Ok(got)
    }
}

/// A summing resistor turned is heard through both its copies, at home in voice 0's block and as
/// its ghost in the summer's (`RoundPlan::ghosts`), as the host plans (`BlockPlanner`):
/// - Turned on one stepper, the blocks take it as rounds go in (at rest, cut at the virtual
///   ground with the ghost in the summer). Turned in rounds, both copies take it at once; the
///   element joins what its blocks read across the cut to what they export, so the edit may break
///   the round map's proof: it is proved again at its round's end (`LiveSim::reprove_edits`), it
///   holds, and the run stays in rounds; the plan made after it cuts the run as it runs. Turned
///   again while voice 0 rings, it goes on through whatever plan follows.
/// - Exact, the summer's output follows a whole run with the same edits within 1e-3 of its swing
///   (a step's shift of a fast edge aside), from a settle after each change: 50 us at rest, a
///   quarter millisecond while ringing (each run cuts its history there in its own way). A ghost
///   left at its old value is off by 1.2 to 2.3 V; with each edit there, by under 0.1 mV.
#[test]
fn a_cut_resistors_edit_reaches_its_ghost() {
    let (stop, h) = (7e-3, 1.0 / 96e3);
    let round = 32.0 * h;
    let replan = Replan { edit: 0.05e-3, lead: 0.05e-3, ..Replan::default() };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: h, res: 0.0, trace: false, replan, ..LiveSpec::default() };
    let plans = Arc::new(Mutex::new(Vec::new()));
    let edit = |sim: &mut LiveSim, ohms: f64| {
        assert!(sim.apply(LiveCmd::SetParam { generation: 1, device: "rsum0".into(), key: "value".into(), value: ohms }), "the edit keeps the run");
    };
    let mut runs = Vec::new();
    for split in [false, true] {
        let mut sim = LiveSim::new(two_voices(), ValuePlan::default(), 1, spec.clone()).expect("the two voices have an operating point");
        if split {
            sim.set_rounds(false).expect("rounds off");
            sim.set_planner(Some(Box::new(Voices(plans.clone())))).expect("the planner waits for rounds");
        }
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        let mut advance = |sim: &mut LiveSim, to: f64| {
            while sim.t() < to {
                sim.advance(to, None, &mut record).unwrap_or_else(|d| panic!("the run steps: {d}"));
            }
        };
        // at rest until voice 0 strikes at 1 ms
        advance(&mut sim, 0.15e-3);
        edit(&mut sim, 15e3);
        advance(&mut sim, 0.2e-3);
        if split {
            sim.set_rounds(true).expect("the plan installs");
            let seen = plans.lock().unwrap().clone();
            assert!(sim.blocks() == 3 && seen[0].as_ref().is_some_and(|g| g.len() == 1 && g[0].0 == 2), "the voices and the summer go in rounds, rsum0's ghost in the summer: {seen:?}");
        }
        // at a round's end: the plan after it is made at the next and goes in at the one after
        advance(&mut sim, round);
        let edited = sim.t();
        edit(&mut sim, 11e3);
        advance(&mut sim, 1.1e-3);
        if split {
            let seen = plans.lock().unwrap().clone();
            assert_eq!((sim.stats().plans, sim.stats().plans_kept, sim.blocks()), (2, 1, 3), "the edit stays in rounds; the plan after it cuts the run as it runs; rsum0's ghosts {seen:?}");
            let st = sim.stats();
            assert!((st.reproofs, st.edit_leaves, st.off_rounds) == (1, 0, 0.0) && matches!(st.last_reproof, Some(Ok(_))) && st.last_plan > edited, "the edit is proved and holds: no leave, no time on one stepper: {} proofs ({:?}), {} leaves, {:e} s", st.reproofs, st.last_reproof, st.edit_leaves, st.off_rounds);
        }
        advance(&mut sim, 4e-3);
        edit(&mut sim, 47e3);
        advance(&mut sim, stop);
        if split {
            assert!(sim.stats().plan_error.is_none(), "{:?}", sim.stats().plan_error);
        }
        runs.push(record);
    }
    let (whole, split) = (&runs[0], &runs[1]);
    let (wm, sm) = (whole.v("master"), split.v("master"));
    let swing = wm.iter().fold(0.0f64, |m, v| m.max((v - 4.5).abs()));
    // the whole run's range over a step either side of t: the two runs step on their own grids
    let tube = |t: f64| {
        let (a, b) = (whole.at.partition_point(|&x| x < t - h), whole.at.partition_point(|&x| x <= t + h));
        [lerp(&whole.at, &wm, t - h), lerp(&whole.at, &wm, t + h)].into_iter().chain(wm[a..b].iter().copied())
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)))
    };
    for (what, from, to) in [("15 k replayed as rounds go in", 0.2e-3 + 50e-6, round), ("11 k, out of rounds until the plan after it goes in anew", round + 50e-6, 4e-3), ("47 k while ringing", 4e-3 + 0.25e-3, stop)] {
        let err = (0..split.at.len()).filter(|&n| split.at[n] > from && split.at[n] < to)
            .map(|n| {
                let (lo, hi) = tube(split.at[n]);
                (sm[n] - hi).max(lo - sm[n]).max(0.0)
            })
            .fold(0.0f64, f64::max);
        assert!(err <= 1e-3 * swing, "rsum0 {what}: the summer's output in rounds is off the whole run's by {err:e} V, its swing {swing:e} V");
    }
}

const PAD_SAMPLE: f64 = 1.0 / 96e3;
const PAD_STOP: f64 = 6e-3;

/// A pad (a 3.3 V square through its driver's 25 ohm) read by two voices, each 4.7 k into 47 nF
/// and an amplifier of 2: each draws about 5e-3 of what the pad's driver holds, over `cut_tol` and
/// well under `hub_tol`. Each of `listeners` amplifies voice 0's output 10 times back onto the pad
/// through 10 k: alone a listener's loop returns about 5 % of a volt on the pad (its share there,
/// under `hub_tol`) and its own load a fortieth of that, but seven together return a third.
fn pad_voices(listeners: usize) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("vp", "drive", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 3.3, td: 0.5e-3, tr: 10e-6, tf: 10e-6, pw: 1e-3, per: 2e-3 }));
    c.r("rp", "drive", "pad", 25.0);
    // an amplifier of `gain` from `input` onto `out` behind 100 ohm, loaded by 10 k
    let amp = |c: &mut SimCircuit, name: &str, input: &str, out: &str, gain: f64| {
        let inner = format!("{out}.a");
        c.vctl(name, CtlKind::Vcvs, &inner, "0", input, "0", gain);
        c.r(&format!("{name}.r"), &inner, out, 100.0);
        c.r(&format!("{name}.load"), out, "0", 10e3);
    };
    for k in 0..2 {
        let n = |s: &str| format!("{s}{k}");
        c.r(&n("r"), "pad", &n("in"), 4.7e3);
        c.c(&n("c"), &n("in"), "0", 47e-9);
        amp(&mut c, &n("a"), &n("in"), &n("out"), 2.0);
    }
    for k in 0..listeners {
        amp(&mut c, &format!("al{k}"), "out0", &format!("l{k}"), 10.0);
        c.r(&format!("rl{k}"), &format!("l{k}"), "pad", 10e3);
    }
    c
}

/// The bars of the live runs; an island of one unknown joins its single reader (a voice's RC its
/// amplifier).
fn pad_opts() -> PartOpts {
    PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: 1e-3, hub_tol: 0.1, fmin: 10.0, fmax: 48e3, band: f64::INFINITY, per_octave: 4, min_island: 2, control: vec!["pad".into()], supplies: Vec::new(), round: 32.0 / 96e3 }
}

/// The block planner with the pad as the control set.
#[derive(Clone)]
struct Pads;

impl Planner for Pads {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        Ok(plan_blocks(ckt, whole, t, &pad_opts())?.map(|b| (b.plan, b.solvers)))
    }
}

/// A run of the pad's circuit to `PAD_STOP`, whole or in the planner's rounds: steps of at most
/// `max_step`, time resolution `res` (0 Exact), Newton's `reltol`.
fn pad_run(planner: Option<Box<dyn Planner>>, max_step: f64, res: f64, reltol: f64) -> (Record, LiveSim) {
    let mut c = pad_voices(0);
    c.opts.reltol = reltol;
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step, res, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the operating point converges");
    sim.set_planner(planner).expect("the plan installs");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    while sim.t() < PAD_STOP {
        sim.advance(PAD_STOP, None, &mut record).unwrap_or_else(|d| panic!("the run steps: {d}"));
    }
    (record, sim)
}

/// `v` over the times `at`, linear between them, at `t`.
fn lerp(at: &[f64], v: &[f64], t: f64) -> f64 {
    let n = at.partition_point(|&a| a < t).clamp(1, at.len() - 1);
    v[n - 1] + (v[n] - v[n - 1]) * (t - at[n - 1]) / (at[n] - at[n - 1])
}

/// The block-0 form at a pad, and the round map:
/// - The pad is block 0, alone at stage 0. Each voice is a block that reads the pad in the same
///   round (`pad#in`) and returns what it draws through it one round late, to a current boundary
///   of block 0 (`pad#ret<k>`); no ghost. The round map passes with nothing merged.
/// - Run in rounds of 32 samples, Exact and Fast, each voice hears the pad in step: against a fine
///   run of the whole circuit its output is off by no more than the whole circuit's own error (two
///   times in Exact, three in Fast) and twice the voice's rho of its swing, the pad's late load;
///   read a round late it would be off by most of the swing.
/// - With seven listeners each passes its share and the pad's test, but together their loops return
///   a third of the pad's swing: the round map merges listeners into block 0 (which then reads
///   voice 0 in the same round and takes it) until the rest pass; voice 1 stays apart.
#[test]
fn a_pads_consumers_read_it_in_the_same_round_and_the_round_map_merges_a_loop() {
    let mut whole = Solver::new(&pad_voices(0)).expect("the pad's circuit builds");
    operating_point(&mut whole, false).expect("the pad's circuit has an operating point");
    let plan = plan_blocks(&pad_voices(0), &whole, 0.0, &pad_opts()).expect("the pad's circuit plans").expect("the pad's circuit splits");
    let report = &plan.report;
    eprintln!("{report}");
    let blocks = &plan.plan.blocks;
    assert_eq!(blocks.len(), 3, "the pad and two voices: {report}");
    assert_eq!(blocks.iter().map(|b| b.stage).collect::<Vec<_>>(), [0, 1, 1], "the pad alone at stage 0, the voices after it: {report}");
    let solves = |k: usize, name: &str| blocks[k].own.iter().any(|&(_, g)| whole.unknowns[g as usize].name == name);
    assert!(solves(0, "pad") && solves(1, "out0") && solves(2, "out1"), "block 0 the pad, then each voice: {report}");
    assert!(plan.plan.ghosts.is_empty(), "a node of block 0 keeps no ghost: {:?}", plan.plan.ghosts);
    let read = plan.plan.couplings.iter().find(|l| l.to.iter().any(|t| t.1 == "pad#in")).expect("the voices read the pad");
    assert!(read.lag == Lag::Same && read.from.0 == 0 && read.to.len() == 2, "both voices read the pad in the same round: {read:?}");
    for k in 1..3u32 {
        let ret = plan.plan.couplings.iter().find(|l| l.to == vec![(0, format!("pad#ret{k}"))]).unwrap_or_else(|| panic!("block 0 takes voice {k}'s return: {report}"));
        let drawn = plan.solvers[k as usize].unknowns[ret.from.1 as usize].name.clone();
        assert!(ret.lag == Lag::Round && ret.from.0 == k && drawn == "pad#in#branch" && ret.rho > 1e-3 && ret.rho < 1e-2,
            "voice {k} returns what it draws through its boundary one round late, rho between cut_tol and hub_tol: {ret:?} ({drawn})");
    }
    assert_eq!(plan.map.merges, 0, "the round map passes as planned: {report}");

    let round = 32.0 * PAD_SAMPLE;
    let reltol = pad_voices(0).opts.reltol;
    let (truth, _) = pad_run(None, PAD_SAMPLE / 16.0, 0.0, 1e-5);
    let grid = (PAD_STOP / (PAD_SAMPLE / 8.0)) as usize;
    // the largest difference of `name` in `run` from `truth` `lag` earlier, both read linearly
    // between their points on a grid of an eighth of a sample, from the first round's end
    let off = |run: &Record, name: &str, lag: f64| {
        let (r, w) = (run.v(name), truth.v(name));
        (0..=grid).map(|n| n as f64 * PAD_SAMPLE / 8.0).filter(|&t| t >= round).map(|t| (lerp(&run.at, &r, t) - lerp(&truth.at, &w, t - lag)).abs()).fold(0.0, f64::max)
    };
    for (mode, res, reltol, times) in [("Exact", 0.0, 1e-4, 2.0), ("Fast", PAD_SAMPLE, reltol, 3.0)] {
        let (split, sim) = pad_run(Some(Box::new(Pads)), PAD_SAMPLE, res, reltol);
        assert_eq!(sim.blocks(), 3, "{mode}: the run steps three blocks");
        let (alone, _) = pad_run(None, PAD_SAMPLE, res, reltol);
        for k in 0..2 {
            let name = format!("out{k}");
            let w = truth.v(&name);
            let swing = w.iter().fold(f64::MIN, |m, &v| m.max(v)) - w.iter().fold(f64::MAX, |m, &v| m.min(v));
            let rho = plan.plan.couplings.iter().find(|l| l.to == vec![(0, format!("pad#ret{}", k + 1))]).expect("a return").rho;
            let (got, own, late) = (off(&split, &name, 0.0), off(&alone, &name, 0.0), off(&split, &name, round));
            eprintln!("{mode}: voice {k}: {got:e} V from the fine run in step, {own:e} V whole, {late:e} V read a round late, of a {swing:e} V swing (rho {rho:.1e})");
            assert!(swing > 6.0, "voice {k} swings with the pad: {swing:e} V");
            let allowed = times * own + 2.0 * rho * swing;
            assert!(got <= allowed, "{mode}: voice {k} follows the fine run in step within {times} times the whole circuit's error and twice its rho of the swing: {got:e} V, {allowed:e} V allowed");
            assert!(late > 0.5 * swing, "{mode}: voice {k} hears the pad in step, not a round late: {late:e} V off a round late, of {swing:e} V");
        }
    }

    let mut whole = Solver::new(&pad_voices(7)).expect("the listeners' circuit builds");
    operating_point(&mut whole, false).expect("the listeners' circuit has an operating point");
    let h = held(&whole, 0.0, &pad_opts()).expect("the listeners' circuit holds");
    let pad = h.node("pad").expect("the pad is held");
    assert!(pad.zero && pad.consumers.len() == 9 && pad.rho.iter().all(|&r| r < 0.1),
        "each listener is a consumer of the pad, under hub_tol in its share and in the pad's test: {}", h.report("listeners"));
    let plan = plan_blocks(&pad_voices(7), &whole, 0.0, &pad_opts()).expect("the listeners' circuit plans").expect("the listeners' circuit splits");
    let report = &plan.report;
    eprintln!("{report}");
    assert!(plan.map.merges >= 1 && report.contains("the round map"), "the round map merges the loop: {report}");
    let (g, go) = (plan.map.gain.0, plan.map.gain_ordinary.0);
    assert!(g * g <= 0.1 && go * go <= 1e-3, "the plan that runs passes the round map: loop gains {g:.2e} and {go:.2e} a round: {report}");
    let block_of = |name: &str| plan.plan.blocks.iter().position(|b| b.own.iter().any(|&(_, g)| whole.unknowns[g as usize].name == name))
        .unwrap_or_else(|| panic!("a block solves {name}: {report}"));
    let joined = (0..7).filter(|k| block_of(&format!("l{k}")) == 0).count();
    assert!(block_of("pad") == 0 && block_of("out0") == 0 && (1..7).contains(&joined) && block_of("out1") != 0,
        "listeners join block 0 until the rest pass, which then holds voice 0; voice 1 stays apart: {report}");
}

/// A pad behind its driver's 25 ohm, loaded through 10 ohm at `mid` by 50 ohm, so heavily that
/// `mid` holds with the pad. Voice 0 reads the pad and voice 1 reads `mid`, each through 4.7 k into
/// 47 nF and an amplifier of 2 (about 5e-3 of what holds the node: over `cut_tol`, under
/// `hub_tol`); a listener reads `mid` through 1 M (well under `cut_tol`).
fn pad_and_mid() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("vp", "drive", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 3.3, td: 0.5e-3, tr: 10e-6, tf: 10e-6, pw: 1e-3, per: 2e-3 }));
    c.r("rp", "drive", "pad", 25.0);
    c.r("rm", "pad", "mid", 10.0);
    c.r("rl", "mid", "0", 50.0);
    for (k, at, r) in [(0, "pad", 4.7e3), (1, "mid", 4.7e3), (2, "mid", 1e6)] {
        let n = |s: &str| format!("{s}{k}");
        c.r(&n("r"), at, &n("in"), r);
        c.c(&n("c"), &n("in"), "0", 47e-9);
        c.vctl(&n("a"), CtlKind::Vcvs, &n("out.a"), "0", &n("in"), "0", 2.0);
        c.r(&n("a.r"), &n("out.a"), &n("out"), 100.0);
        c.r(&n("a.load"), &n("out"), "0", 10e3);
    }
    c
}

/// Only a pad takes the block-0 form; block 0's other nodes are ordinary:
/// - `mid` holds with the pad in block 0, at `cut_tol`: voice 1's input (its 4.7 k into 47 nF) draws
///   over it there and joins block 0, where at the pad voice 0's, drawing as much, returns its
///   current a round late.
/// - The listener at `mid` is an ordinary cut through its resistor, with a ghost in block 0; no
///   instance that is not a resistor or a capacitor is cut at `mid`.
/// - The plan returns currents at the pad only.
#[test]
fn a_node_of_block_0_that_is_not_a_pad_is_ordinary() {
    let c = pad_and_mid();
    let mut whole = Solver::new(&c).expect("the circuit builds");
    operating_point(&mut whole, false).expect("the circuit has an operating point");
    let h = held(&whole, 0.0, &pad_opts()).expect("the circuit holds");
    let report = h.report("pad and mid");
    eprintln!("{report}");
    let index = |name: &str| whole.unknowns.iter().position(|u| u.name == name).unwrap_or_else(|| panic!("an unknown {name}")) as u32;
    let island = |name: &str| h.islands.iter().position(|i| i.unknowns.contains(&index(name))).unwrap_or_else(|| panic!("an island holds {name}: {report}"));
    let (pad, mid) = (h.node("pad").expect("the pad is held"), h.node("mid").expect("mid is held"));
    assert!(pad.hub && pad.zero && pad.bar == 0.1, "the pad takes the block-0 form at hub_tol: {report}");
    assert!(!mid.hub && mid.zero && mid.bar == 1e-3, "mid is held by block 0 at cut_tol, not in the block-0 form: {report}");
    assert!(h.islands[island("in1")].zero && !h.islands[island("in0")].zero,
        "voice 1's input draws over cut_tol at mid and joins block 0; voice 0's, drawing as much at the pad, does not: {report}");
    assert!(mid.consumers.len() == 1 && mid.consumers[0].rc && mid.consumers[0].island == island("in2") && h.device_outside.is_empty(),
        "the listener is mid's one consumer, cut through its resistor; no device is cut at mid: {report}");
    let plan = plan_blocks(&c, &whole, 0.0, &pad_opts()).expect("the circuit plans").expect("the circuit splits");
    let returns: Vec<&str> = plan.plan.couplings.iter().filter(|l| l.lag == Lag::Round).flat_map(|l| l.to.iter().map(|t| t.1.as_str())).collect();
    assert!(returns.iter().any(|t| t.starts_with("pad#ret")) && !returns.iter().any(|t| t.starts_with("mid#")),
        "currents come back a round late at the pad only: {returns:?}\n{}", plan.report);
}

/// A 5 V rail behind 25 ohm read by two voices, each through 4.7 k into 47 nF and an amplifier of 2
/// (about 5e-3 of what holds the rail: over `cut_tol`, under `hub_tol`); no pad.
fn rail_voices(declared: bool) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("vs", "src", "0", Source::dc(5.0));
    c.r("rs", "src", "rail", 25.0);
    for k in 0..2 {
        let n = |s: &str| format!("{s}{k}");
        c.r(&n("r"), "rail", &n("in"), 4.7e3);
        c.c(&n("c"), &n("in"), "0", 47e-9);
        c.vctl(&n("a"), CtlKind::Vcvs, &n("out.a"), "0", &n("in"), "0", 2.0);
        c.r(&n("a.r"), &n("out.a"), &n("out"), 100.0);
        c.r(&n("a.load"), &n("out"), "0", 10e3);
    }
    if declared {
        c.declare_supply("rail");
    }
    c
}

/// A node the circuit declares a supply takes the block-0 form a pad does; undeclared, the same
/// node is ordinary:
/// - Undeclared, each voice's input draws over `cut_tol` at the rail and joins its island.
/// - Declared, the rail holds at `hub_tol`: each voice is its consumer, reads it in the same round
///   and returns its current a round late (`rail#ret<k>`). With no pad the rail's block is block 0,
///   and the round map passes with nothing merged.
#[test]
fn a_declared_supply_takes_its_consumers_currents_a_round_late() {
    let opts = PartOpts { control: Vec::new(), ..pad_opts() };
    for declared in [false, true] {
        let c = rail_voices(declared);
        let mut whole = Solver::new(&c).expect("the rail's circuit builds");
        operating_point(&mut whole, false).expect("the rail's circuit has an operating point");
        let h = held(&whole, 0.0, &circuit_opts(&c, &opts)).expect("the rail's circuit holds");
        let report = h.report(if declared { "declared" } else { "undeclared" });
        let index = |name: &str| whole.unknowns.iter().position(|u| u.name == name).unwrap_or_else(|| panic!("an unknown {name}")) as u32;
        let island = |name: &str| h.islands.iter().position(|i| i.unknowns.contains(&index(name))).unwrap_or_else(|| panic!("an island holds {name}: {report}"));
        if !declared {
            assert!(island("in0") == island("rail") && island("in1") == island("rail") && h.node("rail").is_none_or(|n| !n.hub && n.bar == 1e-3),
                "undeclared, the rail is ordinary: each voice's input joins it: {report}");
            continue;
        }
        let rail = h.node("rail").unwrap_or_else(|| panic!("the declared rail is held: {report}"));
        assert!(rail.hub && rail.bar == 0.1 && rail.consumers.len() == 2 && island("in0") != island("rail") && island("in1") != island("rail"),
            "declared, the rail holds at hub_tol with each voice its consumer: {report}");
        let plan = plan_blocks(&c, &whole, 0.0, &opts).expect("the rail's circuit plans").expect("the rail's circuit splits");
        let block_of = |name: &str| plan.plan.blocks.iter().position(|b| b.own.iter().any(|&(_, g)| whole.unknowns[g as usize].name == name))
            .unwrap_or_else(|| panic!("a block solves {name}: {}", plan.report));
        let returns = plan.plan.couplings.iter().filter(|l| l.lag == Lag::Round && l.to.iter().any(|t| t.1.starts_with("rail#ret"))).count();
        assert!(block_of("rail") == 0 && returns == 2 && plan.map.merges == 0,
            "the rail's block is block 0, each voice returns its current there a round late, and the round map passes: {}", plan.report);
    }
}

/// A supply behind 100 ohm with 1 uF on it, declared, driving `a` through 50 ohm; `a` couples
/// through 1 uF into `b`, which only 1 Mohm holds. The coupling capacitor carries about 1e-4 of
/// what holds `a`, so it is cut at `a`; but `b` follows `a` through it, and the cut fails. Tested
/// alone (the capacitor's ghost holding its far end), `a` draws from the supply through 50 ohm and
/// 1 uF, half a volt a volt on the supply at 1 kHz; with `b`, through 1 Mohm as well, under 1e-3.
/// `reversed` declares the elements in the opposite order, so the solver numbers `a` and `b`
/// ahead of the supply.
fn coupled_load(reversed: bool) -> SimCircuit {
    let mut parts: [fn(&mut SimCircuit); 6] = [
        |c| c.v("vs", "src", "0", Source::dc(5.0)),
        |c| c.r("rs", "src", "rail", 100.0),
        |c| c.c("crail", "rail", "0", 1e-6),
        |c| c.r("rup", "rail", "a", 50.0),
        |c| c.c("cac", "a", "b", 1e-6),
        |c| c.r("rb", "b", "0", 1e6),
    ];
    if reversed {
        parts.reverse();
    }
    let mut c = SimCircuit::new();
    for part in parts {
        part(&mut c);
    }
    c.declare_supply("rail");
    c
}

/// A test made stale in its own pass is judged again on its consumer as grown (`coupled_load`):
/// - The islands are the same with the elements declared in either order (the solver numbers the
///   supply ahead of `a` in one and after it in the other).
/// - `b` joins `a`, whose cut it fails, and the supply keeps an island apart from them: in the
///   first pass its test fails on `a` tested alone and waits, and tested again on `a` and `b`
///   together it passes.
#[test]
fn a_test_made_stale_in_its_own_pass_is_judged_again_on_its_consumer_as_grown() {
    let opts = PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: 1e-3, hub_tol: 0.1, fmin: 10.0, fmax: 24e3, band: f64::INFINITY, per_octave: 4, min_island: 1, control: Vec::new(), supplies: Vec::new(), round: 32.0 / 48e3 };
    let islands = |reversed: bool| -> (Vec<Vec<String>>, bool, String) {
        let c = coupled_load(reversed);
        let mut whole = Solver::new(&c).expect("the load builds");
        operating_point(&mut whole, false).expect("the load has an operating point");
        let h = held(&whole, 0.0, &circuit_opts(&c, &opts)).expect("the load holds");
        let index = |name: &str| whole.unknowns.iter().position(|u| u.name == name).unwrap_or_else(|| panic!("an unknown {name}"));
        let mut out: Vec<Vec<String>> = h.islands.iter().map(|i| {
            let mut names: Vec<String> = i.unknowns.iter().map(|&u| whole.unknowns[u as usize].name.clone()).collect();
            names.sort();
            names
        }).collect();
        out.sort();
        (out, index("rail") < index("a"), h.report(if reversed { "reversed" } else { "in order" }))
    };
    let ((forward, rail_first, report), (reversed, rail_first_reversed, report_reversed)) = (islands(false), islands(true));
    assert!(rail_first && !rail_first_reversed, "the two orders number the supply and a the other way round");
    assert_eq!(forward, reversed, "the islands do not depend on the order the elements are declared in: {report}\n{report_reversed}");
    let island = |name: &str| forward.iter().position(|i| i.iter().any(|n| n == name)).unwrap_or_else(|| panic!("an island holds {name}: {report}"));
    assert!(island("a") == island("b"), "b joins a, whose cut it fails: {report}");
    assert!(island("rail") != island("a"), "tested again on a and b together, the supply passes and keeps its own island: {report}");
    assert!(report.contains("the cut at rail fails") && report.contains("tested again"), "the supply's test on a alone fails and waits: {report}");
}
