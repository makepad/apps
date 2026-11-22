//! Transmission lines (T, and O with an LTRA card) against their closed forms: the delayed and
//! reflected waves of matched, open and shorted lossless lines, a distortionless line's attenuated
//! wave, the exact AC two-port of lossless and lossy lines, and the located error of a lossy line in
//! a transient.

use crate::linear::run_ok;
use fab_elec_sim::spice::parse;
use fab_elec_sim::*;
use std::f64::consts::PI;

/// The source of every transient here: 0 V to 2 V between 1 ns and 2 ns.
fn vs(t: f64) -> f64 {
    (2.0 * (t - 1e-9) / 1e-9).clamp(0.0, 2.0)
}

/// Three 50 ohm, 10 ns lines, each fed from that source through 50 ohm (the source end matched),
/// and each written another way: T with z0 and td (matched load), O with an LTRA card (open,
/// 1e12 ohm) and T with f and nl (shorted). At every accepted point:
/// - matched: the near end is vs / 2 and the far end vs(t - td) / 2;
/// - open: the far end is (1 + G) vs(t - td) / 2 and the near end adds G vs(t - 2 td) / 2, with
///   G = (1e12 - 50) / (1e12 + 50);
/// - shorted: the near end returns to 0 after 2 td: vs(t) / 2 - vs(t - 2 td) / 2.
/// The delayed corners are breakpoints, so the interpolated delay is exact at 1e-9 V.
#[test]
fn lossless_lines_delay_and_reflect_exactly() {
    let deck = parse(
        "lines\nVs s 0 PWL(0 0 1n 0 2n 2)\nRm s a 50\nTm a 0 b 0 z0=50 td=10n\nRl b 0 50\n\
         Ro s c 50\nOo c 0 d 0 lt\nRd d 0 1e12\n.model lt ltra(r=0 l=500n g=0 c=200p len=1)\n\
         Rs s e 50\nTs e 0 0 0 z0=50 f=25meg nl=0.25\n.tran 0.1n 60n\n.end\n",
    );
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    let o = &deck.circuit.devices.o;
    assert!((o[2].l - 500e-9).abs() < 1e-18 && (o[2].c - 200e-12).abs() < 1e-21, "f = 25 MHz with nl = 0.25 is td = 10 ns: {:?}", o[2]);
    let (r, _) = run_ok(&deck.circuit, &deck.analyses[0]);
    let (td, gamma) = (10e-9, (1e12 - 50.0) / (1e12 + 50.0));
    let at = |t: f64| vs(t);
    let cases: [(&str, &dyn Fn(f64) -> f64); 5] = [
        ("a", &|t| at(t) / 2.0),
        ("b", &|t| at(t - td) / 2.0),
        ("c", &|t| at(t) / 2.0 + gamma * at(t - 2.0 * td) / 2.0),
        ("d", &|t| (1.0 + gamma) * at(t - td) / 2.0),
        ("e", &|t| at(t) / 2.0 - at(t - 2.0 * td) / 2.0),
    ];
    for (node, want) in cases {
        for (t, v) in r.at.iter().zip(r.v(node)) {
            assert!((v - want(*t)).abs() < 1e-9, "{node} at {t:e} s: {v}, the closed form {}", want(*t));
        }
    }
    assert!(r.at.iter().all(|w| w.is_finite()) && r.at.windows(2).all(|w| w[1] - w[0] <= td / 2.0 * (1.0 + 1e-12)), "the step is at most td / 2");
    let lands = |t: f64| r.at.iter().any(|&s| (s - t).abs() < 1e-18);
    assert!([11e-9, 12e-9, 21e-9, 22e-9].iter().all(|&t| lands(t)), "a point on every delayed corner: {:?}", r.at);
}

