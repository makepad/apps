//! Gummel-Poon device checks (T23), common-emitter DC/AC and a current mirror (T11).

use crate::devcheck::{charge_cycle, continuity, jacobian, Lcg};
use crate::linear::run_ok;
use fab_elec_sim::*;
use std::f64::consts::PI;

const DEVICES: [(&str, f64); 3] = [("qn", 1.0), ("qp", -1.0), ("qq", 1.0)];

fn full_circuit() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.temp_c = 47.0;
    let params = [
        ("is", 2e-16), ("bf", 140.0), ("nf", 1.05), ("vaf", 70.0), ("ikf", 0.02), ("ise", 3e-15), ("ne", 1.6),
        ("br", 2.5), ("nr", 1.1), ("var", 25.0), ("ikr", 0.008), ("isc", 2e-15), ("nc", 1.8),
        ("rb", 50.0), ("irb", 1e-3), ("rbm", 5.0), ("re", 2.0), ("rc", 10.0),
        ("cje", 2e-12), ("vje", 0.72), ("mje", 0.4), ("tf", 3e-10), ("xtf", 1.5), ("vtf", 8.0), ("itf", 0.015),
        ("cjc", 1e-12), ("vjc", 0.68), ("mjc", 0.45), ("xcjc", 0.7), ("tr", 8e-9),
        ("cjs", 0.8e-12), ("vjs", 0.69), ("mjs", 0.2), ("xtb", 0.7), ("eg", 1.08), ("xti", 2.7), ("fc", 0.45), ("tnom", 25.0),
    ];
    c.model("mn", ModelKind::Npn, &params);
    c.model("mp", ModelKind::Pnp, &params);
    let mut qb_params = params.to_vec();
    qb_params.push(("irb", f64::INFINITY));
    c.model("mq", ModelKind::Npn, &qb_params);
    for ((name, _), model) in DEVICES.into_iter().zip(["mn", "mp", "mq"]) {
        c.q4(name, &format!("{name}_c"), &format!("{name}_b"), &format!("{name}_e"), &format!("{name}_s"), model);
    }
    for (q, area) in c.devices.q.iter_mut().zip([0.75, 1.4, 2.0]) {
        q.area = area;
    }
    c
}

/// External c, b, e, s and internal c', b', e'. All three devices have all three resistors.
fn nodes(s: &Solver, name: &str) -> [usize; 7] {
    [format!("{name}_c"), format!("{name}_b"), format!("{name}_e"), format!("{name}_s"), format!("{name}#c"), format!("{name}#b"), format!("{name}#e")]
        .map(|n| s.unknown(&n).unwrap_or_else(|| panic!("no BJT unknown {n}")) as usize)
}

fn bias(rng: &mut Lcg, s: &Solver, x: &mut [f64]) {
    for v in x.iter_mut().skip(1) {
        *v = rng.range(-1.0, 1.0);
    }
    for (name, p) in DEVICES {
        let [c, b, e, sub, ci, bi, ei] = nodes(s, name);
        x[bi] = x[ei] + p * rng.range(-3.0, 0.9);
        x[ci] = x[bi] - p * rng.range(-3.0, 0.85);
        x[c] = x[ci] + rng.range(-0.3, 0.3);
        x[b] = x[bi] + rng.range(-0.3, 0.3);
        x[e] = x[ei] + rng.range(-0.3, 0.3);
        x[sub] = x[ci] + p * rng.range(-3.0, 0.0);
    }
}

#[test]
fn t23_device_checks_bjt() {
    let c = full_circuit();
    let mut rng = Lcg(0xb17);
    // each point draws a fresh bias for all three transistors: 10 000 biases per device
    let rep = jacobian(&c, 3_400, &mut rng, &mut bias);
    assert!(rep.g.0 < 1e-6, "BJT G = df/dx: {:.2e} at row/col {:?}, bias {:?}", rep.g.0, rep.g.2, rep.g.1);
    assert!(rep.c.0 < 1e-6, "BJT C = dq/dx: {:.2e} at row/col {:?}, bias {:?}", rep.c.0, rep.c.2, rep.c.1);
    assert!(rep.neutral < 1e-12, "BJT terminal charge neutrality: {:.2e}", rep.neutral);
    let cycle = charge_cycle(&c, &mut rng, &mut bias);
    assert!(cycle == 0.0, "BJT charges return bit for bit after a bias cycle: {cycle:.2e}");
    // A smooth parabola visits cutoff, reverse active, saturation and forward active.
    // Resistor drops and the substrate bias move too, with their exact dx/dt supplied.
    let path = |t: f64, s: &Solver, x: &mut [f64], dx: &mut [f64]| {
        for (k, (name, p)) in DEVICES.into_iter().enumerate() {
            let [c, b, e, sub, ci, bi, ei] = nodes(s, name);
            (x[ei], dx[ei]) = (0.1 * t + 0.2 * k as f64, 0.1);
            (x[bi], dx[bi]) = (x[ei] + p * 0.8 * t, dx[ei] + p * 0.8);
            (x[ci], dx[ci]) = (x[bi] - p * (0.75 - 1.7 * t * t), dx[bi] + p * 3.4 * t);
            (x[c], dx[c]) = (x[ci] + p * (0.05 + 0.03 * t), dx[ci] + p * 0.03);
            (x[b], dx[b]) = (x[bi] + p * (0.03 + 0.02 * t), dx[bi] + p * 0.02);
            (x[e], dx[e]) = (x[ei] + p * (0.02 - 0.01 * t), dx[ei] - p * 0.01);
            (x[sub], dx[sub]) = (x[ci] + p * (-1.5 + 0.3 * t), dx[ci] + p * 0.3);
        }
    };
    let (worst, at) = continuity(&c, -1.0, 1.0, 4000, 1e-3, &path);
    assert!(worst < 1e-6, "BJT current/charge/slope continuity: {worst:.2e} at t = {at}");
}

