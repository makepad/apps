//! Behavioural sources (B), resistors, capacitors and inductors in textbook circuits against
//! hand-computed answers, built in Rust and read from SPICE decks: operating points, dividers, a
//! waveform of time, a comparator's crossings, the charge a capacitor and the flux an inductor hold
//! over a cycle, and the located failures of an expression that has no solution or no finite value.

use fab_elec_sim::expr::Expr;
use fab_elec_sim::spice::parse;
use fab_elec_sim::{run, Analysis, BKind, Control, CtlKind, Record, SimCircuit, SimDiag, Source, TranSpec, Wave};
use std::f64::consts::PI;

fn expr(text: &str) -> Expr {
    Expr::parse(text).unwrap_or_else(|e| panic!("{text} parses: {e}"))
}

fn solve(c: &SimCircuit, a: &Analysis) -> Result<Record, SimDiag> {
    let mut record = Record::default();
    run(c, a, &mut record, &Control::default()).map(|_| record)
}

/// The diode's operating point by bisection: (1 V - va) / 1 kohm = 1e-14 (exp(va / 25 mV) - 1).
fn diode_va() -> f64 {
    let f = |v: f64| (1.0 - v) / 1e3 - 1e-14 * ((v / 0.025).exp() - 1.0);
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if f(mid) > 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

const DECK: &str = "behavioural sources
V1 s 0 1
R1 s a 1k
B1 a 0 I=1e-14*(exp(V(a)/0.025)-1)
Vx x 0 2
Vy y 0 3
B2 m 0 V={v(x)*v(y)}
Rm m 0 1k
V3 t 0 5
Rt t u 1k
Vs u 0 0
B3 h 0 V = 100*i(Vs)
Rh h 0 1k
B4 tc 0 v=temper
Rtc tc 0 1k
X1 x y m2 mul k=3
.subckt mul a b o params: k=2
B1 o 0 V='k*v(a)*v(b)'
R1 o 0 1k
.ends mul
.options reltol=1e-9 vntol=1e-12
.op
.end
";

fn built() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.opts.reltol = 1e-9;
    c.opts.vntol = 1e-12;
    c.v("v1", "s", "0", Source::dc(1.0));
    c.r("r1", "s", "a", 1e3);
    c.b("b1", BKind::I, "a", "0", expr("1e-14*(exp(V(a)/0.025)-1)"));
    c.v("vx", "x", "0", Source::dc(2.0));
    c.v("vy", "y", "0", Source::dc(3.0));
    c.b("b2", BKind::V, "m", "0", expr("v(x)*v(y)"));
    c.r("rm", "m", "0", 1e3);
    c.v("v3", "t", "0", Source::dc(5.0));
    c.r("rt", "t", "u", 1e3);
    c.v("vs", "u", "0", Source::dc(0.0));
    c.b("b3", BKind::V, "h", "0", expr("100*i(vs)"));
    c.r("rh", "h", "0", 1e3);
    c.b("b4", BKind::V, "tc", "0", expr("temper"));
    c.r("rtc", "tc", "0", 1e3);
    c.b("x1.b1", BKind::V, "m2", "0", expr("3*v(x)*v(y)"));
    c.r("x1.r1", "m2", "0", 1e3);
    c
}

/// An exponential diode as `I=` against its bisected equation, a multiplier, a transresistance of
/// a sensed branch current, the temperature, and a subcircuit's multiplier with its own parameter:
/// the same answers built in Rust and read from a deck (bare, braced, quoted and spaced forms).
#[test]
fn behavioural_sources_give_their_textbook_operating_points() {
    let deck = parse(DECK);
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    let expected = [
        ("a", diode_va(), "I=: the diode-resistor equation, bisected"),
        ("m", 6.0, "V=: the multiplier v(x) v(y) = 2 x 3"),
        ("h", 0.5, "V=: 100 ohm times the 5 mA a 0 V source senses"),
        ("tc", 27.0, "temper is the circuit temperature in degrees C"),
        ("m2", 18.0, "a subcircuit's own k = 3 and its renamed nodes"),
    ];
    for (what, circuit) in [("built", built()), ("read", deck.circuit)] {
        let record = solve(&circuit, &Analysis::Op).unwrap_or_else(|d| panic!("{what}: the operating point converges: {d:?}"));
        for (node, want, law) in expected {
            let got = record.last(node);
            assert!((got - want).abs() <= 1e-9 * want.abs().max(1.0), "{what}: {law}: {node} = {got}, want {want}");
        }
        let i_diode = (1.0 - record.last("a")) / 1e3;
        assert!(i_diode > 3e-4, "{what}: the diode conducts: {i_diode}");
    }
}

