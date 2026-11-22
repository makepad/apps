//! E3 8.4 device rows T9, T10 and the per-device checks of 2.2 (T23) for the classic set and the
//! diode.

use crate::devcheck::{charge_cycle, continuity, jacobian, Lcg};
use crate::linear::run_ok;
use fab_elec_sim::expr::Expr;
use fab_elec_sim::newton::{Extras, NewtonFail};
use fab_elec_sim::stamp::UnkKind;
use fab_elec_sim::*;

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs().max(1e-300)
}

/// `w` with `w e^w = e^lz`, the argument given by its logarithm (it overflows for many volts).
fn lambert_w_exp(lz: f64) -> f64 {
    // Newton on w + ln w = lz: increasing and concave, so the iterates approach from below
    let mut w = if lz > 1.0 { lz - lz.ln() } else { lz.exp() / (1.0 + lz.exp()) };
    for _ in 0..100 {
        let nw = w * (1.0 + lz - w.ln()) / (1.0 + w);
        let nw = if nw > 0.0 { nw } else { 0.1 * w };
        if (nw - w).abs() <= 1e-16 * nw {
            return nw;
        }
        w = nw;
    }
    w
}

const T300: f64 = 300.0 - 273.15;

/// A restart from a saved solution restores the devices' limiter references as well as `x`
/// (ESIM-Q4 A.4-4): two solvers at the same operating point, one of which first failed a Newton
/// from a far point, take the same next stage after a reseat, iteration for iteration and bit for
/// bit. Without the reseat the second one's limiters measure its first step from the far iterate.
#[test]
fn a_retry_starts_from_the_saved_solution() {
    let mut c = SimCircuit::new();
    c.model("dm", ModelKind::Diode, &[]);
    c.model("rm", ModelKind::Reg, &[]);
    c.v("vin", "in", "0", Source::dc(5.0));
    c.r("rd", "in", "d", 1e3);
    c.d("d1", "d", "0", "dm");
    c.reg("reg", "in", "0", "out", "in", "rm");
    c.r("load", "out", "0", 66.0);

    let mut clean = Solver::new(&c).unwrap();
    let mut retry = Solver::new(&c).unwrap();
    let cx = clean.base_cx();
    let g = 1e-6;
    let ex = Extras { gshunt: g, ..Default::default() };
    let max_iter = c.opts.itl1;
    clean.newton(&cx, true, None, &ex, max_iter).expect("the clean solver reaches the operating point at shunt g");
    retry.newton(&cx, true, None, &ex, max_iter).expect("the retrying solver reaches the operating point at shunt g");
    let saved = clean.x.clone();
    for (u, (&a, &b)) in saved.iter().zip(&retry.x).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "both solvers start at the same operating point: {}", clean.unknowns[u].name);
    }

    for (u, info) in retry.unknowns.iter().enumerate().skip(1) {
        if info.kind == UnkKind::Voltage {
            retry.x[u] = 10.0;
        }
    }
    assert!(matches!(retry.newton(&cx, false, None, &ex, 1), Err(NewtonFail::NoConvergence { .. })),
        "one Newton iteration from every node at 10 V does not converge");
    retry.x.copy_from_slice(&saved);

    clean.reseat(&cx);
    retry.reseat(&cx);
    let ex = Extras { gshunt: g / 1.0001, ..Default::default() };
    let clean_iters = clean.newton(&cx, false, None, &ex, max_iter).expect("the clean solver converges at the next shunt");
    let retry_iters = retry.newton(&cx, false, None, &ex, max_iter).expect("the reseated solver converges at the next shunt");
    assert_eq!(clean_iters, retry_iters, "the reseated retry takes the clean solver's iteration count");
    for (u, (&a, &b)) in clean.x.iter().zip(&retry.x).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "the reseated retry ends at the clean solver's solution bit for bit: {}", clean.unknowns[u].name);
    }
}

