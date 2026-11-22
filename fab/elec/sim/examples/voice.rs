//! What analog drum voices cost the live worker at audio rate. One voice is a bridged-T resonator
//! in the feedback of an inverting amplifier stage (the classic ringing voice), struck by a
//! trigger pulse through a transistor and soft-clipped by a diode pair; a noise source through a
//! high-pass can join its mix. A board puts several voices, tuned apart and struck on staggered
//! beats, into one inverting summer: the analog side of a drum machine.
//!
//! - The resonator: a VCVS amplifier (gain 1e5) between the inverting node and a 4.5 V reference,
//!   R1 1 Mohm from the output to the inverting node, C1 and C2 in series beside it, R2 4.7 kohm
//!   from their midpoint to the reference: f0 = 1 / (2 pi C sqrt(R1 R2)), 155 Hz at 15 nF,
//!   Q = sqrt(R1 / R2) / 2, about 7.3, a decay time constant of about 15 ms.
//! - The trigger: a 1 ms, 5 V pulse every 250 ms switches an NPN whose collector steps the
//!   inverting node through 100 nF and 100 kohm.
//! - The mix: the output through 10 kohm and 1 uF into two antiparallel diodes, then 10 kohm into
//!   the voice's 1 nF; the noise (sigma 1 V, sampled at 96 kHz) through 4.7 nF into 10 kohm.
//!
//! Per case: the live worker's loop unpaced over a simulated window, `max_step` one output sample
//! (48 kHz, or 96 kHz where the board is said to need it), at Exact and at a resolution of one
//! sample: the realtime factor, steps, Newton iterations, wall per step and, profiled, device
//! evaluation, factorisation and solves. The first voice's measured frequency and decay confirm it
//! rings as designed.
//!
//! `ELEC_TIERS=0` runs every case with whole loads (`Options::tiers` off); `ELEC_BYPASS=0` runs
//! Fast without its device bypass (`Options::bypass` off); `ELEC_SINGLE=<grade>` sets Fast's
//! single pass (`Options::single_pass`, 0 off).
//!
//! Run it native and alone on a quiet box:
//! `cargo run --release -p fab-elec-sim --example voice -- [sim seconds]`.
//! `crossover [sim seconds]` times the dense against the sparse LU per Newton iteration on RC-diode
//! ladders and voice boards of 6 to 130 unknowns; `compare <sim seconds> 1` runs one voice with
//! noise and the six-voice board with the dense LU forced against the default layout. `<sim seconds>
//! metal` runs only the cases with the metal bank: six Schmitt-trigger RC oscillators of two
//! hysteretic switches each, about 7 600 switching edges a second, alone and beside six voices.
//! `partition` reports the islands and weak cuts of the six- and twelve-voice boards and of the
//! drum16 stand-in (`rig/edrum.rs`: the real board's rail, VREF buffer and op-amp macro-models) at
//! 96 kHz. The cases include the stand-in's cost, and one noisy voice at 96 kHz. `held [shares]`
//! reports the same boards' held nodes (`analysis::held`, the hubs rule's report step).

#[path = "rig/edrum.rs"]
mod edrum;

use fab_elec_sim::analysis::partition::{partition, PartOpts};
use fab_elec_sim::live::ValuePlan;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::{CtlKind, ModelKind, SimCircuit, Sink, Source, Wave};
use std::time::{Duration, Instant};

const BEAT: f64 = 0.25;
/// The case name that runs `edrum::board()` in place of the rig's board.
const STAND_IN: &str = "drum16 stand-in";

