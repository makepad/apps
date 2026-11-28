//! drum16, an analog drum machine. Its voices are textbook topologies dimensioned from their
//! formulas; each test runs a voice's bench through the whole chain (its documents, the model
//! binding, the lowering and the live solver) and holds the sound it makes to those formulas.

use fab_elec_bench::input::Taper;
use fab_elec_bench::{lower, ModelLibs, SimCmd};
use fab_elec_circuit::CheckKind;
use fab_elec_core::ids::CompRef;
use fab_elec_core::{NetRef, Sel};
use fab_elec_lang::{evaluate, std_libs, Evaluated, UnitState};
use fab_elec_sim::fft::{band_rms, resample_hermite, Window};
use fab_elec_sim::livesim::{LiveSim, LiveSpec, AUDIO_SAMPLE_RATE};
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_mcu_avr::machine::AvrMachine;
use fab_elec_mcu_cosim::{CoMachine, Config, LogicMachine};
use fab_elec_mcu_emu::Lockstep;
use fab_elec_sim::cosim::{CoSimDevice, CoSimSlot};
use fab_elec_sim::live::LiveCmd;
use fab_elec_sim::{Options, Sink};
use std::f64::consts::PI;
use std::time::Instant;

fn drum16() -> Evaluated {
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("drum16 evaluates: {diags:?}"));
    for state in ev.units.iter().map(|u| &u.state).chain(ev.benches.iter().map(|b| &b.state)) {
        assert_eq!(*state, UnitState::Built, "every drum16 unit and bench builds: {:?}", state.diags());
    }
    ev
}

/// Some nets' voltages at every accepted point of a run. A net is named as the design names it
/// (`OUT`), or inside a block by its binding (`y[0]` is `metal.y[0]`).
struct Scope {
    nets: Vec<&'static str>,
    at: Vec<usize>,
    t: Vec<f64>,
    v: Vec<Vec<f64>>,
}

impl Scope {
    fn new(nets: &[&'static str]) -> Scope {
        Scope { nets: nets.to_vec(), at: Vec::new(), t: Vec::new(), v: vec![Vec::new(); nets.len()] }
    }

    /// The net's (t, v) points.
    fn trace(&self, net: &str) -> Vec<(f64, f64)> {
        let k = self.nets.iter().position(|n| *n == net).expect("a recorded net");
        self.t.iter().copied().zip(self.v[k].iter().copied()).collect()
    }
}

impl Sink for Scope {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.at = self.nets.iter().map(|net| {
            let inside = format!(".{net}");
            unknowns.iter().position(|u| u.name == *net)
                .or_else(|| unknowns.iter().position(|u| u.name.ends_with(&inside)))
                .unwrap_or_else(|| panic!("no unknown is net {net}: {:?}", unknowns.iter().map(|u| &u.name).collect::<Vec<_>>()))
        }).collect();
    }
    fn point(&mut self, t: f64, _outer: Option<f64>, x: &[f64]) {
        self.t.push(t);
        for (v, &i) in self.v.iter_mut().zip(&self.at) {
            v.push(x[i]);
        }
    }
}

/// A ring struck at `strike`, measured until `to`: its pitch (Hz), decay time constant (s) and
/// largest peak (V). About the voltage it rested at before the strike, its zero crossings give
/// the period, and the logarithm of its half-cycles' peaks falls linearly with the decay. Peaks
/// under a twentieth of the largest are left out (what remains of a ring then is small beside the
/// solver's tolerance). Crossings count from the trigger's end (1 ms): a ring's first crossing
/// comes a quarter period after the pulse's middle, and any wiggle about the rest level before
/// the pulse takes hold would count a half-cycle that is not there.
fn ring(points: &[(f64, f64)], strike: f64, to: f64) -> (f64, f64, f64) {
    let rest = points.iter().take_while(|p| p.0 < strike).last().expect("points before the strike").1;
    let ring: Vec<(f64, f64)> = points.iter().filter(|p| p.0 > strike && p.0 < to).map(|&(t, v)| (t, v - rest)).collect();
    let mut crossings = Vec::new();
    let mut peaks: Vec<(f64, f64)> = Vec::new();
    let mut lobe = (0.0, 0.0_f64);
    for w in ring.windows(2) {
        let ((t0, v0), (t1, v1)) = (w[0], w[1]);
        if v1.abs() > lobe.1.abs() {
            lobe = (t1, v1);
        }
        if v0 != 0.0 && v0.signum() != v1.signum() && t0 > strike + 1e-3 {
            crossings.push(t0 + (t1 - t0) * v0 / (v0 - v1));
            peaks.push(lobe);
            lobe = (t1, 0.0);
        }
    }
    let first = peaks.iter().map(|p| p.1.abs()).fold(0.0, f64::max);
    let peaks: Vec<(f64, f64)> = peaks.into_iter().filter(|p| p.1.abs() > first / 20.0).collect();
    let last = peaks.last().expect("a ring").0;
    let crossings: Vec<f64> = crossings.into_iter().filter(|&t| t <= last).collect();
    assert!(crossings.len() > 6, "the ring lasts several cycles: {} crossings", crossings.len());
    let f0 = (crossings.len() - 1) as f64 / (2.0 * (crossings[crossings.len() - 1] - crossings[0]));
    // the least-squares slope of ln|peak| over time is -1 / tau
    let n = peaks.len() as f64;
    let (st, sl) = peaks.iter().fold((0.0, 0.0), |(st, sl), p| (st + p.0, sl + p.1.abs().ln()));
    let (mt, ml) = (st / n, sl / n);
    let (cov, var) = peaks.iter().fold((0.0, 0.0), |(c, v), p| (c + (p.0 - mt) * (p.1.abs().ln() - ml), v + (p.0 - mt).powi(2)));
    (f0, -var / cov, first)
}

