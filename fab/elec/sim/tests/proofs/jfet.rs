//! JFET T23 device checks and closed-form channel, self-bias and gate-junction proofs.

use crate::devcheck::{charge_cycle, continuity, eval, exact_cx, jacobian, Lcg};
use crate::linear::run_ok;
use fab_elec_sim::*;

const DEVICES: [(&str, f64); 2] = [("jn", 1.0), ("jp", -1.0)];

fn full_circuit(series: bool) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.temp_c = 47.0;
    for (name, kind) in [("nm", ModelKind::Njf), ("pm", ModelKind::Pjf)] {
        c.model(name, kind, &[
            ("vto", -2.0), ("beta", 8e-4), ("lambda", 0.035),
            ("rd", if series { 14.0 } else { 0.0 }), ("rs", if series { 9.0 } else { 0.0 }),
            ("cgs", 2.3e-12), ("cgd", 1.7e-12), ("pb", 0.78), ("m", 0.37), ("fc", 0.42),
            ("is", 2e-14), ("n", 1.2), ("tnom", 25.0), ("xti", 2.7), ("eg", 1.08),
        ]);
    }
    for (((name, _), model), area) in DEVICES.into_iter().zip(["nm", "pm"]).zip([0.8, 1.4]) {
        c.jfet(name, &format!("{name}_d"), &format!("{name}_g"), &format!("{name}_s"), model, area);
    }
    c
}

/// External d, g, s and intrinsic d', s'; without a resistor the corresponding nodes coincide.
fn nodes(s: &Solver, name: &str) -> [usize; 5] {
    let [d, g, source] = ["d", "g", "s"].map(|pin| {
        s.unknown(&format!("{name}_{pin}")).expect("JFET external terminal") as usize
    });
    [d, g, source, s.unknown(&format!("{name}#d")).map_or(d, |n| n as usize), s.unknown(&format!("{name}#s")).map_or(source, |n| n as usize)]
}

fn bias(rng: &mut Lcg, s: &Solver, x: &mut [f64]) {
    for (name, p) in DEVICES {
        let [d, g, source, di, si] = nodes(s, name);
        let mode = (5.0 * rng.next()) as u8;
        let vgs = match mode {
            0 => rng.range(-3.5, -2.2),
            4 => rng.range(0.25, 0.7),
            _ => rng.range(-1.8, -0.2),
        };
        let vds = match mode {
            1 => rng.range(0.1, 0.85) * (vgs + 2.0),
            2 => vgs + 2.0 + rng.range(0.2, 2.0),
            _ => rng.range(0.1, 3.0),
        };
        let reverse = mode == 3 || (mode == 4 && rng.next() < 0.5);
        let (fd, fs) = if reverse { (si, di) } else { (di, si) };
        x[fs] = rng.range(-0.8, 0.8);
        x[fd] = x[fs] + p * vds;
        x[g] = x[fs] + p * vgs;
        if d != di { x[d] = x[di] + rng.range(-0.08, 0.08); }
        if source != si { x[source] = x[si] + rng.range(-0.08, 0.08); }
    }
}

