//! What a block costs in a run in rounds: the fixed cost of a block per round against the cost of
//! its unknowns, for a planner deciding how to bin small islands into blocks.
//!
//! The circuit: `n` identical islands, each a 1 kHz sine source driving 1 kohm into 100 nF (three
//! unknowns). Island 0 is block 0 (stage 0); the others go, in contiguous runs, into `b` blocks at
//! stage 1. Linked, every other island also reads island 0's capacitor (a VCCS of 10 uS), so each
//! of those blocks has one boundary source, fed from one coupling each round. The runs are Fast at
//! 96 kHz (a step a sample), unpaced, into a sink that keeps nothing.
//!
//! Per configuration (islands, blocks, helpers, linked, round length): the wall time of a round
//! (the minimum over `REPS` runs of `SIM` simulated seconds, after a warm-up), and per block.
//! Then per helper count and linking, least-squares fits over the block counts and the island
//! counts:
//! - what one more block costs a round with the unknowns held fixed, at 8, 32 and 128 samples a
//!   round, and fitted over those, its two parts: the round's own work for a block (dispatch,
//!   feeds, windows, packing, merging: once a round) and the block's fixed work a step;
//! - what one more unknown costs a step at a fixed block count (32 samples a round).
//!
//! The argument is the number of passes over all of it (default 1); under the JIT read the last,
//! after the tier-up. Run it native and alone on a quiet box (`cargo run --release -p
//! fab-elec-sim --example rounds_cost -- 2`), or under the JIT (`makepad run fab-elec-sim
//! --example rounds_cost -- 2`).

use fab_elec_sim::analysis::Sink;
use fab_elec_sim::circuit::{Boundary, BoundaryKind, CtlKind};
use fab_elec_sim::live::ValuePlan;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::rounds::{seed_from, Block, Coupling, Helpers, Lag, Planner, Post, RoundPlan};
use fab_elec_sim::{SimCircuit, SimDiag, Solver, Source, Wave};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

const SAMPLE: f64 = 1.0 / 96e3;
/// Simulated seconds a timed run covers, after `WARM`.
const SIM: f64 = 0.04;
const WARM: f64 = 0.005;
const REPS: usize = 3;

/// A sink that keeps nothing.
struct Discard;

impl Sink for Discard {}

/// Island `k`: a sine source at `a{k}` through 1 kohm into 100 nF at `b{k}`; linked, an island
/// other than 0 also takes 10 uS of island 0's capacitor voltage into its own.
fn island(c: &mut SimCircuit, k: usize, linked: bool) {
    let (a, b) = (format!("a{k}"), format!("b{k}"));
    c.v(&format!("vs{k}"), &a, "0", Source::wave(Wave::Sin { vo: 0.0, va: 1.0, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }));
    c.r(&format!("r{k}"), &a, &b, 1e3);
    c.c(&format!("c{k}"), &b, "0", 100e-9);
    if linked && k > 0 {
        c.vctl(&format!("g{k}"), CtlKind::Vccs, "0", &b, "b0", "0", 1e-5);
    }
}

fn whole(n: usize, linked: bool) -> SimCircuit {
    let mut c = SimCircuit::new();
    for k in 0..n {
        island(&mut c, k, linked);
    }
    c
}

/// Island 0 alone as block 0; islands 1.. in `blocks` contiguous runs at stage 1, each reading
/// island 0's capacitor through a boundary source when linked.
#[derive(Clone)]
struct Bins {
    n: usize,
    blocks: usize,
    linked: bool,
}

impl Planner for Bins {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, _t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        let rest = self.n - 1;
        let v0 = whole.x[whole.unknown("b0").expect("island 0's capacitor") as usize];
        for j in 0..=self.blocks {
            let mut c = SimCircuit::new();
            let range = if j == 0 { 0..1 } else { 1 + (j - 1) * rest / self.blocks..1 + j * rest / self.blocks };
            for k in range {
                island(&mut c, k, self.linked);
            }
            if j > 0 && self.linked {
                let node = c.node("b0");
                c.devices.boundary.push(Boundary { name: "b0in".into(), node, kind: BoundaryKind::Voltage, v0 });
            }
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let mut own = seed_from(&mut s, whole);
            if j > 0 {
                own.retain(|&(l, _)| s.unknowns[l as usize].name != "b0");
            }
            plan.blocks.push(Block { name: format!("block {j}"), stage: u32::from(j > 0), own });
            solvers.push(s);
        }
        if self.linked {
            let from = (0, solvers[0].unknown("b0").expect("block 0 solves b0"));
            plan.couplings.push(Coupling { from, to: (1..=self.blocks as u32).map(|j| (j, "b0in".to_string())).collect(), lag: Lag::Same, cuts: Vec::new(), rho: 0.0 });
        }
        Ok(Some((plan, solvers)))
    }
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

