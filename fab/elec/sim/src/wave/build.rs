//! A kernel from a circuit and its operating point: its ports, its root's scattering by modified
//! nodal analysis, and the plan's coefficients.
//!
//! What the root holds, and how:
//! - resistors as conductances (at the circuit's temperature);
//! - independent voltage and current sources as inputs: a source whose value never changes adds
//!   into the constant input's coefficients, any other is an input of its own, its value at each
//!   sample time given by the caller ([`Kernel::inputs_at`]);
//! - voltage-controlled current and voltage sources as they are;
//! - behavioural current sources linearised at the operating point: their slope there in every
//!   voltage they read, and the rest as a constant current (an op-amp's output stage and its rail
//!   clamp, whose slope is zero between the rails).
//!
//! Each capacitor is a port of resistance `T / 2C`, its wave `a` an injection `a / R` beside the
//! conductance `1 / R`. One dense factorisation of the root's matrix gives each port's voltage and
//! each probed node's as a linear function of the ports' waves and the inputs; `b = 2 v - a`.
//! Any other device is refused with its name: the kernel does not model it yet.

use super::WavePlan;
use crate::circuit::{BKind, CtlKind, Node, SimCircuit, Source, Wave, GND};
use crate::expr::{Env, Leaf, Ref, Scratch};
use crate::sparse::dense::DenseLu;

/// What a kernel input is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// The constant one: the root's fixed currents and voltages, folded into its coefficients.
    One,
    /// Independent voltage source `devices.v[i]`'s value.
    V(usize),
    /// Independent current source `devices.i[i]`'s value.
    I(usize),
}

/// A built kernel: its plan, its coefficients and its state at the operating point.
#[derive(Clone, Debug)]
pub struct Kernel {
    pub plan: WavePlan,
    pub consts: Vec<f64>,
    /// The ports' incident waves at the operating point (a capacitor's: its voltage there).
    pub a0: Vec<f64>,
    /// The ports' names (the capacitors), in port order.
    pub ports: Vec<String>,
    /// What each input is, in input order; input 0 is the constant one.
    pub inputs: Vec<Input>,
    /// The node each output is, in output order.
    pub outputs: Vec<Node>,
    /// The largest `|b - a|` over the ports at the operating point with the inputs at time 0: a
    /// kernel at rest reflects what it is sent, so this is the build's own check (rounding only).
    pub rest_error: f64,
}

/// The constant value of a source that never changes, or None.
fn constant(src: &Source) -> Option<f64> {
    (matches!(src.wave, Wave::None) && src.then.is_none()).then(|| src.dc.unwrap_or(0.0))
}

