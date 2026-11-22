//! The four controlled sources and the current-controlled switch in textbook circuits, built in
//! Rust and read from a SPICE deck, against their hand-computed operating points and switching
//! times.

use fab_elec_sim::spice::parse;
use fab_elec_sim::{run, Analysis, Control, CtlKind, ModelKind, Record, SimCircuit, Source, TranSpec, Wave};
use std::f64::consts::PI;

/// E as an op-amp of open-loop gain A = 1e5 in a non-inverting stage (R1 = 1 kohm to ground from
/// the inverting input, R2 = 9 kohm from the output to it; beta = 0.1) on 0.1 V; G as a
/// transconductance of 2 mS sensing 0.2 V into a 5 kohm load; F as a current mirror of gain 3 and
/// H as a transresistance of 50 ohm, both sensing the 5 mA a 5 V source drives through 1 kohm into
/// a 0 V sense source, F into a 100 ohm load and H into 1 kohm.
const DECK: &str = "controlled sources
Vin in 0 0.1
E1 out 0 in fb 1e5
R1 fb 0 1k
R2 out fb 9k
Vg gin 0 0.2
G1 gout 0 gin 0 2m
RL1 gout 0 5k
V1 a 0 5
Ra a s 1k
Vsense s 0 0
F1 fout 0 Vsense 3
RL2 fout 0 100
H1 hout 0 Vsense 50
RL3 hout 0 1k
.op
.end
";

fn built() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("vin", "in", "0", Source::dc(0.1));
    c.vctl("e1", CtlKind::Vcvs, "out", "0", "in", "fb", 1e5);
    c.r("r1", "fb", "0", 1e3);
    c.r("r2", "out", "fb", 9e3);
    c.v("vg", "gin", "0", Source::dc(0.2));
    c.vctl("g1", CtlKind::Vccs, "gout", "0", "gin", "0", 2e-3);
    c.r("rl1", "gout", "0", 5e3);
    c.v("v1", "a", "0", Source::dc(5.0));
    c.r("ra", "a", "s", 1e3);
    c.v("vsense", "s", "0", Source::dc(0.0));
    c.ictl("f1", CtlKind::Cccs, "fout", "0", "vsense", 3.0);
    c.r("rl2", "fout", "0", 100.0);
    c.ictl("h1", CtlKind::Ccvs, "hout", "0", "vsense", 50.0);
    c.r("rl3", "hout", "0", 1e3);
    c
}

/// Each source's output against its textbook answer, the same for the circuit built in Rust and
/// the one read from the deck: E's closed-loop gain A / (1 + A beta), G's -gm RL, F's mirrored
/// current through its load (-3 x 5 mA x 100 ohm: a CCCS's current leaves the + node) and H's
/// Rm I (with the SPICE sign: the sensed current flows into the sense source's + node).
#[test]
fn controlled_sources_give_their_textbook_operating_points() {
    let deck = parse(DECK);
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    let (a, beta, vin) = (1e5, 1e3 / (1e3 + 9e3), 0.1);
    let i_sense = 5.0 / 1e3;
    let expected = [
        ("out", a * vin / (1.0 + a * beta), "E: a non-inverting stage's closed-loop gain A / (1 + A beta)"),
        ("gout", -2e-3 * 5e3 * 0.2, "G: -gm RL vin"),
        ("fout", -3.0 * i_sense * 100.0, "F: the mirrored current 3 x 5 mA out of its + node, through 100 ohm"),
        ("hout", 50.0 * i_sense, "H: Rm times the sensed 5 mA"),
    ];
    for (what, circuit) in [("built", built()), ("read", deck.circuit)] {
        let mut record = Record::default();
        run(&circuit, &Analysis::Op, &mut record, &Control::default())
            .unwrap_or_else(|d| panic!("{what}: the operating point converges: {d}"));
        assert!((record.last("s")).abs() < 1e-12, "{what}: the sense source holds its node at 0 V");
        for (node, want, law) in expected {
            let got = record.last(node);
            assert!((got - want).abs() <= 1e-9 * want.abs().max(1.0), "{what}: {law}: {node} = {got}, want {want}");
        }
    }
}

