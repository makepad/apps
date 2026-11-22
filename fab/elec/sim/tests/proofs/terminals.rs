//! Published pin currents conserve current at devices and at circuit nodes.

use fab_elec_sim::analysis::op::operating_point;
use fab_elec_sim::expr::Expr;
use fab_elec_sim::{BKind, CtlKind, ModelKind, SimCircuit, Solver, Source};

pub(super) fn terminal_circuit() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.opts.reltol = 1e-12;
    c.opts.vntol = 1e-14;
    c.opts.abstol = 1e-16;
    c.model("swm", ModelKind::Sw, &[("ron", 10.0), ("roff", 1e6), ("vt", 0.5), ("vh", 0.1), ("vband", 0.05)]);
    c.model("cswm", ModelKind::Csw, &[("ron", 10.0), ("roff", 1e6), ("it", -0.5e-3), ("ih", 0.1e-3)]);
    c.model("dr", ModelKind::Diode, &[("rs", 2.0), ("cjo", 2e-12), ("tt", 5e-9)]);
    c.model("dp", ModelKind::Diode, &[("cjo", 1e-12), ("tt", 2e-9)]);
    c.model("npn", ModelKind::Npn, &[
        ("is", 1e-15), ("rb", 50.0), ("rbm", 5.0), ("irb", 1e-3), ("rc", 10.0), ("re", 2.0),
        ("cje", 2e-12), ("cjc", 1e-12), ("xcjc", 0.7), ("cjs", 0.8e-12), ("tf", 3e-10), ("tr", 8e-9),
    ]);
    c.model("pnp", ModelKind::Pnp, &[
        ("is", 1e-15), ("cje", 2e-12), ("cjc", 1e-12), ("xcjc", 0.7), ("cjs", 0.8e-12), ("tf", 3e-10), ("tr", 8e-9),
    ]);
    c.model("regm", ModelKind::Reg, &[]);
    c.v("vdd", "supply", "0", Source::dc(5.0));
    c.v("vctrl", "ctrl", "0", Source::dc(0.7));
    c.v("vpb", "pb", "0", Source::dc(4.3));
    c.i("i1", "0", "mix", Source::dc(0.5e-3));
    for (name, a, b, r) in [
        ("r1", "supply", "mix", 1e3), ("r2", "mix", "0", 2e3), ("rl", "low", "0", 1.5e3),
        ("rctrl", "ctrl", "0", 1e3), ("re", "e", "0", 1e3), ("rf", "f", "0", 1e3), ("rh", "h", "0", 1e3),
        ("rsw", "sw", "0", 1e3), ("rd1", "supply", "dr", 1e3), ("rd2", "supply", "dp", 2e3),
        ("rnc", "supply", "nc", 1e3), ("rpc", "pc", "0", 1e3), ("rreg", "regulated", "0", 66.0),
        ("rbv", "bv", "0", 1e3), ("rel", "el", "0", 1e3), ("rw", "wsw", "0", 1e3), ("rt", "tl", "0", 1e3),
    ] {
        c.r(name, a, b, r);
    }
    c.c("c1", "mix", "supply", 2e-9);
    c.l("l1", "mix", "mid", 1e-3);
    c.l("l2", "mid", "low", 2e-3);
    c.k("k1", "l1", "l2", 0.7);
    c.vctl("e1", CtlKind::Vcvs, "e", "0", "mix", "0", 2.0);
    c.vctl("g1", CtlKind::Vccs, "e", "mix", "ctrl", "0", 1e-3);
    c.ictl("f1", CtlKind::Cccs, "f", "0", "vctrl", 2.0);
    c.ictl("h1", CtlKind::Ccvs, "h", "0", "vctrl", 100.0);
    c.sw("s1", "mix", "sw", "ctrl", "0", "swm", false);
    c.csw("w1", "mix", "wsw", "vctrl", "cswm", false);
    c.model("ltm", ModelKind::Ltra, &[("r", 2.0), ("l", 250e-9), ("g", 1e-4), ("c", 100e-12), ("len", 0.5)]);
    c.ltra("o1", "mix", "0", "tl", "0", "ltm");
    c.d("d1", "dr", "0", "dr");
    c.d("d2", "dp", "0", "dp");
    c.q4("qn", "nc", "ctrl", "0", "0", "npn");
    c.q4("qp", "pc", "pb", "supply", "supply", "pnp");
    c.reg("reg1", "supply", "0", "regulated", "supply", "regm");
    c.b("b1", BKind::I, "f", "mix", Expr::parse("1e-4*v(ctrl)*v(mix)").unwrap());
    c.b("b2", BKind::V, "bv", "0", Expr::parse("v(mix)/2 + 10*i(vctrl)").unwrap());
    c.b("b3", BKind::R, "f", "0", Expr::parse("1e3*(1 + v(ctrl)^2)").unwrap());
    c.b("b4", BKind::C, "mix", "0", Expr::parse("1e-9*(1 + v(mix)/5)").unwrap());
    c.b("b5", BKind::L, "e", "el", Expr::parse("1e-3*(1 + v(ctrl)^2)").unwrap());

    c
}

