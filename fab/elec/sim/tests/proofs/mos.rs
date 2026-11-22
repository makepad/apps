//! Level-1 MOS device checks (T24), square-law operating points and a CMOS inverter (T12), and
//! the level-1 card and instance parameters.

use crate::devcheck::{charge_cycle, continuity, eval, exact_cx, jacobian, Lcg};
use crate::linear::run_ok;
use fab_elec_sim::*;

const DEVICES: [(&str, f64); 2] = [("mn", 1.0), ("mp", -1.0)];

fn full_circuit(explicit_caps: bool) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.temp_c = 47.0;
    for (name, kind, p) in [("nm", ModelKind::Nmos, 1.0), ("pm", ModelKind::Pmos, -1.0)] {
        let mut params = vec![
            ("vto", p * 0.65), ("kp", 8e-5), ("gamma", 0.48), ("phi", 0.7), ("lambda", 0.035),
            ("rd", 14.0), ("rs", 9.0), ("is", 2e-14), ("pb", 0.78),
            ("cgso", 1.3e-10), ("cgdo", 1.7e-10), ("cgbo", 0.9e-10),
            ("cj", 1.7e-3), ("mj", 0.37), ("cjsw", 2.3e-10), ("mjsw", 0.28),
            ("tox", 1.8e-8), ("fc", 0.42), ("tnom", 25.0),
        ];
        if explicit_caps {
            params.extend_from_slice(&[("cbd", 3.3e-14), ("cbs", 2.1e-14)]);
        }
        c.model(name, kind, &params);
    }
    for (((name, _), model), (w, l)) in DEVICES.into_iter().zip(["nm", "pm"]).zip([(12e-6, 1.1e-6), (18e-6, 1.3e-6)]) {
        c.m(name, &format!("{name}_d"), &format!("{name}_g"), &format!("{name}_s"), &format!("{name}_b"), model, w, l);
    }
    for (inst, scale) in c.devices.m.iter_mut().zip([0.8, 1.4]) {
        inst.ad = scale * 9e-12;
        inst.as_ = scale * 7e-12;
        inst.pd = scale * 14e-6;
        inst.ps = scale * 12e-6;
    }
    c
}

/// External d, g, s, b and internal d', s'. Both full models have both series resistors.
fn nodes(s: &Solver, name: &str) -> [usize; 6] {
    [format!("{name}_d"), format!("{name}_g"), format!("{name}_s"), format!("{name}_b"), format!("{name}#d"), format!("{name}#s")]
        .map(|n| s.unknown(&n).unwrap_or_else(|| panic!("MOS topology contains {n}")) as usize)
}

fn bias(rng: &mut Lcg, s: &Solver, x: &mut [f64]) {
    for (name, p) in DEVICES {
        let [d, g, source, b, di, si] = nodes(s, name);
        let mode = (4.0 * rng.next()) as u8;
        let vbs = rng.range(-2.5, 0.4);
        let vth = 0.65 + 0.48 * ((0.7 - vbs).sqrt() - 0.7_f64.sqrt());
        let vgt = if mode == 0 { rng.range(-2.0, -0.15) } else { rng.range(0.4, 2.5) };
        let vds = match mode {
            1 => rng.range(0.1, 0.85) * vgt,
            2 => vgt + rng.range(0.2, 2.5),
            _ => rng.range(0.1, 3.0),
        };
        let (fd, fs) = if mode == 3 { (si, di) } else { (di, si) };
        x[fs] = rng.range(-0.8, 0.8);
        x[fd] = x[fs] + p * vds;
        x[g] = x[fs] + p * (vth + vgt);
        x[b] = x[fs] + p * vbs;
        x[d] = x[di] + rng.range(-0.08, 0.08);
        x[source] = x[si] + rng.range(-0.08, 0.08);
    }
}

