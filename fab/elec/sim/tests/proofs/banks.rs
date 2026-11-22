//! Instance footprints contain every read and write; selecting every instance preserves eval.

use crate::devcheck::Lcg;
use crate::terminals::terminal_circuit;
use fab_elec_sim::circuit::{Banks, Boundary, BoundaryKind};
use fab_elec_sim::expr::Expr;
use fab_elec_sim::stamp::{Bank, EvalCx, Stamps, UnkKind};
use fab_elec_sim::{BKind, CtlKind, GpioParams, ModelKind, PinMode, SimCircuit, Solver, Source, Wave};

#[test]
fn subcircuits_keep_only_used_nodes_and_preserve_devices() {
    let mut c = SimCircuit::new();
    c.opts.reltol = 1e-5;
    c.temp_c = 42.0;
    for (name, kind) in [
        ("dm", ModelKind::Diode), ("qm", ModelKind::Npn),
        ("sm", ModelKind::Sw), ("wm", ModelKind::Csw),
        ("mm", ModelKind::Nmos), ("jm", ModelKind::Njf), ("regm", ModelKind::Reg),
    ] {
        c.model(name, kind, &[]);
    }
    // Interleave the two halves' old indices so both need remapping. Several nodes occur
    // only in a control, substrate, rail, line port or expression read.
    let labels = [
        "a", "b", "c", "cp", "cn", "read", "substrate", "pad", "vdd", "vss",
        "boundary", "enable", "bulk", "gate", "jgate", "p1", "n1", "p2", "n2",
    ];
    for label in labels {
        for side in ["left", "right"] {
            c.node(&format!("{side}.{label}"));
        }
    }
    for side in ["left", "right"] {
        let n = |label: &str| format!("{side}.{label}");
        c.r(&n("r1"), &n("a"), &n("b"), 1e3);
        c.r(&n("r2"), &n("c"), "0", 2e3);
        c.c(&n("c1"), &n("b"), &n("c"), 1e-9);
        c.l(&n("l1"), &n("a"), &n("b"), 1e-3);
        c.l(&n("l2"), &n("b"), &n("c"), 2e-3);
        c.k(&n("k1"), &n("l1"), &n("l2"), 0.5);
        c.v(&n("v1"), &n("a"), "0", Source::dc(5.0));
        c.i(&n("i1"), &n("c"), &n("b"), Source::dc(1e-3));
        let node = c.node(&n("boundary"));
        c.devices.boundary.push(Boundary { name: n("cut"), node, kind: BoundaryKind::Voltage, v0: 0.5 });
        c.vctl(&n("g1"), CtlKind::Vccs, &n("c"), &n("b"), &n("cp"), &n("cn"), 1e-3);
        c.ictl(&n("f1"), CtlKind::Cccs, &n("b"), &n("c"), &n("v1"), 2.0);
        let expr = Expr::parse(&format!("v({}, {}) + i({})", n("read"), n("cn"), n("v1")))
            .expect("the behavioural source expression parses");
        c.b(&n("b1"), BKind::I, &n("c"), &n("a"), expr);
        c.sw(&n("s1"), &n("c"), &n("a"), &n("cp"), &n("cn"), "sm", false);
        c.csw(&n("w1"), &n("a"), &n("b"), &n("v1"), "wm", false);
        c.gpio(&n("pin"), &n("pad"), &n("vdd"), &n("vss"), GpioParams {
            r_on_h: 25.0, r_on_l: 20.0, r_pull: 40e3, r_leak: 1e9,
            t_edge: 10e-9, vih: 0.7, vil: 0.3,
        }, PinMode::OutHigh);
        c.d(&n("d1"), &n("b"), &n("c"), "dm");
        c.q4(&n("q1"), &n("a"), &n("b"), &n("c"), &n("substrate"), "qm");
        c.reg(&n("reg1"), &n("a"), &n("vss"), &n("c"), &n("enable"), "regm");
        c.m(&n("m1"), &n("a"), &n("gate"), &n("b"), &n("bulk"), "mm", 10e-6, 1e-6);
        c.jfet(&n("j1"), &n("c"), &n("jgate"), &n("b"), "jm", 1.0);
        c.tline(&n("t1"), &n("p1"), &n("n1"), &n("p2"), &n("n2"), 50.0, 10e-9);
        c.set_ic(&n("substrate"), 0.2);
        c.set_nodeset(&n("read"), 0.3);
    }

    for side in ["left", "right"] {
        let prefix = format!("{side}.");
        let mut part = c.subcircuit(|name| name.starts_with(&prefix));
        let names: Vec<_> = std::iter::once("0".to_string())
            .chain(labels.iter().map(|label| format!("{side}.{label}"))).collect();
        let got: Vec<_> = part.nodes.iter().map(|info| info.name.clone()).collect();
        assert_eq!(got, names, "{side}: exactly the used nodes plus ground, in old index order");
        for (node, info) in part.nodes.iter().enumerate() {
            let old = c.find_node(&info.name).expect("a retained node exists in the whole circuit");
            assert_eq!(info, &c.nodes[old as usize], "{side}: {} keeps its node metadata", info.name);
            assert_eq!(part.find_node(&info.name), Some(node as u32), "{side}: {} has its rebuilt index", info.name);
        }
        assert_eq!(part.find_node("gnd"), Some(0), "{side}: the ground alias is preserved");
        let omitted = if side == "left" { "right.a" } else { "left.a" };
        assert_eq!(part.find_node(omitted), None, "{side}: omitted nodes leave the name index");
        assert_eq!(part.models, c.models, "{side}: model cards are preserved");
        assert_eq!(part.opts, c.opts, "{side}: solver options are preserved");
        assert_eq!(part.temp_c, c.temp_c, "{side}: temperature is preserved");

        let mut expected_walk = Vec::new();
        macro_rules! check {
            ($bank:ident $(, $field:ident)*) => {{
                let originals: Vec<_> = c.devices.$bank.iter().filter(|d| d.name.starts_with(&prefix)).collect();
                assert_eq!(part.devices.$bank.len(), originals.len(), "{side}: {} retains exactly the selected devices", stringify!($bank));
                for (device, original) in part.devices.$bank.iter().zip(originals) {
                    assert_eq!(device.name, original.name, "{side}: {} preserves device order", stringify!($bank));
                    $(
                        assert_eq!(part.nodes.get(device.$field as usize).map(|info| &info.name),
                            Some(&c.nodes[original.$field as usize].name),
                            "{side}: {}.{} keeps its node name", device.name, stringify!($field));
                        expected_walk.push(device.$field);
                    )*
                }
            }};
        }
        check!(r, a, b);
        check!(c, a, b);
        check!(l, a, b);
        check!(k);
        check!(v, p, n);
        check!(i, p, n);
        check!(boundary, node);
        check!(ctl, p, n, cp, cn);
        check!(b, p, n);
        for (device, original) in part.devices.b.iter().zip(c.devices.b.iter().filter(|d| d.name.starts_with(&prefix))) {
            assert_eq!(device.expr, original.expr, "{side}: {} keeps its expression and branch names", device.name);
            assert_eq!(device.reads.len(), original.reads.len(), "{side}: {} keeps every expression read", device.name);
            for ((name, node), (old_name, old_node)) in device.reads.iter().zip(&original.reads) {
                assert_eq!(name, old_name, "{side}: {} keeps the expression read name", device.name);
                assert_eq!(part.nodes.get(*node as usize).map(|info| &info.name), Some(&c.nodes[*old_node as usize].name),
                    "{side}: {} remaps the expression read node", device.name);
                expected_walk.push(*node);
            }
        }
        check!(sw, p, n, cp, cn);
        check!(gpio, pad, vdd, vss);
        check!(d, a, k);
        check!(q, c, b, e, s);
        check!(reg, vin, gnd, vout, en);
        check!(m, d, g, s, b);
        check!(j, d, g, s);
        check!(o, p1, n1, p2, n2);
        let mut visited = Vec::new();
        part.devices.for_each_node(|node| visited.push(node));
        assert_eq!(visited, expected_walk, "{side}: the read-only walk visits every node field in order");
        visited.clear();
        part.devices.for_each_node_mut(|node| visited.push(*node));
        assert_eq!(visited, expected_walk, "{side}: the mutable walk visits every node field in order");
        assert_eq!(part.devices.k[0].l1, format!("{side}.l1"), "{side}: the first coupled inductor name stays intact");
        assert_eq!(part.devices.k[0].l2, format!("{side}.l2"), "{side}: the second coupled inductor name stays intact");
        assert_eq!(part.devices.ctl[1].ctrl, format!("{side}.v1"), "{side}: the controlled source's device name stays intact");
        assert_eq!(part.devices.sw[1].ctrl, Some(format!("{side}.v1")), "{side}: the switch's device name stays intact");
        Solver::new(&part).unwrap_or_else(|err| panic!("{side}: the extracted circuit builds a solver: {err:?}"));
    }
    assert_eq!(c.subcircuit(|_| true), c, "selecting every device preserves the whole circuit");
    let empty = c.subcircuit(|_| false);
    assert_eq!(empty.nodes, c.nodes[..1], "selecting no devices leaves only ground");
    assert_eq!(empty.devices, Banks::default(), "selecting no devices clears every bank");
}