/// Voice `k`: its nodes carry the suffix `k`; its tuning capacitor is `c` and it is struck
/// `delay` into each beat. With `noise` a noise source joins its mix.
fn add_voice(c: &mut SimCircuit, k: usize, cap: f64, delay: f64, noise: bool) {
    let n = |s: &str| format!("{s}{k}");
    c.v(&n("vtrig"), &n("trig"), "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: delay, tr: 10e-6, tf: 10e-6, pw: 1e-3, per: BEAT }));
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
    if noise {
        c.v(&n("vnoise"), &n("nz"), "0", Source::wave(Wave::Noise { seed: 7 + k as u64, sigma: 1.0, dt: 1.0 / 96_000.0, base: 1.0 / 96_000.0 }));
        c.c(&n("chp"), &n("nz"), &n("hp"), 4.7e-9);
        c.r(&n("rhp"), &n("hp"), "0", 10e3);
        c.r(&n("rnz"), &n("hp"), &n("mix"), 47e3);
    }
}

/// The metal bank (the drum machine's cymbal and hats source): six Schmitt-trigger RC
/// oscillators on a 5 V rail, each an inverter made of two hysteretic switches (the input above
/// 2.85 V closes the one to ground, below 1.85 V the one to the rail; 50 ohm on, 1 Gohm off), a
/// feedback resistor and 10 nF to ground: f = 1.23 / (R C), so 390k to 110k give 315, 410, 513,
/// 615, 820 and 1118 Hz, about 7 600 output edges a second in all. Each output drives 100k into the
/// `metal` node, 16k to ground. The rail ramps up over 10 us from 0 V, so the oscillators start
/// from a defined state.
///
/// Both switches read the same control, the input against ground (the one to the rail through
/// negated thresholds), so their bands sit at the same input and they flip at the same instant.
/// Read against the rail, the rail switch's band lies a fraction of a millivolt away: it flips
/// first, the output stops at mid-rail with both closed, and the input settles there.
fn add_metal(c: &mut SimCircuit) {
    c.model("schmitt_lo", ModelKind::Sw, &[("vt", 2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    c.model("schmitt_hi", ModelKind::Sw, &[("vt", -2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    c.v("v5", "v5", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.0, tr: 10e-6, tf: 10e-6, pw: 1e3, per: 2e3 }));
    c.declare_supply("v5");
    for (k, r) in [390e3, 300e3, 240e3, 200e3, 150e3, 110e3].into_iter().enumerate() {
        let (i, o) = (format!("mi{k}"), format!("mo{k}"));
        c.sw(&format!("slo{k}"), &o, "0", &i, "0", "schmitt_lo", false);
        c.sw(&format!("shi{k}"), "v5", &o, "0", &i, "schmitt_hi", true);
        c.r(&format!("rf{k}"), &o, &i, r);
        c.c(&format!("ct{k}"), &i, "0", 10e-9);
        c.c(&format!("co{k}"), &o, "0", 10e-12);
        c.r(&format!("rm{k}"), &o, "metal", 100e3);
    }
    c.r("rmetal", "metal", "0", 16e3);
}

/// `voices` voices (the first `noisy` of them with noise) into an inverting summer at the
/// reference, and with `metal` the metal bank into the summer too.
fn board(voices: usize, noisy: usize, metal: bool) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("qn", ModelKind::Npn, &[("is", 1e-14), ("bf", 200.0), ("vaf", 100.0), ("cje", 8e-12), ("cjc", 4e-12), ("tf", 4e-10), ("tr", 5e-8)]);
    c.model("dsig", ModelKind::Diode, &[("is", 2.5e-9), ("n", 1.75), ("rs", 0.6), ("cjo", 4e-12), ("tt", 6e-9)]);
    c.v("vcc", "vcc", "0", Source::dc(9.0));
    c.v("vref", "ref", "0", Source::dc(4.5));
    // the rails, as a design declares them; the mixer's sum is a signal
    c.declare_supply("vcc");
    c.declare_supply("ref");
    const CAPS: [f64; 8] = [15e-9, 47e-9, 33e-9, 22e-9, 10e-9, 6.8e-9, 4.7e-9, 3.3e-9];
    for k in 0..voices {
        add_voice(&mut c, k, CAPS[k % CAPS.len()], 1e-3 + k as f64 * BEAT / (2 * voices) as f64, k < noisy);
        c.r(&format!("rsum{k}"), &format!("mix{k}"), "sum", 22e3);
    }
    if metal {
        add_metal(&mut c);
        if voices > 0 {
            c.r("rsum_metal", "metal", "sum", 22e3);
        }
    }
    if voices > 1 || (voices > 0 && metal) {
        c.vctl("summer", CtlKind::Vcvs, "master", "ref", "ref", "sum", 1e5);
        c.r("rf", "master", "sum", 22e3);
        c.c("cf", "master", "sum", 100e-12);
    }
    c
}

/// Keeps one node's samples (the first voice's output against the reference) and, when `all`,
/// every point's solution for the accuracy comparison.
struct Probe {
    out: usize,
    reference: usize,
    at: Vec<f64>,
    v: Vec<f64>,
    all: bool,
    x: Vec<f64>,
    n: usize,
    /// The metal bank's oscillator outputs, whether each is above 2.5 V, and their edges counted.
    osc: Vec<usize>,
    high: Vec<bool>,
    edges: u64,
    /// With `ELEC_DIGEST` set: every accepted point's time and solution bits folded into one
    /// hash, and the points counted (a bit-exact comparison between builds).
    digest: Option<(u64, u64)>,
}

impl Sink for Probe {
    fn point(&mut self, at: f64, _outer: Option<f64>, x: &[f64]) {
        self.at.push(at);
        self.v.push(x[self.out] - x[self.reference]);
        if let Some(h) = &mut self.digest {
            for b in std::iter::once(at.to_bits()).chain(x.iter().map(|v| v.to_bits())) {
                h.0 = (h.0 ^ b).wrapping_mul(0x100000001b3);
            }
            h.1 += 1;
        }
        for (k, &o) in self.osc.iter().enumerate() {
            let high = x[o] > 2.5;
            if high != self.high[k] {
                self.high[k] = high;
                self.edges += 1;
            }
        }
        if self.all {
            self.n = x.len();
            self.x.extend_from_slice(x);
        }
    }
}

/// Linear interpolation of unknown `i` of `r` at the times `grid` (ascending).
fn resample(r: &Probe, i: usize, grid: &[f64], out: &mut Vec<f64>) {
    out.clear();
    let mut k = 0;
    for &g in grid {
        while k + 2 < r.at.len() && r.at[k + 1] <= g {
            k += 1;
        }
        let (t0, t1) = (r.at[k], r.at[k + 1]);
        let (a, b) = (r.x[k * r.n + i], r.x[(k + 1) * r.n + i]);
        out.push(if t1 > t0 { a + (b - a) * ((g - t0) / (t1 - t0)).clamp(0.0, 1.0) } else { b });
    }
}

/// How far `cand` is from `base`, per unknown against the base's range: the worst RMS difference
/// and the worst difference of means.
fn compare(base: &Probe, cand: &Probe, names: &[String]) -> String {
    let (from, to) = (base.at[0].max(cand.at[0]), base.at[base.at.len() - 1].min(cand.at[cand.at.len() - 1]));
    let samples = 400_000;
    let grid: Vec<f64> = (0..samples).map(|k| from + (to - from) * (k as f64 + 0.5) / samples as f64).collect();
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut worst_rms = (0.0, 0usize, 0.0);
    let mut worst_mean = (0.0, 0usize);
    for i in 1..base.n {
        resample(base, i, &grid, &mut a);
        resample(cand, i, &grid, &mut b);
        let (lo, hi) = a.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        let range = hi - lo;
        if !(range > 1e-3) {
            continue;
        }
        let ss: f64 = a.iter().zip(&b).map(|(x, y)| (x - y) * (x - y)).sum();
        let rms = (ss / samples as f64).sqrt() / range;
        let mean = (a.iter().sum::<f64>() - b.iter().sum::<f64>()).abs() / samples as f64 / range;
        if rms > worst_rms.0 {
            worst_rms = (rms, i, range);
        }
        if mean > worst_mean.0 {
            worst_mean = (mean, i);
        }
    }
    format!("vs baseline over {:.1} ms: worst RMS difference {:.2e} of range ({} range {:.3}), worst mean difference {:.2e} of range ({})",
        (to - from) * 1e3, worst_rms.0, names[worst_rms.1], worst_rms.2, worst_mean.0, names[worst_mean.1])
}

/// A diode-loaded RC ladder of `stages` (about `stages` unknowns), driven by a 1 kHz pulse: the
/// sparsest MNA shape, for the dense and sparse LU crossover.
fn ladder(stages: usize) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("dsig", ModelKind::Diode, &[("is", 2.5e-9), ("n", 1.75), ("rs", 0.6), ("cjo", 4e-12), ("tt", 6e-9)]);
    c.v("vin", "n0", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.0, tr: 10e-6, tf: 10e-6, pw: 0.5e-3, per: 1e-3 }));
    for k in 0..stages {
        let (a, b) = (format!("n{k}"), format!("n{}", k + 1));
        c.r(&format!("r{k}"), &a, &b, 1e3);
        c.c(&format!("c{k}"), &b, "0", 10e-9);
        c.d(&format!("d{k}"), &b, "0", "dsig");
    }
    c
}

/// The dense against the sparse LU on the same circuits: us a Newton iteration and the LU's own
/// share (factor + solve per iteration), 48 kHz steps, Fast.
fn crossover(secs: f64) {
    let mut cases: Vec<(String, SimCircuit)> = [2usize, 4, 8, 12, 16, 24, 32, 48, 64].iter().map(|&k| (format!("ladder {k}"), ladder(k))).collect();
    for v in 1..=3 {
        cases.push((format!("{v} voice(s)"), board(v, 0, false)));
    }
    cases.push(("1 voice + noise".into(), board(1, 1, false)));
    for (name, c) in cases {
        let mut line = format!("{name:>16}:");
        for dense in [true, false] {
            let mut c = c.clone();
            c.opts.force_dense = Some(dense);
            c.opts.profile = true;
            let spec = LiveSpec { rate: f64::INFINITY, max_step: 1.0 / 48e3, res: 1.0 / 48e3, trace: true, ..LiveSpec::default() };
            let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("an operating point");
            let n = sim.solver().n;
            let mut sink = Null;
            let start = Instant::now();
            while sim.t() < secs {
                sim.advance(secs, Some(Duration::from_millis(8)), &mut sink).expect("it steps");
            }
            let wall = start.elapsed();
            let st = sim.solver().stats;
            let p = sim.solver().profile.as_deref().cloned().unwrap_or_default();
            let it = st.iterations.max(1) as f64;
            line += &format!("  {} n {n}: {:.2} us an iteration, LU {:.2} us an iteration;", if dense { "dense" } else { "sparse" },
                wall.as_secs_f64() * 1e6 / it, (p.factor_ns + p.refactor_ns + p.solve_ns) as f64 / 1e3 / it);
        }
        println!("{line}");
    }
}

struct Null;
impl Sink for Null {}

/// The first strike's ringing: its frequency from the upward zero crossings between 5 and 60 ms
/// after it, and its decay time constant from the first and last of those cycles' peaks.
fn ringing(p: &Probe) -> (f64, f64) {
    let (from, to) = (1e-3 + 5e-3, 1e-3 + 60e-3);
    let mut ups = Vec::new();
    let mut peaks: Vec<(f64, f64)> = Vec::new();
    for k in 1..p.at.len() {
        if p.at[k] < from || p.at[k] > to {
            continue;
        }
        let (a, b) = (p.v[k - 1], p.v[k]);
        if a < 0.0 && b >= 0.0 {
            let t = p.at[k - 1] + (p.at[k] - p.at[k - 1]) * (-a / (b - a));
            ups.push(t);
            peaks.push((t, 0.0));
        }
        if let Some(last) = peaks.last_mut() {
            last.1 = last.1.max(b.abs());
        }
    }
    if ups.len() < 3 {
        return (f64::NAN, f64::NAN);
    }
    let f = (ups.len() - 1) as f64 / (ups[ups.len() - 1] - ups[0]);
    let (first, last) = (peaks[0], peaks[peaks.len() - 2]);
    (f, (last.0 - first.0) / (first.1 / last.1).ln())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

#[allow(clippy::too_many_arguments)]
fn run(name: &str, voices: usize, noisy: usize, metal: bool, rate: f64, exact: bool, profile: bool, secs: f64, variant: u32, all: bool) -> (Probe, Vec<String>) {
    let mut c = if name == STAND_IN { edrum::board() } else { board(voices, noisy, metal) };
    c.opts.profile = profile;
    if variant & 1 != 0 || std::env::var_os("ELEC_FORCE_DENSE").is_some() {
        c.opts.force_dense = Some(true);
    }
    // variant 2 + 2g: Fast's single pass at grade g
    // `ELEC_TIERS=0`: whole loads, without the three-tier load
    if std::env::var("ELEC_TIERS").as_deref() == Ok("0") {
        c.opts.tiers = false;
    }
    // `ELEC_BYPASS=0`: Fast evaluates every device at every iteration
    if std::env::var("ELEC_BYPASS").as_deref() == Ok("0") {
        c.opts.bypass = false;
    }
    // `ELEC_SINGLE=<grade>`: Fast's single pass at that grade (0 off)
    if let Ok(grade) = std::env::var("ELEC_SINGLE") {
        c.opts.single_pass = grade.parse().expect("ELEC_SINGLE is a grade, 0 to 2");
    }
    if variant & 6 != 0 {
        c.opts.single_pass = (variant / 2 - 1) as u8;
    }
    let sample = 1.0 / rate;
    let spec = LiveSpec { rate: f64::INFINITY, max_step: sample, res: if exact { 0.0 } else { sample }, trace: true, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the voices' operating point");
    let s = sim.solver();
    let out = s.unknown("out0").or_else(|| s.unknown("bd.out")).or_else(|| s.unknown("mo0")).expect("a voice or an oscillator") as usize;
    let reference = s.unknown("ref").expect("ref") as usize;
    let (n, dense) = (s.n, s.layout.is_dense());
    let osc: Vec<usize> = (0..6).filter_map(|k| s.unknown(&format!("mo{k}")).map(|u| u as usize)).collect();
    let high = vec![false; osc.len()];
    let mut probe = Probe { out, reference, at: Vec::new(), v: Vec::new(), all, x: Vec::new(), n: 0, osc, high, edges: 0,
        digest: std::env::var_os("ELEC_DIGEST").map(|_| (0xcbf29ce484222325, 0)) };
    let start = Instant::now();
    let mut last_values = start;
    let progress = std::env::var_os("ELEC_PROGRESS").is_some();
    let mut last_progress = start;
    while sim.t() < secs {
        sim.advance(secs, Some(Duration::from_millis(8)), &mut probe).expect("the voices step");
        if progress && last_progress.elapsed() >= Duration::from_millis(500) {
            let st = sim.solver().stats;
            eprintln!("  progress: t {:.6} ms, {} points, {} steps, {} rejected (newton {}, crossing {}, provisional {}, error {}), floor steps {}",
                sim.t() * 1e3, probe.at.len(), st.steps, st.rejected, st.rejected_newton, st.rejected_crossing, st.rejected_provisional, st.rejected_error, st.floor_steps);
            last_progress = Instant::now();
        }
        if last_values.elapsed() >= Duration::from_millis(16) {
            sim.values(0.0);
            sim.take_delta();
            last_values = Instant::now();
        }
    }
    let wall = start.elapsed();
    let t = sim.t();
    let st = sim.solver().stats;
    let rs = sim.restarts();
    let (f, tau) = ringing(&probe);
    let edges = if probe.osc.is_empty() { String::new() } else { format!("; metal bank: {:.0} oscillator edges a second (about 7 600 expected)", probe.edges as f64 / t) };
    let label = format!("{name}, {:.0} kHz, {}{}{}", rate / 1e3, if exact { "Exact" } else { "Fast" }, if profile { " (profiled)" } else { "" },
        format!("{}{}", if variant & 1 != 0 { ", dense LU forced" } else { "" },
            if variant & 6 != 0 { format!(", single pass {}", variant / 2 - 1) } else { String::new() }));
    println!();
    println!("{label}: {:.0} ms simulated in {:.1} ms wall: realtime x{:.3}; {n} unknowns ({}); voice 0 rings at {:.1} Hz, tau {:.1} ms{edges}",
        t * 1e3, ms(wall), t / wall.as_secs_f64(), if dense { "dense LU" } else { "sparse LU" }, f, tau * 1e3);
    let per = |x: u64| x as f64 / t;
    println!("  per simulated second: {:.0} points ({:.2} a sample), {:.0} steps tried, {:.0} rejected (newton {:.0}, crossing {:.0}, provisional {:.0}, error {:.0}), floor steps {:.0}; restarts: corner {:.0}, crossing {:.0} (cut ahead {:.0}; cuts ahead short {:.0}), change {:.0}",
        per(probe.at.len() as u64), probe.at.len() as f64 / (t * rate), per(st.steps), per(st.rejected), per(st.rejected_newton), per(st.rejected_crossing),
        per(st.rejected_provisional), per(st.rejected_error), per(st.floor_steps), per(rs.corner), per(rs.crossing), per(rs.crossing_ahead), per(rs.ahead_short), per(rs.change));
    if probe.edges > 0 {
        // the steps an oscillator edge costs: what is tried beyond one step a sample, per edge
        let e = probe.edges as f64;
        println!("  per oscillator edge: {:.2} steps tried beyond one a sample; rejected: newton {:.2}, crossing {:.2}, provisional {:.2}, error {:.2}",
            (st.steps as f64 - t * rate) / e, st.rejected_newton as f64 / e, st.rejected_crossing as f64 / e, st.rejected_provisional as f64 / e, st.rejected_error as f64 / e);
    }
    println!("  {:.2} Newton iterations a step, {:.2} us a step tried, {:.2} us an iteration",
        st.iterations as f64 / st.steps.max(1) as f64, wall.as_secs_f64() * 1e6 / st.steps.max(1) as f64, wall.as_secs_f64() * 1e6 / st.iterations.max(1) as f64);
    if let Some(p) = &sim.solver().profile {
        let total = wall.as_nanos() as f64;
        let share = |x: u64| 100.0 * x as f64 / total;
        println!("  profile: load {:.0} %, factor {:.0} % ({}), refactor {:.0} % ({}), solve {:.0} %, the rest {:.0} %",
            share(p.load_ns), share(p.factor_ns), p.factors, share(p.refactor_ns), p.refactors, share(p.solve_ns),
            100.0 - share(p.load_ns + p.factor_ns + p.refactor_ns + p.solve_ns));
        let us = |x: u64| x as f64 / 1e3 / st.steps.max(1) as f64;
        println!("  us a step tried: load {:.2}, factor {:.2}, refactor {:.2}, solve {:.2}, the rest {:.2}",
            us(p.load_ns), us(p.factor_ns), us(p.refactor_ns), us(p.solve_ns),
            us(wall.as_nanos() as u64 - p.load_ns - p.factor_ns - p.refactor_ns - p.solve_ns));
        let kinds = ["on its way", "after a restart", "cut to a crossing"];
        for k in 0..3 {
            println!("  steps {}: {:.0} a simulated second, {:.2} iterations each", kinds[k], per(p.kind_steps[k]),
                p.kind_iterations[k] as f64 / p.kind_steps[k].max(1) as f64);
        }
        println!("  refactors: {:.2} a step tried, {:.1} % on a matrix bit-equal to the one before", (p.refactors + p.factors) as f64 / st.steps.max(1) as f64,
            100.0 * p.same_matrix as f64 / p.refactors.max(1) as f64);
        if let Some((fill, refactor, solve)) = sim.solver().lu_work() {
            println!("  LU: {n} unknowns, {} matrix entries, {fill} in L and U, {refactor} multiply-subtracts a refactor, {solve} a solve",
                sim.solver().layout.len());
        }
    }
    if let Some((h, n)) = probe.digest {
        println!("  digest {label}: {n} points {h:016x}");
    }
    let names = sim.solver().unknowns.iter().map(|u| u.name.clone()).collect();
    (probe, names)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("partition") {
        let opts = PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: 1e-3, hub_tol: 0.1, fmin: 10.0, fmax: 48e3, band: f64::INFINITY, per_octave: 4, min_island: 8, control: vec![], supplies: Vec::new(), round: 32.0 / 96e3 };
        // the stand-in also with an ideal VA rail (what a cut at the rail would give), and at a
        // cut tolerance of 1e-2 (an accuracy trade, shown, not chosen)
        let loose = PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: 1e-2, ..opts.clone() };
        for (name, voices, noisy, o) in [("6 voices + 2 noise + metal bank", 6, 2, &opts), ("12 voices + 4 noise + metal bank", 12, 4, &opts),
            ("drum16 stand-in", 0, 0, &opts), ("drum16 stand-in, ideal VA rail", 0, 1, &opts), ("drum16 stand-in, cut_tol 1e-2", 0, 0, &loose)] {
            let circuit = if voices == 0 { edrum::board_with(noisy == 1) } else { board(voices, noisy, true) };
            let start = Instant::now();
            let result = partition(&circuit, o);
            let ms = start.elapsed().as_secs_f64() * 1e3;
            match result {
                Ok(p) => {
                    print!("{}", p.report(name));
                    // the shared nodes as an exact border instead of weak cuts
                    if name == STAND_IN {
                        for border in [&["ref"][..], &["ref", "va"], &["ref", "va", "mx.inn"]] {
                            match p.border_view(border) {
                                Ok(view) => print!("{view}"),
                                Err(e) => eprintln!("{name}: border view: {e}"),
                            }
                        }
                    }
                }
                Err(d) => eprintln!("{name}: partition failed: {d}"),
            }
            println!("partition wall time: {ms:.3} ms");
        }
        return;
    }
    if args.first().map(String::as_str) == Some("held") {
        // the held step (`analysis::held`) and the round plan it gives (`analysis::round`) on the
        // same boards at their operating points; `held shares` adds each searched node's shares
        let opts = PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: 1e-3, hub_tol: 0.1, fmin: 10.0, fmax: 48e3, band: f64::INFINITY, per_octave: 4, min_island: 8, control: vec![], supplies: Vec::new(), round: 32.0 / 96e3 };
        for (name, voices, noisy) in [("6 voices + 2 noise + metal bank", 6, 2), ("12 voices + 4 noise + metal bank", 12, 4), ("drum16 stand-in", 0, 0), ("drum16 stand-in, ideal VA rail", 0, 1)] {
            let circuit = if voices == 0 { edrum::board_with(noisy == 1) } else { board(voices, noisy, true) };
            let mut s = fab_elec_sim::Solver::new(&circuit).expect("the board builds");
            fab_elec_sim::analysis::op::operating_point(&mut s, false).expect("the board's operating point");
            println!();
            match fab_elec_sim::analysis::held::held(&s, 0.0, &fab_elec_sim::analysis::round::circuit_opts(&circuit, &opts)) {
                Ok(h) => {
                    print!("{}", h.report(name));
                    if args.get(1).map(String::as_str) == Some("shares") {
                        print!("{}", h.share_table(8));
                    }
                }
                Err(d) => eprintln!("{name}: held failed: {d}"),
            }
            match fab_elec_sim::analysis::round::plan_blocks(&circuit, &s, 0.0, &opts) {
                Ok(Some(plan)) => print!("{}", plan.report),
                Ok(None) => println!("{name}: no round plan"),
                Err(d) => eprintln!("{name}: round plan failed: {d}"),
            }
        }
        return;
    }
    if args.first().map(String::as_str) == Some("crossover") {
        crossover(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.1));
        return;
    }
    if args.first().map(String::as_str) == Some("metaltrace") {
        // the metal bank alone, Exact then Fast: the first oscillator's input and output every 0.2 ms
        let modes: &[bool] = if args.get(1).map(String::as_str) == Some("fast") { &[false] } else { &[true, false] };
        for &exact in modes {
            let (p, names) = run("metal bank", 0, 0, true, 96e3, exact, false, 0.008, 0, true);
            let (i, o) = (names.iter().position(|n| n == "mi0").unwrap(), names.iter().position(|n| n == "mo0").unwrap());
            let mut next = 0.0;
            for k in 0..p.at.len() {
                if p.at[k] >= next {
                    println!("  t {:.3} ms: mi0 {:.4} V, mo0 {:.4} V ({} points so far)", p.at[k] * 1e3, p.x[k * p.n + i], p.x[k * p.n + o], k);
                    next += 2e-4;
                }
            }
        }
        return;
    }
    if args.first().map(String::as_str) == Some("compare") {
        // `compare <secs> <variant,..> [base variant]` (1: the dense LU forced; 2 + 2g: Fast's single
        // pass at grade g): one voice with noise, the six-voice board, the metal bank and the
        // drum16 stand-in, Exact and Fast, each variant against the base (default 0)
        let secs: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.25);
        let variants: Vec<u32> = args.get(2).map_or("1", String::as_str).split(',').filter_map(|v| v.parse().ok()).collect();
        let base_variant: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
        let cases = [("1 voice + noise", 1, 1, false, 48e3, secs), ("6 voices + 2 noise", 6, 2, false, 96e3, secs / 2.0),
            ("metal bank", 0, 0, true, 96e3, secs / 2.0), (STAND_IN, 0, 0, true, 96e3, secs / 2.0)];
        for (name, voices, noisy, metal, rate, window) in cases {
            for exact in [true, false] {
                let (base, names) = run(name, voices, noisy, metal, rate, exact, false, window, base_variant, true);
                for &e in &variants {
                    let (cand, _) = run(name, voices, noisy, metal, rate, exact, false, window, e, true);
                    println!("  {}", compare(&base, &cand, &names));
                }
            }
        }
        return;
    }
    let secs: f64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(0.5);
    // `metal` runs the metal-bank cases only, `nometal` all the others
    let metal_only = args.get(1).map(String::as_str) == Some("metal");
    if args.get(1).map(String::as_str) == Some("board") {
        // the drum board alone, Fast: plain (for a sampled profile), then profiled
        run("6 voices + 2 noise + metal bank", 6, 2, true, 96e3, false, false, secs, 0, false);
        run("6 voices + 2 noise + metal bank", 6, 2, true, 96e3, false, true, secs, 0, false);
        return;
    }
    if args.get(1).map(String::as_str) == Some("standin") {
        // the drum16 stand-in alone, Fast: plain, then profiled
        run(STAND_IN, 0, 0, true, 96e3, false, false, secs, 0, false);
        run(STAND_IN, 0, 0, true, 96e3, false, true, secs, 0, false);
        return;
    }
    let no_metal = args.get(1).map(String::as_str) == Some("nometal");
    let cases: [(&str, usize, usize, bool, f64, f64); 9] = [
        ("1 voice", 1, 0, false, 48e3, secs),
        ("1 voice + noise", 1, 1, false, 48e3, secs),
        ("1 voice + noise", 1, 1, false, 96e3, secs),
        ("6 voices + 2 noise", 6, 2, false, 48e3, secs / 2.0),
        ("6 voices + 2 noise", 6, 2, false, 96e3, secs / 2.0),
        ("12 voices + 4 noise", 12, 4, false, 96e3, secs / 4.0),
        ("metal bank", 0, 0, true, 96e3, secs / 2.0),
        ("6 voices + 2 noise + metal bank", 6, 2, true, 96e3, secs / 2.0),
        (STAND_IN, 0, 0, true, 96e3, secs / 2.0),
    ];
    for (name, voices, noisy, metal, rate, window) in cases {
        if (metal_only && !metal) || (no_metal && metal) {
            continue;
        }
        for exact in [true, false] {
            run(name, voices, noisy, metal, rate, exact, false, window, 0, false);
            run(name, voices, noisy, metal, rate, exact, true, window, 0, false);
        }
    }
}