#[test]
fn t24_device_checks_mos() {
    let mut rng = Lcg(0x405fe7);
    // Exercise both explicit bulk capacitances and the area/perimeter fallback, for both
    // polarities at a non-nominal temperature. Keep random points away from region corners;
    // the boundary checks below probe those directly.
    for explicit_caps in [false, true] {
        let c = full_circuit(explicit_caps);
        let rep = jacobian(&c, 1600, &mut rng, &mut bias);
        assert!(rep.g.0 < 1e-6, "MOS G = df/dx (explicit caps {explicit_caps}): {:.2e} at {:?}, bias {:?}", rep.g.0, rep.g.2, rep.g.1);
        assert!(rep.c.0 < 1e-6, "MOS C = dq/dx (explicit caps {explicit_caps}): {:.2e} at {:?}, bias {:?}", rep.c.0, rep.c.2, rep.c.1);
        assert!(rep.neutral < 1e-12, "MOS terminal charge neutrality (explicit caps {explicit_caps}): {:.2e}", rep.neutral);
        let cycle = charge_cycle(&c, &mut rng, &mut bias);
        assert!(cycle == 0.0, "MOS charge cycle closes bit for bit (explicit caps {explicit_caps}): {cycle:.2e}");

        let mut solver = Solver::new(&c).unwrap_or_else(|d| panic!("MOS full model setup: {d}"));
        let mut x = vec![0.0; solver.n + 1];
        bias(&mut rng, &solver, &mut x);
        let (f, q, g, cm) = eval(&mut solver, &x);
        assert!(f.iter().chain(&q).chain(g.iter().flatten()).chain(cm.iter().flatten()).all(|v| v.is_finite()), "MOS currents, charges and Jacobians are finite");
        let dot: Vec<f64> = (0..x.len()).map(|_| rng.range(-1e6, 1e6)).collect();
        let cx = exact_cx(&solver);
        for (name, _) in DEVICES {
            let [d, gate, source, b, _, _] = nodes(&solver, name);
            let (bank, inst) = solver.find_device(name).expect("MOS bank exposes its instance");
            let mut pins = [f64::NAN; 4];
            solver.banks[bank].terminal_currents(inst, &x, &dot, &cx, &mut pins);
            for (pin, u) in [d, gate, source, b].into_iter().enumerate() {
                let want = f[u] + cm[u].iter().zip(&dot).map(|(c, dv)| c * dv).sum::<f64>();
                assert!((pins[pin] - want).abs() < 1e-12 * want.abs().max(1e-18), "MOS {name} terminal {pin} current agrees with stamped conduction and charge rate: {} vs {want}", pins[pin]);
            }
        }
    }

    let c = full_circuit(false);
    let mut smooth = c.clone();
    // continuity also demands continuous charge slopes. Intrinsic threshold capacitances
    // intentionally jump, so use the charge-free channel for the C1 CURRENT proof.
    for m in &mut smooth.models {
        m.params.retain(|(key, _)| key != "tox");
    }
    for reverse in [false, true] {
        let path = |t: f64, solver: &Solver, x: &mut [f64], dx: &mut [f64]| {
            let vth = 0.65 + 0.48 * (1.5_f64.sqrt() - 0.7_f64.sqrt());
            for (k, (name, p)) in DEVICES.into_iter().enumerate() {
                let [d, g, source, b, di, si] = nodes(solver, name);
                let (fd, fs) = if reverse { (si, di) } else { (di, si) };
                x[fs] = 0.2 * k as f64;
                x[fd] = x[fs] + p * 0.5;
                x[g] = x[fs] + p * (vth + t);
                dx[g] = p;
                x[b] = x[fs] - p * 0.8;
                x[d] = x[di] + 0.01;
                x[source] = x[si] - 0.01;
            }
        };
        let (worst, at) = continuity(&smooth, -0.3, 0.9, 600, 1e-3, &path);
        assert!(worst < 1e-6, "MOS current and first derivative continuous at threshold and linear/saturation (reverse {reverse}): {worst:.2e} at {at}");
    }

    // One-sided tangent extrapolations check charge VALUE continuity while permitting the
    // specified capacitance jumps. Include accumulation and drain/source reversal too.
    let mut solver = Solver::new(&c).unwrap_or_else(|d| panic!("MOS charge boundary setup: {d}"));
    let vth = 0.65 + 0.48 * (1.5_f64.sqrt() - 0.7_f64.sqrt());
    let vfb = 0.65 - 0.7 - 0.48 * 0.7_f64.sqrt();
    for (label, vgs, vds, gate_sweep) in [
        ("accumulation", vfb - 0.8, 0.5, true),
        ("threshold", vth, 0.5, true),
        ("linear/saturation", vth + 0.5, 0.5, true),
        ("mode reversal", vth + 1.0, 0.0, false),
    ] {
        let h = 1e-7;
        let mut ends = Vec::new();
        for t in [-h, h] {
            let mut x = vec![0.0; solver.n + 1];
            let mut dx = vec![0.0; solver.n + 1];
            for (name, p) in DEVICES {
                let [d, g, source, b, di, si] = nodes(&solver, name);
                x[di] = p * (vds + if gate_sweep { 0.0 } else { t });
                x[g] = p * (vgs + if gate_sweep { t } else { 0.0 });
                x[b] = -p * 0.8;
                x[d] = x[di];
                x[source] = x[si];
                dx[if gate_sweep { g } else { di }] = p;
                if !gate_sweep { dx[d] = p; }
            }
            let (_, q, _, cm) = eval(&mut solver, &x);
            let tangent: Vec<f64> = q.iter().zip(&cm).map(|(q, row)| q - t * row.iter().zip(&dx).map(|(c, dv)| c * dv).sum::<f64>()).collect();
            ends.push(tangent);
        }
        let scale = ends.iter().flatten().fold(1e-30_f64, |a, q| a.max(q.abs()));
        let error = ends[0].iter().zip(&ends[1]).map(|(a, b)| (a - b).abs()).fold(0.0_f64, f64::max);
        assert!(error < 1e-10 * scale, "MOS intrinsic charge value continuous at {label}: {error:e}, scale {scale:e}");
    }

    // The body-root clamp has zero body derivative away from its corner, in both modes.
    let mut clamped = |rng: &mut Lcg, solver: &Solver, x: &mut [f64]| {
        bias(rng, solver, x);
        for (name, p) in DEVICES {
            let [_, g, _, b, di, si] = nodes(solver, name);
            let fs = if p * (x[di] - x[si]) < 0.0 { di } else { si };
            x[b] = x[fs] + p * 0.9;
            x[g] = x[fs] + p * 2.0;
        }
    };
    let rep = jacobian(&c, 32, &mut rng, &mut clamped);
    assert!(rep.g.0 < 1e-6 && rep.c.0 < 1e-6, "MOS clamped body-root analytic derivatives: G {:.2e}, C {:.2e}", rep.g.0, rep.c.0);
}