/// Behavioural resistors against their dividers: 1 kohm (1 + v(o)) from 5 V into 2 kohm reads its
/// own node, o (1k (1 + o) + 2k) = 10k, so o^2 + 3 o - 10 = 0 and o = 2 V (R = 3 kohm); 1 kohm +
/// 1 kohm v(c) on a 2 V control is 3 kohm and draws 1 mA from 3 V. Built in Rust and read from a
/// deck, where a constant brace value stays an ideal resistor.
#[test]
fn behavioural_resistors_give_their_dividers() {
    let deck = parse("resistors\n.param base=1k\nV1 s 0 5\nR1 s o {base*(1+v(o))}\nR2 o 0 {2*base}\nVc c 0 2\nV3 t 0 3\nR3 t 0 '1k + 1k*v(c)'\n.op\n.end\n");
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    assert_eq!(deck.circuit.devices.r.len(), 1, "the constant brace value stays an ideal resistor");
    assert_eq!(deck.circuit.devices.b.len(), 2, "the two values that read the circuit are behavioural");
    let mut c = SimCircuit::new();
    c.v("v1", "s", "0", Source::dc(5.0));
    c.b("r1", BKind::R, "s", "o", expr("1k*(1+v(o))"));
    c.r("r2", "o", "0", 2e3);
    c.v("vc", "c", "0", Source::dc(2.0));
    c.v("v3", "t", "0", Source::dc(3.0));
    c.b("r3", BKind::R, "t", "0", expr("1k + 1k*v(c)"));
    for (what, circuit) in [("built", c), ("read", deck.circuit)] {
        let record = solve(&circuit, &Analysis::Op).unwrap_or_else(|d| panic!("{what}: the operating point converges: {d:?}"));
        let o = record.last("o");
        assert!((o - 2.0).abs() < 1e-9, "{what}: the self-reading divider's root of o^2 + 3o - 10: {o}");
        let i3 = record.last("v3#branch");
        assert!((i3 + 1e-3).abs() < 1e-12, "{what}: 3 V across 3 kohm draws 1 mA: {i3}");
    }
}

/// Constant behavioural values are the ideal elements: a 1 V step through 1 kohm charges 1 uF to
/// 1 - exp(-t / RC) volts and builds 1 H's current to (1 - exp(-t R / L)) mA, as the ideal
/// capacitor and inductor do, at the same accepted times.
#[test]
fn constant_behavioural_values_are_the_ideal_elements() {
    let step = Source::wave(Wave::Pulse { v1: 0.0, v2: 1.0, td: 0.0, tr: 1e-9, tf: 1e-9, pw: 1.0, per: 2.0 });
    let tran = Analysis::Tran(TranSpec { tstop: 5e-3, max_step: Some(20e-6), uic: false });
    // the capacitor's voltage in volts, the inductor's current in milliamps
    for (kind, unknown, scale) in [(BKind::C, "o", 1.0), (BKind::L, "x1#branch", 1e3)] {
        let mut runs = Vec::new();
        for behavioural in [false, true] {
            let mut c = SimCircuit::new();
            c.v("v1", "s", "0", step.clone());
            c.r("r1", "s", "o", 1e3);
            match (behavioural, kind) {
                (true, BKind::C) => c.b("x1", kind, "o", "0", expr("1u")),
                (true, _) => c.b("x1", kind, "o", "0", expr("1")),
                (false, BKind::C) => c.c("x1", "o", "0", 1e-6),
                (false, _) => c.l("x1", "o", "0", 1.0),
            }
            runs.push(solve(&c, &tran).unwrap_or_else(|d| panic!("{kind:?}: the transient runs: {d:?}")));
        }
        let (ideal, behav) = (&runs[0], &runs[1]);
        assert_eq!(behav.at, ideal.at, "{kind:?}: the same accepted times as the ideal element");
        let mut worst = 0.0_f64;
        for ((t, b), i) in behav.at.iter().zip(behav.v(unknown)).zip(ideal.v(unknown)) {
            worst = worst.max((scale * b - (1.0 - (-t / 1e-3).exp())).abs());
            assert!((b - i).abs() < 1e-9 / scale, "{kind:?}: at {t:e} s the behavioural element gives {b}, the ideal one {i}");
        }
        assert!(worst < 1e-3, "{kind:?}: 1 - exp(-t / 1 ms): worst error {worst:e}");
    }
}