/// T9: a source, a resistor and a diode, against the Lambert-W solution of the Shockley equation
/// with series resistance; from a fraction of a volt to tens of volts, a plain model and one with
/// its own `rs` and emission coefficient.
#[test]
fn t9_diode_resistor() {
    let models = [("dp", 1e-14, 1.0, 0.0), ("dr", 2e-9, 1.8, 5.0)];
    let cases = [(0.3, 1e3), (0.65, 1e3), (1.0, 1e3), (5.0, 1e3), (20.0, 100.0), (2.0, 10.0)];
    let mut c = SimCircuit::new();
    c.temp_c = T300;
    c.opts.tnom = T300;
    // no shunt across the junction: the closed form has none
    c.opts.gmin = 0.0;
    for (name, is, n, rs) in models {
        c.model(name, ModelKind::Diode, &[("is", is), ("n", n), ("rs", rs)]);
    }
    for (mi, _) in models.iter().enumerate() {
        for (k, &(vs, r)) in cases.iter().enumerate() {
            let (a, b) = (format!("a{mi}_{k}"), format!("b{mi}_{k}"));
            c.v(&format!("v{mi}_{k}"), &a, "0", Source::dc(vs));
            c.r(&format!("r{mi}_{k}"), &a, &b, r);
            c.d(&format!("d{mi}_{k}"), &b, "0", models[mi].0);
        }
    }
    let (rec, sum) = run_ok(&c, &Analysis::Op);
    assert_eq!(sum.aid, Some(OpAid::Newton), "plain Newton suffices");
    let mut worst = 0.0f64;
    for (mi, &(_, is, n, rs)) in models.iter().enumerate() {
        let nvt = n * 300.0 * 8.617333262e-5;
        for (k, &(vs, r)) in cases.iter().enumerate() {
            let rt = r + rs;
            let lz = (is * rt / nvt).ln() + (vs + is * rt) / nvt;
            let want = nvt / rt * lambert_w_exp(lz) - is;
            let got = -rec.last(&format!("v{mi}_{k}#branch"));
            // to 1e-9, or a tenth of abstol for a junction that barely conducts
            let e = (got - want).abs() / (1e-9 * want.abs() + 0.1 * c.opts.abstol);
            worst = worst.max(rel(got, want));
            assert!(e < 1.0, "model {mi} at {vs} V through {r} ohm: {got} A vs {want} A");
            // the drop across the external terminals includes the model's rs
            let vd = rec.last(&format!("b{mi}_{k}"));
            let vwant = nvt * (want / is + 1.0).ln() + want * rs;
            assert!((vd - vwant).abs() < 1e-9 * vwant.abs().max(1.0), "model {mi} at {vs} V: drop {vd} vs {vwant}");
        }
    }
    eprintln!("T9 worst relative current error {worst:.2e}");
}

/// T10: the thermal voltage from the solver (the drop difference of a decade of current is
/// `n Vt ln 10`) is 25.852 mV at 300 K; the drop at fixed current follows the model's
/// temperature equations from -40 to 125 C, and its slope at the nominal temperature is the
/// textbook `(Vd - Eg - XTI Vt) / T`.
#[test]
fn t10_diode_temperature() {
    let (is, eg, xti) = (1e-14, 1.11, 3.0);
    let drops = |temp_c: f64| -> (f64, f64) {
        let mut c = SimCircuit::new();
        c.temp_c = temp_c;
        c.opts.tnom = T300;
        // the model equations, not the Newton tolerance, are under test
        c.opts.reltol = 1e-10;
        c.opts.vntol = 1e-13;
        c.model("dm", ModelKind::Diode, &[("is", is)]);
        c.i("i1", "0", "a1", Source::dc(1e-3));
        c.d("d1", "a1", "0", "dm");
        c.i("i10", "0", "a10", Source::dc(1e-2));
        c.d("d10", "a10", "0", "dm");
        let (rec, _) = run_ok(&c, &Analysis::Op);
        (rec.last("a1"), rec.last("a10"))
    };
    let (v1, v10) = drops(T300);
    let vt = (v10 - v1) / 10f64.ln();
    assert!((vt - 25.852e-3).abs() < 5e-7, "thermal voltage at 300 K: {vt}");
    let gmin = SimCircuit::new().opts.gmin;
    for temp_c in [-40.0, 0.0, T300, 60.0, 125.0] {
        let t = temp_c + 273.15;
        let vt = 8.617333262e-5 * t;
        let ist = is * (t / 300.0).powf(xti) * ((t / 300.0 - 1.0) * eg / vt).exp();
        let (got, _) = drops(temp_c);
        // the 1 mA less the shunt gmin draws
        let want = vt * ((1e-3 - gmin * got) / ist + 1.0).ln();
        assert!(rel(got, want) < 1e-9, "drop at {temp_c} C: {got} vs {want}");
    }
    let slope = drops(T300 + 0.5).0 - drops(T300 - 0.5).0;
    let vt = 300.0 * 8.617333262e-5;
    let want = (v1 - eg - xti * vt) / 300.0;
    assert!(rel(slope, want) < 1e-4, "forward-drop slope {slope} V/K vs {want} V/K");
    assert!(slope < -1.5e-3 && slope > -2.5e-3, "silicon's -2 mV/K: {slope}");
}