struct Written {
    g: Vec<f64>,
    c: Vec<f64>,
    f: Vec<f64>,
    q: Vec<f64>,
    noncon: u32,
    noncon_at: u32,
}

impl Written {
    fn new(s: &Solver) -> Self {
        Self {
            g: vec![0.0; s.g.len()], c: vec![0.0; s.c.len()],
            f: vec![0.0; s.f.len()], q: vec![0.0; s.q.len()],
            noncon: 0, noncon_at: 0,
        }
    }
    fn clear(&mut self) {
        for v in [&mut self.g, &mut self.c, &mut self.f, &mut self.q] {
            v.fill(0.0);
        }
        self.noncon = 0;
        self.noncon_at = 0;
    }
    fn eval(&mut self, bank: &mut dyn Bank, x: &[f64], cx: &EvalCx, insts: Option<&[u32]>) {
        let mut st = Stamps {
            g: &mut self.g, c: &mut self.c, f: &mut self.f, q: &mut self.q,
            noncon: self.noncon, noncon_at: self.noncon_at,
        };
        if let Some(insts) = insts {
            bank.eval_some(x, cx, &mut st, insts);
        } else {
            bank.eval(x, cx, &mut st);
        }
        self.noncon = st.noncon;
        self.noncon_at = st.noncon_at;
    }
    fn same(&self, other: &Self, kind: &str, name: &str, why: &str) {
        for (entry, a, b) in [
            ("g", &self.g, &other.g), ("c", &self.c, &other.c),
            ("f", &self.f, &other.f), ("q", &self.q, &other.q),
        ] {
            assert_eq!(a.len(), b.len(), "{kind} {name}: {why}, {entry} length");
            for (j, (a, b)) in a.iter().zip(b).enumerate() {
                assert_eq!(a.to_bits(), b.to_bits(), "{kind} {name}: {why}, {entry}[{j}]: {a:e} vs {b:e}");
            }
        }
        assert_eq!(self.noncon, other.noncon, "{kind} {name}: {why}, noncon");
        assert_eq!(self.noncon_at, other.noncon_at, "{kind} {name}: {why}, noncon_at");
    }
}

