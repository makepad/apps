//! The wave mode's kernel of a whole voice bench against Exact (SIMWDF W0): one strike's pitch
//! (cents), decay and peak (%), its third-octave bands to 10 kHz (dB), and the kernel's time a
//! sample interpreted and compiled (the best of five runs), with the compiled run's bits against
//! the interpreter's, at each sample rate asked.
//!
//! `wave [bench] [net] [seconds] [rates..]`, by default the drum16 kick's bench, its `OUT`, 0.5 s,
//! at 48 and 96 kHz. The bench's first strike is taken to start at its first source edge after
//! 10 ms. Run it natively for timings:
//! `cargo run --release -p fab-elec-fixtures --example wave -- kick OUT 0.5 48000 96000`.

use fab_elec_bench::{lower, ModelLibs};
use fab_elec_lang::{evaluate, std_libs};
use fab_elec_sim::fft::{band_rms, resample_hermite, Window};
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_sim::wave::build::build;
use fab_elec_sim::wave::{CompiledWave, WaveCompiler as _, WavePlan};
use fab_elec_sim::{Options, Sink};
use std::time::Instant;

/// One net's voltage at every accepted point.
struct Probe {
    net: String,
    at: usize,
    t: Vec<f64>,
    v: Vec<f64>,
}

impl Sink for Probe {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.at = unknowns.iter().position(|u| u.name == self.net).unwrap_or_else(|| panic!("no unknown is net {}", self.net));
    }
    fn point(&mut self, t: f64, _outer: Option<f64>, x: &[f64]) {
        self.t.push(t);
        self.v.push(x[self.at]);
    }
}

/// A strike's ring from `strike` to `to` on samples `y` every `dt`: its pitch from the zero
/// crossings about the rest level, its decay time constant from the lobes' peaks, its first peak.
fn ring(y: &[f64], dt: f64, strike: f64, to: f64) -> (f64, f64, f64) {
    let (k0, k1) = ((strike / dt) as usize, ((to / dt) as usize).min(y.len()));
    let rest = y[k0.saturating_sub(1)];
    let (mut crossings, mut peaks, mut lobe) = (Vec::new(), Vec::new(), (0.0, 0.0_f64));
    for k in k0..k1 - 1 {
        let (t0, v0, t1, v1) = (k as f64 * dt, y[k] - rest, (k + 1) as f64 * dt, y[k + 1] - rest);
        if v1.abs() > lobe.1.abs() {
            lobe = (t1, v1);
        }
        if v0 != 0.0 && v0.signum() != v1.signum() && t0 > strike + 1e-3 {
            crossings.push(t0 + (t1 - t0) * v0 / (v0 - v1));
            peaks.push(lobe);
            lobe = (t1, 0.0);
        }
    }
    let first = peaks.iter().map(|p: &(f64, f64)| p.1.abs()).fold(0.0, f64::max);
    let peaks: Vec<(f64, f64)> = peaks.into_iter().filter(|p| p.1.abs() > first / 20.0).collect();
    let last = peaks.last().expect("a ring").0;
    let crossings: Vec<f64> = crossings.into_iter().filter(|&t| t <= last).collect();
    assert!(crossings.len() > 6, "the ring lasts several cycles: {} crossings", crossings.len());
    let f0 = (crossings.len() - 1) as f64 / (2.0 * (crossings[crossings.len() - 1] - crossings[0]));
    let n = peaks.len() as f64;
    let (st, sl) = peaks.iter().fold((0.0, 0.0), |(st, sl), p| (st + p.0, sl + p.1.abs().ln()));
    let (mt, ml) = (st / n, sl / n);
    let (cov, var) = peaks.iter().fold((0.0, 0.0), |(c, v), p| (c + (p.0 - mt) * (p.1.abs().ln() - ml), v + (p.0 - mt).powi(2)));
    (f0, -var / cov, first)
}

