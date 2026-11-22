//! E3 8.4 closed-form rows on linear circuits: T1-T8, T13, T14, T24.

use fab_elec_sim::*;
use std::f64::consts::PI;

pub fn run_ok(c: &SimCircuit, a: &Analysis) -> (Record, Summary) {
    let mut r = Record::default();
    let s = run(c, a, &mut r, &Control::default()).unwrap_or_else(|d| panic!("{d}"));
    (r, s)
}

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs().max(1e-300)
}

fn tran(tstop: f64, max_step: Option<f64>, uic: bool) -> Analysis {
    Analysis::Tran(TranSpec { tstop, max_step, uic })
}

/// T1: a divider, a bridge and an R-2R ladder, on the dense and the sparse solver.
#[test]
fn t1_resistor_networks() {
    for dense in [true, false] {
        let mut c = SimCircuit::new();
        c.opts.force_dense = Some(dense);
        // divider
        c.v("vd", "d_in", "0", Source::dc(10.0));
        c.r("rd1", "d_in", "d_mid", 1e3);
        c.r("rd2", "d_mid", "0", 3e3);
        // bridge: a-b, b-0, a-c, c-0, b-c
        let (r1, r2, r3, r4, r5, va) = (1e3, 2.2e3, 4.7e3, 1.5e3, 3.3e3, 5.0);
        c.v("vb", "b_a", "0", Source::dc(va));
        c.r("rb1", "b_a", "b_b", r1);
        c.r("rb2", "b_b", "0", r2);
        c.r("rb3", "b_a", "b_c", r3);
        c.r("rb4", "b_c", "0", r4);
        c.r("rb5", "b_b", "b_c", r5);
        // R-2R ladder of 60 stages, terminated in 2R: node k sits at V / 2^k
        let stages = 60;
        c.v("vl", "l0", "0", Source::dc(1.0));
        for k in 0..stages {
            c.r(&format!("rs{k}"), &format!("l{k}"), &format!("l{}", k + 1), 1e3);
            c.r(&format!("rp{}", k + 1), &format!("l{}", k + 1), "0", if k + 1 == stages { 1e3 } else { 2e3 });
        }
        let (r, s) = run_ok(&c, &Analysis::Op);
        assert_eq!(s.dense, dense, "layout choice");
        assert!(rel(r.last("d_mid"), 7.5) < 1e-9, "divider: {}", r.last("d_mid"));
        // bridge by nodal analysis, Cramer's rule
        let (g1, g2, g3, g4, g5) = (1.0 / r1, 1.0 / r2, 1.0 / r3, 1.0 / r4, 1.0 / r5);
        let (a11, a12, a22) = (g1 + g2 + g5, -g5, g3 + g4 + g5);
        let (b1, b2) = (g1 * va, g3 * va);
        let det = a11 * a22 - a12 * a12;
        let (vb, vc) = ((b1 * a22 - a12 * b2) / det, (a11 * b2 - a12 * b1) / det);
        assert!(rel(r.last("b_b"), vb) < 1e-9 && rel(r.last("b_c"), vc) < 1e-9, "bridge: {} {} vs {vb} {vc}", r.last("b_b"), r.last("b_c"));
        for k in [1usize, 2, 10, 30, 59] {
            let v = r.last(&format!("l{k}"));
            let want = 0.5f64.powi(k as i32);
            assert!(rel(v, want) < 1e-9, "ladder node {k}: {v} vs {want} (dense {dense})");
        }
        assert!(rel(r.last("vd#branch"), -10.0 / 4e3) < 1e-9, "source branch current sign: into p through the source");
        // a DC sweep, and a nested one (outer source, inner resistor)
        let (sw, _) = run_ok(&c, &Analysis::Dc(vec![DcSweep::lin("vd", "dc", 0.0, 10.0, 0.5)]));
        assert_eq!(sw.at.len(), 21, "sweep points");
        assert!(sw.at.iter().zip(sw.v("d_mid")).all(|(&v, m)| (m - 0.75 * v).abs() < 1e-9 * v.max(1.0)), "divider along the sweep");
        let (nest, _) = run_ok(&c, &Analysis::Dc(vec![DcSweep::lin("vd", "dc", 1.0, 3.0, 1.0), DcSweep { device: "rd2".into(), key: "r".into(), values: vec![1e3, 3e3] }]));
        let mid = nest.v("d_mid");
        for k in 0..nest.at.len() {
            let (vin, r2) = (nest.outer[k].unwrap(), nest.at[k]);
            assert!(rel(mid[k], vin * r2 / (1e3 + r2)) < 1e-9, "nested sweep point {vin} V, {r2} ohm");
        }
    }
}

