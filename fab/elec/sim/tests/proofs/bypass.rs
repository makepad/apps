//! Fast's device bypass (`Options::bypass`, ESIMRT S3c): a nonlinear device whose controlling
//! voltages moved by less than a tenth of Newton's tolerance since its last evaluation keeps that
//! evaluation's stamps.
//! - A value edit breaks it (and the single pass): `edits.rs` edits a resting diode's own value, the
//!   resistor feeding it and a resting transistor's beta on the live path.
//! - Exact never bypasses: its run is bit-equal with the bypass allowed or not.
//! - A junction kept from a failed step's retry, under its raised gmin, is evaluated afresh once
//!   gmin is back: the bypass keeps an evaluation only at the gmin it was made at.
//! - Fast with it follows Fast without it within Newton's tolerance (diodes and a behavioural
//!   source), while skipping most of a resting device's evaluations.
//! - Fast's single pass (`Options::single_pass`, on by default with the bypass) is under all of
//!   the above, and saves iterations: without it, a step takes more.
//! - A behavioural source with a corner (`abs`, `uramp`, `min`, `max`, `limit`) keeps its line
//!   only while each corner's operands stay on their side: an op-amp's rail clamp leaves its rail
//!   when its drive turns, as it does without the bypass, and is still bypassed while it rests.

use fab_elec_sim::analysis::tran::{Accepted, Stepper};
use fab_elec_sim::expr::Expr;
use fab_elec_sim::{BKind, CtlKind, ModelKind, SimCircuit, Solver, Source, Wave};

/// Fast at an audio sample rate.
const SAMPLE: f64 = 1.0 / 48_000.0;

fn bypassed(s: &Solver) -> u64 {
    s.banks.iter().map(|b| b.bypassed()).sum()
}

/// Runs `c` to `t_end` at resolution `res`: the points of `net` and the evaluations skipped.
fn run(c: &SimCircuit, res: f64, t_end: f64, net: &str) -> (Vec<(f64, f64)>, u64) {
    let mut s = Solver::new(c).expect("the circuit builds");
    let out = s.unknown(net).expect("the net") as usize;
    let mut st = Stepper::start(&mut s, false, 1e-6).expect("the operating point converges");
    st.set_res(&mut s, res);
    let mut points = Vec::new();
    while st.t() < t_end {
        st.step(&mut s, t_end, &mut |s: &Solver, a: &Accepted| points.push((a.t, s.x[out]))).expect("the transient runs");
    }
    (points, bypassed(&s))
}

/// A 1 kHz sine through a resistor into two diodes in antiparallel (a soft clipper) loaded by an
/// RC, beside a resting diode bias, and a behavioural gain stage on the clipped signal, its gain
/// set by the bias (a voltage-controlled amplifier) into a resistor.
fn clipper(bypass: bool) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("dm", ModelKind::Diode, &[("is", 1e-14), ("cjo", 2e-12), ("tt", 5e-9)]);
    c.v("v1", "in", "0", Source { wave: Wave::Sin { vo: 0.0, va: 2.0, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }, ..Source::dc(0.0) });
    c.r("r1", "in", "clip", 2.2e3);
    c.d("d1", "clip", "0", "dm");
    c.d("d2", "0", "clip", "dm");
    c.r("r2", "clip", "out", 10e3);
    c.c("c1", "out", "0", 10e-9);
    c.v("v2", "rail", "0", Source::dc(9.0));
    c.r("r3", "rail", "bias", 22e3);
    c.d("d3", "bias", "0", "dm");
    c.b("b1", BKind::I, "0", "vca", Expr::parse("1e-4*v(bias)*tanh(2*v(clip))").expect("parses"));
    c.r("r4", "vca", "0", 10e3);
    c.c("c2", "vca", "0", 1e-9);
    c.opts.bypass = bypass;
    c
}

