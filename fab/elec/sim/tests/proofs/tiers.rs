//! The three-tier load (`tiers`, ESIMRT S3). A transient load folds the linear devices (R, C, L,
//! K, E, F, G, H, V and I) and evaluates the others on top:
//! - its `g` and `c` are bit-equal to a whole load's, and `f` and `q` agree within rounding, on a
//!   circuit with every kind of device;
//! - a Newton iteration's factor with it is bit-equal to one without it, whatever moved between
//!   iterations;
//! - a transient with it agrees with one without it within rounding-level differences;
//! - a value edit mid-run is heard at once: the load right after it is bit-equal to a fresh
//!   solver's built with the new value, and the run follows the whole-load reference from there,
//!   in Exact and in Fast.

use fab_elec_sim::analysis::op::operating_point;
use fab_elec_sim::analysis::tran::{Accepted, Stepper};
use fab_elec_sim::expr::Expr;
use fab_elec_sim::newton::{Extras, Integ};
use fab_elec_sim::stamp::{Tier, UnkKind};
use fab_elec_sim::{BKind, CtlKind, GpioParams, ModelKind, PinMode, PinPull, SimCircuit, Solver, Source, Wave};

const PIN: GpioParams = GpioParams { r_on_h: 25.0, r_on_l: 20.0, r_pull: 40e3, r_leak: 1e12, t_edge: 5e-9, vih: 0.55, vil: 0.45 };

fn expr(text: &str) -> Expr {
    Expr::parse(text).unwrap_or_else(|e| panic!("{text} parses: {e}"))
}