/// W, a current-controlled switch with hysteresis: a 2 mA, 1 kHz sine through the 0 V sense source
/// Vs controls W1 (it = 1 mA, ih = 0.5 mA, 1 ohm closed, 1 Mohm open) from a 1 V, 1 kohm divider's
/// output to ground. It closes across the 1.5 uA band (its default) at it + ih on the rise and
/// opens across the band at it - ih on the fall, so from 0.5 mA to 1.5 mA the rise leaves it open
/// and the fall closed. Away from the band edges every accepted point holds the open or closed
/// divider exactly, and a step is cut where the current enters each band: a point lies on the edge
/// within 0.1 % of the 5 us step plus the linear interpolation's error h^2 omega tan(theta) / 8.
/// A W whose sense source is missing is a located failure.
#[test]
fn a_current_controlled_switch_follows_its_hysteresis() {
    let deck = parse("current switch\nI1 0 c SIN(0 2m 1k)\nVs c 0 0\nV1 s 0 1\nR1 s o 1k\nW1 o 0 Vs wm\n.model wm csw it=1m ih=0.5m ron=1 roff=1meg\n.tran 1u 1m 0 5u\n.end\n");
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    let mut c = SimCircuit::new();
    c.model("wm", ModelKind::Csw, &[("it", 1e-3), ("ih", 0.5e-3), ("ron", 1.0), ("roff", 1e6)]);
    c.i("i1", "0", "c", Source::wave(Wave::Sin { vo: 0.0, va: 2e-3, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }));
    c.v("vs", "c", "0", Source::dc(0.0));
    c.v("v1", "s", "0", Source::dc(1.0));
    c.r("r1", "s", "o", 1e3);
    c.csw("w1", "o", "0", "vs", "wm", false);
    let tran = Analysis::Tran(TranSpec { tstop: 1e-3, max_step: Some(5e-6), uic: false });
    let (amp, omega, band, h) = (2e-3, 2.0 * PI * 1e3, 1.5e-6, 5e-6);
    let (open, closed) = (1e6 / (1e6 + 1e3), 1.0 / (1.0 + 1e3));
    // the band edges in time, and each entry's tolerance at its angle theta
    let rising = |i: f64| (i / amp).asin() / omega;
    let falling = |i: f64| (PI - (i / amp).asin()) / omega;
    let (rise_in, rise_out) = (rising(1.5e-3 - band / 2.0), rising(1.5e-3 + band / 2.0));
    let (fall_in, fall_out) = (falling(0.5e-3 + band / 2.0), falling(0.5e-3 - band / 2.0));
    let tol = |theta: f64| 1e-3 * h + h * h * omega * theta.tan() / 8.0;
    let (rise_tol, fall_tol) = (tol((0.75f64).asin()), tol((0.25f64).asin()));
    for (what, circuit, analysis) in [("built", c, &tran), ("read", deck.circuit, &deck.analyses[0])] {
        let mut record = Record::default();
        run(&circuit, analysis, &mut record, &Control::default()).unwrap_or_else(|d| panic!("{what}: the transient runs: {d}"));
        let (i, o) = (record.v("vs#branch"), record.v("o"));
        for (k, &t) in record.at.iter().enumerate() {
            let want_i = amp * (omega * t).sin();
            assert!((i[k] - want_i).abs() < 1e-12, "{what}: at {t:e} s the sense source carries {} A, the sine {want_i} A", i[k]);
            if t < rise_in - rise_tol || t > fall_out + fall_tol {
                assert!((o[k] - open).abs() < 1e-9, "{what}: at {t:e} s ({} A) the switch is open, the output {}", i[k], o[k]);
            } else if t > rise_out + rise_tol && t < fall_in - fall_tol {
                assert!((o[k] - closed).abs() < 1e-9, "{what}: at {t:e} s ({} A) the switch is closed, the output {}", i[k], o[k]);
            }
        }
        for (edge, tol, how) in [(rise_in, rise_tol, "closing on the rise"), (fall_in, fall_tol, "opening on the fall")] {
            let near = record.at.iter().map(|t| (t - edge).abs()).fold(f64::INFINITY, f64::min);
            assert!(near <= tol, "{what}: a step is cut where the current enters the band {how} at {edge:e} s: the nearest point is {near:e} s off, the tolerance {tol:e}");
        }
    }
    let missing = parse("missing\nV1 s 0 1\nR1 s o 1k\nW1 o 0 Vx wm\n.model wm csw it=1m\n.op\n.end\n");
    let mut record = Record::default();
    let Err(diag) = run(&missing.circuit, &Analysis::Op, &mut record, &Control::default()) else { panic!("a W without its sense source fails") };
    assert!(diag.message.contains("w1: no voltage source or inductor named vx"), "the failure names the switch and its source: {}", diag.message);
}

/// A Schmitt-trigger inverter of two voltage-controlled switches with hysteresis (to ground,
/// closing above 3 V; to the 5 V rail, closing below 2 V; 50 ohm closed, 1 Gohm open), fed back
/// through 100 kohm onto 10 nF: an RC relaxation oscillator. The input swings between the two
/// thresholds, charging towards the rail and discharging towards ground, so the period is
/// 2 R C ln(3 / 2), 811 us. The feedback turns the input back as soon as the output moves, so a
/// switch that blended its conductance with the control across its band would hold there half
/// closed and stop the oscillator: a switch with hysteresis commits once its control enters the
/// band, as the trigger's own regeneration does. Both switches read the input against ground (the
/// rail's through negated thresholds), so their bands meet at the same input and they flip at the
/// same instant.
fn relaxation(ct: f64) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("lo", ModelKind::Sw, &[("vt", 2.5), ("vh", 0.5), ("vband", 1e-4), ("ron", 50.0), ("roff", 1e9)]);
    c.model("hi", ModelKind::Sw, &[("vt", -2.5), ("vh", 0.5), ("vband", 1e-4), ("ron", 50.0), ("roff", 1e9)]);
    // the rail ramps up from 0 V, so the oscillator starts from a defined state
    c.v("v5", "v5", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.0, tr: 10e-6, tf: 10e-6, pw: 1.0, per: 2.0 }));
    c.sw("slo", "o", "0", "i", "0", "lo", false);
    c.sw("shi", "v5", "o", "0", "i", "hi", true);
    c.r("rf", "o", "i", 100e3);
    c.c("ct", "i", "0", ct);
    c.c("co", "o", "0", 10e-12);
    c
}