/// A failed step's retry raises gmin tenfold (`Stepper::step`) and puts it back once the hard
/// spot is passed. A quiet junction kept across that change carries the other gmin's conductance
/// and current (`gmin` times the junction's volts), which at a 1 GOhm node moves the node by tens
/// of millivolts: the first load under the raised gmin and the first at the normal one again
/// evaluate it afresh, bypass or not, and the next one keeps it. A reverse-biased diode and a
/// transistor held off, each at a 1 GOhm node from a 5 V rail.
#[test]
fn a_junction_kept_under_the_retry_gmin_is_evaluated_afresh_once_gmin_is_back() {
    use fab_elec_sim::stamp::EvalCx;
    let quiet = |bypass: bool| {
        let mut c = SimCircuit::new();
        c.model("dm", ModelKind::Diode, &[("is", 1e-14), ("cjo", 2e-12)]);
        c.model("qm", ModelKind::Npn, &[("is", 1e-15), ("bf", 200.0), ("cje", 1e-12), ("cjc", 1e-12)]);
        c.v("v1", "rail", "0", Source::dc(5.0));
        c.r("r1", "rail", "k", 1e9);
        c.d("d1", "0", "k", "dm");
        c.r("r2", "rail", "c", 1e9);
        c.r("r3", "b", "0", 1e9);
        c.q("q1", "c", "b", "0", "qm");
        c.opts.bypass = bypass;
        c
    };
    // the operating point, then loads in a transient's Fast iterations: one under the retry's
    // gmin, two at the normal one, each one's stamps
    let loads = |bypass: bool| {
        let mut s = Solver::new(&quiet(bypass)).expect("the circuit builds");
        Stepper::start(&mut s, false, 1e-6).expect("the operating point converges");
        let normal = EvalCx { tran: true, bypass: true, ..s.base_cx() };
        let mut stamps = Vec::new();
        for cx in [EvalCx { gmin: 10.0 * normal.gmin, ..normal.clone() }, normal.clone(), normal] {
            s.load(&cx);
            stamps.push((s.f.clone(), s.g.clone(), bypassed(&s)));
        }
        stamps
    };
    let (on, off) = (loads(true), loads(false));
    let bits = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    for (k, what) in ["the retry's gmin", "gmin back", "gmin back, again"].iter().enumerate() {
        assert_eq!(bits(&on[k].0), bits(&off[k].0), "{what}: the currents with the bypass are those without it");
        assert_eq!(bits(&on[k].1), bits(&off[k].1), "{what}: the conductances with the bypass are those without it");
    }
    assert_eq!(on[1].2, 0, "gmin back: no junction kept from the retry's gmin");
    assert!(on[2].2 > on[1].2, "at an unchanged gmin the quiet junctions are kept: {} bypassed", on[2].2);
}

/// Newton's device test agrees with no value that is not finite: an evaluation that overflowed
/// is never accepted on a solve's line (`Bank::settle`).
#[test]
fn an_evaluation_that_overflowed_agrees_with_nothing() {
    let s = Solver::new(&clipper(true)).expect("the circuit builds");
    let cx = s.base_cx();
    assert!(cx.reltol > 0.0);
    assert!(cx.agrees(1.0, 1.0 + 1e-9, cx.abstol), "a finite value agrees within the tolerance");
    for (fresh, line) in [(f64::INFINITY, 1.0), (1.0, f64::INFINITY), (f64::NEG_INFINITY, -1.0), (f64::INFINITY, f64::INFINITY), (f64::NAN, 1.0), (1.0, f64::NAN)] {
        assert!(!cx.agrees(fresh, line, cx.abstol), "{fresh} agrees with {line}");
    }
}

