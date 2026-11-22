//! A value edit is heard: a resistor changed in a running simulation changes the circuit's
//! response from the moment it is applied, as the physics says, in Exact and in Fast. Whatever
//! the solver keeps between solves (a resting block's evaluation, a skipped column, a bypassed
//! device, the single pass's accepted evaluations) must give way to the new value, a device's own
//! value (a diode's saturation current, a transistor's beta) as much as a resistor's.

use fab_elec_sim::analysis::op::operating_point;
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec, AUDIO_SAMPLE_RATE};
use fab_elec_sim::{ModelKind, Record, SimCircuit, Sink, Solver, Source, Wave};

const MODES: [(&str, f64); 2] = [("Exact", 0.0), ("Fast", 1e-4)];

/// The runs step at an audio rate.
fn period() -> f64 {
    1.0 / AUDIO_SAMPLE_RATE
}

fn start(circuit: SimCircuit, res: f64) -> (LiveSim, Record) {
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, res, trace: false, sample_rate: AUDIO_SAMPLE_RATE, ..LiveSpec::default() };
    let sim = LiveSim::new(circuit, ValuePlan::default(), 1, spec).expect("the operating point converges");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    (sim, record)
}

fn run_to(sim: &mut LiveSim, record: &mut Record, stop: f64) {
    while sim.t() < stop {
        sim.advance(stop, None, record).unwrap_or_else(|d| panic!("the circuit steps on: {d}"));
    }
}

fn set_r(sim: &mut LiveSim, device: &str, value: f64) {
    set(sim, device, "r", value);
}

fn set(sim: &mut LiveSim, device: &str, key: &str, value: f64) {
    assert!(sim.apply(LiveCmd::SetParam { generation: sim.generation(), device: device.into(), key: key.into(), value }), "the edit keeps the run");
}

/// The (time, value) points of `net` from `t0` to `t1`.
fn points(record: &Record, net: &str, t0: f64, t1: f64) -> Vec<(f64, f64)> {
    record.at.iter().copied().zip(record.v(net)).filter(|&(t, _)| t > t0 && t <= t1).collect()
}

#[test]
fn a_resistor_edit_retunes_an_rc() {
    let period = period();
    for (mode, res) in MODES {
        // A divider loaded by a capacitor: out settles to the divider's ratio with the time
        // constant of the two resistors in parallel times the capacitor.
        let mut c = SimCircuit::new();
        c.v("v1", "in", "0", Source::dc(1.0));
        c.r("r1", "in", "out", 10e3);
        c.r("r2", "out", "0", 10e3);
        c.c("c1", "out", "0", 100e-9);
        let (mut sim, mut record) = start(c, res);
        run_to(&mut sim, &mut record, 5e-3);
        let (edit, before) = (sim.t(), record.last("out"));
        assert!((before - 0.5).abs() < 1e-3, "{mode}: the divider rests at half the input: {before} V");
        set_r(&mut sim, "r2", 30e3);
        run_to(&mut sim, &mut record, edit + 6e-3);
        let (settled, tau) = (0.75, 7.5e3 * 100e-9);
        let after = points(&record, "out", edit, edit + 6e-3);
        assert!(!after.is_empty(), "{mode}: the run goes on after the edit");
        let first = settled + (before - settled) * (-(after[0].0 - edit).max(period) / tau).exp();
        assert!((after[0].1 - first).abs() < 1e-3, "{mode}: the first point after the edit is {:.5} V, a sample on the new time constant {first:.5} V", after[0].1);
        // Fast draws a value edit as it draws an edge: the first step's end at the edit's instant
        for &(t, v) in after.iter().filter(|p| p.0 > edit + 0.5 * period) {
            let want = settled + (before - settled) * (-(t - edit) / tau).exp();
            assert!((v - want).abs() < 1e-3, "{mode}: {:.1} us after the edit out is {v:.5} V, the new time constant's {want:.5} V", (t - edit) * 1e6);
        }

        // A low-pass on a 1 kHz sine: its amplitude is 1 / sqrt(1 + (w r c)^2).
        let mut c = SimCircuit::new();
        let f = 1e3;
        let cap = 1.0 / (2.0 * std::f64::consts::PI * f * 1e3);
        c.v("v1", "in", "0", Source { wave: Wave::Sin { vo: 0.0, va: 1.0, freq: f, td: 0.0, theta: 0.0, phase: 0.0 }, ..Source::dc(0.0) });
        c.r("r1", "in", "out", 1e3);
        c.c("c1", "out", "0", cap);
        let (mut sim, mut record) = start(c, res);
        let peak = |record: &Record, t0: f64, t1: f64| points(record, "out", t0, t1).iter().map(|p| p.1.abs()).fold(0.0, f64::max);
        run_to(&mut sim, &mut record, 10e-3);
        let edit = sim.t();
        let want = 1.0 / 2f64.sqrt();
        let got = peak(&record, edit - 2e-3, edit);
        assert!((got / want - 1.0).abs() < 0.01, "{mode}: at w r c = 1 the amplitude is {got:.4}, the filter's {want:.4}");
        set_r(&mut sim, "r1", 3e3);
        run_to(&mut sim, &mut record, edit + 10e-3);
        let want = 1.0 / 10f64.sqrt();
        let got = peak(&record, edit + 8e-3, edit + 10e-3);
        assert!((got / want - 1.0).abs() < 0.01, "{mode}: at w r c = 3 the amplitude is {got:.4}, the filter's {want:.4}");
    }
}