/// One circuit with every classic device, two behavioural sources (a current and a voltage of
/// node voltages and branch currents), a behavioural resistor, capacitor and inductor, and the full diode model (series resistance, transit time,
/// graded depletion, breakdown, recombination, high injection), for the generic checks.
fn t23_circuit() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("dfull", ModelKind::Diode, &[
        ("is", 1e-14), ("n", 1.05), ("rs", 2.0), ("cjo", 2e-12), ("vj", 0.7), ("m", 0.4), ("fc", 0.5), ("tt", 5e-9),
        ("bv", 6.0), ("ibv", 1e-4), ("nbv", 1.2), ("isr", 1e-11), ("nr", 2.0), ("ikf", 0.05),
    ]);
    c.model("dmin", ModelKind::Diode, &[("cjo", 1e-12)]);
    c.model("swm", ModelKind::Sw, &[("ron", 1.0), ("roff", 1e6), ("vt", 1.0), ("vh", 0.2), ("vband", 0.2)]);
    c.model("cswm", ModelKind::Csw, &[("ron", 1.0), ("roff", 1e6), ("it", 0.5e-3), ("ih", 0.2e-3), ("iband", 0.2e-3)]);
    c.d("d1", "a", "k", "dfull");
    c.d("d2", "k", "0", "dmin");
    c.r("r1", "a", "x", 1e3);
    c.c("c1", "x", "0", 1e-9);
    c.l("l1", "x", "y", 1e-3);
    c.l("l2", "y", "0", 2e-3);
    c.k("k1", "l1", "l2", 0.7);
    c.v("vc", "cn", "0", Source::dc(1.0));
    c.sw("s1", "k", "y", "cn", "0", "swm", false);
    c.csw("w1", "a", "y", "vc", "cswm", false);
    // a lossless line by its delay and a lossy one by its card: the DC two-port's every entry
    c.tline("t1", "a", "k", "x", "h", 50.0, 1e-9);
    c.model("ltm", ModelKind::Ltra, &[("r", 2.0), ("l", 250e-9), ("g", 1e-4), ("c", 100e-12), ("len", 0.5)]);
    c.ltra("o1", "e", "x", "y", "0", "ltm");
    c.vctl("e1", CtlKind::Vcvs, "e", "0", "a", "k", 2.0);
    c.vctl("g1", CtlKind::Vccs, "e", "x", "cn", "0", 1e-3);
    c.ictl("f1", CtlKind::Cccs, "y", "0", "vc", 3.0);
    c.ictl("h1", CtlKind::Ccvs, "h", "0", "vc", 50.0);
    c.r("re", "e", "h", 1e3);
    let expr = |text: &str| Expr::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
    c.b("bi", BKind::I, "e", "x", expr("1e-3*v(a, k)*v(cn) + 1e-4*sin(v(y)) + 2*i(vc)"));
    c.b("bv", BKind::V, "bv", "0", expr("v(x)^2 + 10*i(vc) + exp(v(cn)/4) + atan(v(h, e))"));
    c.r("rbv", "bv", "0", 1e3);
    c.b("br", BKind::R, "x", "y", expr("1e3*(2 + sin(v(cn))) + 100*v(x, y)^2"));
    c.b("bc", BKind::C, "x", "h", expr("1e-9*(2 + sin(v(a, k))) + 1e-10*v(x, h)^2 + 1e-8*i(vc)"));
    c.b("bl", BKind::L, "h", "y", expr("1e-3*(1 + 0.5*sin(v(cn))) + 1e-1*i(bl)^2 + 1e-5*v(x, y)"));
    c
}