/// T2 and T3: RC and RL steps from initial conditions, at default tolerances (0.1% of full
/// scale) and at reltol 1e-6. The step control bounds the local error of each step by
/// trtol * reltol (7e-6 here); the global error of a decaying response is a few local errors.
#[test]
fn t2_t3_rc_rl_steps() {
    for (reltol, bound) in [(1e-3, 1e-3), (1e-6, 1e-4)] {
        let mut c = SimCircuit::new();
        c.opts.reltol = reltol;
        c.v("v1", "in", "0", Source::dc(1.0));
        c.r("r1", "in", "rc", 1e3);
        c.c("c1", "rc", "0", 1e-6);
        c.set_ic("rc", 0.0);
        c.v("v2", "in2", "0", Source::dc(1.0));
        c.r("r2", "in2", "rl", 10.0);
        c.l("l1", "rl", "0", 10e-3);
        let (r, _) = run_ok(&c, &tran(5e-3, None, true));
        let tau = 1e-3;
        let mut worst_rc = 0.0f64;
        let mut worst_rl = 0.0f64;
        for (k, &t) in r.at.iter().enumerate() {
            let rc = r.x[k][r.index("rc")];
            let il = r.x[k][r.index("l1#branch")];
            worst_rc = worst_rc.max((rc - (1.0 - (-t / tau).exp())).abs());
            worst_rl = worst_rl.max((il / 0.1 - (1.0 - (-t / tau).exp())).abs());
        }
        assert!(worst_rc < bound, "T2 RC at reltol {reltol}: worst error {worst_rc:e} of 1 V full scale");
        assert!(worst_rl < bound, "T3 RL at reltol {reltol}: worst error {worst_rl:e} of 0.1 A full scale");
        let at1 = r.at_time("rc", 1e-3);
        assert!((at1 - 0.63212).abs() < 1e-4 + bound, "T2 at 1 ms: {at1}");
    }
}

/// T4: series RLC 10 ohm, 1 mH, 1 uF: ringing at 4969.6 Hz under an exp(-5000 t) envelope.
#[test]
fn t4_series_rlc() {
    let (r_, l, cv): (f64, f64, f64) = (10.0, 1e-3, 1e-6);
    let alpha = r_ / (2.0 * l);
    let w0 = 1.0 / (l * cv).sqrt();
    let wd = (w0 * w0 - alpha * alpha).sqrt();
    assert!((w0 / (2.0 * PI) - 5032.9).abs() < 0.05 && (wd / (2.0 * PI) - 4969.6).abs() < 0.05, "closed form constants");
    let exact = |t: f64| 1.0 - (-alpha * t).exp() * ((wd * t).cos() + alpha / wd * (wd * t).sin());
    let fs = 1.0 + (-alpha * PI / wd).exp();
    for reltol in [1e-3, 1e-6] {
        let mut c = SimCircuit::new();
        c.opts.reltol = reltol;
        c.v("v1", "in", "0", Source::dc(1.0));
        c.r("r1", "in", "a", r_);
        c.l("l1", "a", "b", l);
        c.c("c1", "b", "0", cv);
        c.set_ic("b", 0.0);
        let (r, _) = run_ok(&c, &tran(2e-3, None, true));
        let v = r.v("b");
        if reltol < 1e-3 {
            // the waveform itself: frequency, damping and envelope at once
            let worst = r.at.iter().zip(&v).map(|(&t, v)| (v - exact(t)).abs()).fold(0.0, f64::max);
            assert!(worst < 5e-4 * fs, "RLC at reltol {reltol}: worst error {worst:e} of full scale {fs}");
        } else {
            // at default tolerances the control lets each step's local error reach trtol * reltol;
            // for trapezoidal on a ringing pair that is (wh)^3 / 12, and the frequency then reads
            // low by (wh)^2 / 12: the bound the tolerance itself implies
            let wh = (12.0 * c.opts.trtol * reltol).powf(1.0 / 3.0);
            let implied = wh * wh / 12.0;
            let ups: Vec<f64> = (1..v.len()).filter(|&k| v[k - 1] < 1.0 && v[k] >= 1.0).map(|k| r.at[k - 1] + (1.0 - v[k - 1]) / (v[k] - v[k - 1]) * (r.at[k] - r.at[k - 1])).collect();
            let fd = (ups.len() - 1) as f64 / (ups[ups.len() - 1] - ups[0]);
            let err = 1.0 - fd / (wd / (2.0 * PI));
            assert!(ups.len() >= 8 && err.abs() < 1.5 * implied, "ringing at {fd} Hz over {} crossings: {err:e} low, the tolerance implies {implied:e}", ups.len());
        }
    }
}