/// Seconds of wall time a round, the minimum over `REPS` runs.
fn per_round(n: usize, blocks: usize, linked: bool, round: u32, post: &Option<Post>, helpers: usize) -> f64 {
    let mut best = f64::INFINITY;
    for _ in 0..REPS {
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: SAMPLE, res: SAMPLE, sample_rate: 96e3, round, trace: false, ..LiveSpec::default() };
        let mut sim = LiveSim::new(whole(n, linked), ValuePlan::default(), 1, spec).expect("the islands' operating point");
        sim.set_helpers(post.clone().map(|post| Helpers { post, count: helpers }));
        sim.set_planner(Some(Box::new(Bins { n, blocks, linked }))).expect("the plan installs");
        assert_eq!(sim.blocks(), blocks + 1, "the run steps its blocks");
        let mut sink = Discard;
        while sim.t() < WARM {
            sim.advance(WARM, None, &mut sink).expect("the islands step");
        }
        let (r0, start) = (sim.stats().rounds, Instant::now());
        while sim.t() < WARM + SIM {
            sim.advance(WARM + SIM, None, &mut sink).expect("the islands step");
        }
        let rounds = sim.stats().rounds - r0;
        best = best.min(start.elapsed().as_secs_f64() / rounds as f64);
    }
    best
}

/// Least-squares slope and intercept of `y` over `x`.
fn fit(points: &[(f64, f64)]) -> (f64, f64) {
    let n = points.len() as f64;
    let (sx, sy) = points.iter().fold((0.0, 0.0), |(a, b), &(x, y)| (a + x, b + y));
    let (mx, my) = (sx / n, sy / n);
    let (sxy, sxx) = points.iter().fold((0.0, 0.0), |(a, b), &(x, y)| (a + (x - mx) * (y - my), b + (x - mx) * (x - mx)));
    let slope = sxy / sxx;
    (slope, my - slope * mx)
}

fn main() {
    let passes: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(1);
    const N: usize = 65;
    const BLOCKS: [usize; 7] = [1, 2, 4, 8, 16, 32, 64];
    const ROUNDS: [u32; 3] = [8, 32, 128];
    const ISLANDS: [usize; 4] = [9, 17, 33, 65];
    let us = |s: f64| s * 1e6;
    let pools = [0, 1, 2].map(|helpers| (helpers > 0).then(|| pool(helpers)));
    for pass in 1..=passes {
        let mut summary = Vec::new();
        for (helpers, post) in pools.iter().enumerate() {
            for linked in [false, true] {
                let link = if linked { "linked" } else { "apart" };
                let mut slopes = Vec::new();
                for round in ROUNDS {
                    let mut points = Vec::new();
                    for blocks in BLOCKS {
                        let t = per_round(N, blocks, linked, round, post, helpers);
                        println!("pass {pass}: {N} islands ({} unknowns), 1+{blocks} blocks, {helpers} helpers, {link}, {round} samples a round: {:.1} us a round, {:.2} us a block-round",
                            3 * N, us(t), us(t) / (blocks + 1) as f64);
                        points.push((blocks as f64, t));
                    }
                    slopes.push((f64::from(round), fit(&points).0));
                }
                // a block's cost a round = its share of the round's own work + steps a round x its work a step
                let (block_step, block_round) = fit(&slopes);
                let mut points = Vec::new();
                for n in ISLANDS {
                    let t = per_round(n, 8, linked, 32, post, helpers);
                    println!("pass {pass}: {n} islands ({} unknowns), 1+8 blocks, {helpers} helpers, {link}, 32 samples a round: {:.1} us a round", 3 * n, us(t));
                    points.push(((3 * n) as f64, t));
                }
                let unknown_step = fit(&points).0 / 32.0;
                summary.push(format!(
                    "{helpers} helpers, {link}: a block costs {:.3} us a step and {:.2} us a round (one more block: {:.2}, {:.2} and {:.2} us a round at 8, 32 and 128 samples); an unknown {:.4} us a step; a block's steps cost as much as {:.1} unknowns",
                    us(block_step), us(block_round), us(slopes[0].1), us(slopes[1].1), us(slopes[2].1), us(unknown_step), block_step / unknown_step));
            }
        }
        println!("\npass {pass} of {passes}:");
        for line in summary {
            println!("{line}");
        }
    }
}