/// Every kind of device around one supply: folded ones (R, C, L, K, E, F, G, H, a DC V, a pulsed V,
/// a noise I) and iterating ones (D, Q, B, S, GPIO).
fn mixed() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("dm", ModelKind::Diode, &[("cjo", 2e-12), ("rs", 5.0), ("bv", 6.0)]);
    c.model("npn", ModelKind::Npn, &[("is", 1e-15), ("cje", 2e-12), ("cjc", 1e-12), ("tf", 3e-10)]);
    c.model("swm", ModelKind::Sw, &[("ron", 10.0), ("roff", 1e6), ("vt", 0.5), ("vh", 0.1), ("vband", 0.05)]);
    c.v("vdd", "vdd", "0", Source::dc(5.0));
    c.v("vin", "in", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 1.0, td: 1e-6, tr: 1e-7, tf: 1e-7, pw: 5e-6, per: 1e-5 }));
    c.i("inoise", "nz", "0", Source::wave(Wave::Noise { seed: 7, sigma: 1e-6, dt: 1e-7, base: 1e-7 }));
    c.r("rnz", "nz", "0", 1e3);
    c.r("r1", "in", "a", 1e3);
    c.c("c1", "a", "0", 1e-9);
    c.d("d1", "a", "b", "dm");
    c.r("r2", "b", "0", 2.2e3);
    c.r("rb", "a", "base", 10e3);
    c.q("q1", "col", "base", "0", "npn");
    c.r("rc", "vdd", "col", 4.7e3);
    c.l("l1", "col", "lx", 1e-6);
    c.l("l2", "ly", "0", 1e-6);
    c.k("k1", "l1", "l2", 0.5);
    c.r("rl", "lx", "0", 100.0);
    c.r("rly", "ly", "0", 50.0);
    c.vctl("e1", CtlKind::Vcvs, "e", "0", "a", "0", 2.0);
    c.r("re", "e", "0", 1e3);
    c.vctl("g1", CtlKind::Vccs, "g", "0", "b", "0", 1e-3);
    c.r("rg", "g", "0", 1e3);
    c.ictl("f1", CtlKind::Cccs, "fo", "0", "vdd", 0.01);
    c.r("rf", "fo", "0", 1e3);
    c.ictl("h1", CtlKind::Ccvs, "ho", "0", "vin", 100.0);
    c.r("rh", "ho", "0", 1e3);
    c.b("b1", BKind::I, "bo", "0", expr("1e-3*tanh(v(a))"));
    c.r("rbo", "bo", "0", 1e3);
    c.sw("s1", "vdd", "so", "a", "0", "swm", false);
    c.r("rso", "so", "0", 1e3);
    c.gpio("p1", "pad", "vdd", "0", PIN, PinMode::Input(PinPull::Up));
    c.r("rpad", "pad", "0", 1e5);
    c
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[test]
fn tiered_load_matches_whole_load() {
    // one loads whole
    let mut whole = Solver::new(&mixed()).expect("the mixed circuit builds");
    whole.opts.tiers = false;
    let mut tiered = Solver::new(&mixed()).expect("the mixed circuit builds");
    let tiers: Vec<(&str, Tier)> = tiered.banks.iter().flat_map(|b| (0..b.len()).map(move |i| (b.kind(), b.tier(i)))).collect();
    for (kind, tier) in &tiers {
        let folded = matches!(*kind, "R" | "C" | "L" | "K" | "ctl" | "V" | "I");
        assert_eq!(tier.folded(), folded, "{kind} is folded exactly when it is linear: {tier:?}");
    }
    assert_eq!(tiers.iter().filter(|t| t.1 == Tier::Timed).count(), 2, "the pulsed V and the noise I follow time");
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for round in 0..6 {
        for u in 1..whole.x.len() {
            let v = if whole.unknowns[u].kind == UnkKind::Voltage { 6.0 * rng.next() - 1.0 } else { 1e-3 * (rng.next() - 0.5) };
            whole.x[u] = v;
            tiered.x[u] = v;
        }
        let mut cx = whole.base_cx();
        cx.tran = true;
        // two loads at each time (a Newton iteration's), the pulse's edge and plateau, the noise
        cx.t = (round / 2) as f64 * 1.03e-6;
        cx.limit = round > 0;
        let (a, b) = (whole.load(&cx), tiered.load(&cx));
        assert_eq!(a.0, b.0, "round {round}: the same instances flag non-convergence");
        for s in 1..whole.g.len() {
            let (r, c) = (&whole.unknowns[whole.slot_row[s] as usize].name, &whole.unknowns[whole.slot_col[s] as usize].name);
            assert_eq!(whole.g[s].to_bits(), tiered.g[s].to_bits(), "round {round}: g[{r}, {c}] {:e} against {:e}", whole.g[s], tiered.g[s]);
            assert_eq!(whole.c[s].to_bits(), tiered.c[s].to_bits(), "round {round}: c[{r}, {c}] {:e} against {:e}", whole.c[s], tiered.c[s]);
        }
        // f and q: the folded part is a product against each row's entries, so rounding is
        // measured against the size of the row's terms
        for u in 1..whole.f.len() {
            let mut scale_f = 0.0f64;
            let mut scale_q = 0.0f64;
            for s in 1..whole.g.len() {
                if whole.slot_row[s] as usize == u {
                    let xc = whole.x[whole.slot_col[s] as usize].abs();
                    scale_f += whole.g[s].abs() * xc;
                    scale_q += whole.c[s].abs() * xc;
                }
            }
            let name = &whole.unknowns[u].name;
            let (fa, fb, qa, qb) = (whole.f[u], tiered.f[u], whole.q[u], tiered.q[u]);
            assert!((fa - fb).abs() <= 1e-14 * (scale_f + fa.abs()) + 1e-300, "round {round}: f[{name}] {fa:e} against {fb:e} (terms {scale_f:e})");
            assert!((qa - qb).abs() <= 1e-14 * (scale_q + qa.abs()) + 1e-300, "round {round}: q[{name}] {qa:e} against {qb:e} (terms {scale_q:e})");
        }
    }
    let plan = tiered.tiers.as_ref().expect("a transient load builds the plan");
    println!("{}", plan.report(&tiered.banks));
}

/// The accepted points of a transient from the operating point to `t_end`, with `edit` applied at
/// the first accepted point at or after its time (a `set_param`, then a restart, as live mode does;
/// `whole_after` turns the tiers off there, so the rest runs on whole loads).
fn run(c: &SimCircuit, res: f64, t_end: f64, edit: Option<(f64, &str, f64)>, whole_after: bool) -> (Solver, Vec<(f64, Vec<f64>)>) {
    let mut s = Solver::new(c).expect("the circuit builds");
    let mut st = Stepper::start(&mut s, false, 1e-6).expect("the operating point converges");
    st.set_res(&mut s, res);
    let mut points = Vec::new();
    let mut pending = edit;
    while st.t() < t_end {
        st.step(&mut s, t_end, &mut |s: &Solver, a: &Accepted| points.push((a.t, s.x.clone()))).expect("the transient runs");
        if let Some((at, name, value)) = pending {
            if st.t() >= at {
                assert!(s.set_param(name, "value", value), "{name} takes a value");
                if whole_after {
                    s.opts.tiers = false;
                }
                st.restart(&s);
                pending = None;
            }
        }
    }
    (s, points)
}