/// T5: RC low-pass, 1 kohm and 159.155 nF: -3.0103 dB and -45 degrees at 1 kHz, -20 dB/decade.
#[test]
fn t5_rc_lowpass_ac() {
    let (rv, cv) = (1e3, 159.155e-9);
    let mut c = SimCircuit::new();
    c.v("v1", "in", "0", Source::ac(0.0, 1.0, 0.0));
    c.r("r1", "in", "out", rv);
    c.c("c1", "out", "0", cv);
    let (r, _) = run_ok(&c, &Analysis::Ac(AcSweep::List(vec![1e3, 1e5, 1e6])));
    let h = r.ac_v("out");
    for (k, &f) in r.freq.iter().enumerate() {
        let want = C64::new(1.0, 0.0) / C64::new(1.0, 2.0 * PI * f * rv * cv);
        assert!((h[k] - want).norm() < 1e-9 * want.norm(), "H at {f} Hz: {:?} vs {want:?}", h[k]);
    }
    assert!((h[0].db() + 3.0103).abs() < 1e-3 && (h[0].arg().to_degrees() + 45.0).abs() < 1e-3, "corner at 1 kHz");
    assert!((h[2].db() - h[1].db() + 20.0).abs() < 0.01, "slope above the corner: {}", h[2].db() - h[1].db());
    let r2 = run_ok(&c, &Analysis::Ac(AcSweep::Dec { n: 10, fstart: 10.0, fstop: 1e6 })).0;
    assert_eq!(r2.freq.len(), 51, "decade sweep includes both ends");
}

fn lc(method: Method, reltol: f64, max_step: Option<f64>, periods: f64) -> (Record, f64) {
    let (l, cv) = (1e-3, 1e-6);
    let mut c = SimCircuit::new();
    c.opts.method = method;
    c.opts.reltol = reltol;
    c.l("l1", "a", "0", l);
    c.c("c1", "a", "0", cv);
    c.set_ic("a", 1.0);
    let period = 2.0 * PI * (l * cv).sqrt();
    (run_ok(&c, &tran(periods * period, max_step.map(|m| m * period), true)).0, period)
}

fn energy(r: &Record, k: usize) -> f64 {
    let v = r.x[k][r.index("a")];
    let i = r.x[k][r.index("l1#branch")];
    0.5 * 1e-6 * v * v + 0.5 * 1e-3 * i * i
}

