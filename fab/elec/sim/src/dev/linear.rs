//! R, C, L, K: the linear elements.

use crate::circuit::{Cap, Ind, Mutual, Res};
use crate::stamp::{Bank, EvalCx, Stamps, Tier, Topology, Unk};

/// Resistors with first- and second-order temperature coefficients.
#[derive(Clone)]
pub struct RBank {
    pub inst: Vec<Res>,
    g: Vec<f64>,
    slots: Vec<u32>,
}

impl RBank {
    pub fn new(inst: Vec<Res>) -> Self {
        let g = inst.iter().map(|r| 1.0 / r.r).collect();
        RBank { inst, g, slots: Vec::new() }
    }
    fn eval_iter(&mut self, x: &[f64], _cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let r = &self.inst[i];
            let g = self.g[i];
            st.g4(&self.slots[i * 4..i * 4 + 4], g);
            st.i2(r.a, r.b, g * (x[r.a as usize] - x[r.b as usize]));
        }
    }
}

impl Bank for RBank {
    fn terminals(&self) -> usize {
        2
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], _xdot: &[f64], _cx: &EvalCx, out: &mut [f64]) {
        let r = &self.inst[inst];
        let i = self.g[inst] * (x[r.a as usize] - x[r.b as usize]);
        out[..2].copy_from_slice(&[i, -i]);
    }
    fn kind(&self) -> &'static str {
        "R"
    }
    fn tier(&self, _i: usize) -> Tier {
        Tier::Fixed
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let r = &self.inst[i];
        out.extend_from_slice(&[r.a, r.b]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * 4..(i + 1) * 4]);
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for r in &self.inst {
            if !(r.r.is_finite() && r.r != 0.0) {
                return Err(format!("{}: resistance {} is not usable (use a voltage source for a short)", r.name, r.r));
            }
            self.slots.extend_from_slice(&topo.slot4(r.a, r.b));
        }
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut self.slots
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
    fn temperature(&mut self, temp: f64, tnom: f64) {
        let dt = temp - tnom;
        for (i, r) in self.inst.iter().enumerate() {
            self.g[i] = 1.0 / (r.r * (1.0 + r.tc1 * dt + r.tc2 * dt * dt));
        }
    }
    fn set_param(&mut self, inst: usize, key: &str, v: f64) -> bool {
        match key {
            "r" | "value" if v.is_finite() && v != 0.0 => {
                self.g[inst] = self.g[inst] * self.inst[inst].r / v;
                self.inst[inst].r = v;
                true
            }
            _ => false,
        }
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        matches!(key, "r" | "value").then(|| self.inst[inst].r)
    }
}

#[derive(Clone)]
pub struct CBank {
    pub inst: Vec<Cap>,
    slots: Vec<u32>,
}

impl CBank {
    pub fn new(inst: Vec<Cap>) -> Self {
        CBank { inst, slots: Vec::new() }
    }
    fn eval_iter(&mut self, x: &[f64], _cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let c = &self.inst[i];
            st.c4(&self.slots[i * 4..i * 4 + 4], c.c);
            st.q2(c.a, c.b, c.c * (x[c.a as usize] - x[c.b as usize]));
        }
    }
}

impl Bank for CBank {
    fn terminals(&self) -> usize {
        2
    }
    fn terminal_currents(&self, inst: usize, _x: &[f64], xdot: &[f64], _cx: &EvalCx, out: &mut [f64]) {
        let c = &self.inst[inst];
        let i = c.c * (xdot[c.a as usize] - xdot[c.b as usize]);
        out[..2].copy_from_slice(&[i, -i]);
    }
    fn kind(&self) -> &'static str {
        "C"
    }
    fn tier(&self, _i: usize) -> Tier {
        Tier::Fixed
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let c = &self.inst[i];
        out.extend_from_slice(&[c.a, c.b]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * 4..(i + 1) * 4]);
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for c in &self.inst {
            self.slots.extend_from_slice(&topo.slot4(c.a, c.b));
        }
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut self.slots
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
    fn set_param(&mut self, inst: usize, key: &str, v: f64) -> bool {
        match key {
            "c" | "value" => {
                self.inst[inst].c = v;
                true
            }
            _ => false,
        }
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        matches!(key, "c" | "value").then(|| self.inst[inst].c)
    }
}

/// Inductors: a branch current `i` from a to b; the branch row is `v(a) - v(b) - d(flux)/dt = 0`
/// with the flux `L i` (plus mutual terms from K) as that row's charge.
#[derive(Clone)]
pub struct LBank {
    pub inst: Vec<Ind>,
    pub br: Vec<Unk>,
    slots: Vec<u32>,
}