/// Runs `sim` on to `t`, every accepted point into `scope`. A co-simulated MCU may end an advance
/// at the instant it began (serving the process); a run that stays put for long is stuck.
fn run_to(sim: &mut LiveSim, scope: &mut Scope, t: f64) {
    sim.apply(fab_elec_sim::live::LiveCmd::Step(t - sim.t()));
    let mut still = 0;
    while sim.t() < t {
        let before = sim.t();
        sim.advance(f64::INFINITY, None, scope).unwrap_or_else(|e| panic!("the run goes on: {}", e.message));
        still = if sim.t() > before { 0 } else { still + 1 };
        assert!(still < 1000, "the run moves (stuck at {before} s)");
    }
}

/// The kick: a ringing filter struck by an unaccented trigger (3.4 V for 1 ms) rings at the
/// textbook pitch and decay of its parts, and peaks at v tp / (r1 c). Turning DECAY while it
/// plays (a live command, no edit) makes the next strike ring with the new r2, pitch and decay
/// both as the formulas say.
#[test]
fn kick_rings_at_its_formula_pitch_and_decay_and_decay_turns_live() {
    let ev = drum16();
    let bench = ev.benches.iter().find(|b| b.name == "kick").expect("drum16 has the kick bench");
    let libs = ModelLibs::generic();
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &libs);
    assert!(lowered.diags.is_empty(), "the kick lowers without diagnostics: {:?}", lowered.diags);

    let unit = ev.design.unit(bench.scope_id()).expect("the kick bench runs the kick unit");
    let comp = |path: &str| {
        let c = unit.circuit.comp(path).unwrap_or_else(|| panic!("the kick has {path}"));
        CompRef { board: unit.id, comp: c.id }
    };
    let (decay, tune) = (comp("drum.decay"), comp("drum.tune"));
    assert_eq!(lowered.index.knobs(), [(tune, 0.65), (decay, 0.73)],
        "TUNE and DECAY are the bench's knobs, each starting where the design puts it");

    // the formulas, with each pot wired as a variable resistor that a clockwise turn turns up:
    // DECAY's A-W half, TUNE's W-B half
    let (c, r1, tp, v) = (100e-9, 22e3, 1e-3, 3.4);
    let aw = |ohms: f64, pos: f64| Taper::Linear.halves(ohms, pos, libs.pot_end).0;
    let r3 = 330.0 + Taper::Linear.halves(1e3, 0.65, libs.pot_end).1;
    let expect = |r2: f64| {
        let rp = r1 * r3 / (r1 + r3);
        (1.0 / (2.0 * PI * c * (r2 * rp).sqrt()), r2 * c)
    };

    let spec = LiveSpec { rate: f64::INFINITY, reltol: Some(Options::default().reltol), trace: false, sample_rate: AUDIO_SAMPLE_RATE, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .unwrap_or_else(|e| panic!("the kick's operating point: {}", e.message));
    let mut scope = Scope::new(&["OUT"]);
    scope.begin(&sim.solver().unknowns);

    // strike 1 at 10 ms, DECAY at its start (r2 = 1.2 Mohm)
    run_to(&mut sim, &mut scope, 0.45);
    let (f0, tau, peak) = ring(&scope.trace("OUT"), 0.01, 0.45);
    let (want_f0, want_tau) = expect(470e3 + aw(1e6, 0.73));
    let want_peak = v * tp / (r1 * c);
    assert!((want_f0 - 56.0).abs() < 1.0 && (want_tau - 0.12).abs() < 0.002, "the design's kick: {want_f0:.1} Hz, {want_tau:.3} s");
    assert!((f0 / want_f0 - 1.0).abs() < 0.03, "pitch {f0:.2} Hz is the formula's {want_f0:.2} Hz within 3 %");
    assert!((tau / want_tau - 1.0).abs() < 0.10, "decay {:.1} ms is the formula's {:.1} ms within 10 %", tau * 1e3, want_tau * 1e3);
    assert!((peak / want_peak - 1.0).abs() < 0.10, "peak {peak:.3} V is v tp / (r1 c) = {want_peak:.3} V within 10 %");

    // DECAY turned down to 0.2 between the strikes: both halves of the pot follow at once
    let cmds = lowered.index.command(&SimCmd::SetParam { comp: decay, value: 0.2 });
    assert_eq!(cmds.len(), 2, "a pot's turn sets its two halves");
    for cmd in cmds {
        sim.apply(cmd);
    }
    assert_eq!(sim.stats().unknown, 0, "the kernel knows both halves");
    run_to(&mut sim, &mut scope, 0.9);
    let (f0, tau, _) = ring(&scope.trace("OUT"), 0.51, 0.9);
    let (want_f0, want_tau) = expect(470e3 + aw(1e6, 0.2));
    assert!((f0 / want_f0 - 1.0).abs() < 0.03, "turned: pitch {f0:.2} Hz is the formula's {want_f0:.2} Hz within 3 %");
    assert!((tau / want_tau - 1.0).abs() < 0.10, "turned: decay {:.1} ms is the formula's {:.1} ms within 10 %", tau * 1e3, want_tau * 1e3);
}