/// A distortionless line (r = 5 ohm/m, g = 2 mS/m, l = 500 nH/m, c = 200 pF/m: r / l = g / c, z0 =
/// 50 ohm, td = 10 ns) between matched ends: the far end is exp(-r len / z0) vs(t - td) / 2 at
/// every accepted point, and its operating point on 2 V is the DC two-port's exp(-sqrt(r g)) = the
/// same attenuation, the near end at 1 V.
#[test]
fn a_distortionless_line_attenuates_its_delayed_wave() {
    let text = |src: &str| format!("distortionless\nVs s 0 {src}\nRm s a 50\nO1 a 0 b 0 lt\nRl b 0 50\n.model lt ltra(r=5 l=500n g=2m c=200p len=1)\n.tran 0.1n 40n\n.end\n");
    let deck = parse(&text("PWL(0 0 1n 0 2n 2)"));
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    let (r, _) = run_ok(&deck.circuit, &deck.analyses[0]);
    let a = (-0.1f64).exp();
    for ((t, near), far) in r.at.iter().zip(r.v("a")).zip(r.v("b")) {
        assert!((near - vs(*t) / 2.0).abs() < 1e-9, "the near end at {t:e} s: {near}");
        let want = a * vs(t - 10e-9) / 2.0;
        assert!((far - want).abs() < 1e-9, "the far end at {t:e} s: {far}, a vs(t - td) / 2 = {want}");
    }
    let deck = parse(&text("2"));
    let (op, _) = run_ok(&deck.circuit, &Analysis::Op);
    assert!((op.last("a") - 1.0).abs() < 1e-12 && (op.last("b") - a).abs() < 1e-12, "the DC two-port: {} and {}", op.last("a"), op.last("b"));
}

fn sqrt(z: C64) -> C64 {
    C64::polar(z.norm().sqrt(), 0.5 * z.arg())
}

fn tanh(z: C64) -> C64 {
    let e = (-z.scale(2.0)).exp();
    (C64::new(1.0, 0.0) - e) / (C64::new(1.0, 0.0) + e)
}

/// AC sees each line's exact two-port at every frequency:
/// - a lossless 50 ohm, 10 ns line shorted at its far end has the input impedance j z0 tan(w td),
///   j 50 ohm at 12.5 MHz (an eighth wave);
/// - between matched ends its transfer is exp(-j w td) / 2;
/// - a lossy line (r = 10 ohm/m, g = 1 mS/m, not distortionless) shorted at its far end has
///   Zc tanh(gamma len), with gamma = sqrt((r + jwl)(g + jwc)) and Zc = sqrt((r + jwl) / (g + jwc)),
///   from 1 Hz (its DC resistance) to 30 MHz.
/// That lossy line is a located error in a transient.
#[test]
fn ac_sees_the_exact_two_port() {
    let freqs = vec![1.0, 1e6, 5e6, 12.5e6, 20e6, 30e6];
    let rel = |got: C64, want: C64| (got - want).norm() / want.norm().max(1e-300);
    let w = |f: f64| 2.0 * PI * f;
    // the stubs: 1 A into the near end, the far end shorted
    let mut c = SimCircuit::new();
    c.i("i1", "0", "a", Source::ac(0.0, 1.0, 0.0));
    c.tline("t1", "a", "0", "0", "0", 50.0, 10e-9);
    c.i("i2", "0", "x", Source::ac(0.0, 1.0, 0.0));
    c.model("lossy", ModelKind::Ltra, &[("r", 10.0), ("l", 500e-9), ("g", 1e-3), ("c", 200e-12), ("len", 1.0)]);
    c.ltra("o1", "x", "0", "0", "0", "lossy");
    // the matched lossless line
    c.v("vm", "s", "0", Source::ac(0.0, 1.0, 0.0));
    c.r("rm", "s", "m", 50.0);
    c.tline("t2", "m", "0", "n", "0", 50.0, 10e-9);
    c.r("rl", "n", "0", 50.0);
    let (r, _) = run_ok(&c, &Analysis::Ac(AcSweep::List(freqs.clone())));
    let (stub, lossy, far) = (r.ac_v("a"), r.ac_v("x"), r.ac_v("n"));
    for (k, &f) in freqs.iter().enumerate() {
        let want = C64::new(0.0, 50.0 * (w(f) * 10e-9).tan());
        if f > 1.0 {
            assert!(rel(stub[k], want) < 1e-9, "the shorted stub at {f:e} Hz: {:?}, j z0 tan(w td) = {want:?}", stub[k]);
        }
        let want = C64::polar(0.5, -w(f) * 10e-9);
        assert!(rel(far[k], want) < 1e-9, "the matched transfer at {f:e} Hz: {:?}, exp(-j w td) / 2 = {want:?}", far[k]);
        let (z, y) = (C64::new(10.0, w(f) * 500e-9), C64::new(1e-3, w(f) * 200e-12));
        let want = sqrt(z / y) * tanh(sqrt(z * y));
        assert!(rel(lossy[k], want) < 1e-9, "the lossy stub at {f:e} Hz: {:?}, Zc tanh(gamma len) = {want:?}", lossy[k]);
    }
    assert!((stub[3].im - 50.0).abs() < 1e-7, "an eighth-wave shorted stub is j z0: {:?}", stub[3]);
    let Err(diag) = run(&c, &Analysis::Tran(TranSpec { tstop: 1e-6, max_step: None, uic: false }), &mut Record::default(), &Control::default()) else {
        panic!("a lossy line that is not distortionless refuses a transient")
    };
    assert!(diag.message.contains("o1: a lossy line other than distortionless"), "the error names the line: {}", diag.message);
}