/// A divider with a capacitor and a diode clamp, its lower resistor changed mid-run.
fn divider(r2: f64) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("dm", ModelKind::Diode, &[("cjo", 1e-12)]);
    c.v("v1", "in", "0", Source::dc(5.0));
    c.r("r1", "in", "out", 1e3);
    c.r("r2", "out", "0", r2);
    c.c("c1", "out", "0", 10e-9);
    c.d("d1", "out", "clamp", "dm");
    c.r("r3", "clamp", "0", 4.7e3);
    c
}

#[test]
fn a_resistor_edit_mid_run_is_heard_at_once() {
    let (t_edit, t_end) = (40e-6, 120e-6);
    for (mode, res) in [("Exact", 0.0), ("Fast", 4e-6)] {
        let (_, plain) = run(&divider(1e3), res, t_end, None, false);
        let (edited, tiered) = run(&divider(1e3), res, t_end, Some((t_edit, "r2", 4.7e3)), false);
        let (_, whole) = run(&divider(1e3), res, t_end, Some((t_edit, "r2", 4.7e3)), true);
        let out = edited.unknown("out").expect("out") as usize;
        // the same run up to the edit, bit for bit
        let before = tiered.iter().take_while(|p| p.0 < t_edit).count();
        assert!(before > 3, "{mode}: points before the edit");
        for (a, b) in tiered.iter().zip(&plain).take(before) {
            assert_eq!((a.0.to_bits(), a.1[out].to_bits()), (b.0.to_bits(), b.1[out].to_bits()), "{mode}: the runs agree before the edit at {:e}", a.0);
        }
        // after it, the tiered run follows the whole-load reference within rounding
        assert_eq!(tiered.len(), whole.len(), "{mode}: the same steps after the edit");
        for (a, b) in tiered.iter().zip(&whole) {
            assert_eq!(a.0.to_bits(), b.0.to_bits(), "{mode}: the same step times");
            assert!((a.1[out] - b.1[out]).abs() <= 1e-9 * b.1[out].abs() + 1e-12, "{mode}: out at {:e}: {:e} against the whole-load {:e}", a.0, a.1[out], b.1[out]);
        }
        // and it is heard: the edited run settles where a fresh solver with the new value sits,
        // the unedited one where the old value's does (ten time constants on)
        let end = |p: &[(f64, Vec<f64>)]| p.last().expect("points").1[out];
        let rest = |r2: f64| {
            let mut s = Solver::new(&divider(r2)).expect("builds");
            operating_point(&mut s, false).expect("the divider's operating point");
            s.x[out]
        };
        let (old, new, old_rest, new_rest) = (end(&plain), end(&tiered), rest(1e3), rest(4.7e3));
        assert!((old - old_rest).abs() < 1e-3 && (new - new_rest).abs() < 1e-3 && (new - old).abs() > 0.5,
            "{mode}: out settles at {old} unedited (rest {old_rest}), {new} edited (rest {new_rest})");
    }
    // the load right after an edit is a fresh solver's with the new value, bit for bit
    let mut edited = Solver::new(&divider(1e3)).expect("builds");
    let mut fresh = Solver::new(&divider(4.7e3)).expect("builds");
    let mut cx = edited.base_cx();
    cx.tran = true;
    cx.limit = false;
    cx.t = 3e-6;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for u in 1..edited.x.len() {
        let v = 5.0 * rng.next();
        edited.x[u] = v;
        fresh.x[u] = v;
    }
    edited.load(&cx);
    assert!(edited.set_param("r2", "value", 4.7e3), "r2 takes a value");
    edited.load(&cx);
    fresh.load(&cx);
    for (what, a, b) in [("g", &edited.g, &fresh.g), ("c", &edited.c, &fresh.c), ("f", &edited.f, &fresh.f), ("q", &edited.q, &fresh.q)] {
        for (k, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "{what}[{k}] after the edit {x:e} against a fresh solver's {y:e}");
        }
    }
}