/// Behavioural capacitors hold the charge q = C v (charge form). A charge meter (the current of a
/// 0 V sense source mirrored into an ideal 1 uF) holds the charge the source delivered:
/// - C0 (1 + v(o)) on a ramp to 1 V, a hold and a ramp back to 0: the meter reads C(v) v at every
///   accepted point, so 2 C0 at 1 V (integrating C dv would give 1.5 C0), and after the cycle the
///   net charge is back to zero;
/// - C0 (1 + 0.5 sin(2 pi 1k time)) held at 1 V: a capacitance that changes in time moves the
///   charge C(t) v - C(0) v = 0.5 C0 sin(2 pi 1k t) at a fixed voltage.
/// Built in Rust and read from a deck, where a constant brace value stays an ideal capacitor.
#[test]
fn a_behavioural_capacitor_holds_its_charge_over_a_cycle() {
    let deck = parse(
        "charge cycle\n.param c0=1u\nV1 s 0 PWL(0 0 1m 1 2m 1 3m 0 4m 0)\nVm s o 0\nC1 o 0 {c0*(1+v(o))}\nF1 0 m Vm 1\nCm m 0 {c0}\nRm m 0 1e15\n\
         V2 s2 0 1\nVm2 s2 o2 0\nC2 o2 0 {c0*(1+0.5*sin(2*pi*1k*time))}\nF2 0 m2 Vm2 1\nCm2 m2 0 1u\nRm2 m2 0 1e15\n.tran 10u 4m 0 10u\n.end\n",
    );
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    assert_eq!(deck.circuit.devices.c.len(), 2, "the constant brace value stays an ideal capacitor");
    assert_eq!(deck.circuit.devices.b.len(), 2, "the two values that read the circuit are behavioural");
    let mut c = SimCircuit::new();
    c.v("v1", "s", "0", Source::wave(Wave::Pwl(vec![(0.0, 0.0), (1e-3, 1.0), (2e-3, 1.0), (3e-3, 0.0), (4e-3, 0.0)])));
    c.v("v2", "s2", "0", Source::dc(1.0));
    for (sense, src, node, meter, cap) in [("vm", "s", "o", "m", "1u*(1+v(o))"), ("vm2", "s2", "o2", "m2", "1u*(1+0.5*sin(2*pi*1k*time))")] {
        c.v(sense, src, node, Source::dc(0.0));
        c.b(&format!("c{}", &sense[2..]), BKind::C, node, "0", expr(cap));
        c.ictl(&format!("f{}", &sense[2..]), CtlKind::Cccs, "0", meter, sense, 1.0);
        c.c(&format!("c{meter}"), meter, "0", 1e-6);
        c.r(&format!("r{meter}"), meter, "0", 1e15);
    }
    let tran = Analysis::Tran(TranSpec { tstop: 4e-3, max_step: Some(10e-6), uic: false });
    for (what, circuit, analysis) in [("built", c, &tran), ("read", deck.circuit, &deck.analyses[0])] {
        let record = solve(&circuit, analysis).unwrap_or_else(|d| panic!("{what}: the transient runs: {d:?}"));
        assert!(record.at.len() > 300, "{what}: the transient keeps its 10 us steps: {} points", record.at.len());
        let (o, m, m2) = (record.v("o"), record.v("m"), record.v("m2"));
        for (k, t) in record.at.iter().enumerate() {
            // the meter's 1 uF holds the delivered charge, in volts per microcoulomb
            let q = (1.0 + o[k]) * o[k];
            let q2 = 0.5 * (2.0 * PI * 1e3 * t).sin();
            assert!((m[k] - q).abs() < 1e-9, "{what}: at {t:e} s the delivered charge is {} uC, C(v) v gives {q} uC at {} V", m[k], o[k]);
            assert!((m2[k] - q2).abs() < 1e-9, "{what}: at {t:e} s the time-varying capacitance moved {} uC, (C(t) - C(0)) v gives {q2} uC", m2[k]);
        }
        let full = record.at_time("m", 2e-3);
        assert!((full - 2.0).abs() < 1e-9, "{what}: charged to 1 V the source delivered {full} uC: C(1) 1 = 2 C0 (not the 1.5 C0 of integrating C dv)");
        let net = record.last("m");
        assert!(net.abs() < 1e-9, "{what}: after the charge-discharge cycle the net charge is {net} uC, not zero");
    }
}