fn rel(got: f64, want: f64) -> f64 {
    (got - want).abs() / want.abs().max(1e-300)
}

fn inverter(input: Source) -> SimCircuit {
    let mut c = SimCircuit::new();
    // With lambda = 0, both channels saturate at the switching point and only gmin
    // fixes the output. Do not demand voltage steps below current-roundoff / gmin there.
    c.opts.reltol = 1e-7;
    c.opts.vntol = 1e-9;
    c.opts.abstol = 1e-16;
    c.model("nm", ModelKind::Nmos, &[("vto", 0.7), ("kp", 1e-4), ("tox", 2e-8)]);
    c.model("pm", ModelKind::Pmos, &[("vto", -0.7), ("kp", 4e-5), ("tox", 2e-8)]);
    c.v("vdd", "vdd", "0", Source::dc(3.3));
    c.v("vin", "in", "0", input);
    c.m("mn", "out", "in", "0", "0", "nm", 10e-6, 1e-6);
    c.m("mp", "out", "in", "vdd", "vdd", "pm", 25e-6, 1e-6);
    c.c("load", "out", "0", 50e-15);
    c
}

/// Integrate the delivered supply current, clipping and interpolating at period boundaries.
fn supply_charge(r: &Record, start: f64, end: f64) -> f64 {
    let column = r.index("vdd#branch");
    r.at.windows(2).zip(r.x.windows(2)).map(|(t, x)| {
        let a = start.max(t[0]);
        let b = end.min(t[1]);
        if b <= a || t[1] <= t[0] {
            return 0.0;
        }
        let i0 = x[0][column];
        let i1 = x[1][column];
        let current = |at: f64| i0 + (i1 - i0) * ((at - t[0]) / (t[1] - t[0]));
        -0.5 * (current(a) + current(b)) * (b - a)
    }).sum()
}