/// A random bias: every voltage in [-1, 1] V, every current in [-10, 10] mA, then d1's junction
/// across forward, depletion, reverse and breakdown, its series resistor across a fraction of a
/// volt, and the switch control across both thresholds.
fn t23_bias(rng: &mut Lcg, s: &Solver, x: &mut [f64]) {
    for (u, info) in s.unknowns.iter().enumerate().skip(1) {
        x[u] = match info.kind {
            UnkKind::Voltage => rng.range(-1.0, 1.0),
            UnkKind::Current => rng.range(-1e-2, 1e-2),
        };
    }
    let at = |n: &str| s.unknown(n).unwrap_or_else(|| panic!("no unknown {n}")) as usize;
    let (k, ai, a, cn) = (at("k"), at("d1#a"), at("a"), at("cn"));
    x[k] = rng.range(-0.5, 0.75);
    x[ai] = x[k] + rng.range(-6.3, 0.95);
    x[a] = x[ai] + rng.range(-0.3, 0.3);
    x[cn] = rng.range(-1.0, 3.0);
}

/// T23 for the classic set and the diode: Jacobians at 10 000 random biases, charges neutral and
/// a function of the bias alone, and continuity of currents, charges and their slopes along a path
/// through every diode region and both switch thresholds.
#[test]
fn t23_device_checks_classic_and_diode() {
    let c = t23_circuit();
    let mut rng = Lcg(23);
    let rep = jacobian(&c, 10_000, &mut rng, &mut t23_bias);
    assert!(rep.g.0 < 1e-6, "G = df/dx: {:.2e} at row/col {:?}, bias {:?}", rep.g.0, rep.g.2, rep.g.1);
    assert!(rep.c.0 < 1e-6, "C = dq/dx: {:.2e} at row/col {:?}, bias {:?}", rep.c.0, rep.c.2, rep.c.1);
    assert!(rep.neutral < 1e-12, "row charges sum to zero: {:.2e}", rep.neutral);
    let cycle = charge_cycle(&c, &mut rng, &mut t23_bias);
    assert!(cycle == 0.0, "charges return bit for bit after a bias cycle: {cycle:.2e}");
    // one path: d1 from deep breakdown to strong forward bias, d2 across zero, the switch control
    // from below to above both thresholds, the inductor currents and linear nodes moving too
    let path = |t: f64, s: &Solver, x: &mut [f64], dx: &mut [f64]| {
        let at = |n: &str| s.unknown(n).unwrap() as usize;
        for (u, info) in s.unknowns.iter().enumerate().skip(1) {
            (x[u], dx[u]) = match info.kind {
                UnkKind::Voltage => (0.1 * t + 0.01 * u as f64, 0.1),
                UnkKind::Current => (1e-3 * t, 1e-3),
            };
        }
        let (k, ai, a, cn) = (at("k"), at("d1#a"), at("a"), at("cn"));
        (x[k], dx[k]) = (0.2 + 0.05 * t, 0.05);
        (x[ai], dx[ai]) = (x[k] + t, dx[k] + 1.0);
        (x[a], dx[a]) = (x[ai] + 0.01, dx[ai]);
        (x[cn], dx[cn]) = (1.0 + 0.4 * t, 0.4);
    };
    let (worst, at) = continuity(&c, -6.3, 0.95, 4000, 1e-3, &path);
    assert!(worst < 1e-6, "continuity along the path: {worst:.2e} at t = {at}");
    eprintln!("T23 classic+diode: G {:.2e} C {:.2e} continuity {worst:.2e}", rep.g.0, rep.c.0);
}

/// A junction's cached depletion (`Depletion`, the diode's) gives `depletion`'s bits on both
/// branches, at the corner and across it, for grading coefficients and corners in use.
#[test]
fn cached_depletion_matches_depletion() {
    use fab_elec_sim::dev::junction::{depletion, Depletion};
    for (cj0, pb, m, fc) in [(2e-12, 0.75, 0.5, 0.5), (4e-12, 0.6, 0.333, 0.5), (1e-12, 1.0, 0.25, 0.75), (0.0, 0.7, 0.5, 0.5)] {
        let cached = Depletion::new(cj0, pb, m, fc);
        for k in -60..=60 {
            let v = fc * pb + k as f64 * 0.017;
            for v in [v, fc * pb] {
                let (a, b) = (depletion(v, cj0, pb, m, fc), cached.at(v));
                assert_eq!((a.0.to_bits(), a.1.to_bits()), (b.0.to_bits(), b.1.to_bits()), "cj0 {cj0:e} pb {pb} m {m} fc {fc} at {v}: {a:?} against {b:?}");
            }
        }
    }
}
