//! Averaged regulator Jacobians, continuity and DC load-line/power proofs.

use crate::devcheck::{continuity, jacobian, Lcg};
use crate::linear::run_ok;
use fab_elec_sim::*;

const PORTS: [[&str; 4]; 2] = [["in_a", "ground_a", "out_a", "en_a"], ["in_b", "ground_b", "out_b", "en_b"]];

fn nodes(s: &Solver, ports: [&str; 4]) -> [usize; 4] {
    ports.map(|n| s.unknown(n).unwrap_or_else(|| panic!("no regulator node {n}")) as usize)
}

#[test]
fn t23_device_checks_reg() {
    let mut c = SimCircuit::new();
    c.model("ma", ModelKind::Reg, &[]);
    c.model("mb", ModelKind::Reg, &[
        ("vref", 1.8), ("vdo", 0.12), ("rout", 0.08), ("iq", 75e-6),
        ("ilim", 0.35), ("isink", 2e-3), ("ven", 0.8), ("wen", 0.08), ("w", 0.03),
    ]);
    for (ports, model, load) in [(PORTS[0], "ma", 66.0), (PORTS[1], "mb", 22.0)] {
        let [vin, gnd, vout, en] = ports;
        c.reg(&format!("reg_{model}"), vin, gnd, vout, en, model);
        c.v(&format!("v_{vin}"), vin, gnd, Source::dc(5.0));
        c.v(&format!("v_{en}"), en, gnd, Source::dc(3.0));
        c.v(&format!("v_{gnd}"), gnd, "0", Source::dc(0.0));
        c.r(&format!("load_{model}"), vout, gnd, load);
    }
    let mut bias = |rng: &mut Lcg, s: &Solver, x: &mut [f64]| {
        for ports in PORTS {
            let [vi, g, vo, en] = nodes(s, ports);
            x[g] = rng.range(-0.2, 0.2);
            x[vi] = x[g] + rng.range(0.0, 8.0);
            x[vo] = x[g] + rng.range(-0.5, 6.0);
            x[en] = x[g] + rng.range(-0.5, 3.0);
        }
    };
    let rep = jacobian(&c, 10_000, &mut Lcg(0x23_ae6), &mut bias);
    assert!(rep.g.0 < 1e-6, "regulator G = df/dx: {:.2e} at row/col {:?}, bias {:?}", rep.g.0, rep.g.2, rep.g.1);
    assert_eq!(rep.c.0, 0.0, "regulator has no charge Jacobian");
    assert_eq!(rep.neutral, 0.0, "regulator has no terminal charges");
    // Enable rises before the first regulator leaves dropout; the output parabola
    // crosses its regulated load line and current limit, with a moving ground too.
    let path = |t: f64, s: &Solver, x: &mut [f64], dx: &mut [f64]| {
        for (ports, vref) in [(PORTS[0], 3.3), (PORTS[1], 1.8)] {
            let [vi, g, vo, en] = nodes(s, ports);
            (x[g], dx[g]) = (0.2 * t, 0.2);
            (x[vi], dx[vi]) = (x[g] + 4.0 + 4.0 * t, dx[g] + 4.0);
            (x[vo], dx[vo]) = (x[g] + vref - 0.01 - 2.0 * t * t, dx[g] - 4.0 * t);
            (x[en], dx[en]) = (x[g] + 1.4 + 1.6 * t, dx[g] + 1.6);
        }
    };
    let (worst, at) = continuity(&c, -1.0, 1.0, 4000, 1e-3, &path);
    assert!(worst < 1e-6, "regulator current/slope continuity: {worst:.2e} at t = {at}");
}