#[test]
fn t23_device_checks_jfet() {
    let mut rng = Lcg(0x4a464554);
    for series in [true, false] {
        let c = full_circuit(series);
        let rep = jacobian(&c, 1600, &mut rng, &mut bias);
        assert!(rep.g.0 < 1e-6, "JFET G = df/dx (series {series}): {:.2e} at {:?}, bias {:?}", rep.g.0, rep.g.2, rep.g.1);
        assert!(rep.c.0 < 1e-6, "JFET C = dq/dx (series {series}): {:.2e} at {:?}, bias {:?}", rep.c.0, rep.c.2, rep.c.1);
        assert!(rep.neutral < 1e-12, "JFET charge neutrality (series {series}): {:.2e}", rep.neutral);
        let cycle = charge_cycle(&c, &mut rng, &mut bias);
        assert!(cycle == 0.0, "JFET charge cycle closes bit for bit (series {series}): {cycle:.2e}");

        let mut solver = Solver::new(&c).expect("JFET full model setup");
        let mut x = vec![0.0; solver.n + 1];
        bias(&mut rng, &solver, &mut x);
        let (f, q, g, cm) = eval(&mut solver, &x);
        assert!(f.iter().chain(&q).chain(g.iter().flatten()).chain(cm.iter().flatten()).all(|v| v.is_finite()), "JFET values and Jacobians are finite");
        let mut dot: Vec<f64> = (0..x.len()).map(|_| rng.range(-1e6, 1e6)).collect();
        dot[0] = 0.0;
        let cx = exact_cx(&solver);
        for ((name, _), area) in DEVICES.into_iter().zip([0.8, 1.4]) {
            let [d, gate, source, di, si] = nodes(&solver, name);
            if series {
                assert!(d != di && source != si, "JFET series resistors claim internal nodes");
                assert!(rel(g[d][d], area / 14.0) < 1e-14 && rel(g[source][source], area / 9.0) < 1e-14, "JFET area divides rd and rs");
            }
            let (bank, inst) = solver.find_device(name).expect("JFET bank exposes its instance");
            assert_eq!(solver.banks[bank].terminals(), 3, "JFET terminal count");
            let mut pins = [f64::NAN; 3];
            solver.banks[bank].terminal_currents(inst, &x, &dot, &cx, &mut pins);
            for (pin, u) in [d, gate, source].into_iter().enumerate() {
                let want = f[u] + cm[u].iter().zip(&dot).map(|(c, dv)| c * dv).sum::<f64>();
                assert!((pins[pin] - want).abs() < 1e-12 * want.abs().max(1e-18), "JFET {name} terminal {pin} agrees with stamped conduction and charge rate: {} vs {want}", pins[pin]);
            }
        }
    }

    let c = full_circuit(true);
    for reverse in [false, true] {
        // Sweep through cutoff and saturation/linear, then both depletion continuation corners.
        let path = |t: f64, solver: &Solver, x: &mut [f64], dx: &mut [f64]| {
            for (name, p) in DEVICES {
                let [d, g, source, di, si] = nodes(solver, name);
                let (fd, fs) = if reverse { (si, di) } else { (di, si) };
                x[fd] = x[fs] + p * 0.5;
                x[g] = x[fs] + p * (-2.0 + t);
                dx[g] = p;
                x[d] = x[di];
                x[source] = x[si];
            }
        };
        let (worst, at) = continuity(&c, -0.3, 2.95, 650, 1e-3, &path);
        assert!(worst < 1e-6, "JFET values and first derivatives continuous across cutoff, saturation and depletion corners (reverse {reverse}): {worst:.2e} at {at}");
    }
    let reversal = |t: f64, solver: &Solver, x: &mut [f64], dx: &mut [f64]| {
        for (name, p) in DEVICES {
            let [d, g, source, di, si] = nodes(solver, name);
            x[di] = p * t;
            dx[di] = p;
            x[g] = -p;
            x[d] = x[di];
            dx[d] = p;
            x[source] = x[si];
        }
    };
    let (worst, at) = continuity(&c, -0.6, 0.6, 600, 1e-3, &reversal);
    assert!(worst < 1e-6, "JFET values and first derivatives continuous at vds = 0: {worst:.2e} at {at}");
}

fn rel(got: f64, want: f64) -> f64 {
    (got - want).abs() / want.abs().max(1e-300)
}

fn precise(c: &mut SimCircuit) {
    (c.opts.reltol, c.opts.vntol, c.opts.abstol, c.opts.gmin) = (1e-11, 1e-13, 1e-16, 0.0);
}