/// A voice's bench lowered without diagnostics, and its live run from the operating point. The
/// run steps at least as often as the circuit's own noise is sampled.
fn voice(ev: &Evaluated, name: &str) -> (fab_elec_bench::Lowered, LiveSim) {
    let bench = ev.benches.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("drum16 has the {name} bench"));
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "the {name} lowers without diagnostics: {:?}", lowered.diags);
    let defaults = LiveSpec::default();
    let spec = LiveSpec {
        rate: f64::INFINITY,
        reltol: Some(Options::default().reltol),
        trace: false,
        sample_rate: AUDIO_SAMPLE_RATE,
        ..defaults
    };
    let sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .unwrap_or_else(|e| panic!("the {name}'s operating point: {}", e.message));
    (lowered, sim)
}

/// The least-squares time constant of an exponential fall: the slope of ln(v) over (t, v) points.
fn fall(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    let (st, sl) = points.iter().fold((0.0, 0.0), |(st, sl), p| (st + p.0, sl + p.1.ln()));
    let (mt, ml) = (st / n, sl / n);
    let (cov, var) = points.iter().fold((0.0, 0.0), |(c, v), p| (c + (p.0 - mt) * (p.1.ln() - ml), v + (p.0 - mt).powi(2)));
    -var / cov
}

/// A trace's RMS in a band (Hz) over the 20 ms from `a`: resampled at 96 kHz about its mean, so a
/// voice's level in the band is told from its rest level and from what other bands carry.
fn band(points: &[(f64, f64)], a: f64, lo: f64, hi: f64) -> f64 {
    let (t, v): (Vec<f64>, Vec<f64>) = points.iter().copied().unzip();
    let rate = 96e3;
    let mut s = resample_hermite(&t, &v, None, a, 1.0 / rate, (0.02 * rate) as usize);
    let mean = s.iter().sum::<f64>() / s.len() as f64;
    s.iter_mut().for_each(|x| *x -= mean);
    band_rms(&s, 1.0 / rate, Window::Hann, lo, hi)
}