/// The matrix a tiered Newton iteration factors is the whole one, bit for bit, whatever moved
/// between iterations: `a0` held, `a0` changed, a new time, a non-transient load between (the
/// operating point's), a value edit. Checked through the factor itself: the same right-hand side
/// solved through both solvers' LUs gives the same bits, so the partial matrix and the refactor of
/// only the dirty columns leave nothing stale.
#[test]
fn tiered_iterations_factor_the_whole_matrix() {
    // one loads whole
    let mut whole = Solver::new(&mixed()).expect("the mixed circuit builds");
    whole.opts.tiers = false;
    let mut tiered = Solver::new(&mixed()).expect("the mixed circuit builds");
    let n1 = whole.x.len();
    let hist = vec![0.0; n1];
    let mut rng = Rng(0x51_7cc1_b727_220a);
    let mut x = vec![0.0; n1];
    let a0s = [1e6, 1e6, 1e6, 2e6, 2e6, 1e6, 1e6, 3e5, 3e5, 3e5, 1e6, 1e6];
    for (k, &a0) in a0s.iter().enumerate() {
        for u in 1..n1 {
            x[u] = if whole.unknowns[u].kind == UnkKind::Voltage { 5.0 * rng.next() } else { 1e-3 * (rng.next() - 0.5) };
        }
        let mut cx = whole.base_cx();
        cx.tran = true;
        cx.t = (k / 3) as f64 * 2.1e-6;
        if k == 5 {
            // an operating-point load between: the folded slots are written whole
            let dc = whole.base_cx();
            whole.load(&dc);
            tiered.load(&dc);
        }
        if k == 9 {
            assert!(whole.set_param("r1", "value", 1.5e3) && tiered.set_param("r1", "value", 1.5e3), "r1 takes a value");
        }
        for s in [&mut whole, &mut tiered] {
            s.x.copy_from_slice(&x);
            let _ = s.newton(&cx, false, Some(Integ { a0, hist: &hist }), &Extras::default(), 1);
        }
        let e: Vec<f64> = (0..n1).map(|i| if i == 0 { 0.0 } else { rng.next() - 0.5 }).collect();
        let (mut ew, mut et) = (e.clone(), e);
        whole.filter_charge_error(a0, &mut ew);
        tiered.filter_charge_error(a0, &mut et);
        for (i, (a, b)) in ew.iter().zip(&et).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "iteration {k} (a0 {a0:e}): the factors solve row {i} to {a:e} and {b:e}");
        }
    }
}

/// The mixed circuit's transient with and without the tiers, Fast and Exact. The residual's
/// rounding differs, so the error estimates do in their last bits and the step sizes part after a
/// few steps; until then the solutions agree to rounding, and the runs take about as many steps
/// and end in the same state.
#[test]
fn tiered_transient_matches_whole_loads() {
    for res in [0.0, 1e-6] {
        let mut reference = mixed();
        reference.opts.tiers = false;
        let (s, a) = run(&mixed(), res, 20e-6, None, false);
        let (_, b) = run(&reference, res, 20e-6, None, false);
        let mut same = 0;
        let mut worst = 0.0f64;
        for (p, q) in a.iter().zip(&b) {
            if p.0 != q.0 {
                break;
            }
            same += 1;
            for (x, y) in p.1.iter().zip(&q.1) {
                worst = worst.max((x - y).abs() / (y.abs() + 1e-6));
            }
        }
        println!("res {res:e}: {} and {} points, the first {same} the same steps, worst relative difference {worst:e}", a.len(), b.len());
        assert!(same >= 5, "res {res:e}: the steps part at point {same}");
        assert!(worst < 1e-6, "res {res:e}: worst relative difference {worst:e} over the first {same} points");
        assert!((a.len() as f64 - b.len() as f64).abs() <= 0.05 * b.len() as f64, "res {res:e}: {} points against {}", a.len(), b.len());
        // the end within the step control's own reach: Exact's tolerance, Fast's coarser one
        let (ea, eb) = (&a.last().expect("points").1, &b.last().expect("points").1);
        let tol = if res == 0.0 { 1e-3 } else { 5e-3 };
        for u in 1..ea.len() {
            if s.unknowns[u].kind == UnkKind::Voltage && s.unknowns[u].name != "nz" {
                assert!((ea[u] - eb[u]).abs() < tol * eb[u].abs().max(1.0), "res {res:e}: {} ends at {:e} against {:e}", s.unknowns[u].name, ea[u], eb[u]);
            }
        }
    }
}