/// A live rebuild (an unrelated resistor added mid-flight) carries the matched line's record: the
/// edge launched at 1 ns still reaches the far end at 11 ns, as vs(t - td) / 2 at every point after
/// the rebuild. Without the record the line would restart from the present and show the near end's
/// 1 V at once.
#[test]
fn a_live_rebuild_keeps_the_waves_in_flight() {
    use fab_elec_sim::live::{LiveCmd, ValuePlan};
    use fab_elec_sim::livesim::{LiveSim, LiveSpec};
    let circuit = |extra: bool| {
        let mut c = SimCircuit::new();
        c.v("vs", "s", "0", Source::wave(Wave::Pwl(vec![(0.0, 0.0), (1e-9, 0.0), (2e-9, 2.0)])));
        c.r("rm", "s", "a", 50.0);
        c.tline("t1", "a", "0", "b", "0", 50.0, 10e-9);
        c.r("rl", "b", "0", 50.0);
        if extra {
            c.r("rx", "s", "0", 1e3);
        }
        c
    };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 1e-9, ..LiveSpec::default() };
    let mut sim = LiveSim::new(circuit(false), ValuePlan::default(), 1, spec).expect("the line's operating point");
    let mut record = Record::default();
    let run_to = |sim: &mut LiveSim, record: &mut Record, t: f64| {
        assert!(sim.apply(LiveCmd::Step(t - sim.t())), "the Step is taken");
        while sim.running() {
            sim.advance(f64::INFINITY, None, record).unwrap_or_else(|d| panic!("the line steps: {d}"));
        }
    };
    record.begin(&sim.solver().unknowns);
    run_to(&mut sim, &mut record, 6e-9);
    let t1 = sim.t();
    assert!(t1 > 2e-9 && t1 < 10e-9, "the edge is in flight at {t1:e} s");
    assert!(sim.apply(LiveCmd::Replace { generation: 2, circuit: Box::new(circuit(true)), plan: Box::new(ValuePlan::default()) }), "the rebuild is taken");
    assert_eq!(sim.generation(), 2, "the rebuild replaced the solver");
    record.begin(&sim.solver().unknowns);
    run_to(&mut sim, &mut record, 30e-9);
    let b = record.index("b");
    assert!(record.at.last().is_some_and(|&t| t >= 30e-9 * (1.0 - 1e-12)), "the run reaches 30 ns");
    for (t, x) in record.at.iter().zip(&record.x) {
        let want = vs(t - 10e-9) / 2.0;
        assert!((x[b] - want).abs() < 1e-9, "the far end at {t:e} s after the rebuild: {}, vs(t - td) / 2 = {want}", x[b]);
    }
}