/// Behavioural inductors hold the flux L i (flux form). A flux meter (the inductor's voltage
/// mirrored by 1 mS into an ideal 1 uF, so 1 V per mWb) reads the flux the inductor took: L0 (1 +
/// i(l1)^2), a saturating core of its own current, driven by a ramp to 1 A, a hold and a ramp
/// back, holds L(i) i at every accepted point, so 2 L0 at 1 A (integrating L di would give 4/3
/// L0), and after the cycle the net flux is back to zero. Built in Rust and read from a deck, where
/// a constant brace value stays an ideal inductor.
#[test]
fn a_behavioural_inductor_holds_its_flux_over_a_cycle() {
    let deck = parse(
        "flux cycle\n.param l0=1m\nI1 0 n PWL(0 0 1m 1 2m 1 3m 0 4m 0)\nL1 n 0 {l0*(1+i(l1)^2)}\nG1 0 m n 0 1m\nCm m 0 1u\nRm m 0 1e15\n\
         L2 n2 0 {l0}\nR2 n2 0 1\n.tran 10u 4m 0 10u\n.end\n",
    );
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    assert_eq!(deck.circuit.devices.l.len(), 1, "the constant brace value stays an ideal inductor");
    assert_eq!(deck.circuit.devices.b.len(), 1, "the value that reads the circuit is behavioural");
    let mut c = SimCircuit::new();
    c.i("i1", "0", "n", Source::wave(Wave::Pwl(vec![(0.0, 0.0), (1e-3, 1.0), (2e-3, 1.0), (3e-3, 0.0), (4e-3, 0.0)])));
    c.b("l1", BKind::L, "n", "0", expr("1m*(1+i(l1)^2)"));
    c.vctl("g1", CtlKind::Vccs, "0", "m", "n", "0", 1e-3);
    c.c("cm", "m", "0", 1e-6);
    c.r("rm", "m", "0", 1e15);
    let tran = Analysis::Tran(TranSpec { tstop: 4e-3, max_step: Some(10e-6), uic: false });
    for (what, circuit, analysis) in [("built", c, &tran), ("read", deck.circuit, &deck.analyses[0])] {
        let record = solve(&circuit, analysis).unwrap_or_else(|d| panic!("{what}: the transient runs: {d:?}"));
        assert!(record.at.len() > 300, "{what}: the transient keeps its 10 us steps: {} points", record.at.len());
        let (i, m) = (record.v("l1#branch"), record.v("m"));
        for (k, t) in record.at.iter().enumerate() {
            let flux = (1.0 + i[k] * i[k]) * i[k];
            assert!((m[k] - flux).abs() < 1e-9, "{what}: at {t:e} s the inductor took {} mWb, L(i) i gives {flux} mWb at {} A", m[k], i[k]);
        }
        let full = record.at_time("m", 2e-3);
        assert!((full - 2.0).abs() < 1e-9, "{what}: at 1 A the inductor holds {full} mWb: L(1) 1 = 2 L0 (not the 4/3 L0 of integrating L di)");
        let net = record.last("m");
        assert!(net.abs() < 1e-9, "{what}: after the cycle the net flux is {net} mWb, not zero");
    }
}

/// `V=sin(2 pi 1k time)` holds its output on the analytic sine at every accepted time.
#[test]
fn a_behavioural_sine_follows_time_exactly() {
    let mut c = SimCircuit::new();
    c.b("b1", BKind::V, "o", "0", expr("sin(2*pi*1k*time)"));
    c.r("r1", "o", "0", 1e3);
    let record = solve(&c, &Analysis::Tran(TranSpec { tstop: 1e-3, max_step: Some(10e-6), uic: false })).expect("the transient runs");
    assert!(record.at.len() > 100, "the transient keeps its 10 us steps: {} points", record.at.len());
    for (t, o) in record.at.iter().zip(record.v("o")) {
        let want = (2.0 * PI * 1e3 * t).sin();
        assert!((o - want).abs() < 1e-12, "at {t:e} s the output is {o}, sin gives {want}");
    }
}