#[test]
fn t12_mos_square_law_and_inverter() {
    let mut c = SimCircuit::new();
    c.opts.reltol = 1e-11;
    c.opts.vntol = 1e-13;
    c.opts.abstol = 1e-16;
    c.opts.gmin = 0.0;
    // No bulk leakage in the closed-form channel proof, and no intrinsic oxide charge.
    c.model("nm", ModelKind::Nmos, &[("vto", 0.7), ("kp", 1e-4), ("lambda", 0.02), ("is", 0.0)]);
    c.model("pm", ModelKind::Pmos, &[("vto", -0.7), ("kp", 1e-4), ("lambda", 0.02), ("is", 0.0)]);
    c.model("body", ModelKind::Nmos, &[("vto", 0.7), ("kp", 1e-4), ("lambda", 0.02), ("gamma", 0.5), ("phi", 0.7), ("is", 0.0)]);
    let beta = 1e-4 * 10.0;
    let sat = beta * 0.5 * 1.3 * 1.3 * 1.1;
    let lin = beta * (1.3 - 0.25) * 0.5 * 1.01;
    let body_vth = 0.7 + 0.5 * (2.7_f64.sqrt() - 0.7_f64.sqrt());
    let body_current = beta * 0.5 * (2.0 - body_vth).powi(2) * 1.1;
    let cases = [
        ("saturation", "nm", [5.0, 2.0, 0.0, 0.0], sat),
        ("linear", "nm", [0.5, 2.0, 0.0, 0.0], lin),
        ("reverse_saturation", "nm", [0.0, 2.0, 5.0, 0.0], -sat),
        ("reverse_linear", "nm", [0.0, 2.0, 0.5, 0.0], -lin),
        ("body_effect", "body", [5.0, 2.0, 0.0, -2.0], body_current),
        ("pmos_mirror", "pm", [-5.0, -2.0, 0.0, 0.0], -sat),
    ];
    for (name, model, volts, _) in cases {
        let [d, g, s, b] = ["d", "g", "s", "b"].map(|pin| format!("{name}_{pin}"));
        for (node, v) in [&d, &g, &s, &b].into_iter().zip(volts) {
            c.v(&format!("v_{node}"), node, "0", Source::dc(v));
        }
        c.m(name, &d, &g, &s, &b, model, 10e-6, 1e-6);
    }
    c.devices.m[2].off = true;
    let (op, _) = run_ok(&c, &Analysis::Op);
    for (name, _, _, want) in cases {
        let got = -op.last(&format!("v_{name}_d#branch"));
        assert!(rel(got, want) < 1e-9, "MOS square-law {name} drain current: {got:e} vs {want:e}");
    }
    for (key, value) in [("w", 0.0), ("l", -1e-6), ("w", f64::NAN), ("l", f64::INFINITY)] {
        let mut invalid = c.clone();
        if key == "w" { invalid.devices.m[0].w = value; } else { invalid.devices.m[0].l = value; }
        let error = Solver::new(&invalid).err();
        assert!(error.as_ref().is_some_and(|e| e.kind == DiagKind::Setup && e.message.contains("saturation") && e.message.contains(key)), "MOS invalid {key} gives an instance-naming build error");
    }

    let c = inverter(Source::dc(0.0));
    let (sweep, summary) = run_ok(&c, &Analysis::Dc(vec![DcSweep::lin("vin", "dc", 0.0, 3.3, 0.05)]));
    let output = sweep.v("out");
    assert_eq!(summary.points, 67, "CMOS DC sweep converges at every 0.05 V point");
    assert!(output.iter().all(|v| v.is_finite()), "CMOS DC sweep output is finite");
    assert!(output.windows(2).all(|v| v[1] <= v[0] + 1e-6), "CMOS DC transfer is monotonically falling");
    assert!(output[0] > 3.29, "CMOS low input produces the high rail: {}", output[0]);
    assert!(output[66] < 0.01, "CMOS high input produces the low rail: {}", output[66]);
    let switching = output.windows(2).zip(sweep.at.windows(2)).find_map(|(v, input)| {
        if v[0] >= 1.65 && v[1] <= 1.65 && v[0] > v[1] {
            Some(input[0] + (input[1] - input[0]) * ((v[0] - 1.65) / (v[0] - v[1])))
        } else { None }
    }).unwrap_or(f64::NAN);
    assert!((1.2..=2.1).contains(&switching), "CMOS switching point lies in 1.2..2.1 V: {switching}");

    let period = 100e-9;
    let mut c = inverter(Source::wave(Wave::Pulse { v1: 0.0, v2: 3.3, td: 0.0, tr: 1e-9, tf: 1e-9, pw: 49e-9, per: period }));
    c.opts.reltol = 1e-7;
    c.opts.chgtol = 1e-22;
    c.opts.trtol = 1.0;
    let (tran, _) = run_ok(&c, &Analysis::Tran(TranSpec { tstop: 3.0 * period, max_step: Some(1e-9), uic: false }));
    assert!(tran.at.last().is_some_and(|&t| t >= 3.0 * period), "CMOS transient completes three periods");
    for k in 0..3 {
        let low = tran.at_time("out", (k as f64 + 0.25) * period);
        let high = tran.at_time("out", (k as f64 + 0.75) * period);
        assert!(low.abs() <= 0.01 * 3.3, "CMOS period {k} output reaches the low rail within 1%: {low}");
        assert!((high - 3.3).abs() <= 0.01 * 3.3, "CMOS period {k} output reaches the high rail within 1%: {high}");
    }
    let first = supply_charge(&tran, period, 2.0 * period);
    let next = supply_charge(&tran, 2.0 * period, 3.0 * period);
    assert!(first > 0.0 && next > 0.0, "CMOS supply delivers positive net charge each settled period: {first:e}, {next:e}");
    assert!(rel(first, next) < 1e-3, "CMOS periodic supply charge has no drift: {first:e} vs {next:e}, relative {:.2e}", rel(first, next));
}