/// A node's rising crossings of 2.5 V in a run, interpolated.
fn rising_edges(record: &Record, node: &str) -> Vec<f64> {
    let o = record.v(node);
    (1..record.at.len())
        .filter(|&k| o[k - 1] < 2.5 && o[k] >= 2.5)
        .map(|k| record.at[k - 1] + (record.at[k] - record.at[k - 1]) * (2.5 - o[k - 1]) / (o[k] - o[k - 1]))
        .collect()
}

#[test]
fn a_schmitt_trigger_of_two_switches_oscillates_at_its_rc_period() {
    let c = relaxation(10e-9);
    let tran = Analysis::Tran(TranSpec { tstop: 10e-3, max_step: Some(5e-6), uic: false });
    let mut record = Record::default();
    run(&c, &tran, &mut record, &Control::default()).unwrap_or_else(|d| panic!("the oscillator runs: {d}"));
    let i = record.v("i");
    // the output's rising edges through 2.5 V; the first is the rail's own ramp
    let ups = rising_edges(&record, "o");
    assert!(ups.len() >= 11, "the oscillator runs through 10 ms: {} rising edges at {ups:?}", ups.len());
    let period = (ups[ups.len() - 1] - ups[1]) / (ups.len() - 2) as f64;
    let want = 2.0 * 100e3 * 10e-9 * 1.5f64.ln();
    assert!((period / want - 1.0).abs() < 1e-3, "the period is {period:e} s, 2 R C ln(3/2) is {want:e} s");
    for (k, &t) in record.at.iter().enumerate() {
        if t > ups[1] {
            assert!((1.99..=3.01).contains(&i[k]), "at {t:e} s the input {} V has left the thresholds' window", i[k]);
        }
    }
}

/// The oscillator above as Logic mode runs it (A-L1's L2): its switch pair and input network one
/// timing cell (`with_cells`), the input in closed form (its node the cell's alone, set to it). Its period is the
/// switch pair's under Exact to 0.1 %, with the timing capacitor as drawn, halved and doubled, and
/// the closed form needs next to no calibration: its delay a flip is under 1e-4 of a period.
#[test]
fn a_timing_cell_keeps_its_schmitt_oscillators_period() {
    use fab_elec_sim::analysis::logic::with_cells;
    for ct in [10e-9, 5e-9, 20e-9] {
        // the rail stands at 5 V from the start (a cell calibrates at the operating point), the
        // input starts at 0 V
        let mut c = relaxation(ct);
        c.devices.v.iter_mut().for_each(|v| v.src = Source::dc(5.0));
        c.set_ic("i", 0.0);
        let (cells, taken, analog) = with_cells(&c);
        assert_eq!(taken.len(), 1, "the section and its input are one cell (left analog: {analog:?})");
        let cal = &taken[0];
        println!("ct {ct:e} F: calibration: the pair {:e} s, the cell {:e} s, delay {:e} s a flip", cal.period, cal.cell_period, cal.delay);
        assert!(cal.delay.abs() < 1e-4 * cal.period, "ct {ct:e} F: the closed form needs a delay of {:e} s a flip", cal.delay);
        assert!(cells.devices.sw.is_empty(), "the switches leave the solve");
        let i = cells.find_node("i").expect("the input stays a node");
        let mut on = 0;
        cells.devices.for_each_node(|n| on += usize::from(n == i));
        assert_eq!(on, 1, "the input has its cell on it and nothing else");
        let want = 2.0 * 100e3 * ct * 1.5f64.ln();
        let period = |c: &SimCircuit| {
            let tran = Analysis::Tran(TranSpec { tstop: 12.0 * want, max_step: Some(want / 200.0), uic: false });
            let mut record = Record::default();
            run(c, &tran, &mut record, &Control::default()).unwrap_or_else(|d| panic!("the oscillator runs: {d}"));
            let ups = rising_edges(&record, "o");
            assert!(ups.len() >= 11, "{} rising edges at {ups:?}", ups.len());
            (ups[ups.len() - 1] - ups[1]) / (ups.len() - 2) as f64
        };
        let (exact, cell) = (period(&c), period(&cells));
        println!("ct {ct:e} F: cell {cell:e} s, switch pair {exact:e} s, ratio - 1 = {:.3e}", cell / exact - 1.0);
        assert!((cell / exact - 1.0).abs() < 1e-3, "ct {ct:e} F: the cell's period {cell:e} s against the switch pair's {exact:e} s ({want:e} s by hand)");
    }
}
