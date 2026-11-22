//! Shichman-Hodges level-1 JFETs: the square-law channel, two gate junctions and their
//! depletion charges, optional drain/source resistances and junction temperature scaling.
//! The channel's vto (default -2 V), beta and lambda retain their card values in normalised
//! coordinates for either polarity. Noise parameters kf, af, nlev and gdsnoi are unused.
//! Charges are functions of the junction voltages alone.

use super::junction::{self, depletion, exp_lin, pnjlim, vcrit};
use crate::circuit::{Jfet, ModelCard, ModelKind};
use crate::stamp::{Bank, EvalCx, Stamps, Topology, Unk};

#[derive(Clone, Debug)]
pub struct JfetModel {
    pub p: f64,
    pub vto: f64,
    pub beta: f64,
    pub lambda: f64,
    pub rd: f64,
    pub rs: f64,
    pub cgs: f64,
    pub cgd: f64,
    pub pb: f64,
    pub m: f64,
    pub fc: f64,
    pub is: f64,
    pub n: f64,
    pub xti: f64,
    pub eg: f64,
    /// Kelvin; absent takes the circuit nominal temperature (27 C by default).
    pub tnom: Option<f64>,
    pub level: f64,
    unsupported: Option<(&'static str, f64)>,
}

impl JfetModel {
    pub fn from_card(m: &ModelCard) -> Self {
        let unsupported = [
            ("b", 1.0), ("isr", 0.0), ("alpha", 0.0), ("vk", 0.0),
            ("tcv", 0.0), ("vtotc", 0.0), ("bex", 0.0), ("betatce", 0.0),
        ].into_iter().find_map(|(key, default)| {
            let value = m.or(key, default);
            (value != default).then_some((key, value))
        });
        Self {
            p: if m.kind == ModelKind::Pjf { -1.0 } else { 1.0 },
            vto: m.get("vto").or(m.get("vt0")).unwrap_or(-2.0),
            beta: m.or("beta", 1e-4), lambda: m.or("lambda", 0.0),
            rd: m.or("rd", 0.0), rs: m.or("rs", 0.0),
            cgs: m.or("cgs", 0.0), cgd: m.or("cgd", 0.0),
            pb: m.or("pb", 1.0), m: m.or("m", 0.5), fc: m.or("fc", 0.5),
            is: m.or("is", 1e-14), n: m.or("n", 1.0),
            xti: m.or("xti", 3.0), eg: m.or("eg", 1.11),
            tnom: m.get("tnom").map(|t| t + 273.15), level: m.or("level", 1.0), unsupported,
        }
    }