/// The kernel of all of `c` at sample period `period`, its operating point `x0` (each node's
/// voltage, by node index), its outputs the voltages of `probes`. O(m^3 + (ports + inputs) m^2)
/// for `m` nodes and voltage-source branches.
pub fn build(c: &SimCircuit, x0: &[f64], period: f64, probes: &[Node]) -> Result<Kernel, String> {
    let d = &c.devices;
    let refuse = |what: &str, names: Vec<&str>| -> Result<(), String> {
        match names.first() {
            Some(name) => Err(format!("{what} {name}: the wave kernel does not model it yet")),
            None => Ok(()),
        }
    };
    refuse("inductor", d.l.iter().map(|x| x.name.as_str()).collect())?;
    refuse("coupling", d.k.iter().map(|x| x.name.as_str()).collect())?;
    refuse("boundary source", d.boundary.iter().map(|x| x.name.as_str()).collect())?;
    refuse("switch", d.sw.iter().map(|x| x.name.as_str()).collect())?;
    refuse("pin", d.gpio.iter().map(|x| x.name.as_str()).collect())?;
    refuse("timing cell", d.cell.iter().map(|x| x.name.as_str()).collect())?;
    refuse("diode", d.d.iter().map(|x| x.name.as_str()).collect())?;
    refuse("transistor", d.q.iter().map(|x| x.name.as_str()).collect())?;
    refuse("regulator", d.reg.iter().map(|x| x.name.as_str()).collect())?;
    refuse("MOSFET", d.m.iter().map(|x| x.name.as_str()).collect())?;
    refuse("JFET", d.j.iter().map(|x| x.name.as_str()).collect())?;
    refuse("line", d.o.iter().map(|x| x.name.as_str()).collect())?;
    refuse("current-controlled source", d.ctl.iter().filter(|x| matches!(x.kind, CtlKind::Cccs | CtlKind::Ccvs)).map(|x| x.name.as_str()).collect())?;
    refuse("behavioural source", d.b.iter().filter(|x| x.kind != BKind::I).map(|x| x.name.as_str()).collect())?;

    let nodes = c.nodes.len();
    assert_eq!(x0.len(), nodes, "one operating-point voltage a node");
    let vcvs: Vec<usize> = (0..d.ctl.len()).filter(|&k| d.ctl[k].kind == CtlKind::Vcvs).collect();
    // unknowns: nodes 1.. then each voltage source's branch, then each VCVS's
    let m = nodes - 1 + d.v.len() + vcvs.len();
    let row = |n: Node| (n != GND).then(|| n as usize - 1);
    let mut a = vec![0.0; 1 + m * m];
    let mut add = |r: Option<usize>, col: Option<usize>, v: f64| {
        if let (Some(r), Some(col)) = (r, col) {
            a[1 + r * m + col] += v;
        }
    };
    let conductance = |add: &mut dyn FnMut(Option<usize>, Option<usize>, f64), p: Node, n: Node, g: f64| {
        add(row(p), row(p), g);
        add(row(n), row(n), g);
        add(row(p), row(n), -g);
        add(row(n), row(p), -g);
    };
    let dt = c.temp_c - c.opts.tnom;
    for r in d.r.iter().filter(|r| r.a != r.b) {
        let ohms = r.r * (1.0 + r.tc1 * dt + r.tc2 * dt * dt);
        conductance(&mut add, r.a, r.b, 1.0 / ohms);
    }
    let caps: Vec<usize> = (0..d.c.len()).filter(|&k| d.c[k].a != d.c[k].b).collect();
    for &k in &caps {
        conductance(&mut add, d.c[k].a, d.c[k].b, 2.0 * d.c[k].c / period);
    }

    // inputs: the constant one, then each source whose value changes
    let mut inputs = vec![Input::One];
    let mut rhs: Vec<Vec<f64>> = vec![vec![0.0; m]];
    let put = |rhs: &mut Vec<Vec<f64>>, col: usize, r: Option<usize>, v: f64| {
        if let Some(r) = r {
            rhs[col][r] += v;
        }
    };
    for (k, v) in d.v.iter().enumerate() {
        let br = nodes - 1 + k;
        add(row(v.p), Some(br), 1.0);
        add(row(v.n), Some(br), -1.0);
        add(Some(br), row(v.p), 1.0);
        add(Some(br), row(v.n), -1.0);
        match constant(&v.src) {
            Some(value) => put(&mut rhs, 0, Some(br), value),
            None => {
                inputs.push(Input::V(k));
                rhs.push(vec![0.0; m]);
                let col = rhs.len() - 1;
                put(&mut rhs, col, Some(br), 1.0);
            }
        }
    }
    for (k, s) in d.i.iter().enumerate() {
        // a current from p through the source to n
        let col = match constant(&s.src) {
            Some(value) => {
                put(&mut rhs, 0, row(s.p), -value);
                put(&mut rhs, 0, row(s.n), value);
                continue;
            }
            None => {
                inputs.push(Input::I(k));
                rhs.push(vec![0.0; m]);
                rhs.len() - 1
            }
        };
        put(&mut rhs, col, row(s.p), -1.0);
        put(&mut rhs, col, row(s.n), 1.0);
    }
    for (j, &k) in vcvs.iter().enumerate() {
        let e = &d.ctl[k];
        let br = nodes - 1 + d.v.len() + j;
        add(row(e.p), Some(br), 1.0);
        add(row(e.n), Some(br), -1.0);
        add(Some(br), row(e.p), 1.0);
        add(Some(br), row(e.n), -1.0);
        add(Some(br), row(e.cp), -e.gain);
        add(Some(br), row(e.cn), e.gain);
    }
    for g in d.ctl.iter().filter(|x| x.kind == CtlKind::Vccs) {
        // a current gain * v(cp, cn) from p through the source to n
        add(row(g.p), row(g.cp), g.gain);
        add(row(g.p), row(g.cn), -g.gain);
        add(row(g.n), row(g.cp), -g.gain);
        add(row(g.n), row(g.cn), g.gain);
    }
    let mut scratch = Scratch::default();
    for b in &d.b {
        let tape = b.expr.compile(&mut |r| match r {
            Ref::Node(name) => {
                let node = b.reads.iter().find(|(s, _)| s == name).map(|&(_, node)| node).ok_or_else(|| format!("no node named {name}"))?;
                Ok(if node == GND { Leaf::Value(0.0) } else { Leaf::Unknown(node) })
            }
            Ref::Branch(dev) => Err(format!("reads the current of {dev}")),
            Ref::Param(p) => Err(format!("unknown parameter {p}")),
        });
        let tape = tape.map_err(|e| format!("behavioural source {}: {e}: the wave kernel does not model it yet", b.name))?;
        if tape.reads_time() {
            return Err(format!("behavioural source {} reads the time: the wave kernel does not model it yet", b.name));
        }
        scratch.fit(&tape);
        let mut grad = vec![0.0; tape.inputs().len()];
        let f = tape.eval(x0, Env { time: 0.0, temper: c.temp_c }, &mut scratch, &mut grad);
        // a current f + sum g (v - v0) from p through the source to n
        let mut rest = f;
        for (&node, &g) in tape.inputs().iter().zip(&grad) {
            add(row(b.p), row(node), g);
            add(row(b.n), row(node), -g);
            rest -= g * x0[node as usize];
        }
        put(&mut rhs, 0, row(b.p), -rest);
        put(&mut rhs, 0, row(b.n), rest);
    }

    let mut lu = DenseLu::new(m);
    lu.factor(&a).map_err(|s| format!("the root's matrix is singular at unknown {}", s.col + 1))?;
    let solve = |lu: &mut DenseLu<f64>, mut b: Vec<f64>| {
        lu.solve(&mut b);
        b
    };
    let volt = |x: &[f64], n: Node| row(n).map_or(0.0, |r| x[r]);
    // each port's wave as an injection 2C/T * a into its a node, out of its b node
    let port_cols: Vec<Vec<f64>> = caps.iter().map(|&k| {
        let cap = &d.c[k];
        let g = 2.0 * cap.c / period;
        let mut e = vec![0.0; m];
        if let Some(r) = row(cap.a) {
            e[r] += g;
        }
        if let Some(r) = row(cap.b) {
            e[r] -= g;
        }
        solve(&mut lu, e)
    }).collect();
    let input_cols: Vec<Vec<f64>> = rhs.into_iter().map(|e| solve(&mut lu, e)).collect();

    let ports = caps.len();
    let mut rows = Vec::with_capacity(ports + probes.len());
    let mut terms = Vec::new();
    let mut consts = Vec::new();
    let mut row_of = |coef: &mut dyn FnMut(&[f64], Option<usize>) -> f64| {
        let start = terms.len() as u32;
        for (j, col) in port_cols.iter().enumerate() {
            let v = coef(col, Some(j));
            if v != 0.0 {
                terms.push(j as u32);
                consts.push(v);
            }
        }
        for (i, col) in input_cols.iter().enumerate() {
            let v = coef(col, None);
            if v != 0.0 {
                terms.push((ports + i) as u32);
                consts.push(v);
            }
        }
        rows.push((start, terms.len() as u32));
    };
    for &k in &caps {
        let cap = &d.c[k];
        let me = caps.iter().position(|&q| q == k);
        row_of(&mut |x, j| 2.0 * (volt(x, cap.a) - volt(x, cap.b)) - if j.is_some() && j == me { 1.0 } else { 0.0 });
    }
    for &n in probes {
        row_of(&mut |x, _| volt(x, n));
    }
    let plan = WavePlan { ports: ports as u32, inputs: inputs.len() as u32, outputs: probes.len() as u32, rows, terms };
    let a0: Vec<f64> = caps.iter().map(|&k| x0[d.c[k].a as usize] - x0[d.c[k].b as usize]).collect();
    let mut kernel = Kernel {
        plan, consts, a0, ports: caps.iter().map(|&k| d.c[k].name.clone()).collect(), inputs, outputs: probes.to_vec(), rest_error: 0.0,
    };
    let mut u = vec![0.0; kernel.inputs.len()];
    kernel.inputs_at(c, 0.0, &mut u);
    let (mut a, mut b, mut y) = (kernel.a0.clone(), vec![0.0; ports], vec![0.0; probes.len()]);
    kernel.plan.run(&kernel.consts, &mut a, &u, &mut y, &mut b, 1);
    kernel.rest_error = a.iter().zip(&kernel.a0).fold(0.0, |e, (b, a)| f64::max(e, (b - a).abs()));
    Ok(kernel)
}

impl Kernel {
    /// The inputs' values at time `t` into `out` (one a kernel input). O(inputs).
    pub fn inputs_at(&self, c: &SimCircuit, t: f64, out: &mut [f64]) {
        for (o, input) in out.iter_mut().zip(&self.inputs) {
            *o = match *input {
                Input::One => 1.0,
                Input::V(k) => c.devices.v[k].src.value_at(t),
                Input::I(k) => c.devices.i[k].src.value_at(t),
            };
        }
    }
}