/// S16's MOS1 card and instance parameters, read from a deck, each against its hand formula or the
/// explicit circuit it stands for:
/// - kp from `u0` and `tox` (u0 1e-4 eps_ox / tox), the effective length l - 2 ld and m = 2 give
///   the saturation current m kp (w / (l - 2 ld)) (vgs - vto)^2 (1 + lambda vds) / 2;
/// - `rsh` times `nrd` and `nrs` is the card's `rd` and `rs`;
/// - `js` times the junction areas is the saturation current: a forward-biased bulk carries
///   2 js ad (exp(v / vt) - 1) through its two junctions;
/// - an instance at `temp` = 77 C is the device in a circuit at 77 C;
/// - `level` 3, `nsub` and a lateral diffusion of half the channel are located errors.
#[test]
fn level_one_cards_take_every_parameter() {
    let deck = fab_elec_sim::spice::parse(
        "level 1\n.options reltol=1e-11 vntol=1e-13 abstol=1e-16 gmin=0\n\
         .model nm nmos level=1 vto=0.7 u0=600 tox=20n ld=0.1u lambda=0.02 is=0\n\
         .model nr nmos level=1 vto=0.7 kp=1e-4 rsh=10 is=0\n\
         .model nj nmos level=1 vto=0.7 kp=1e-4 js=1e-3\n\
         Vd1 d1 0 5\nVg1 g1 0 2\nM1 d1 g1 0 0 nm w=10u l=1.2u m=2\n\
         Vd2 d2 0 1\nVg2 g2 0 2\nM2 d2 g2 0 0 nr w=10u l=1u nrd=5 nrs=3\n\
         Vb3 b3 0 0.6\nM3 0 0 0 b3 nj w=10u l=1u ad=2p as=2p\n\
         Vb4 b4 0 0.6\nM4 0 0 0 b4 nj w=10u l=1u ad=2p as=2p temp=77 off\n.op\n.end\n",
    );
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    let m4 = &deck.circuit.devices.m[3];
    assert!(m4.off && m4.temp == Some(77.0) && m4.ad == 2e-12 && m4.as_ == 2e-12, "the instance parameters reach the device: {m4:?}");
    let (op, _) = run_ok(&deck.circuit, &Analysis::Op);

    let kp = 600e-4 * 3.9 * 8.854e-12 / 20e-9;
    let want = 2.0 * kp * (10e-6 / 1e-6) * 0.5 * 1.3 * 1.3 * (1.0 + 0.02 * 5.0);
    let got = -op.last("vd1#branch");
    assert!(rel(got, want) < 1e-9, "kp from u0 and tox, l - 2 ld and m = 2: {got:e} vs {want:e}");

    let vt: f64 = 8.617333262e-5 * 300.15;
    let want = 2.0 * 1e-3 * 2e-12 * ((0.6 / vt).exp() - 1.0);
    let got = -op.last("vb3#branch");
    assert!(rel(got, want) < 1e-9, "js times the junction areas: the bulk carries {got:e}, 2 js ad (exp(v / vt) - 1) = {want:e}");

    // the explicit circuits: rd and rs on the card; the device at 77 C
    let mut c = SimCircuit::new();
    (c.opts.reltol, c.opts.vntol, c.opts.abstol, c.opts.gmin) = (1e-11, 1e-13, 1e-16, 0.0);
    c.model("nr", ModelKind::Nmos, &[("vto", 0.7), ("kp", 1e-4), ("rd", 50.0), ("rs", 30.0), ("is", 0.0)]);
    c.v("vd2", "d2", "0", Source::dc(1.0));
    c.v("vg2", "g2", "0", Source::dc(2.0));
    c.m("m2", "d2", "g2", "0", "0", "nr", 10e-6, 1e-6);
    let (explicit, _) = run_ok(&c, &Analysis::Op);
    let (got, want) = (op.last("vd2#branch"), explicit.last("vd2#branch"));
    assert!(rel(got, want) < 1e-9, "rsh nrd and rsh nrs are rd = 50 and rs = 30 ohm: {got:e} vs {want:e}");
    let mut c = SimCircuit::new();
    (c.opts.reltol, c.opts.vntol, c.opts.abstol, c.opts.gmin) = (1e-11, 1e-13, 1e-16, 0.0);
    c.temp_c = 77.0;
    c.model("nj", ModelKind::Nmos, &[("vto", 0.7), ("kp", 1e-4), ("js", 1e-3)]);
    c.v("vb4", "b4", "0", Source::dc(0.6));
    c.m("m4", "0", "0", "0", "b4", "nj", 10e-6, 1e-6);
    (c.devices.m[0].ad, c.devices.m[0].as_) = (2e-12, 2e-12);
    let (hot, _) = run_ok(&c, &Analysis::Op);
    let (got, want) = (op.last("vb4#branch"), hot.last("vb4#branch"));
    assert!(rel(got, want) < 1e-9 && rel(got, op.last("vb3#branch")) > 1e-3, "temp = 77 C is the device at 77 C: {got:e} vs {want:e}");

    for (card, why) in [("level=3", "level 3 is not supported"), ("nsub=1e16", "nsub"), ("ld=0.5u", "l - 2 ld must be finite and positive")] {
        let text = format!("bad\n.model nb nmos {card}\nVd d 0 1\nMb d d 0 0 nb w=10u l=1u\n.op\n.end\n");
        let bad = fab_elec_sim::spice::parse(&text);
        assert!(bad.diags.is_empty(), "{card}: the deck reads cleanly: {:?}", bad.diags);
        let error = Solver::new(&bad.circuit).err();
        assert!(error.as_ref().is_some_and(|e| e.kind == DiagKind::Setup && e.message.contains("mb") && e.message.contains(why)), "{card}: a located setup error saying {why:?}: {error:?}");
    }
}