/// The snare: its noise source (a 1 Mohm resistor's thermal noise, three stages of 31) carries
/// the level the textbook gives: the resistor's density times the gain over the three poles'
/// noise bandwidth. The trigger's envelope then falls between its two bounds: with the chopper
/// always on, c_env (rc || r_dis); never on, c_env r_dis.
#[test]
fn snare_noise_is_its_formula_level_and_its_envelope_falls_within_bounds() {
    let ev = drum16();
    let (lowered, mut sim) = voice(&ev, "snare");
    assert_eq!(sim.step_cap(), 1.0 / AUDIO_SAMPLE_RATE, "the run steps at its sample rate, and the noise resistor draws that often");
    let mut scope = Scope::new(&["NOISE", "ENV"]);
    scope.begin(&sim.solver().unknowns);
    run_to(&mut sim, &mut scope, 0.2);

    // 0.53 V rms: 129 nV per root hertz, a gain of 31^3, three poles at 1 MHz / 31 (their noise
    // bandwidth is 3 pi / 16 of one pole's frequency)
    let libs = ModelLibs::generic();
    let kelvin = lowered.circuit.temp_c + 273.15;
    let density = (4.0 * 1.380649e-23 * kelvin * 1e6_f64).sqrt();
    let want = density * 31f64.powi(3) * (3.0 * PI / 16.0 * libs.opamp.gbw / 31.0).sqrt();
    assert!((want - 0.53).abs() < 0.01, "the textbook level: {want:.3} V rms");
    // the level, time-weighted over the run's accepted points (they are not evenly spaced)
    let noise: Vec<(f64, f64)> = scope.trace("NOISE").into_iter().filter(|p| p.0 > 0.01).collect();
    let span = noise[noise.len() - 1].0 - noise[0].0;
    let mean = noise.windows(2).map(|w| (w[1].0 - w[0].0) * (w[0].1 + w[1].1) / 2.0).sum::<f64>() / span;
    let rms = (noise.windows(2).map(|w| (w[1].0 - w[0].0) * ((w[0].1 - mean).powi(2) + (w[1].1 - mean).powi(2)) / 2.0).sum::<f64>() / span).sqrt();
    assert!((mean - 2.5).abs() < 0.05, "the noise sits on the signal ground: mean {mean:.3} V");
    assert!((rms / want - 1.0).abs() < 0.2, "noise {rms:.3} V rms is the formula's {want:.3} V within 20 %");

    // the envelope from 20 ms after the strike, down to a tenth of its height
    let env = scope.trace("ENV");
    let top = env.iter().filter(|p| p.0 > 0.01 && p.0 < 0.015).map(|p| p.1).fold(0.0, f64::max);
    assert!(top > 2.0, "a 3.4 V trigger charges the envelope through the follower: {top:.2} V");
    let tail: Vec<(f64, f64)> = env.into_iter().filter(|p| p.0 > 0.031 && p.1 > top / 10.0).collect();
    let tau = fall(&tail);
    let (c_env, rc, r_dis) = (470e-9, 100e3, 150e3);
    let (lo, hi) = (c_env * rc * r_dis / (rc + r_dis), c_env * r_dis);
    assert!(tau > lo && tau < hi, "the envelope falls in {:.1} ms, between {:.1} and {:.1} ms", tau * 1e3, lo * 1e3, hi * 1e3);
}

/// The metal voices. Each of the metal bank's six Schmitt oscillators runs at the textbook pitch of
/// its r and c with the inverter's thresholds, and the closed hat chokes the open hat: its
/// envelope, up just before the second closed hat at 260 ms, is gone just after. The cymbal's
/// envelope falls between its two bounds: with its chopper always on, c_env (rc || r_dis); never
/// on, c_env r_dis. And each voice is heard at the output in its band, against the quiet before
/// it: both hats above 5 kHz, the cymbal in its band-pass's 3 to 6 kHz, which holds more than the
/// bands below and above it.
#[test]
fn metal_voices_ring_at_their_formula_pitch_choke_fall_and_are_heard_in_their_bands() {
    let ev = drum16();
    let (_, mut sim) = voice(&ev, "hats");
    let ys = ["y[0]", "y[1]", "y[2]", "y[3]", "y[4]", "y[5]"];
    let mut nets = ys.to_vec();
    nets.extend(["OENV", "OUT", "yenv"]);
    let mut scope = Scope::new(&nets);
    scope.begin(&sim.solver().unknowns);
    run_to(&mut sim, &mut scope, 0.48);
    let card = ModelLibs::generic().schmitt;
    let (vp, vm, vdd, c) = (card.vt_plus, card.vt_minus, 5.0, 10e-9);
    let periods = ((vp * (vdd - vm)) / (vm * (vdd - vp))).ln();
    for (y, r) in ys.iter().zip([330e3, 240e3, 200e3, 160e3, 120e3, 91e3]) {
        let want = 1.0 / (r * c * periods);
        // rising crossings of mid-supply after the first cycles
        let rises: Vec<f64> = scope.trace(y).windows(2).filter(|w| w[0].0 > 0.02 && w[0].1 < vdd / 2.0 && w[1].1 >= vdd / 2.0)
            .map(|w| w[0].0 + (w[1].0 - w[0].0) * (vdd / 2.0 - w[0].1) / (w[1].1 - w[0].1)).collect();
        assert!(rises.len() > 10, "{y} oscillates: {} rising edges", rises.len());
        let f = (rises.len() - 1) as f64 / (rises[rises.len() - 1] - rises[0]);
        assert!((f / want - 1.0).abs() < 0.01, "{y} runs at {f:.1} Hz, the formula's {want:.1} Hz within 1 %");
    }

    let oenv = scope.trace("OENV");
    let at = |t: f64| oenv.iter().take_while(|p| p.0 <= t).last().expect("a point before").1;
    assert!(at(0.255) > 1.0, "the open hat still rings before the choke: {:.2} V", at(0.255));
    assert!(at(0.265) < 0.1, "the closed hat at 260 ms chokes it: {:.3} V", at(0.265));

    // the cymbal, struck at 385 ms
    let yenv = scope.trace("yenv");
    let top = yenv.iter().filter(|p| p.0 > 0.385 && p.0 < 0.39).map(|p| p.1).fold(0.0, f64::max);
    assert!(top > 2.0, "a 3.4 V trigger charges the cymbal's envelope through the follower: {top:.2} V");
    let tau = fall(&yenv.into_iter().filter(|p| p.0 > 0.39).collect::<Vec<_>>());
    let (c_env, rc, r_dis) = (4.7e-6, 100e3, 470e3);
    let (lo, hi) = (c_env * rc * r_dis / (rc + r_dis), c_env * r_dis);
    assert!(tau > lo && tau < hi, "the cymbal's envelope falls in {tau:.3} s, between {lo:.3} and {hi:.3} s");

    let out = scope.trace("OUT");
    let band = |a: f64, lo: f64, hi: f64| band(&out, a, lo, hi);
    let (quiet, quiet_late) = (band(0.11, 5e3, 20e3), band(0.355, 3e3, 6e3));
    for (name, a) in [("closed hat", 0.011), ("open hat", 0.136)] {
        let heard = band(a, 5e3, 20e3);
        assert!(heard > 10.0 * quiet, "the {name} is heard above 5 kHz: {heard:.4} V rms against {quiet:.5} V before it");
    }
    let (below, cymbal, above) = (band(0.386, 200.0, 3e3), band(0.386, 3e3, 6e3), band(0.386, 7e3, 20e3));
    assert!(cymbal > 10.0 * quiet_late, "the cymbal is heard at 3 to 6 kHz: {cymbal:.4} V rms against {quiet_late:.5} V before it");
    assert!(cymbal > below && cymbal > above,
        "its band-pass holds the most at 3 to 6 kHz: {below:.4} V rms below, {cymbal:.4} V, {above:.4} V above");
}