/// Two common-emitter stages on one rail, one driven by a sine and one at rest. A stage's
/// collector current is set by its base and emitter, so the drop across its collector resistor
/// scales with that resistor. Cut at the rail, the resting stage's block keeps its evaluation
/// between loads; an edit inside it must still be heard.
#[test]
fn a_resistor_edit_moves_a_transistor_stage() {
    let board = |border: &[&str]| {
        let mut c = SimCircuit::new();
        c.model("npn", ModelKind::Npn, &[("is", 1e-15), ("bf", 200.0)]);
        c.v("vrail", "VA", "0", Source::dc(9.0));
        for v in ["k", "s"] {
            let n = |what: &str| format!("{v}.{what}");
            let input = if v == "k" {
                Source { wave: Wave::Sin { vo: 2.0, va: 0.2, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }, ..Source::dc(2.0) }
            } else {
                Source::dc(2.0)
            };
            c.v(&format!("vin.{v}"), &n("in"), "0", input);
            c.r(&format!("rb.{v}"), &n("in"), &n("b"), 10e3);
            c.q4(&format!("q.{v}"), &n("c"), &n("b"), &n("e"), "0", "npn");
            c.r(&format!("rc.{v}"), "VA", &n("c"), 4.7e3);
            c.r(&format!("re.{v}"), &n("e"), "0", 1e3);
            c.c(&format!("cc.{v}"), &n("c"), "0", 10e-9);
        }
        c.opts.border = border.iter().map(|s| s.to_string()).collect();
        c
    };
    for (mode, res) in MODES {
        for (how, border) in [("whole", &[][..]), ("cut at the rail", &["VA"][..])] {
            let (mut sim, mut record) = start(board(border), res);
            assert_eq!(sim.solver().blocks.is_some(), !border.is_empty(), "{mode}, {how}: loaded as asked");
            run_to(&mut sim, &mut record, 2e-3);
            let (edit, before) = (sim.t(), record.last("s.c"));
            assert!(before > 1.5 && before < 6.0, "{mode}, {how}: the resting stage is biased mid-rail: {before} V");
            set_r(&mut sim, "rc.s", 2.2e3);
            run_to(&mut sim, &mut record, edit + 2e-3);
            // the collector node's time constant is now 22 us: two samples on it has moved most
            // of the way
            let soon = points(&record, "s.c", edit + 2.0 * period(), edit + 2e-3)[0];
            assert!(soon.1 - before > 1.0, "{mode}, {how}: {:.1} us after the edit the collector has moved from {before:.3} V to {:.3} V",
                (soon.0 - edit) * 1e6, soon.1);
            let after = record.last("s.c");
            let (ratio, want) = ((9.0 - after) / (9.0 - before), 2.2 / 4.7);
            assert!((ratio / want - 1.0).abs() < 0.01, "{mode}, {how}: the collector drop went from {:.4} V to {:.4} V, a ratio of {ratio:.4} against the resistors' {want:.4}",
                9.0 - before, 9.0 - after);
        }
    }
}