/// A comparator on a 1 kHz, 1 V sine: `V = v(in) > 0.5 ? 5 : 0`. Every accepted point holds the
/// branch its own input chooses (never a stale one), and the steps are cut to each crossing: the
/// analytic time (asin(0.5) / omega rising, (pi - asin(0.5)) / omega falling) lies inside the
/// switching step, and an end of that step lies on it within the 0.1 % of the step the stepper
/// accepts plus the linear interpolation's error, h^2 omega / (8 cos 30 deg). A cut aims a hair
/// past the interpolated crossing, so the step usually ends there; where the interpolation falls
/// short (the falling edge, on the sine's concave side) the cut step ends a hair before it and the
/// stepper restarts there, so the step starts on it.
#[test]
fn a_comparator_switches_at_its_crossing_and_never_on_a_stale_branch() {
    let deck = parse("comparator\nVin in 0 SIN(0 1 1k)\nB1 out 0 V=v(in) > 0.5 ? 5 : 0\nRl out 0 1k\n.tran 1u 1m 0 20u\n.end\n");
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    let record = solve(&deck.circuit, &deck.analyses[0]).expect("the transient runs");
    let (vin, out) = (record.v("in"), record.v("out"));
    for ((t, i), o) in record.at.iter().zip(&vin).zip(&out) {
        let want = if *i > 0.5 { 5.0 } else { 0.0 };
        assert_eq!(*o, want, "at {t:e} s the input {i} chooses {want}, the output is {o}");
    }
    let omega = 2.0 * PI * 1e3;
    let crossings = [(PI / 6.0) / omega, (PI - PI / 6.0) / omega];
    let switches: Vec<(f64, f64)> = (1..out.len()).filter(|&k| out[k] != out[k - 1]).map(|k| (record.at[k - 1], record.at[k])).collect();
    assert_eq!(switches.len(), 2, "one rise and one fall in the first millisecond: {switches:?}");
    for ((ta, tb), want) in switches.into_iter().zip(crossings) {
        let h = tb - ta;
        let tolerance = 1e-3 * h + h * h * omega / (8.0 * (PI / 6.0).cos());
        assert!(ta < want && want <= tb, "the step [{ta:e}, {tb:e}] contains the crossing at {want:e}");
        let off = (want - ta).min(tb - want);
        assert!(off <= tolerance, "the step [{ta:e}, {tb:e}] has no end on the crossing at {want:e}: the nearer is {off:e} s off, the tolerance {tolerance:e}");
    }
}

/// An expression with no solution (`v(out) > 0.5 ? 0 : 1` driving its own input) flips its branch
/// every iteration and never converges; one with no finite value (`1/v(a)` at 0 V, a resistance of
/// 0) never converges either; one that reads a branch nobody has fails at setup, as a coupling of a
/// behavioural inductor does. Each failure names the element and its expression.
#[test]
fn an_expression_without_a_solution_is_a_located_failure() {
    let cases = [
        ("flip\nB1 out 0 V=v(out) > 0.5 ? 0 : 1\nR1 out 0 1k\n.end\n", "b1", "v(out) > 0.5 ? 0 : 1 changed its branch"),
        ("infinite\nV1 a 0 0\nB2 o 0 V=1/v(a)\nR1 o 0 1k\n.end\n", "b2", "1/v(a) is not finite"),
        ("unread\nB3 o 0 I=2*i(nosuch)\nR1 o 0 1k\n.end\n", "b3", "no voltage source or inductor named nosuch"),
        ("zero\nVc c 0 0\nV1 s 0 1\nR4 s 0 {v(c)}\n.end\n", "r4", "R=v(c) is not finite"),
        ("coupled\nI1 0 a 1m\nL1 a 0 {1m*(1+i(l1)^2)}\nL2 b 0 1m\nR2 b 0 1\nK1 L1 L2 0.5\n.end\n", "k1", "l1 is a behavioural inductor"),
    ];
    for (text, source, why) in cases {
        let deck = parse(text);
        assert!(deck.diags.is_empty(), "{source}: the deck reads cleanly: {:?}", deck.diags);
        let Err(diag) = solve(&deck.circuit, &Analysis::Op) else { panic!("{source}: the operating point fails") };
        assert!(diag.message.contains(source) && diag.message.contains(why), "{source}: the failure names it and says {why:?}: {}", diag.message);
    }
}