/// Both toms ring at their parts' formula pitch, decay and peak. Turning TOM TUNE live changes
/// the low tom's next strike to the pitch of its new r3, with the same decay and level.
#[test]
fn toms_ring_at_their_formula_pitch_decay_and_peak_and_tom_tune_turns_live() {
    let ev = drum16();
    let (lowered, mut sim) = voice(&ev, "toms");
    let bench = ev.benches.iter().find(|b| b.name == "toms").expect("the toms have a bench");
    let unit = ev.design.unit(bench.scope_id()).expect("the toms bench runs the toms unit");
    let tune = unit.circuit.comp("lt.tune").expect("the low tom has TOM TUNE");
    let comp = CompRef { board: unit.id, comp: tune.id };
    let start = 0.581;
    assert_eq!(lowered.index.knobs(), [(comp, start)],
        "TOM TUNE starts where the design puts it");

    // Each pitch uses r1 || r3; the low tom's r3 includes the pot's W-B half. A pulse of v
    // volts for tp rings at the pulse's area through the filter, v tp / (r1 c), times its sinc at
    // the pitch, sin(x) / x with x = pi f0 tp, centred on the pulse's middle and falling with
    // tau = r2 c. Its largest swing comes at the pulse's end, cos(x) of the ring, or half a
    // period later, whichever the decay leaves larger.
    let libs = ModelLibs::generic();
    let r3 = |pos: f64| 470.0 + Taper::Linear.halves(500.0, pos, libs.pot_end).1;
    let expect = |c: f64, r1: f64, r3: f64| {
        let (r2, tp, v) = (1.5e6, 1e-3, 3.4);
        let rp = r1 * r3 / (r1 + r3);
        let (f0, tau) = (1.0 / (2.0 * PI * c * (r2 * rp).sqrt()), r2 * c);
        let x = PI * f0 * tp;
        let ring = v * tp / (r1 * c) * x.sin() / x;
        let at_end = x.cos() * (-tp / 2.0 / tau).exp();
        let half_period_on = (-(tp / 2.0 + 0.5 / f0) / tau).exp();
        (f0, tau, ring * at_end.max(half_period_on))
    };
    let check = |name: &str, got: (f64, f64, f64), want: (f64, f64, f64)| {
        let (f0, tau, peak) = got;
        let (want_f0, want_tau, want_peak) = want;
        assert!((f0 / want_f0 - 1.0).abs() < 0.03,
            "{name}: pitch {f0:.2} Hz is the formula's {want_f0:.2} Hz within 3 %");
        assert!((tau / want_tau - 1.0).abs() < 0.03,
            "{name}: decay {:.1} ms is the formula's {:.1} ms within 3 %", tau * 1e3, want_tau * 1e3);
        assert!((peak / want_peak - 1.0).abs() < 0.03,
            "{name}: peak {peak:.3} V is the pulse's {want_peak:.3} V within 3 %");
    };

    let mut scope = Scope::new(&["LOW", "HIGH"]);
    scope.begin(&sim.solver().unknowns);

    // The low tom strikes at 10 ms, the high tom at 260 ms and the low tom again at 510 ms.
    run_to(&mut sim, &mut scope, 0.25);
    let low = ring(&scope.trace("LOW"), 0.01, 0.25);
    check("low tom", low, expect(47e-9, 47e3, r3(start)));

    // Turn as the first ring has died away. A new r3 moves the filter's input bias, which rings
    // the filter by itself as a real board would: a quarter second, over three decay times,
    // leaves under 1 % of that ring under the next strike.
    let cmds = lowered.index.command(&SimCmd::SetParam { comp, value: 0.8 });
    assert_eq!(cmds.len(), 2, "TOM TUNE sets both halves of the pot");
    for cmd in cmds {
        sim.apply(cmd);
    }
    assert_eq!(sim.stats().unknown, 0, "the kernel knows both halves");
    run_to(&mut sim, &mut scope, 0.74);
    check("high tom", ring(&scope.trace("HIGH"), 0.26, 0.49), expect(33e-9, 68e3, 560.0));
    let turned = ring(&scope.trace("LOW"), 0.51, 0.74);
    check("low tom turned", turned, expect(47e-9, 47e3, r3(0.8)));
    assert!(turned.0 > low.0, "turning TOM TUNE clockwise lowers r3 and raises the low tom's pitch: {:.2} Hz to {:.2} Hz", low.0, turned.0);
}