#[test]
fn jfet_square_law_rust_and_deck() {
    let mut c = SimCircuit::new();
    precise(&mut c);
    let mut text = String::from("JFET square law\n.options reltol=1e-11 vntol=1e-13 abstol=1e-16 gmin=0\n");
    for (model, kind, card) in [("nm", ModelKind::Njf, "njf"), ("pm", ModelKind::Pjf, "pjf")] {
        // No gate leakage in the channel-only closed form. P uses the same normalised vto.
        c.model(model, kind, &[("vto", -2.0), ("beta", 1e-3), ("lambda", 0.02), ("is", 0.0)]);
        text.push_str(&format!(".model {model} {card} vt0=-2 beta=1m lambda=0.02 is=0\n"));
    }
    let sat = 1e-3 * 1.0 * (1.0 + 0.1);
    let lin = 1e-3 * 0.5 * (2.0 - 0.5) * 1.01;
    let cases = [
        ("sat", [5.0, -1.0, 0.0], sat),
        ("lin", [0.5, -1.0, 0.0], lin),
        ("cut", [5.0, -2.5, 0.0], 0.0),
        ("rev_sat", [0.0, -1.0, 5.0], -sat),
        ("rev_lin", [0.0, -1.0, 0.5], -lin),
    ];
    let mut expected = Vec::new();
    for (label, model, p) in [("n", "nm", 1.0), ("p", "pm", -1.0)] {
        for (case, volts, current) in cases {
            for area in [1.0, 2.0] {
                let name = format!("j_{label}_{case}_{area}");
                let [d, g, s] = ["d", "g", "s"].map(|pin| format!("{name}_{pin}"));
                for (node, v) in [&d, &g, &s].into_iter().zip(volts) {
                    c.v(&format!("v_{node}"), node, "0", Source::dc(p * v));
                    text.push_str(&format!("v_{node} {node} 0 {}\n", p * v));
                }
                c.jfet(&name, &d, &g, &s, model, area);
                let off = case == "rev_sat";
                c.devices.j.last_mut().unwrap().off = off;
                let area_text = if area == 1.0 { String::new() } else { format!(" {area}") };
                text.push_str(&format!("{name} {d} {g} {s} {model}{area_text}{}\n", if off { " off" } else { "" }));
                expected.push((name, p * area * current));
            }
        }
    }
    text.push_str(".op\n.end\n");
    let deck = fab_elec_sim::spice::parse(&text);
    assert!(deck.diags.is_empty(), "JFET deck reads cleanly: {:?}", deck.diags);
    assert_eq!(c.devices.j, deck.circuit.devices.j, "JFET nodes, model, default/explicit area and off reach the instances");
    for (label, circuit) in [("Rust", &c), ("deck", &deck.circuit)] {
        let (op, _) = run_ok(circuit, &Analysis::Op);
        for (name, want) in &expected {
            let got = -op.last(&format!("v_{name}_d#branch"));
            if *want == 0.0 {
                assert!(got.abs() < 1e-16, "JFET {label} {name} cutoff: {got:e}");
            } else {
                assert!(rel(got, *want) < 1e-9, "JFET {label} {name} square law: {got:e} vs {want:e}");
            }
        }
    }
    let mut changed = c.clone();
    changed.devices.j[0].area *= 2.0;
    assert!(c.value_changes(&changed).is_none(), "a JFET parameter edit requires topology replacement, not a value command");
}

#[test]
fn jfet_self_bias_closed_form() {
    let mut c = SimCircuit::new();
    precise(&mut c);
    let (beta, vto, vdd, rd, rs): (f64, f64, f64, f64, f64) = (1e-3, -2.0, 10.0, 2e3, 500.0);
    c.model("jm", ModelKind::Njf, &[("vto", vto), ("beta", beta), ("is", 0.0)]);
    c.v("vdd", "vdd", "0", Source::dc(vdd));
    c.r("rd", "vdd", "d", rd);
    c.r("rs", "s", "0", rs);
    c.jfet("j1", "d", "0", "s", "jm", 1.0);
    // Write u = -id RS - vto. Then beta RS u^2 + u + vto = 0;
    // the positive root (rationalised) is the saturation solution.
    let u = -2.0 * vto / (1.0 + (1.0 - 4.0 * beta * rs * vto).sqrt());
    let id = beta * u * u;
    let (vs, vd) = (id * rs, vdd - id * rd);
    assert!(u > 0.0 && vd - vs >= u, "the hand root is in saturation");
    let (op, _) = run_ok(&c, &Analysis::Op);
    for (label, got, want) in [("id", -op.last("vdd#branch"), id), ("vs", op.last("s"), vs), ("vd", op.last("d"), vd)] {
        assert!(rel(got, want) < 1e-9, "JFET self-bias {label}: {got:e} vs {want:e}");
    }
}