#[test]
fn terminal_currents_balance() {
    let c = terminal_circuit();
    // External terminals only, in live::Terminal order. Controls carry no current;
    // K couples branch fluxes and has no terminals. Include ground in the node proof.
    let terminals: &[(&str, &[&str])] = &[
        ("vdd", &["supply", "0"]), ("vctrl", &["ctrl", "0"]), ("vpb", &["pb", "0"]),
        ("i1", &["0", "mix"]),
        ("r1", &["supply", "mix"]), ("r2", &["mix", "0"]), ("rl", &["low", "0"]),
        ("rctrl", &["ctrl", "0"]), ("re", &["e", "0"]), ("rf", &["f", "0"]), ("rh", &["h", "0"]),
        ("rsw", &["sw", "0"]), ("rd1", &["supply", "dr"]), ("rd2", &["supply", "dp"]),
        ("rnc", &["supply", "nc"]), ("rpc", &["pc", "0"]), ("rreg", &["regulated", "0"]), ("rbv", &["bv", "0"]),
        ("rel", &["el", "0"]), ("rw", &["wsw", "0"]), ("rt", &["tl", "0"]), ("o1", &["mix", "0", "tl", "0"]),
        ("c1", &["mix", "supply"]), ("l1", &["mix", "mid"]), ("l2", &["mid", "low"]), ("k1", &[]),
        ("e1", &["e", "0"]), ("g1", &["e", "mix"]), ("f1", &["f", "0"]), ("h1", &["h", "0"]),
        ("s1", &["mix", "sw"]), ("w1", &["mix", "wsw"]), ("d1", &["dr", "0"]), ("d2", &["dp", "0"]),
        ("qn", &["nc", "ctrl", "0", "0"]), ("qp", &["pc", "pb", "supply", "supply"]),
        ("reg1", &["supply", "0", "regulated", "supply"]),
        ("b1", &["f", "mix"]), ("b2", &["bv", "0"]), ("b3", &["f", "0"]), ("b4", &["mix", "0"]),
        ("b5", &["e", "el"]),
    ];
    let mut s = Solver::new(&c).unwrap();
    operating_point(&mut s, false).unwrap();
    let cx = s.base_cx();
    let at = |name: &str| s.unknown(name).unwrap_or_else(|| panic!("no unknown {name}")) as usize;
    let mut xdot = vec![0.0; s.x.len()];
    let mut node_sum = vec![0.0; c.nodes.len()];
    let mut node_max = vec![0.0_f64; c.nodes.len()];
    assert_eq!(terminals.len(), s.banks.iter().map(|b| b.len()).sum::<usize>(), "table covers every instance");
    for &(name, nodes) in terminals {
        let (b, inst) = s.find_device(name).unwrap_or_else(|| panic!("no device {name}"));
        let bank = &s.banks[b];
        assert_eq!(bank.terminals(), nodes.len(), "{name}: terminal count");
        let mut out = vec![f64::NAN; bank.terminals()];
        bank.terminal_currents(inst, &s.x, &xdot, &cx, &mut out);
        assert!(out.iter().all(|i| i.is_finite()), "{name}: all terminal currents written and finite: {out:?}");
        let sum: f64 = out.iter().sum();
        let largest = out.iter().map(|i| i.abs()).fold(0.0_f64, f64::max);
        assert!(sum.abs() <= 1e-12 * largest, "{name}: terminal balance {sum:e}, currents {out:?}");
        for (&node, &current) in nodes.iter().zip(&out) {
            let u = at(node);
            node_sum[u] += current;
            node_max[u] = node_max[u].max(current.abs());
        }
    }
    for node in &c.nodes {
        let u = at(&node.name);
        assert!(node_sum[u].abs() <= 1e-9 * node_max[u], "{}: KCL residual {:e}, largest current {:e}", node.name, node_sum[u], node_max[u]);
    }

    let mut out = [0.0; 2];
    let (b, inst) = s.find_device("r1").unwrap();
    s.banks[b].terminal_currents(inst, &s.x, &xdot, &cx, &mut out);
    let want = (s.x[at("supply")] - s.x[at("mix")]) / 1e3;
    assert!((out[0] - want).abs() <= 1e-12 * want.abs(), "resistor Ohm's law: {} vs {want}", out[0]);
    let (b, inst) = s.find_device("vdd").unwrap();
    s.banks[b].terminal_currents(inst, &s.x, &xdot, &cx, &mut out);
    let want = s.x[at("vdd#branch")];
    assert_eq!(out, [want, -want], "voltage source branch current and sign");
    xdot[at("mix")] = 3e5;
    xdot[at("supply")] = -2e5;
    let (b, inst) = s.find_device("c1").unwrap();
    s.banks[b].terminal_currents(inst, &s.x, &xdot, &cx, &mut out);
    let want = 2e-9 * (3e5 - (-2e5));
    assert!((out[0] - want).abs() <= 1e-12 * want.abs(), "capacitor charge rate: {} vs {want}", out[0]);
    assert_eq!(out[1], -out[0], "capacitor return current");
    // q = C(v) v with C = 1 nF (1 + v / 5): dq/dt = (C + v dC/dv) dv/dt = 1 nF (1 + 2 v / 5) dv/dt
    let (b, inst) = s.find_device("b4").unwrap();
    s.banks[b].terminal_currents(inst, &s.x, &xdot, &cx, &mut out);
    let v = s.x[at("mix")];
    let want = 1e-9 * (1.0 + 2.0 * v / 5.0) * 3e5;
    assert!((out[0] - want).abs() <= 1e-12 * want.abs(), "behavioural capacitor charge rate: {} vs {want}", out[0]);
    assert_eq!(out[1], -out[0], "behavioural capacitor return current");
    let (b, inst) = s.find_device("b5").unwrap();
    s.banks[b].terminal_currents(inst, &s.x, &xdot, &cx, &mut out);
    let want = s.x[at("b5#branch")];
    assert!(want.abs() > 1e-6, "the behavioural inductor carries its load's current: {want}");
    assert_eq!(out, [want, -want], "behavioural inductor branch current and sign");
    // a line: each port's current flows into its + terminal; port 2 feeds its 1 kohm load
    let mut out = [0.0; 4];
    let (b, inst) = s.find_device("o1").unwrap();
    s.banks[b].terminal_currents(inst, &s.x, &xdot, &cx, &mut out);
    let (i1, i2) = (s.x[at("o1#1#branch")], s.x[at("o1#2#branch")]);
    let load = s.x[at("tl")] / 1e3;
    assert!(load.abs() > 1e-6 && (i2 + load).abs() <= 1e-12 * load.abs(), "the far port's current is its load's, reversed: {i2} vs {load}");
    assert!((i1 - load).abs() > 1e-9 * load.abs(), "the lossy line's shunt draws: {i1} in, {load} out");
    assert_eq!(out, [i1, -i1, i2, -i2], "line port currents and signs");
}