/// A resistor's thermal noise is part of its value: across a bare resistor the noise voltage's
/// variance at a sample is 2 k T R / period, so a resistor four times larger is four times as
/// noisy in variance (not sixteen, as its old noise current through the new resistance would be).
#[test]
fn a_resistor_edit_retunes_its_thermal_noise() {
    let mut c = SimCircuit::new();
    c.r("load", "out", "0", 1e6);
    assert!(c.thermal_noise("load", 1.0 / 192_000.0, 7), "the load has thermal current noise");
    let kt2 = 2.0 * 1.380649e-23 * (c.temp_c + 273.15);
    let (mut sim, mut record) = start(c, 0.0);
    let period = period();
    let count = 4096.0;
    // the variance of out at the sample knots between t0 and t1
    let variance = |record: &Record, t0: f64, t1: f64| {
        let v: Vec<f64> = points(record, "out", t0, t1).into_iter().filter(|p| (p.0 / period - (p.0 / period).round()).abs() < 1e-6).map(|p| p.1).collect();
        assert!(v.len() as f64 >= 0.9 * count, "enough samples on the knots: {}", v.len());
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64
    };
    run_to(&mut sim, &mut record, count * period);
    let edit = sim.t();
    let before = variance(&record, 0.0, edit);
    let want = kt2 * 1e6 / period;
    assert!((before / want - 1.0).abs() < 0.125, "1 Mohm: noise variance {before:e} against 2 k T R / period {want:e}");
    set_r(&mut sim, "load", 4e6);
    run_to(&mut sim, &mut record, edit + count * period);
    let after = variance(&record, edit, edit + count * period);
    let want = kt2 * 4e6 / period;
    assert!((after / want - 1.0).abs() < 0.125, "4 Mohm: noise variance {after:e} against 2 k T R / period {want:e}");
}

/// Fast keeps a resting device's evaluation (the bypass) and accepts a solve on it (the single
/// pass): a value edit to the device itself, or to the resistor feeding it, must still be heard
/// within two samples, most of the way to where a fresh solver built with the new value sits, and
/// as a run that keeps nothing (the bypass off) hears it. A diode forward-biased through a
/// resistor from a supply, and a common-emitter stage biased from the rail (its collector then
/// settles on the base's slower time constant, the same in both runs).
#[test]
fn a_device_value_edit_breaks_the_bypass() {
    let clamp = |is: f64, r1: f64| {
        let mut c = SimCircuit::new();
        c.model("dm", ModelKind::Diode, &[("is", is), ("cjo", 2e-12)]);
        c.v("v1", "in", "0", Source::dc(5.0));
        c.r("r1", "in", "a", r1);
        c.d("d1", "a", "0", "dm");
        c.c("c1", "a", "0", 1e-9);
        c
    };
    let stage = |bf: f64| {
        let mut c = SimCircuit::new();
        c.model("npn", ModelKind::Npn, &[("is", 1e-15), ("bf", bf), ("cje", 2e-12), ("cjc", 1e-12), ("tf", 3e-10)]);
        c.v("vcc", "vcc", "0", Source::dc(9.0));
        c.r("rb", "vcc", "b", 1e6);
        c.r("rc", "vcc", "c", 4.7e3);
        c.q("q1", "c", "b", "0", "npn");
        c.c("c1", "c", "0", 470e-12);
        c
    };
    let rest = |c: &SimCircuit, net: &str| {
        let mut s = Solver::new(c).expect("builds");
        operating_point(&mut s, false).expect("the operating point");
        s.x[s.unknown(net).expect("the net") as usize]
    };
    let cases = [
        ("the diode's saturation current", "a", clamp(1e-14, 10e3), clamp(1e-13, 10e3), "d1", "is", 1e-13),
        ("the resistor feeding the diode", "a", clamp(1e-14, 10e3), clamp(1e-14, 2.2e3), "r1", "r", 2.2e3),
        ("the transistor's forward beta", "c", stage(100.0), stage(150.0), "q1", "bf", 150.0),
    ];
    let (fast, t_edit) = (MODES[1].1, 1e-3);
    for (what, net, from, to, device, key, value) in cases {
        let (old, new) = (rest(&from, net), rest(&to, net));
        assert!((new - old).abs() > 0.03, "{what}: the edit moves {net}: {old:.4} V to {new:.4} V");
        // two samples after the edit, and the evaluations kept before it
        let heard = |c: SimCircuit| {
            let (mut sim, mut record) = start(c, fast);
            run_to(&mut sim, &mut record, t_edit);
            let kept: u64 = sim.solver().banks.iter().map(|b| b.bypassed()).sum();
            let edit = sim.t();
            set(&mut sim, device, key, value);
            run_to(&mut sim, &mut record, edit + 4.0 * period());
            (points(&record, net, edit + 2.0 * period(), edit + 4.0 * period())[0].1, kept)
        };
        let (soon, kept) = heard(from.clone());
        let mut plain = from;
        plain.opts.bypass = false;
        let (soon_plain, none) = heard(plain);
        assert!(kept > 0 && none == 0, "{what}: Fast kept the resting device's evaluation before the edit ({kept}), the plain run nothing ({none})");
        assert!((soon - old).abs() > 0.9 * (new - old).abs(), "{what}: two samples after the edit {net} is {soon:.5} V, on its way from the old value's {old:.5} V to a fresh solver's {new:.5} V");
        assert!((soon - soon_plain).abs() < 0.01 * (new - old).abs(), "{what}: two samples after the edit {net} is {soon:.5} V, {soon_plain:.5} V in a run that keeps nothing");
    }
}