/// The whole board running its sequencer firmware, the shift registers on the digital side beside
/// the MCU. At 240 BPM (a step every 62.5 ms), over the first four steps:
/// - the chain's latch, data and clock nets are digital: they lower nothing;
/// - each voice's trigger is 1 ms wide and on its own grid within 200 us; an accented kick strikes
///   at about 5 V, an unaccented one at about 3.6 V;
/// - the step LEDs show the kick's track through the registers, the running step inverted, and
///   the PLAY LED is lit;
/// - a step key held 150 ms pulls its row low through its diode, toggles that step of the kick,
///   and the kick strikes on that step in the same bar; its LED shows it from the next step on;
/// - the mix reaches the line out, and the closed hat on step 3 stands out in it above 5 kHz.
/// It prints the board's size and the solver's speed, the MCU included.
#[test]
fn the_whole_board_plays_its_pattern_and_a_step_key_edits_it() {
    // the board's bench with TEMPO turned to its top: the divider, the ADC's sample and the
    // firmware's reading of it play 240 BPM
    let mut sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    sources.add("top.bench.splash", r#"
bench("tempo at the top", {circuit: "drum16"}, fn(b, c) {
    b.firmware(c.mcu.u, "drum16.fw.splash", {autoplay: 1})
    b.turn(c.tempo, 1)
})
"#).expect("the bench document is new");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("drum16 evaluates: {diags:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "tempo at the top").expect("the bench with TEMPO at its top");
    assert_eq!(bench.state, UnitState::Built, "the bench builds: {:?}", bench.state.diags());
    assert_eq!(bench.spec.turned, [("tempo".to_string(), 1.0)], "b.turn sets the pot's wiper for the run");
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "the board lowers without diagnostics: {:?}", lowered.diags);
    let fw = &lowered.firmware[0];
    let digital: Vec<&str> = fw.logic.nets.iter().map(|n| n.name.as_str()).collect();
    for net in ["LATCH", "DATA", "CLK"] {
        assert!(digital.contains(&net), "{net} is on the digital side: {digital:?}");
    }
    assert_eq!(fw.logic.parts.len(), 3, "three shift registers run beside the MCU");

    // the bench's firmware from 240 BPM (TEMPO keeps it there), on the MCU with its registers
    let params: Vec<(&str, f64)> = fw.params.iter().map(|(n, v)| (n.as_str(), *v)).chain([("bpm", 240.0)]).collect();
    let f_clk = fw.f_clk.expect("the crystal clocks the MCU");
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let source = sources.doc(sources.find("drum16.fw.splash").expect("drum16 has its firmware")).expect("its document").text.clone();
    let image = fab_elec_mcu_emit_avr::firmware(&source, f_clk, &params)
        .unwrap_or_else(|d| panic!("the firmware compiles: {d:?}"));
    let pins = lowered.cosim_pins(&fw.part);
    let machine = LogicMachine::new(AvrMachine::new(&image.words, u64::from(image.f_clk)), fw.logic.plan())
        .expect("the registers join the MCU");
    let device = CoMachine::new(Lockstep::new(machine), Config { stretch: 4000, debug: None }, &pins).expect("the co-simulated MCU");
    let defaults = LiveSpec::default();
    let spec = LiveSpec {
        rate: f64::INFINITY,
        reltol: Some(Options::default().reltol),
        trace: false,
        sample_rate: AUDIO_SAMPLE_RATE,
        ..defaults
    };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .unwrap_or_else(|e| panic!("the board's operating point: {}", e.message));
    sim.apply(LiveCmd::CoSim(CoSimSlot::new(Some(CoSimDevice { dev: Box::new(device), pins }))));

    let voices = ["KICK", "SNARE", "LTOM", "HTOM", "CHAT", "OHAT", "CYM"];
    let leds = ["q[0]", "q[1]", "q[2]", "q[3]", "q[4]", "q[16]"];
    let mut nets = voices.to_vec();
    nets.extend(["ROW0", "KICKOUT", "VREF", "LINE"]);
    nets.extend(leds);
    let mut scope = Scope::new(&nets);
    scope.begin(&sim.solver().unknowns);
    let wall = Instant::now();

    // step k strikes at 62.5 (k + 1) ms; key 2 (row 0, column 2) is step 3, at 187.5 ms: two walks
    // 10 ms apart accept it long before then
    let press = "keys.keys[0][2].sw#press";
    assert!(lowered.circuit.devices.v.iter().any(|v| v.name == press), "the step key has its press source");
    let key = |sim: &mut LiveSim, down: bool| sim.apply(LiveCmd::SetParam {
        generation: lowered.index.generation, device: press.into(), key: "dc".into(), value: if down { 1.0 } else { 0.0 },
    });
    run_to(&mut sim, &mut scope, 0.07);
    key(&mut sim, true);
    run_to(&mut sim, &mut scope, 0.18);
    key(&mut sim, false);
    run_to(&mut sim, &mut scope, 0.265);
    let seconds = wall.elapsed().as_secs_f64();

    let at = |net: &str, t: f64| scope.trace(net).into_iter().find(|p| p.0 >= t).expect("a point after t").1;
    let window = |net: &str, a: f64, b: f64| scope.trace(net).into_iter().filter(move |p| p.0 >= a && p.0 < b);

    // the triggers: rises and falls through 1.5 V
    let step = 0.0625;
    let mut first_kick = None;
    for name in voices {
        // until the firmware drives it, a trigger pin is an input and its node sits at the voice
        // input's bias: count from the first time it is low
        let trace: Vec<(f64, f64)> = scope.trace(name).into_iter().skip_while(|p| p.1 >= 1.5).collect();
        let (mut rises, mut widths) = (Vec::new(), Vec::new());
        for w in trace.windows(2) {
            if w[0].1 < 1.5 && w[1].1 >= 1.5 { rises.push(w[1].0) }
            if w[0].1 >= 1.5 && w[1].1 < 1.5 { widths.push(w[1].0 - rises.last().expect("a fall after a rise")) }
        }
        assert_eq!(rises.len(), widths.len(), "{name}: every strike ends");
        for &w in &widths {
            assert!((w - 1e-3).abs() <= 50e-6, "{name}: a trigger is 1 ms wide: {:.1} us", w * 1e6);
        }
        // the toms and the open hat strike later in the bar
        if rises.is_empty() { continue }
        let t0 = *first_kick.get_or_insert(rises[0]);
        let offsets: Vec<f64> = rises.iter().map(|r| { let n = ((r - t0) / step).round(); r - t0 - n * step }).collect();
        if let (Some(lo), Some(hi)) = (offsets.iter().copied().reduce(f64::min), offsets.iter().copied().reduce(f64::max)) {
            assert!(hi - lo <= 200e-6, "{name}: its strikes are on its grid within 200 us: {offsets:?}");
            println!("board: {name} strikes at {:?} ms, {:.0} us after the kick's slot", rises.iter().map(|r| (r * 1e4).round() / 10.0).collect::<Vec<_>>(), lo * 1e6);
        }
    }
    let peak = |net: &str, a: f64, b: f64| window(net, a, b).map(|p| p.1).fold(f64::MIN, f64::max);
    let kick_accented = peak("KICK", 0.062, 0.065);
    let kick_plain = peak("KICK", 0.1875, 0.1905);
    assert!(kick_accented > 4.5, "an accented kick strikes at about 5 V: {kick_accented:.2} V");
    assert!((3.0..4.0).contains(&kick_plain), "an unaccented kick strikes at about 3.6 V: {kick_plain:.2} V");

    // the LEDs during step 1 (lit = 0), before the edit shows: the kick's track 1 5 9 13 with
    // step 1 inverted off, PLAY on
    let high = |net: &str, t: f64| at(net, t) > 2.5;
    let lit = |t: f64| -> Vec<bool> { leds.iter().map(|q| high(q, t)).collect() };
    assert_eq!(lit(0.1), [false, false, false, false, true, true], "LEDs 1 2 3 4 5 and PLAY during step 1: {leds:?}");
    // two walks 10 ms apart accept the key during step 1 (at about 90 ms), and the edit shows from
    // the next step's tail: during step 2 (lit = 1) step 2 is inverted on and step 3 is on
    assert_eq!(lit(0.18), [true, true, true, false, true, true], "LEDs 1 2 3 4 5 and PLAY during step 2, after the edit: {leds:?}");

    // the key: its row low through its diode while pressed (every column rests low), high before
    let (rest, pressed) = (at("ROW0", 0.065), window("ROW0", 0.08, 0.18).map(|p| p.1).fold(f64::MAX, f64::min));
    assert!(rest > 4.5 && pressed < 1.0, "the row rests high and a pressed key pulls it low: {rest:.2} V, {pressed:.2} V");
    // the kick now strikes on step 3 (187.5 ms): the first strike's ring (a 120 ms decay) only
    // falls from one 50 ms window to the next, so a swing that grows is the new strike
    let swing = |a: f64, b: f64| window("KICKOUT", a, b).map(|p| (p.1 - at("VREF", p.0)).abs()).fold(0.0, f64::max);
    let (before, after) = (swing(0.1375, 0.1875), swing(0.1875, 0.2375));
    assert!(after > 1.2 * before, "the edited step strikes the kick: swing {before:.3} V before, {after:.3} V after");
    // and during step 4 (lit = 3) its LED shows the edit: LED 3 on beside the running LED 4
    assert!(high("q[2]", 0.264) && high("q[3]", 0.264), "the edited step's LED is lit after the edit");

    let line = window("LINE", 0.06, 0.265).map(|p| p.1).fold(0.0, |m: f64, v| m.max(v.abs()));
    assert!(line > 0.05, "the mix reaches the line out: {line:.3} V peak");
    // the closed hat on step 3 is heard at the line out above 5 kHz, though the cymbal struck on
    // step 1 still rings: the band's level in the 20 ms after its strike against the 20 ms before
    let hat = first_kick.expect("the kick strikes") + 2.0 * step;
    let line = scope.trace("LINE");
    let (before, after) = (band(&line, hat - 0.022, 5e3, 20e3), band(&line, hat + 0.001, 5e3, 20e3));
    assert!(after > 2.0 * before, "the closed hat on step 3 is heard above 5 kHz: {after:.4} V rms against {before:.4} V before it");
    println!("board: {} unknowns, {} accepted points, 0.265 s simulated in {seconds:.2} s ({:.3}x real time)",
        sim.solver().unknowns.len(), scope.t.len(), 0.265 / seconds);
}