/// The regulator is passive at every output bias: its input never draws less than the quiescent
/// current and its ground pin never returns less. The output is forced from the source side
/// through zero current and past the sink limit, on a coarse grid and in steps of a tenth of
/// 1e-3 iq around zero current and of 1e-3 isink around the sink limit (each knee's own scale).
#[test]
fn reg_passive_through_the_sink_limit() {
    let (vi, ve, iq, isink, rout) = (5.0, 3.0, 50e-6, 1e-3, 0.05);
    // the no-load output: the delivered current is zero there (enabled, `on` is 1)
    let vt = load_line(vi, ve, f64::INFINITY);
    let sink = vt + isink * rout;
    // from 10 source widths delivering to 20 times the sink limit's current drawn: the clamp holds
    // the sink current within 1 % there with any width up to the source limit's
    let mut values: Vec<f64> = (-200..=500).map(|k| vt + f64::from(k) * 2e-6).collect();
    values.extend((-40..=40).map(|k| vt + f64::from(k) * 0.25e-9));
    values.extend((-40..=40).map(|k| sink + f64::from(k) * 5e-9));
    values.sort_by(f64::total_cmp);
    let mut c = SimCircuit::new();
    c.opts.reltol = 1e-10;
    c.opts.vntol = 1e-13;
    c.opts.abstol = 1e-15;
    c.model("m", ModelKind::Reg, &[]);
    c.v("vin", "in", "0", Source::dc(vi));
    c.v("ven", "en", "0", Source::dc(ve));
    c.reg("reg", "in", "ground", "out", "en", "m");
    c.v("sense_ground", "ground", "0", Source::dc(0.0));
    c.v("vforce", "out", "0", Source::dc(vt));
    let (sw, _) = run_ok(&c, &Analysis::Dc(vec![DcSweep { device: "vforce".into(), key: "dc".into(), values }]));
    let (io, ii, ig) = (sw.v("vforce#branch"), sw.v("vin#branch"), sw.v("sense_ground#branch"));
    for k in 0..sw.at.len() {
        let (d, io, ii, ig) = (sw.at[k] - vt, io[k], -ii[k], ig[k]);
        assert!(ii >= iq, "input current at least the quiescent current at vo = vt {d:+e} V (delivering {io:e} A): {ii:e} A");
        assert!(ig >= iq, "ground return at least the quiescent current at vo = vt {d:+e} V (delivering {io:e} A): {ig:e} A");
    }
    let (lo, hi) = io.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &i| (lo.min(i), hi.max(i)));
    assert!(lo < -0.99 * isink && hi > 1e-3, "the sweep sources and reaches the sink limit: delivered {lo:e} to {hi:e} A");
}

/// A Newton step across zero delivered current stops `KNEE` (3) zero-knee widths past it, at the
/// width the card's quiescent current scales (1e-3 iq): beyond that knee the input current's
/// linearisation is wrong. Read from the bias the regulator evaluates at, after one limited load.
#[test]
fn reg_step_across_zero_current_stops_past_its_knee() {
    let (vi, ve, iq, rout) = (5.0, 3.0, 50e-6, 0.05);
    // the zero knee's width in headroom (the regulated level less the output)
    let wz = 1e-3 * iq * rout;
    let mut c = SimCircuit::new();
    c.model("m", ModelKind::Reg, &[]);
    c.v("vin", "in", "0", Source::dc(vi));
    c.v("ven", "en", "0", Source::dc(ve));
    c.reg("reg", "in", "0", "out", "en", "m");
    c.r("load", "out", "0", 66.0);
    let mut s = Solver::new(&c).unwrap();
    let [vin, vout, en] = ["in", "out", "en"].map(|n| s.unknown(n).unwrap() as usize);
    // enabled (`on` is 1), out of dropout: the level is smin(vref, vi - vdo, w)
    let (a, b) = (3.3, vi - 0.25);
    let level = 0.5 * (a + b - ((a - b) * (a - b) + 0.02 * 0.02).sqrt());
    // seated at one width of headroom, delivering about iq / 1000
    (s.x[vin], s.x[en], s.x[vout]) = (vi, ve, level - wz);
    let cx = s.base_cx();
    s.reseat(&cx);
    // a step to 1 mV over the level: across zero current and the sink limit (50 uV over)
    s.x[vout] = level + 1e-3;
    let (limited, _) = s.load(&cx);
    let reg = s.banks.iter().find(|b| b.kind() == "Reg").expect("a regulator bank");
    let mut state = Vec::new();
    reg.iter_state(&mut state);
    // its iteration state starts with the bias it evaluated at: [vi, vo, ve]
    let headroom = level - state[1];
    assert!(limited > 0, "a limited step is not converged");
    assert_eq!((state[0], state[2]), (vi, ve), "the input and enable are not limited");
    assert!((headroom + 3.0 * wz).abs() < 1e-3 * wz, "the step stops 3 zero-knee widths past zero current: headroom {headroom:e} V, want {:e} V", -3.0 * wz);
}

/// Independent scalar evaluation of the specified default-card equations, without the
/// bank's derivatives or cancellation-free rearrangements. Returns (io, ii).
fn hand(vi: f64, vo: f64, ve: f64) -> (f64, f64) {
    let smin = |a: f64, b: f64, w: f64| 0.5 * (a + b - ((a - b) * (a - b) + w * w).sqrt());
    let smax = |a: f64, b: f64, w: f64| -smin(-a, -b, w);
    let on = 0.5 * (1.0 + ((ve - 1.0) / 0.05).tanh());
    let vt = on * smin(3.3, vi - 0.25, 0.02);
    let (ilim, isink, iq) = (0.8, 1e-3, 50e-6);
    let (wl, ws, wz) = (1e-3 * ilim, 1e-3 * isink, 1e-3 * iq);
    let io = smax(smin((vt - vo) / 0.05, ilim, wl), -isink * on, ws);
    (io, smax(io, 0.0, wz) + iq * on + 1e-9 * vi)
}