impl LBank {
    pub fn new(inst: Vec<Ind>) -> Self {
        LBank { inst, br: Vec::new(), slots: Vec::new() }
    }
    fn eval_iter(&mut self, x: &[f64], _cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let l = &self.inst[i];
            let s = &self.slots[i * 5..i * 5 + 5];
            let br = self.br[i] as usize;
            let cur = x[br];
            st.g[s[0] as usize] += 1.0;
            st.g[s[1] as usize] -= 1.0;
            st.g[s[2] as usize] += 1.0;
            st.g[s[3] as usize] -= 1.0;
            st.c[s[4] as usize] -= l.l;
            st.i2(l.a, l.b, cur);
            st.f[br] += x[l.a as usize] - x[l.b as usize];
            st.q[br] -= l.l * cur;
        }
    }
}

impl Bank for LBank {
    fn terminals(&self) -> usize {
        2
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], _xdot: &[f64], _cx: &EvalCx, out: &mut [f64]) {
        let i = x[self.br[inst] as usize];
        out[..2].copy_from_slice(&[i, -i]);
    }
    fn kind(&self) -> &'static str {
        "L"
    }
    fn tier(&self, _i: usize) -> Tier {
        Tier::Fixed
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let l = &self.inst[i];
        out.extend_from_slice(&[l.a, l.b, self.br[i]]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * 5..(i + 1) * 5]);
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for l in &self.inst {
            let br = topo.branch(&l.name);
            self.br.push(br);
            self.slots.extend_from_slice(&[topo.slot(l.a, br), topo.slot(l.b, br), topo.slot(br, l.a), topo.slot(br, l.b), topo.slot(br, br)]);
        }
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut self.slots
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
    fn initial(&self, x: &mut [f64]) {
        for (i, l) in self.inst.iter().enumerate() {
            if let Some(ic) = l.ic {
                x[self.br[i] as usize] = ic;
            }
        }
    }
    fn set_param(&mut self, inst: usize, key: &str, v: f64) -> bool {
        match key {
            "l" | "value" => {
                self.inst[inst].l = v;
                true
            }
            _ => false,
        }
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        matches!(key, "l" | "value").then(|| self.inst[inst].l)
    }
}

/// Mutual inductance between two inductors: `M = k sqrt(L1 L2)` couples their branch fluxes.
#[derive(Clone)]
pub struct KBank {
    pub inst: Vec<Mutual>,
    /// (branch 1, branch 2, M) per instance.
    pair: Vec<(Unk, Unk, f64)>,
    lvals: Vec<(f64, f64)>,
    slots: Vec<u32>,
}

impl KBank {
    /// `lvals[i]` are the two inductances K instance `i` couples.
    pub fn new(inst: Vec<Mutual>, lvals: Vec<(f64, f64)>) -> Self {
        KBank { inst, pair: Vec::new(), lvals, slots: Vec::new() }
    }
    fn eval_iter(&mut self, x: &[f64], _cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let (b1, b2, m) = self.pair[i];
            st.c[self.slots[i * 2] as usize] -= m;
            st.c[self.slots[i * 2 + 1] as usize] -= m;
            st.q[b1 as usize] -= m * x[b2 as usize];
            st.q[b2 as usize] -= m * x[b1 as usize];
        }
    }
}

impl Bank for KBank {
    fn kind(&self) -> &'static str {
        "K"
    }
    fn tier(&self, _i: usize) -> Tier {
        Tier::Fixed
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let (b1, b2, _) = self.pair[i];
        out.extend_from_slice(&[b1, b2]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * 2..(i + 1) * 2]);
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        for (k, &(l1, l2)) in self.inst.iter().zip(&self.lvals) {
            let b1 = topo.branch_of(&k.l1).ok_or_else(|| format!("{}: no inductor named {}", k.name, k.l1))?;
            let b2 = topo.branch_of(&k.l2).ok_or_else(|| format!("{}: no inductor named {}", k.name, k.l2))?;
            if !(k.k.abs() <= 1.0) {
                return Err(format!("{}: coupling {} is outside [-1, 1]", k.name, k.k));
            }
            self.pair.push((b1, b2, k.k * (l1 * l2).sqrt()));
            self.slots.extend_from_slice(&[topo.slot(b1, b2), topo.slot(b2, b1)]);
        }
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut self.slots
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
}