/// T6 and T7: a lossless LC tank keeps its energy under trapezoidal over 1000 periods; Gear 2 and
/// Euler lose it at their known per-step rates; halving the step cuts the error 4x (trapezoidal,
/// Gear 2) and 2x (Euler).
#[test]
fn t6_t7_integrators() {
    // the backward Euler steps after t = 0 dissipate a little; from the first period on the
    // trapezoidal rule is unitary on an LC tank whatever its step
    let (r, period) = lc(Method::Trap, 1e-3, None, 1000.0);
    let k1 = r.at.partition_point(|&t| t < period);
    let e1 = energy(&r, k1);
    let worst = (k1..r.at.len()).map(|k| (energy(&r, k) / e1 - 1.0).abs()).fold(0.0, f64::max);
    assert!(worst < 1e-9, "T6 trapezoidal energy drift over 1000 periods: {worst:e} ({} steps)", r.at.len());
    assert!(energy(&r, k1) / energy(&r, 0) > 0.98, "the start-up loss stays within the backward Euler steps' tolerance");

    // fixed steps of T/40 (loose tolerance, the step held by max_step): the decay per step is the
    // modulus of the method's principal root at z = i w h
    let wh = 2.0 * PI / 40.0;
    let z = C64::new(0.0, wh);
    let one = C64::new(1.0, 0.0);
    let euler = (one / (one - z)).norm();
    // Gear 2: (3/2 - z) r^2 - 2 r + 1/2 = 0, principal root (2 + sqrt(1 + 2z)) / (3 - 2z)
    let sq = {
        let w = one + z.scale(2.0);
        C64::polar(w.norm().sqrt(), 0.5 * w.arg())
    };
    let gear2 = ((C64::new(2.0, 0.0) + sq) / (C64::new(3.0, 0.0) - z.scale(2.0))).norm();
    for (method, root) in [(Method::Euler, euler), (Method::Gear, gear2)] {
        let (r, period) = lc(method, 1.0, Some(1.0 / 40.0), 20.0);
        let k5 = r.at.partition_point(|&t| t < 5.0 * period);
        let k15 = r.at.partition_point(|&t| t < 15.0 * period);
        let steps = (k15 - k5) as i32;
        let h = r.at[k5 + 1] - r.at[k5];
        assert!((h * 40.0 / period - 1.0).abs() < 1e-9, "{method:?}: fixed step held ({h:e})");
        let ratio = energy(&r, k15) / energy(&r, k5);
        let want = root.powi(2 * steps);
        assert!(rel(ratio, want) < 1e-6, "{method:?}: energy decay {ratio} vs {want} over {steps} steps");
    }

    // order: RC at fixed steps tau/20 and tau/40, error at t = tau
    let rc_err = |method: Method, div: f64| {
        let mut c = SimCircuit::new();
        c.opts.method = method;
        c.opts.reltol = 1.0;
        c.v("v1", "in", "0", Source::dc(1.0));
        c.r("r1", "in", "o", 1e3);
        c.c("c1", "o", "0", 1e-6);
        c.set_ic("o", 0.0);
        // a corner at t = tau puts a time point there
        c.v("vmark", "mark", "0", Source::wave(Wave::Pwl(vec![(0.0, 0.0), (1e-3, 0.0), (2e-3, 0.0)])));
        c.r("rmark", "mark", "0", 1.0);
        let (r, _) = run_ok(&c, &tran(2e-3, Some(1e-3 / div), true));
        let k = r.at.iter().position(|&t| (t - 1e-3).abs() < 1e-12).expect("a point at t = tau");
        (r.x[k][r.index("o")] - (1.0 - (-1.0f64).exp())).abs()
    };
    for (method, want) in [(Method::Trap, 4.0), (Method::Gear, 4.0), (Method::Euler, 2.0)] {
        let ratio = rc_err(method, 20.0) / rc_err(method, 40.0);
        assert!((ratio / want - 1.0).abs() < 0.15, "T7 {method:?}: halving the step cut the error {ratio:.3}x, want {want}x");
    }
}

/// T8: two capacitors share their charge through a switch: the total charge holds at every
/// accepted point (the charge-form proof) and both settle at (C1 V1 + C2 V2) / (C1 + C2).
#[test]
fn t8_charge_sharing() {
    let mut c = SimCircuit::new();
    c.model("sw", ModelKind::Sw, &[("ron", 1.0), ("roff", 1e12), ("vt", 0.5)]);
    c.c("c1", "a", "0", 1e-6);
    c.c("c2", "b", "0", 2e-6);
    c.v("vc", "ctl", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 1.0, td: 10e-6, tr: 1e-6, tf: 1e-6, pw: 1.0, per: 0.0 }));
    c.sw("s1", "a", "b", "ctl", "0", "sw", false);
    c.set_ic("a", 1.0);
    c.set_ic("b", 0.0);
    c.set_ic("ctl", 0.0);
    let (r, _) = run_ok(&c, &tran(100e-6, None, true));
    let q0 = 1e-6;
    let worst = (0..r.at.len()).map(|k| (1e-6 * r.x[k][r.index("a")] + 2e-6 * r.x[k][r.index("b")] - q0).abs() / q0).fold(0.0, f64::max);
    assert!(worst < 1e-9, "total charge drift {worst:e}");
    let want = 1.0 / 3.0;
    assert!(rel(r.last("a"), want) < 1e-9 && rel(r.last("b"), want) < 1e-9, "shared voltage {} {} vs {want}", r.last("a"), r.last("b"));
    assert!(rel(r.at_time("a", 9e-6), 1.0) < 1e-9, "open switch holds the charge");
}