fn load_line(vi: f64, ve: f64, load: f64) -> f64 {
    let residual = |vo: f64| vo / load - hand(vi, vo, ve).0;
    let (mut lo, mut hi) = (-0.5, 6.0);
    assert!(residual(lo) <= 0.0 && residual(hi) >= 0.0, "regulator hand solution brackets its root");
    loop {
        let mid = 0.5 * (lo + hi);
        if mid == lo || mid == hi {
            return mid;
        }
        if residual(mid) > 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
}

#[test]
fn reg_closed_forms() {
    let cases = [("reg", 5.0, 3.0, 66.0), ("dropout", 3.4, 3.0, 66.0), ("limit", 5.0, 3.0, 1.0), ("disabled", 5.0, 0.0, 66.0), ("ideal", 5.0, 3.0, 660.0)];
    let mut c = SimCircuit::new();
    c.opts.reltol = 1e-10;
    c.opts.vntol = 1e-13;
    c.opts.abstol = 1e-15;
    c.model("m", ModelKind::Reg, &[]);
    // Here dropout smoothing is negligible and each clamp tail alone is below 1 nV at 5 mA:
    // the source tail shifts the output about 0.25 nV, the sink tail about 2 pV.
    c.model("near_ideal", ModelKind::Reg, &[("w", 1e-6), ("ilim", 0.01)]);
    for (tag, vi, ve, load) in cases {
        let (vin, vout, en, loaded) = (format!("in_{tag}"), format!("out_{tag}"), format!("en_{tag}"), format!("load_{tag}"));
        c.v(&format!("vin_{tag}"), &vin, "0", Source::dc(vi));
        c.v(&format!("ven_{tag}"), &en, "0", Source::dc(ve));
        c.reg(tag, &vin, "0", &vout, &en, if tag == "ideal" { "near_ideal" } else { "m" });
        c.v(&format!("sense_{tag}"), &vout, &loaded, Source::dc(0.0));
        c.r(&format!("r_{tag}"), &loaded, "0", load);
    }
    let (op, _) = run_ok(&c, &Analysis::Op);
    for (tag, vi, ve, load) in cases {
        let vo = op.last(&format!("out_{tag}"));
        let io = op.last(&format!("sense_{tag}#branch"));
        let ii = -op.last(&format!("vin_{tag}#branch"));
        let want = if tag == "ideal" { 3.3 / (1.0 + 0.05 / load) } else { load_line(vi, ve, load) };
        assert!((vo - want).abs() < 1e-9, "{tag} DC load line: {vo} vs {want}");
        if tag != "ideal" {
            let (want_io, want_ii) = hand(vi, vo, ve);
            assert!((io - want_io).abs() < 1e-9, "{tag} delivered current: {io} vs {want_io}");
            assert!((ii - want_ii).abs() < 1e-9, "{tag} input current: {ii} vs {want_ii}");
        }
        assert!((vo - io * load).abs() < 1e-9, "{tag} load voltage equals delivered current times resistance");
        assert!(op.last(&format!("ven_{tag}#branch")).abs() < 1e-15, "{tag} enable draws no current");
        match tag {
            "reg" => {
                let ideal = 3.3 / (1.0 + 0.05 / 66.0);
                // Default smoothing shifts this ideal value by about 69 uV, so the
                // 1 nV comparison above uses the full equations, including both clamps.
                assert!((vo - ideal).abs() < 7e-5, "regulated output near ideal load drop: {vo} vs {ideal}");
                let supplied = vi * ii;
                let load_power = vo * io;
                let dissipated = supplied - load_power;
                assert!(supplied > 0.0 && load_power > 0.0 && dissipated > 0.0, "positive source/load/regulator power: {supplied}, {load_power}, {dissipated}");
            }
            "limit" => assert!((io - 0.8).abs() < 2.0 * 0.0008, "current limit: {io}"),
            "disabled" => assert!(vo.abs() < 1e-3 && io.abs() < 1e-3 / load, "disabled output/current: {vo}, {io}"),
            _ => {}
        }
    }
}

#[test]
fn reg_no_load_draws_its_quiescent_current() {
    let (vi, iq) = (5.0, 50e-6);
    let mut c = SimCircuit::new();
    c.opts.reltol = 1e-10;
    c.opts.vntol = 1e-13;
    c.opts.abstol = 1e-15;
    c.model("m", ModelKind::Reg, &[]);
    c.v("vin", "in", "0", Source::dc(vi));
    c.reg("reg", "in", "ground", "out", "in", "m");
    c.v("sense_ground", "ground", "0", Source::dc(0.0));
    let (op, _) = run_ok(&c, &Analysis::Op);
    let ii = -op.last("vin#branch");
    let ig = op.last("sense_ground#branch");
    // the zero knee's own width (1e-3 iq) leaves half of it on each side at no load: 5e-4 iq
    let want = iq + 1e-9 * vi;
    assert!((ii - want).abs() < 1e-3 * iq, "no-load input current is the quiescent current plus the input shunt: {ii} vs {want}");
    assert!((ig - ii).abs() < 1e-9, "no-load ground terminal returns the input current: {ig} vs {ii}");
}