    fn validate(&self, inst: &Jfet) -> Result<(), String> {
        if self.level != 1.0 {
            return Err(format!("{}: model {}: level {} is not supported (JFET level 1 only)", inst.name, inst.model, self.level));
        }
        if let Some((key, value)) = self.unsupported {
            return Err(format!("{}: model {}: {key} = {value} is not supported yet", inst.name, inst.model));
        }
        for (key, value) in [("area", inst.area), ("pb", self.pb), ("n", self.n), ("tnom (kelvin)", self.tnom.unwrap_or(300.15))] {
            if !value.is_finite() || value <= 0.0 {
                return Err(format!("{}: {key} must be finite and positive", inst.name));
            }
        }
        for (key, value) in [
            ("beta", self.beta), ("lambda", self.lambda), ("rd", self.rd), ("rs", self.rs),
            ("cgs", self.cgs), ("cgd", self.cgd), ("is", self.is), ("eg", self.eg),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{}: {key} must be finite and nonnegative", inst.name));
            }
        }
        for (key, value) in [("m", self.m), ("fc", self.fc)] {
            if !value.is_finite() || !(0.0..1.0).contains(&value) {
                return Err(format!("{}: {key} must be in [0, 1)", inst.name));
            }
        }
        if !self.vto.is_finite() || !self.xti.is_finite() || (self.p != -1.0 && self.p != 1.0) {
            return Err(format!("{}: invalid threshold, temperature exponent or polarity", inst.name));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct Derived {
    beta: f64,
    nvt: f64,
    is: f64,
    pb: f64,
    caps: [f64; 2],
    vcrit: f64,
    gd: f64,
    gs: f64,
}

impl Derived {
    fn new(m: &JfetModel, inst: &Jfet, temp: f64, tnom: f64) -> Self {
        let tn = m.tnom.unwrap_or(tnom);
        let nvt = m.n * junction::vt(temp);
        let is = junction::is_at(m.is, temp, tn, m.eg, m.xti, m.n) * inst.area;
        let pb = junction::pb_at(m.pb, temp, tn);
        Self {
            beta: m.beta * inst.area, nvt, is, pb,
            caps: [m.cgs, m.cgd].map(|c| junction::cj_at(c, m.m, temp, tn, m.pb, pb) * inst.area),
            vcrit: vcrit(nvt, is),
            gd: if m.rd > 0.0 { inst.area / m.rd } else { 0.0 },
            gs: if m.rs > 0.0 { inst.area / m.rs } else { 0.0 },
        }
    }
}

/// A value and its analytic partials in normalised terminal order [d, g, s].
#[derive(Clone, Copy, Debug, Default)]
struct Bias {
    v: f64,
    d: [f64; 3],
}

impl Bias {
    fn linearised(self, delta: [f64; 3]) -> f64 {
        self.v + self.d.iter().zip(delta).map(|(g, v)| g * v).sum::<f64>()
    }
    fn branch(v: f64, g: f64, a: usize, b: usize) -> Self {
        let mut d = [0.0; 3];
        d[a] = g;
        d[b] = -g;
        Self { v, d }
    }
}

fn pair(rows: &mut [Bias; 3], a: usize, b: usize, value: Bias) {
    rows[a].v += value.v;
    rows[b].v -= value.v;
    for k in 0..3 {
        rows[a].d[k] += value.d[k];
        rows[b].d[k] -= value.d[k];
    }
}

/// Channel current from the forward drain to the forward source, with terminal partials.
fn channel(m: &JfetModel, d: &Derived, point: [f64; 3]) -> (usize, usize, Bias) {
    let (drain, source) = if point[0] < point[2] { (2, 0) } else { (0, 2) };
    let vds = point[drain] - point[source];
    let vgst = point[1] - point[source] - m.vto;
    let clm = 1.0 + m.lambda * vds;
    let (id, gm, gds) = if vgst <= 0.0 {
        (0.0, 0.0, 0.0)
    } else if vds < vgst {
        let base = d.beta * vds * (2.0 * vgst - vds);
        (base * clm, 2.0 * d.beta * vds * clm, 2.0 * d.beta * (vgst - vds) * clm + base * m.lambda)
    } else {
        let base = d.beta * vgst * vgst;
        (base * clm, 2.0 * d.beta * vgst * clm, base * m.lambda)
    };
    let mut partials = [0.0; 3];
    partials[drain] = gds;
    partials[1] = gm;
    partials[source] = -gm - gds;
    (drain, source, Bias { v: id, d: partials })
}

#[derive(Clone, Copy, Debug, Default)]
struct Rows {
    i: [Bias; 3],
    q: [Bias; 3],
}

/// The two limited junction voltages define the channel point as well. Tangent-extend each
/// contribution back to the requested point, as the MOS/diode banks do when limiting.
fn evaluate(m: &JfetModel, d: &Derived, raw: [f64; 3], jun: [f64; 2], gmin: f64) -> Rows {
    let point = [jun[0] - jun[1], jun[0], 0.0];
    let (drain, source, mut ids) = channel(m, d, point);
    ids.v = ids.linearised(std::array::from_fn(|k| raw[k] - point[k]));
    let mut rows = Rows::default();
    pair(&mut rows.i, drain, source, ids);
    for (k, terminal) in [2, 0].into_iter().enumerate() {
        let voltage = raw[1] - raw[terminal];
        let (e, de) = exp_lin(jun[k] / d.nvt);
        let current = d.is * (e - 1.0);
        let conductance = d.is * de / d.nvt;
        pair(&mut rows.i, 1, terminal, Bias::branch(
            current + conductance * (voltage - jun[k]) + gmin * voltage,
            conductance + gmin, 1, terminal,
        ));
        let (q, c) = depletion(jun[k], d.caps[k], d.pb, m.m, m.fc);
        pair(&mut rows.q, 1, terminal, Bias::branch(q + c * (voltage - jun[k]), c, 1, terminal));
    }
    rows
}

#[derive(Clone, Copy, Debug, Default)]
struct Last {
    raw: [f64; 3],
    jun: [f64; 2],
    i: [Bias; 3],
}

fn voltages(nodes: [Unk; 3], x: &[f64], p: f64) -> Option<[f64; 3]> {
    let [d, g, s] = nodes.map(|u| x.get(u as usize).copied());
    let s = s?;
    Some([p * (d? - s), p * (g? - s), 0.0])
}

#[derive(Clone)]
pub struct JfetBank {
    inst: Vec<Jfet>,
    model: Vec<JfetModel>,
    der: Vec<Derived>,
    last: Vec<Last>,
    nodes: Vec<[Unk; 3]>,
    slots: Vec<u32>,
}

// Intrinsic 3x3, then the drain and source resistors.
const SLOTS: usize = 17;

impl JfetBank {
    pub fn new(inst: Vec<Jfet>, model: Vec<JfetModel>) -> Self {
        let n = inst.len();
        Self { inst, model, der: vec![Derived::default(); n], last: vec![Last::default(); n], nodes: Vec::new(), slots: Vec::new() }
    }
    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let nodes = self.nodes[i];
            let slots = &self.slots[i * SLOTS..(i + 1) * SLOTS];
            let (inst, m, d) = (&self.inst[i], &self.model[i], &self.der[i]);
            let Some(raw) = voltages(nodes, x, m.p) else { st.not_converged(i); continue };
            if nodes.into_iter().chain([inst.d, inst.s]).any(|u| u as usize >= st.f.len().min(st.q.len()).min(x.len()))
                || slots.iter().any(|&s| s as usize >= st.g.len().min(st.c.len())) {
                st.not_converged(i);
                continue;
            }
            let last = self.last[i];
            let mut jun = [raw[1] - raw[2], raw[1] - raw[0]];
            if cx.limit {
                for (k, v) in jun.iter_mut().enumerate() {
                    if cx.init_junctions {
                        st.not_converged(i);
                        *v = if !inst.off && *v > 0.0 && d.vcrit.is_finite() { d.vcrit } else { 0.0 };
                    } else {
                        let (limited, changed) = pnjlim(*v, last.jun[k], d.nvt, d.vcrit);
                        if changed {
                            st.not_converged(i);
                        }
                        *v = limited;
                    }
                }
            }
            let rows = evaluate(m, d, raw, jun, cx.gmin);
            let step = std::array::from_fn(|k| raw[k] - last.raw[k]);
            if last.i.iter().zip(rows.i).any(|(old, new)| {
                let pred = old.linearised(step);
                (pred - new.v).abs() > cx.reltol * pred.abs().max(new.v.abs()) + cx.abstol
            }) {
                st.not_converged(i);
            }
            self.last[i] = Last { raw, jun, i: rows.i };
            for (r, node) in nodes.into_iter().enumerate() {
                st.f[node as usize] += m.p * rows.i[r].v;
                st.q[node as usize] += m.p * rows.q[r].v;
                for col in 0..3 {
                    let slot = slots[3 * r + col] as usize;
                    // Input and output polarities cancel in each derivative.
                    st.g[slot] += rows.i[r].d[col];
                    st.c[slot] += rows.q[r].d[col];
                }
            }
            for (ext, inner, g, start) in [(inst.d, nodes[0], d.gd, 9), (inst.s, nodes[2], d.gs, 13)] {
                if g > 0.0 {
                    st.g4(&slots[start..start + 4], g);
                    st.i2(ext, inner, g * (x[ext as usize] - x[inner as usize]));
                }
            }
        }
    }
}

impl Bank for JfetBank {
    fn kind(&self) -> &'static str {
        "J"
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        self.inst.get(i).map_or("", |inst| inst.name.as_str())
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let j = &self.inst[i];
        out.extend_from_slice(&[j.d, j.g, j.s]);
        out.extend_from_slice(&self.nodes[i]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * SLOTS..(i + 1) * SLOTS]);
    }
    fn terminals(&self) -> usize {
        3
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        if self.inst.len() != self.model.len() {
            return Err("JFET bank: instance/model count mismatch".into());
        }
        self.nodes.clear();
        self.slots.clear();
        for (inst, m) in self.inst.iter().zip(&self.model) {
            m.validate(inst)?;
            if [inst.d, inst.g, inst.s].iter().any(|&u| u as usize >= topo.unknowns.len()) {
                return Err(format!("{}: invalid terminal node", inst.name));
            }
            let di = if m.rd > 0.0 { topo.internal(&inst.name, "d") } else { inst.d };
            let si = if m.rs > 0.0 { topo.internal(&inst.name, "s") } else { inst.s };
            let nodes = [di, inst.g, si];
            self.nodes.push(nodes);
            for row in nodes {
                for col in nodes {
                    self.slots.push(topo.slot(row, col));
                }
            }
            self.slots.extend_from_slice(&if m.rd > 0.0 { topo.slot4(inst.d, di) } else { [0; 4] });
            self.slots.extend_from_slice(&if m.rs > 0.0 { topo.slot4(inst.s, si) } else { [0; 4] });
        }
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut self.slots
    }
    fn temperature(&mut self, temp: f64, tnom: f64) {
        for ((inst, m), d) in self.inst.iter().zip(&self.model).zip(&mut self.der) {
            *d = Derived::new(m, inst, temp, tnom);
        }
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], xdot: &[f64], cx: &EvalCx, out: &mut [f64]) {
        let (Some(inst), Some(m), Some(d), Some(&nodes)) =
            (self.inst.get(inst), self.model.get(inst), self.der.get(inst), self.nodes.get(inst)) else { return };
        let (Some(raw), Some(dot)) = (voltages(nodes, x, m.p), voltages(nodes, xdot, m.p)) else { return };
        let rows = evaluate(m, d, raw, [raw[1] - raw[2], raw[1] - raw[0]], cx.gmin);
        for (k, output) in out.iter_mut().take(3).enumerate() {
            *output = m.p * (rows.i[k].v + rows.q[k].d.iter().zip(dot).map(|(c, dv)| c * dv).sum::<f64>());
            let resistor = match k {
                0 if d.gd > 0.0 => Some((inst.d, nodes[0], d.gd)),
                2 if d.gs > 0.0 => Some((inst.s, nodes[2], d.gs)),
                _ => None,
            };
            if let Some((ext, inner, g)) = resistor {
                if let (Some(&a), Some(&b)) = (x.get(ext as usize), x.get(inner as usize)) {
                    *output = g * (a - b);
                }
            }
        }
    }
    fn iter_state(&self, out: &mut Vec<f64>) {
        for last in &self.last {
            out.extend_from_slice(&last.raw);
            out.extend_from_slice(&last.jun);
            for current in &last.i {
                out.push(current.v);
                out.extend_from_slice(&current.d);
            }
        }
    }
    fn state(&self, inst: usize) -> Vec<f64> {
        self.last.get(inst).map_or_else(Vec::new, |last| last.jun.to_vec())
    }
    fn set_state(&mut self, inst: usize, state: &[f64]) {
        if let (Some(last), [gs, gd, ..]) = (self.last.get_mut(inst), state) {
            if gs.is_finite() && gd.is_finite() {
                *last = Last { raw: [gs - gd, *gs, 0.0], jun: [*gs, *gd], ..Last::default() };
            }
        }
    }
}