/// Third-octave band levels (dB re 1 V) from 25 Hz to 10 kHz of `y` every `dt`.
fn bands(y: &[f64], dt: f64) -> Vec<(f64, f64)> {
    (0..27).map(|i| {
        let fc = 25.0 * 2f64.powf(i as f64 / 3.0);
        let (lo, hi) = (fc / 2f64.powf(1.0 / 6.0), fc * 2f64.powf(1.0 / 6.0));
        (fc, 20.0 * band_rms(y, dt, Window::Hann, lo, hi).max(1e-30).log10())
    }).filter(|b| b.0 <= 10e3).collect()
}

/// The best of five runs of `samples` samples from `a0`, in calls of `chunk`, seconds a sample.
fn timed(run: &dyn Fn(&mut [f64], &[f64], &mut [f64], &mut [f64], usize), a0: &[f64], u: &[f64], ni: usize, y: &mut [f64], samples: usize, chunk: usize) -> f64 {
    let mut b = vec![0.0; a0.len()];
    let mut best = f64::INFINITY;
    for _ in 0..5 {
        let mut a = a0.to_vec();
        let wall = Instant::now();
        let mut k = 0;
        while k < samples {
            let n = chunk.min(samples - k);
            run(&mut a, &u[k * ni..(k + n) * ni], &mut y[k..k + n], &mut b, n);
            k += n;
        }
        best = best.min(wall.elapsed().as_secs_f64() / samples as f64);
    }
    best
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bench_name = args.first().cloned().unwrap_or_else(|| "kick".into());
    let net = args.get(1).cloned().unwrap_or_else(|| "OUT".into());
    let seconds: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let mut rates: Vec<f64> = args.iter().skip(3).filter_map(|s| s.parse().ok()).collect();
    if rates.is_empty() {
        rates = vec![48000.0, 96000.0];
    }
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("drum16 evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == bench_name).unwrap_or_else(|| panic!("drum16 has no bench {bench_name}"));
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "the bench lowers without diagnostics: {:?}", lowered.diags);
    let circuit = &lowered.circuit;
    let probe = circuit.nodes.iter().position(|n| n.name == net).unwrap_or_else(|| panic!("no node {net}")) as u32;

    for &rate in &rates {
        let period = 1.0 / rate;
        let samples = (seconds * rate).round() as usize;
        // Exact at this sample rate
        let spec = LiveSpec { rate: f64::INFINITY, reltol: Some(Options::default().reltol), trace: false, sample_rate: rate, ..LiveSpec::default() };
        let mut sim = LiveSim::new(circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
            .unwrap_or_else(|e| panic!("the bench's operating point: {}", e.message));
        let x0: Vec<f64> = circuit.nodes.iter().enumerate()
            .map(|(i, n)| if i == 0 { 0.0 } else { sim.solver().unknown(&n.name).map_or(0.0, |u| sim.solver().x[u as usize]) })
            .collect();
        let mut exact = Probe { net: net.clone(), at: 0, t: Vec::new(), v: Vec::new() };
        exact.begin(&sim.solver().unknowns);
        let wall = Instant::now();
        sim.apply(fab_elec_sim::live::LiveCmd::Step(seconds - sim.t()));
        while sim.t() < seconds {
            sim.advance(f64::INFINITY, None, &mut exact).unwrap_or_else(|e| panic!("Exact goes on: {}", e.message));
        }
        let exact_wall = wall.elapsed().as_secs_f64();
        let ye = resample_hermite(&exact.t, &exact.v, None, 0.0, period, samples);

        // the kernel
        let kernel = build(circuit, &x0, period, &[probe]).unwrap_or_else(|e| panic!("the kernel: {e}"));
        let ni = kernel.inputs.len();
        let mut u = vec![0.0; samples * ni];
        for (k, row) in u.chunks_mut(ni).enumerate() {
            kernel.inputs_at(circuit, k as f64 * period, row);
        }
        let plan: &WavePlan = &kernel.plan;
        let consts = &kernel.consts;
        let interp = |a: &mut [f64], u: &[f64], y: &mut [f64], b: &mut [f64], n: usize| plan.run(consts, a, u, y, b, n);
        let (mut y, mut y2) = (vec![0.0; samples], vec![0.0; samples]);
        let run_s = timed(&interp, &kernel.a0, &u, ni, &mut y, samples, samples);
        let rounds_s = timed(&interp, &kernel.a0, &u, ni, &mut y2, samples, 32);
        let same = y.iter().zip(&y2).all(|(p, q)| p.to_bits() == q.to_bits());
        let wall = Instant::now();
        let compiled: Option<std::sync::Arc<dyn CompiledWave>> = fab_elec_simjit::WaveCompiler.compile(plan);
        let compile_s = wall.elapsed().as_secs_f64();
        let jit = compiled.as_ref().map(|c| {
            let run = |a: &mut [f64], u: &[f64], y: &mut [f64], b: &mut [f64], n: usize| c.run(consts, a, u, y, b, n);
            let (mut y3, mut y4) = (vec![0.0; samples], vec![0.0; samples]);
            let one = timed(&run, &kernel.a0, &u, ni, &mut y3, samples, samples);
            let rounds = timed(&run, &kernel.a0, &u, ni, &mut y4, samples, 32);
            let same = y.iter().zip(&y3).chain(y.iter().zip(&y4)).all(|(p, q)| p.to_bits() == q.to_bits());
            (one, rounds, same, c.stats().code_bytes)
        });

        println!("{bench_name} at {:.0} kHz, {seconds} s: kernel of {} ports ({}), {} inputs, {} outputs, {} multiply-adds a sample; rest error {:.2e}",
            rate / 1e3, kernel.plan.ports, kernel.ports.join(" "), ni, kernel.plan.outputs, kernel.plan.ops(), kernel.rest_error);
        println!("  interpreted: {:.4} us a sample in one call, {:.4} us in rounds of 32 (same bits: {same}); Exact {:.1} ms wall for the run",
            run_s * 1e6, rounds_s * 1e6, exact_wall * 1e3);
        match jit {
            Some((one, rounds, same, bytes)) => println!("  compiled ({:.2} ms, {bytes} bytes): {:.4} us a sample in one call, {:.4} us in rounds of 32; same bits as interpreted: {same}",
                compile_s * 1e3, one * 1e6, rounds * 1e6),
            None => println!("  compiled: declined"),
        }
        let strike = 0.01;
        let to = seconds.min(0.45);
        let (fe, te, pe) = ring(&ye, period, strike, to);
        let (fk, tk, pk) = ring(&y, period, strike, to);
        println!("  pitch: Exact {fe:.4} Hz, kernel {fk:.4} Hz: {:+.3} cents", 1200.0 * (fk / fe).log2());
        println!("  decay: Exact {:.2} ms, kernel {:.2} ms: {:+.2} %; peak: Exact {pe:.4} V, kernel {pk:.4} V: {:+.2} %",
            te * 1e3, tk * 1e3, 100.0 * (tk / te - 1.0), 100.0 * (pk / pe - 1.0));
        let (k0, k1) = ((strike / period) as usize, ((to / period) as usize).min(samples));
        let (be, bk) = (bands(&ye[k0..k1], period), bands(&y[k0..k1], period));
        let top = be.iter().map(|b| b.1).fold(f64::NEG_INFINITY, f64::max);
        let worst = be.iter().zip(&bk).filter(|(e, _)| e.1 > top - 60.0).map(|(e, k)| (e.0, k.1 - e.1)).fold((0.0, 0.0), |w: (f64, f64), b| if b.1.abs() > w.1.abs() { b } else { w });
        println!("  bands (within 60 dB of the strongest, {:.1} dB): worst {:+.3} dB at {:.0} Hz", top, worst.1, worst.0);
        let line: Vec<String> = be.iter().zip(&bk).map(|(e, k)| format!("{:.0}:{:+.2}", e.0, k.1 - e.1)).collect();
        println!("  per band (Hz:dB): {}", line.join(" "));
        let diff = y.iter().zip(&ye).skip(k0).take(k1 - k0).fold(0.0, |m: f64, (p, q)| m.max((p - q).abs()));
        println!("  largest sample difference over the ring: {diff:.3e} V");
    }
}