#[test]
fn instance_footprints_and_subset_evaluation() {
    let mut c = terminal_circuit();
    // The terminal proof keeps its original circuit and checks. Add the missing banks here,
    // with series resistors and charges so their internal nodes and matrix slots participate.
    c.model("jm", ModelKind::Njf, &[
        ("rd", 14.0), ("rs", 9.0), ("cgs", 2e-12), ("cgd", 1e-12),
    ]);
    c.jfet("j1", "jd", "jg", "js", "jm", 1.0);
    c.model("mm", ModelKind::Nmos, &[
        ("rd", 14.0), ("rs", 9.0), ("cbd", 2e-12), ("cbs", 1e-12),
        ("cgso", 1e-10), ("cgdo", 1e-10), ("cgbo", 1e-10), ("tox", 2e-8),
    ]);
    c.m("m1", "md", "mg", "ms", "mb", "mm", 10e-6, 1e-6);
    c.gpio("pin1", "pad", "supply", "0", GpioParams {
        r_on_h: 25.0, r_on_l: 20.0, r_pull: 40e3, r_leak: 1e9,
        t_edge: 10e-9, vih: 0.7, vil: 0.3,
    }, PinMode::OutHigh);
    c.tline("t1", "mix", "0", "line_out", "0", 50.0, 10e-9);
    c.i("i2", "e", "f", Source::wave(Wave::Pwl(vec![(0.0, 0.1e-3), (1e-3, 0.3e-3)])));

    let mut s = Solver::new(&c).unwrap();
    let mut rng = Lcg(0x42414e4b);
    for (v, u) in s.x.iter_mut().zip(&s.unknowns).skip(1) {
        *v = match u.kind {
            UnkKind::Voltage => rng.range(-1.5, 1.5),
            UnkKind::Current => rng.range(-2e-3, 2e-3),
        };
    }
    let mut cx = s.base_cx();
    cx.tran = true;
    cx.t = 0.25e-3;
    cx.limit = false;
    cx.init_junctions = false;
    cx.edge_hold = false;
    let mut first = Written::new(&s);
    let mut second = Written::new(&s);
    let mut moved = Written::new(&s);
    for bank in &mut s.banks {
        let kind = bank.kind();
        for i in 0..bank.len() {
            let name = bank.inst_name(i).to_string();
            let mut unks = Vec::new();
            let mut slots = Vec::new();
            bank.inst_unks(i, &mut unks);
            bank.inst_slots(i, &mut slots);
            let insts = [i as u32];
            first.clear();
            first.eval(&mut **bank, &s.x, &cx, Some(&insts));
            for (entry, values, declared) in [
                ("g", &first.g, &slots), ("c", &first.c, &slots),
                ("f", &first.f, &unks), ("q", &first.q, &unks),
            ] {
                for (j, &v) in values.iter().enumerate() {
                    assert!(v == 0.0 || declared.contains(&(j as u32)), "{kind} {name}: undeclared write {entry}[{j}] = {v:e}");
                }
            }
            if first.noncon != 0 {
                assert_eq!(first.noncon_at, i as u32, "{kind} {name}: singleton noncon_at");
            }
            // Warm the instance's iteration history at x before testing undeclared reads.
            second.clear();
            second.eval(&mut **bank, &s.x, &cx, Some(&insts));
            let mut x = s.x.clone();
            for (j, v) in x.iter_mut().enumerate() {
                if !unks.contains(&(j as u32)) {
                    *v += 0.37;
                }
            }
            moved.clear();
            moved.eval(&mut **bank, &x, &cx, Some(&insts));
            second.same(&moved, kind, &name, "undeclared reads");
        }
    }

    for limit in [false, true] {
        // Fresh histories make the limiting pass exercise the first Newton iteration too.
        let mut all = Solver::new(&c).unwrap();
        let mut some = Solver::new(&c).unwrap();
        all.x.copy_from_slice(&s.x);
        some.x.copy_from_slice(&s.x);
        cx.limit = limit;
        let mut a = Written::new(&all);
        let mut b = Written::new(&some);
        for (all_bank, some_bank) in all.banks.iter_mut().zip(&mut some.banks) {
            let kind = all_bank.kind();
            let names = (0..all_bank.len()).map(|i| all_bank.inst_name(i)).collect::<Vec<_>>().join(", ");
            let insts: Vec<u32> = (0..some_bank.len() as u32).collect();
            a.eval(&mut **all_bank, &all.x, &cx, None);
            b.eval(&mut **some_bank, &some.x, &cx, Some(&insts));
            let why = format!("eval vs eval_some, limit={limit}");
            a.same(&b, kind, &names, &why);
            if kind == "I" {
                // Advancing one source must not mark an omitted source's cached value fresh.
                let mut next = cx.clone();
                next.t += 0.25e-3;
                first.clear();
                second.clear();
                first.eval(&mut **all_bank, &all.x, &next, None);
                for &i in &insts {
                    second.eval(&mut **some_bank, &some.x, &next, Some(&[i]));
                }
                first.same(&second, kind, &names, "separate subsets at a new time");
            }
            let mut a_state = Vec::new();
            let mut b_state = Vec::new();
            all_bank.iter_state(&mut a_state);
            some_bank.iter_state(&mut b_state);
            assert_eq!(a_state.len(), b_state.len(), "{kind} {names}: {why}, iter_state length");
            for (j, (a, b)) in a_state.iter().zip(&b_state).enumerate() {
                assert_eq!(a.to_bits(), b.to_bits(), "{kind} {names}: {why}, iter_state[{j}]");
            }
        }
    }
}