#[test]
fn jfet_gate_diodes_and_limiting() {
    for (kind, p) in [(ModelKind::Njf, 1.0), (ModelKind::Pjf, -1.0)] {
        for (temp, tnom, area, n, eg, xti) in [(27.0, 27.0, 1.0, 1.0, 1.11, 3.0), (77.0, 25.0, 1.7, 1.3, 1.08, 2.7)] {
            let mut c = SimCircuit::new();
            precise(&mut c);
            c.temp_c = temp;
            c.model("jm", kind, &[("is", 1e-14), ("n", n), ("tnom", tnom), ("eg", eg), ("xti", xti)]);
            c.v("vg", "g", "0", Source::dc(p * 0.6));
            c.jfet("j1", "0", "g", "0", "jm", area);
            let vt = 8.617333262e-5 * (temp + 273.15);
            let ratio = (temp + 273.15) / (tnom + 273.15);
            let is = area * 1e-14 * (((ratio - 1.0) * eg / (n * vt)).exp()) * ratio.powf(xti / n);
            let want = p * 2.0 * is * ((0.6 / (n * vt)).exp() - 1.0);
            let (op, _) = run_ok(&c, &Analysis::Op);
            let got = -op.last("vg#branch");
            assert!(rel(got, want) < 1e-9, "JFET gate current is two Shockley junctions (p {p}, T {temp}, area {area}): {got:e} vs {want:e}");

            let mut s = Solver::new(&c).expect("JFET limiting setup");
            let gate = s.unknown("g").unwrap() as usize;
            let (bank, inst) = s.find_device("j1").unwrap();
            let critical = n * vt * (n * vt / (std::f64::consts::SQRT_2 * is)).ln();
            let mut cx = s.base_cx();
            cx.init_junctions = true;
            s.x[gate] = p * 0.6;
            assert!(s.load(&cx).0 > 0, "initial junction guesses cannot converge");
            for v in s.banks[bank].state(inst) {
                assert!(rel(v, critical) < 1e-14, "a forward-biased junction starts at vcrit");
            }
            s.x[gate] = -p;
            s.load(&cx);
            assert_eq!(s.banks[bank].state(inst), vec![0.0, 0.0], "reverse-biased junctions start at zero");
            cx.init_junctions = false;
            s.x[gate] = p * 2.0;
            // Remove the prediction-error criterion: nonconvergence must come from the limiter.
            cx.abstol = f64::MAX;
            assert!(s.load(&cx).0 > 0, "a limiting junction prevents convergence");
            assert!(s.banks[bank].state(inst).iter().all(|v| *v > 0.0 && *v < 2.0), "both forward junction steps are limited");

            c.devices.j[0].off = true;
            let mut s = Solver::new(&c).expect("JFET off setup");
            let gate = s.unknown("g").unwrap() as usize;
            s.x[gate] = p * 0.6;
            let mut cx = s.base_cx();
            cx.init_junctions = true;
            s.load(&cx);
            let (bank, inst) = s.find_device("j1").unwrap();
            assert_eq!(s.banks[bank].state(inst), vec![0.0, 0.0], "off starts both junctions at zero");
        }
    }
}

#[test]
fn jfet_unsupported_cards_are_located() {
    for (card, why) in [
        ("b=0.9", "b = 0.9 is not supported yet"),
        ("isr=1", "isr = 1 is not supported yet"),
        ("alpha=1", "alpha = 1 is not supported yet"),
        ("vk=1", "vk = 1 is not supported yet"),
        ("tcv=1", "tcv = 1 is not supported yet"),
        ("vtotc=1", "vtotc = 1 is not supported yet"),
        ("bex=1", "bex = 1 is not supported yet"),
        ("betatce=1", "betatce = 1 is not supported yet"),
        ("level=2", "level 2 is not supported"),
    ] {
        let text = format!("unsupported JFET\n.model jm njf {card}\nJ1 d g 0 jm\n.op\n.end\n");
        let deck = fab_elec_sim::spice::parse(&text);
        assert!(deck.diags.is_empty(), "JFET {card} reads before setup: {:?}", deck.diags);
        let error = Solver::new(&deck.circuit).err();
        assert!(error.as_ref().is_some_and(|e| e.kind == DiagKind::Setup && e.message.contains(&format!("j1: model jm: {why}"))), "JFET {card} gives a located setup error: {error:?}");
    }
    let deck = fab_elec_sim::spice::parse(
        "supported defaults\n.model jm njf b=1 isr=0 alpha=0 vk=0 tcv=0 vtotc=0 bex=0 betatce=0 kf=1 af=2 nlev=3 gdsnoi=4\nJ1 d g 0 jm\n.op\n.end\n",
    );
    assert!(deck.diags.is_empty(), "JFET dormant extensions and noise parameters read cleanly: {:?}", deck.diags);
    assert!(Solver::new(&deck.circuit).is_ok(), "default extension values and unused noise parameters do not reject a level-1 model");
}