/// T13: inverting amplifier on a finite-gain op-amp: closed-loop gain, and the bandwidth of a
/// one-pole model at GBW / (1 + R2/R1).
#[test]
fn t13_inverting_amplifier() {
    let (r1, r2, a0, fp) = (1e3, 10e3, 1e5, 10.0);
    let mut c = SimCircuit::new();
    c.v("vin", "in", "0", Source::ac(0.1, 1.0, 0.0));
    c.r("r1", "in", "inn", r1);
    c.r("r2", "inn", "out", r2);
    // one pole: gm into x across R || C, then a unity buffer to the output
    let (gm, rx) = (1e-3, a0 / 1e-3);
    let cx = 1.0 / (2.0 * PI * fp * rx);
    c.vctl("gm", CtlKind::Vccs, "0", "x", "0", "inn", gm);
    c.r("rx", "x", "0", rx);
    c.c("cx", "x", "0", cx);
    c.vctl("buf", CtlKind::Vcvs, "out", "0", "x", "0", 1.0);
    let gain = -(r2 / r1) / (1.0 + (1.0 + r2 / r1) / a0);
    let (op, _) = run_ok(&c, &Analysis::Op);
    assert!(rel(op.last("out") / 0.1, gain) < 1e-9, "DC gain {} vs {gain}", op.last("out") / 0.1);
    let beta = r1 / (r1 + r2);
    let f3 = fp * (1.0 + a0 * beta);
    assert!(rel(f3, a0 * fp * beta) < 2e-4, "bandwidth is GBW / (1 + R2/R1) to first order");
    let (r, _) = run_ok(&c, &Analysis::Ac(AcSweep::List(vec![1.0, f3])));
    let h = r.ac_v("out");
    assert!(rel(h[1].norm(), h[0].norm() / 2f64.sqrt()) < 1e-6, "-3 dB at {f3} Hz: {} vs {}", h[1].norm(), h[0].norm());
}

/// T14: coupled inductors: open-circuit voltage ratio k sqrt(L2/L1); with the secondary shorted
/// the primary looks like the leakage inductance L1 (1 - k^2).
#[test]
fn t14_coupled_inductors() {
    let (l1, l2, k) = (1e-3, 4e-3, 0.9);
    for shorted in [false, true] {
        let mut c = SimCircuit::new();
        c.v("v1", "in", "0", Source::ac(0.0, 1.0, 0.0));
        c.r("rs", "in", "p", 1.0);
        c.l("lp", "p", "0", l1);
        c.l("ls", "s", "0", l2);
        c.k("k1", "lp", "ls", k);
        if shorted {
            // a short (an ideal one would close a DC loop of an inductor and a source)
            c.r("rshort", "s", "0", 1e-6);
        } else {
            c.r("rload", "s", "0", 1e12);
        }
        let f = 1e5;
        let (r, _) = run_ok(&c, &Analysis::Ac(AcSweep::List(vec![f])));
        let vp = r.ac_v("p")[0];
        if shorted {
            let i = r.ac_v("lp#branch")[0];
            let lin = (vp / i).im / (2.0 * PI * f);
            assert!(rel(lin, l1 * (1.0 - k * k)) < 1e-9, "leakage inductance {lin:e}");
        } else {
            let ratio = (r.ac_v("s")[0] / vp).re;
            assert!(rel(ratio, k * (l2 / l1).sqrt()) < 1e-6, "voltage ratio {ratio}");
        }
    }
}

/// T24: the same run twice gives the same bits.
#[test]
fn t24_determinism() {
    let make = || {
        let mut c = SimCircuit::new();
        c.v("v1", "in", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 1.0, td: 1e-5, tr: 1e-7, tf: 1e-7, pw: 2e-4, per: 5e-4 }));
        c.r("r1", "in", "a", 10.0);
        c.l("l1", "a", "b", 1e-3);
        c.c("c1", "b", "0", 1e-6);
        c
    };
    let (a, _) = run_ok(&make(), &tran(2e-3, None, false));
    let (b, _) = run_ok(&make(), &tran(2e-3, None, false));
    assert_eq!(a.at.len(), b.at.len(), "same step count");
    let same = a.at.iter().zip(&b.at).all(|(x, y)| x.to_bits() == y.to_bits()) && a.x.iter().zip(&b.x).all(|(p, q)| p.iter().zip(q).all(|(x, y)| x.to_bits() == y.to_bits()));
    assert!(same, "bit-identical runs");
}