fn rel(got: f64, want: f64) -> f64 {
    (got - want).abs() / want.abs().max(1e-300)
}

/// An increasing residual, with a bracket that contains its root.
fn bisect(mut lo: f64, mut hi: f64, residual: impl Fn(f64) -> f64) -> f64 {
    assert!(residual(lo) <= 0.0 && residual(hi) >= 0.0, "BJT hand solution brackets its root");
    loop {
        let mid = 0.5 * (lo + hi);
        if hi - lo <= 1e-15 * mid.abs() || mid == lo || mid == hi {
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
fn t11_bjt_mirror_and_common_emitter() {
    let (is, bf, br, vaf, vt) = (1e-15, 120.0, 1.0, 80.0, 300.0 * 8.617333262e-5);
    let (vcc, rc, iref, vout) = (5.0, 2e3, 1e-3, 3.0);
    let drives = [0.6, 0.65, 0.7];
    // Independent closed form: ic, ib and analytic derivatives at fixed vce / fixed vbe.
    let hand = |vbe: f64, vce: f64| {
        let (ef, er) = ((vbe / vt).exp(), ((vbe - vce) / vt).exp());
        let (ibf, ibr) = (is * (ef - 1.0), is * (er - 1.0));
        let (gf, gr) = (is * ef / vt, is * er / vt);
        let early = 1.0 - (vbe - vce) / vaf;
        let ic = (ibf - ibr) * early - ibr / br;
        let ib = ibf / bf + ibr / br;
        let gm = (gf - gr) * early - (ibf - ibr) / vaf - gr / br;
        let go = gr * early + (ibf - ibr) / vaf + gr / br;
        (ic, ib, gm, go)
    };
    let mut c = SimCircuit::new();
    c.temp_c = 300.0 - 273.15;
    c.opts.tnom = c.temp_c;
    c.opts.reltol = 1e-10;
    c.opts.vntol = 1e-13;
    c.opts.abstol = 1e-15;
    c.opts.gmin = 0.0; // the hand equations contain no junction shunts
    for (model, kind, p) in [("mn", ModelKind::Npn, 1.0), ("mp", ModelKind::Pnp, -1.0)] {
        c.model(model, kind, &[("is", is), ("bf", bf), ("br", br), ("vaf", vaf)]);
        for (k, vbe) in drives.into_iter().enumerate() {
            let tag = format!("{model}{k}");
            let (supply, collector, base) = (format!("supply_{tag}"), format!("collector_{tag}"), format!("base_{tag}"));
            c.v(&format!("vcc_{tag}"), &supply, "0", Source::dc(p * vcc));
            c.v(&format!("vb_{tag}"), &base, "0", Source::ac(p * vbe, 1.0, 0.0));
            c.r(&format!("rc_{tag}"), &supply, &collector, rc);
            c.q(&format!("q_{tag}"), &collector, &base, "0", model);
        }
    }
    c.i("iref", "0", "ref", Source::dc(iref));
    c.q("qref", "ref", "ref", "0", "mn");
    c.q("qout", "out", "ref", "0", "mn");
    c.v("vout", "out", "0", Source::dc(vout));
    let (op, _) = run_ok(&c, &Analysis::Op);
    let (ac, _) = run_ok(&c, &Analysis::Ac(AcSweep::List(vec![1e3])));
    for (model, p) in [("mn", 1.0), ("mp", -1.0)] {
        for (k, vbe) in drives.into_iter().enumerate() {
            let want = bisect(0.0, vcc / rc, |ic| ic - hand(vbe, vcc - rc * ic).0);
            let got = -p * op.last(&format!("vcc_{model}{k}#branch"));
            assert!(rel(got, want) < 1e-9, "{model} common-emitter current at vbe = {vbe}: {got} vs {want}");
            let (_, _, gm, go) = hand(vbe, vcc - rc * want);
            let gain = -gm / (1.0 / rc + go); // -gm * (Rc || ro), ro = 1/go
            let h = ac.ac_v(&format!("collector_{model}{k}"))[0];
            assert!(rel(h.norm(), gain.abs()) < 1e-6, "{model} AC gain magnitude at vbe = {vbe}: {} vs {}", h.norm(), gain.abs());
            assert!((h.arg().abs() - PI).abs() < 1e-6, "{model} AC gain phase at vbe = {vbe}: {} rad, want pi", h.arg());
        }
    }
    let vbe = bisect(0.0, 0.9, |v| {
        let (ic1, ib1, _, _) = hand(v, v);
        let (_, ib2, _, _) = hand(v, vout);
        // Iref = ic1 + 2 ib, retaining the two bases' slightly different reverse leakage.
        ic1 + ib1 + ib2 - iref
    });
    let want = hand(vbe, vout).0;
    let got = -op.last("vout#branch");
    assert!(rel(got, want) < 1e-9, "NPN mirror output current: {got} vs {want}");
    assert!(rel(op.last("ref"), vbe) < 1e-9, "NPN mirror shared vbe: {} vs {vbe}", op.last("ref"));
    let ratio = (1.0 + (vout - vbe) / vaf) / (1.0 + 2.0 / bf);
    assert!(rel(got / iref, ratio) < 1e-3, "NPN mirror textbook ratio: {} vs {ratio}", got / iref);
}