#[test]
fn exact_never_bypasses_and_fast_keeps_its_tolerance() {
    let t_end = 3e-3;
    let (on, n_on) = run(&clipper(true), 0.0, t_end, "out");
    let (off, _) = run(&clipper(false), 0.0, t_end, "out");
    assert_eq!(on.len(), off.len(), "Exact: the same steps with the bypass allowed");
    for (a, b) in on.iter().zip(&off) {
        assert_eq!((a.0.to_bits(), a.1.to_bits()), (b.0.to_bits(), b.1.to_bits()), "Exact: bit-equal at {:e}", a.0);
    }
    assert_eq!(n_on, 0, "Exact skipped no evaluation");

    let fast_run = |c: &SimCircuit| {
        let mut s = Solver::new(c).expect("builds");
        let out = s.unknown("out").expect("out") as usize;
        let mut st = Stepper::start(&mut s, false, 1e-6).expect("the operating point converges");
        st.set_res(&mut s, SAMPLE);
        let mut fast = Vec::new();
        while st.t() < t_end {
            st.step(&mut s, t_end, &mut |s: &Solver, a: &Accepted| fast.push((a.t, s.x[out]))).expect("the transient runs");
        }
        (fast, s)
    };
    let (fast, s) = fast_run(&clipper(true));
    let mut without = clipper(true);
    without.opts.single_pass = 0;
    let (_, confirming) = fast_run(&without);
    let per_step = |s: &Solver| s.stats.iterations as f64 / s.stats.steps as f64;
    assert!(per_step(&s) < per_step(&confirming) - 0.3, "the single pass saves iterations: {:.2} a step, {:.2} without it", per_step(&s), per_step(&confirming));
    let (plain, _) = run(&clipper(false), SAMPLE, t_end, "out");
    let (vca, _) = run(&clipper(true), SAMPLE, t_end, "vca");
    let (vca_plain, _) = run(&clipper(false), SAMPLE, t_end, "vca");
    // the resting bias diode alone is a quarter of the evaluations
    let evaluations = s.stats.loads * 4;
    assert!(bypassed(&s) * 5 > evaluations, "Fast skipped {} of {evaluations} device evaluations", bypassed(&s));
    assert!(s.banks.iter().any(|b| b.kind() == "b" && b.bypassed() > 0), "Fast bypassed the gain stage at rest (the sine's peaks)");
    // against Fast without the bypass, interpolated at each point
    for (net, with, without) in [("out", &fast, &plain), ("vca", &vca, &vca_plain)] {
        let swing = without.iter().map(|p| p.1.abs()).fold(0.0, f64::max);
        assert!(swing > 0.05, "{net} swings: {swing} V");
        for &(t, v) in with.iter().filter(|p| p.0 > without[0].0) {
            let k = without.partition_point(|p| p.0 < t).min(without.len() - 1).max(1);
            let ((t0, v0), (t1, v1)) = (without[k - 1], without[k]);
            let want = if t1 > t0 { v0 + (v1 - v0) * (t - t0) / (t1 - t0) } else { v1 };
            assert!((v - want).abs() < 2e-3 * swing, "Fast: {net} at {:.1} us is {v:.5} V with the bypass, {want:.5} V without", t * 1e6);
        }
    }
}

/// An op-amp's dominant-pole node held inside its rails by the library's clamp (`uramp` past
/// either rail, 10 S), driven through a transconductance by a 1 kHz sine hard enough to rail it
/// each half cycle.
fn clamped(bypass: bool) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("v1", "in", "0", Source { wave: Wave::Sin { vo: 0.0, va: 10e-3, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }, ..Source::dc(0.0) });
    c.vctl("gm", CtlKind::Vccs, "0", "x", "in", "0", 0.1);
    c.r("r0", "x", "0", 1e6);
    c.c("c0", "x", "0", 16e-9);
    c.b("clamp", BKind::I, "x", "0", Expr::parse("10*(uramp(v(x)-5)-uramp(-5-v(x)))").expect("parses"));
    c.opts.bypass = bypass;
    c
}

#[test]
fn a_clamp_leaves_its_rail_as_its_drive_turns() {
    // On the rail the clamp's line is its 10 S side. Kept past the corner, it held the node a
    // drive / 10 under the rail, well inside the bypass band, so the node never left.
    let t_end = 1.2e-3;
    let (with, kept) = run(&clamped(true), SAMPLE, t_end, "x");
    assert!(kept > 0, "Fast keeps the clamp's line while it rests on a rail");
    let (without, _) = run(&clamped(false), SAMPLE, t_end, "x");
    let first = |p: &[(f64, f64)], after: f64, past: &dyn Fn(f64) -> bool| p.iter().find(|q| q.0 > after && past(q.1)).map(|q| q.0);
    assert!(first(&without, 0.0, &|v| v > 4.99).is_some_and(|t| t < 0.5e-3), "the drive rails the node high in the first half cycle");
    assert!(first(&without, 0.5e-3, &|v| v < -4.99).is_some_and(|t| t < 1.0e-3), "and low in the second");
    for (rail, after, off) in [("high", 0.3e-3, &(|v: f64| v < 4.0) as &dyn Fn(f64) -> bool), ("low", 0.8e-3, &|v: f64| v > -4.0)] {
        let want = first(&without, after, off).expect("the node leaves its rail without the bypass");
        let got = first(&with, after, off);
        assert!(got.is_some_and(|t| (t - want).abs() < 2.5 * SAMPLE), "the node leaves its {rail} rail at {:.1} us with the bypass, {:.1} us without", got.map_or(f64::NAN, |t| t * 1e6), want * 1e6);
    }
}