/// The whole board placed and routed (Route all's copper): every part of the circuit on it, inside
/// the outline, and its DRC finds nothing at all, an unrouted net included (no courtyard overlap,
/// edge, hole, silk or clearance finding). The board document is the canonical writer's fixed point.
#[test]
fn the_whole_board_is_placed_and_routed_with_no_finding() {
    let ev = drum16();
    let unit = ev.design.units.iter().find(|u| u.name == "drum16").expect("the board unit");
    let board = unit.board.as_ref().expect("drum16 has its board");
    assert_eq!(board.placed.len(), unit.circuit.comps.len(), "every part of the circuit is placed");
    let results = fab_elec_design::engines::full_with(&ev.design, &fab_elec_board::Factories::new());
    for name in ["VA", "GND"] {
        let net = unit.circuit.net(name).expect("the drum machine has this supply rail");
        let item = Sel::Net(NetRef { board: unit.id, net: net.id });
        assert!(!results.erc.iter().any(|v| v.kind == CheckKind::PowerNotDriven && v.items.contains(&item)),
            "ERC recognizes the USB supply on {name}");
    }
    let block_findings: Vec<_> = results.erc.iter().filter(|v| {
        matches!(v.kind, CheckKind::VoltageDomain | CheckKind::RailBudget | CheckKind::OpenPort)
            || matches!(v.kind, CheckKind::PinConflict | CheckKind::PowerNotDriven)
                && v.items.iter().any(|s| matches!(s, Sel::Block(..)))
    }).collect();
    assert!(block_findings.is_empty(), "drum16 units must be free of block-check findings: {block_findings:#?}");
    let drc = &results.boards.iter().find(|b| b.unit == unit.id).expect("the board's results").drc;
    assert!(drc.is_empty(), "the routed board has no finding at all, an unrouted net included: {drc:#?}");
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let text = sources.doc(sources.find("drum16.pcb.splash").expect("drum16 has its board document")).expect("its document").text.clone();
    assert_eq!(fab_elec_lang::write(board, &unit.circuit), *text, "the board document is the canonical writer's fixed point");
}

