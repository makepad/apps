//! The three-tier load (ESIMRT S3): nothing is evaluated more often than it can change.
//!
//! Each instance has a tier its bank names (`Bank::tier`):
//! - `Fixed` and `Timed` instances are folded: their stamps (`g`, `c`) are summed once into `gfix`
//!   and `cfix`, and their currents and charges are affine in `x`, so a load reads them as
//!   `f = Gfix x + b` and `q = Cfix x + d` (one pass over the folded entries, row by row). `b` and `d`
//!   are their `f` and `q` at `x = 0`: the `Fixed` part once, the `Timed` part (a source's waveform)
//!   again at each new time.
//! - `Iterate` instances are evaluated at every load, on top: their slots start from the folded
//!   stamps and their banks add their own.
//!
//! The folded banks come before every iterating bank in `dev::build`'s order, so each slot's sum
//! runs in the same order as a whole load's and `g` and `c` come out bit for bit the same; `f` and
//! `q` differ by rounding (the product against the devices' own formulas).
//!
//! A Newton iteration's matrix `g + a0 c` then changes only on the iterating instances' slots while
//! `a0` holds, and on the slots with a folded capacitance when it moves: `Solver::solve_step` writes
//! those, and tells the LU the columns of those whose value moved (`Lu::refactor_dirty`). An
//! iterating device that stamps what it stamped before (a switch or a pad at rest) costs the LU
//! nothing.
//!
//! The plan is built at the first tiered load and dropped by any value edit on a folded instance
//! (`Solver::set_param`, `retune`, `pin_write`), so a changed resistor is stamped from the next
//! load on.

use crate::stamp::{Bank, EvalCx, Stamps, Tier};

#[derive(Clone, Debug)]
pub struct TierPlan {
    /// Per bank, ascending: the `Fixed` instances, the `Timed` ones and the iterating ones.
    fixed: Vec<Vec<u32>>,
    timed: Vec<Vec<u32>>,
    pub iterate: Vec<Vec<u32>>,
    /// The folded stamps by value index.
    pub gfix: Vec<f64>,
    pub cfix: Vec<f64>,
    /// The `Fixed` instances' `f(0)` and `q(0)` by row, and every folded instance's at `at`.
    b_const: Vec<f64>,
    d_const: Vec<f64>,
    b: Vec<f64>,
    d: Vec<f64>,
    /// The rows the `Timed` instances write, and the slots (cleared before each re-read).
    timed_rows: Vec<u32>,
    timed_slots: Vec<u32>,
    /// The time `b` and `d` hold the `Timed` part for (NaN: none yet).
    at: f64,
    /// The folded entries by row: `row_ptr[r]..row_ptr[r + 1]` index `ent_slot` and `ent_col`.
    row_ptr: Vec<u32>,
    ent_slot: Vec<u32>,
    ent_col: Vec<u32>,
    /// The slots iterating instances write, and the column of each (0-based, as the LU numbers
    /// them); `nl_cols` counts the columns.
    pub nl_slots: Vec<u32>,
    pub nl_slot_cols: Vec<u32>,
    nl_cols: usize,
    /// The slots only folded instances write and that hold a capacitance: `g + a0 c` moves with
    /// `a0` there. The column of each, and how many are not iterating columns too.
    pub a0_slots: Vec<u32>,
    pub a0_slot_cols: Vec<u32>,
    a0_cols: usize,
    /// Ascending, every slot a tiered load can leave a nonzero `c` on: the iterating instances'
    /// and the folded capacitances'. The per-step sums over `C` (`Solver::charge_at_solution`)
    /// visit only these, in the same order.
    pub c_slots: Vec<u32>,
    /// The `a0` the solver's matrix holds the folded slots for; NaN when they must all be written.
    pub jac_a0: f64,
    /// Whether `g` and `c` hold `gfix` and `cfix` on every folded-only slot (a whole load
    /// overwrites them).
    pub synced: bool,
    /// Scratch for the `Timed` re-reads: a zero `x` and value arrays nothing reads.
    zeros: Vec<f64>,
    scratch_g: Vec<f64>,
    scratch_c: Vec<f64>,
    scratch_f: Vec<f64>,
    scratch_q: Vec<f64>,
}

fn sorted_unique(v: &mut Vec<u32>) {
    v.sort_unstable();
    v.dedup();
}

impl TierPlan {
    /// The plan for `banks` (after the layout) over `n` unknowns and `len` value indices, with the
    /// row and column (unknowns, 0 for the trash) of each value index. `cx` is the transient
    /// context the folded values are read in (`t` aside).
    pub fn build(banks: &mut [Box<dyn Bank>], n: usize, len: usize, slot_row: &[u32], slot_col: &[u32], cx: &EvalCx) -> TierPlan {
        let nb = banks.len();
        let (mut folded, mut fixed, mut timed, mut iterate) = (vec![Vec::new(); nb], vec![Vec::new(); nb], vec![Vec::new(); nb], vec![Vec::new(); nb]);
        for (bi, b) in banks.iter().enumerate() {
            for i in 0..b.len() {
                match b.tier(i) {
                    Tier::Fixed => {
                        folded[bi].push(i as u32);
                        fixed[bi].push(i as u32);
                    }
                    Tier::Timed => {
                        folded[bi].push(i as u32);
                        timed[bi].push(i as u32);
                    }
                    Tier::Iterate => iterate[bi].push(i as u32),
                }
            }
        }
        // which value indices and rows each tier writes
        let mut is_folded = vec![false; len];
        let mut is_nl = vec![false; len];
        let mut nl_slots = Vec::new();
        let mut timed_rows = Vec::new();
        let mut timed_slots = Vec::new();
        let mut list = Vec::new();
        for (bi, b) in banks.iter().enumerate() {
            for &i in &folded[bi] {
                list.clear();
                b.inst_slots(i as usize, &mut list);
                for &s in &list {
                    is_folded[s as usize] = true;
                }
            }
            for &i in &timed[bi] {
                b.inst_slots(i as usize, &mut timed_slots);
                b.inst_unks(i as usize, &mut timed_rows);
            }
            for &i in &iterate[bi] {
                list.clear();
                b.inst_slots(i as usize, &mut list);
                for &s in &list {
                    if s != 0 && !is_nl[s as usize] {
                        is_nl[s as usize] = true;
                        nl_slots.push(s);
                    }
                }
            }
        }
        is_folded[0] = false;
        sorted_unique(&mut nl_slots);
        sorted_unique(&mut timed_rows);
        sorted_unique(&mut timed_slots);
        timed_rows.retain(|&u| u != 0);
        // the folded stamps, and the `Fixed` instances' f(0) and q(0), at x = 0
        let zeros = vec![0.0; n + 1];
        let mut cx0 = cx.clone();
        cx0.t = 0.0;
        let (mut gfix, mut cfix) = (vec![0.0; len], vec![0.0; len]);
        let (mut f, mut q) = (vec![0.0; n + 1], vec![0.0; n + 1]);
        for (bi, b) in banks.iter_mut().enumerate() {
            if !folded[bi].is_empty() {
                let mut st = Stamps { g: &mut gfix, c: &mut cfix, f: &mut f, q: &mut q, noncon: 0, noncon_at: 0 };
                b.eval_some(&zeros, &cx0, &mut st, &folded[bi]);
            }
        }
        let (mut b_const, mut d_const) = (vec![0.0; n + 1], vec![0.0; n + 1]);
        let (mut sg, mut sc) = (vec![0.0; len], vec![0.0; len]);
        for (bi, b) in banks.iter_mut().enumerate() {
            if !fixed[bi].is_empty() {
                let mut st = Stamps { g: &mut sg, c: &mut sc, f: &mut b_const, q: &mut d_const, noncon: 0, noncon_at: 0 };
                b.eval_some(&zeros, &cx0, &mut st, &fixed[bi]);
            }
        }
        gfix[0] = 0.0;
        cfix[0] = 0.0;
        b_const[0] = 0.0;
        d_const[0] = 0.0;
        // the folded entries, row by row in value-index order
        let mut row_ptr = vec![0u32; n + 2];
        for s in 1..len {
            if is_folded[s] {
                row_ptr[slot_row[s] as usize + 1] += 1;
            }
        }
        for r in 0..=n {
            row_ptr[r + 1] += row_ptr[r];
        }
        let total = row_ptr[n + 1] as usize;
        let (mut ent_slot, mut ent_col) = (vec![0u32; total], vec![0u32; total]);
        let mut fill = row_ptr.clone();
        for s in 1..len {
            if is_folded[s] {
                let r = slot_row[s] as usize;
                let k = fill[r] as usize;
                ent_slot[k] = s as u32;
                ent_col[k] = slot_col[s];
                fill[r] += 1;
            }
        }
        let nl_slot_cols: Vec<u32> = nl_slots.iter().map(|&s| slot_col[s as usize] - 1).collect();
        let mut nl_cols = nl_slot_cols.clone();
        sorted_unique(&mut nl_cols);
        let mut in_nl_col = vec![false; n];
        for &c in &nl_cols {
            in_nl_col[c as usize] = true;
        }
        let a0_slots: Vec<u32> = (1..len as u32).filter(|&s| is_folded[s as usize] && !is_nl[s as usize] && cfix[s as usize].to_bits() != 0).collect();
        let c_slots: Vec<u32> = (1..len as u32).filter(|&s| is_nl[s as usize] || cfix[s as usize].to_bits() != 0).collect();
        let a0_slot_cols: Vec<u32> = a0_slots.iter().map(|&s| slot_col[s as usize] - 1).collect();
        let mut a0_cols: Vec<u32> = a0_slot_cols.iter().copied().filter(|&c| !in_nl_col[c as usize]).collect();
        sorted_unique(&mut a0_cols);
        TierPlan {
            fixed,
            timed,
            iterate,
            gfix,
            cfix,
            b: b_const.clone(),
            d: d_const.clone(),
            b_const,
            d_const,
            timed_rows,
            timed_slots,
            at: f64::NAN,
            row_ptr,
            ent_slot,
            ent_col,
            nl_slots,
            nl_slot_cols,
            nl_cols: nl_cols.len(),
            a0_slots,
            a0_slot_cols,
            a0_cols: a0_cols.len(),
            c_slots,
            jac_a0: f64::NAN,
            synced: false,
            zeros,
            scratch_g: sg,
            scratch_c: sc,
            scratch_f: f,
            scratch_q: q,
        }
    }

    /// `b` and `d` hold the `Timed` part for no instant: the next load re-reads it, at whatever
    /// time (a boundary fed new points, `Solver::feed`).
    pub fn forget_instant(&mut self) {
        self.at = f64::NAN;
    }

    /// Re-reads the `Timed` instances' `f(0)` and `q(0)` at `cx.t` unless `b` and `d` hold them.
    fn at_time(&mut self, banks: &mut [Box<dyn Bank>], cx: &EvalCx) {
        if self.at.to_bits() == cx.t.to_bits() || self.timed_rows.is_empty() {
            self.at = cx.t;
            return;
        }
        for &u in &self.timed_rows {
            self.scratch_f[u as usize] = 0.0;
            self.scratch_q[u as usize] = 0.0;
        }
        for &s in &self.timed_slots {
            self.scratch_g[s as usize] = 0.0;
            self.scratch_c[s as usize] = 0.0;
        }
        for (bi, b) in banks.iter_mut().enumerate() {
            if !self.timed[bi].is_empty() {
                let mut st = Stamps { g: &mut self.scratch_g, c: &mut self.scratch_c, f: &mut self.scratch_f, q: &mut self.scratch_q, noncon: 0, noncon_at: 0 };
                b.eval_some(&self.zeros, cx, &mut st, &self.timed[bi]);
            }
        }
        for &u in &self.timed_rows {
            let u = u as usize;
            self.b[u] = self.b_const[u] + self.scratch_f[u];
            self.d[u] = self.d_const[u] + self.scratch_q[u];
        }
        self.at = cx.t;
    }

    /// A load at `x` and `cx` (transient, `src_scale` 1): `f`, `q` and every slot of `g` and `c`
    /// as a whole load writes them. Returns the non-converged count and the last such
    /// (bank, instance).
    #[allow(clippy::too_many_arguments)]
    /// With `bank_ns` (a profiled run) each bank's evaluation time is added to it, by bank.
    #[allow(clippy::too_many_arguments)]
    pub fn load(&mut self, banks: &mut [Box<dyn Bank>], x: &[f64], cx: &EvalCx, g: &mut [f64], c: &mut [f64], f: &mut [f64], q: &mut [f64],
        mut bank_ns: Option<&mut Vec<u64>>) -> (u32, Option<(usize, u32)>) {
        if !self.synced {
            g.copy_from_slice(&self.gfix);
            c.copy_from_slice(&self.cfix);
            self.jac_a0 = f64::NAN;
            self.synced = true;
        }
        self.at_time(banks, cx);
        // the folded currents and charges: f = b + Gfix x, q = d + Cfix x
        f[0] = 0.0;
        q[0] = 0.0;
        for r in 1..f.len() {
            let (mut fr, mut qr) = (self.b[r], self.d[r]);
            for k in self.row_ptr[r] as usize..self.row_ptr[r + 1] as usize {
                let (s, xc) = (self.ent_slot[k] as usize, x[self.ent_col[k] as usize]);
                fr += self.gfix[s] * xc;
                qr += self.cfix[s] * xc;
            }
            f[r] = fr;
            q[r] = qr;
        }
        for &s in &self.nl_slots {
            g[s as usize] = self.gfix[s as usize];
            c[s as usize] = self.cfix[s as usize];
        }
        let (mut total, mut at) = (0, None);
        if let Some(ns) = bank_ns.as_deref_mut() {
            ns.resize(banks.len(), 0);
        }
        for (bi, b) in banks.iter_mut().enumerate() {
            if self.iterate[bi].is_empty() {
                continue;
            }
            let mut st = Stamps { g: &mut *g, c: &mut *c, f: &mut *f, q: &mut *q, noncon: 0, noncon_at: 0 };
            let since = bank_ns.is_some().then(std::time::Instant::now);
            b.eval_some(x, cx, &mut st, &self.iterate[bi]);
            if let (Some(since), Some(ns)) = (since, bank_ns.as_deref_mut()) {
                ns[bi] += since.elapsed().as_nanos() as u64;
            }
            if st.noncon > 0 {
                total += st.noncon;
                at = Some((bi, st.noncon_at));
            }
        }
        (total, at)
    }

    /// One line: how the instances and value indices split between the tiers.
    pub fn report(&self, banks: &[Box<dyn Bank>]) -> String {
        let count = |v: &Vec<Vec<u32>>| v.iter().map(Vec::len).sum::<usize>();
        let mut out = format!("tiers: {} fixed, {} timed, {} iterating instances; {} folded entries, {} iterating slots over {} columns, {} a0 slots over {} more columns;",
            count(&self.fixed), count(&self.timed), count(&self.iterate), self.ent_slot.len(), self.nl_slots.len(), self.nl_cols, self.a0_slots.len(), self.a0_cols);
        for (bi, b) in banks.iter().enumerate() {
            if !self.iterate[bi].is_empty() {
                out += &format!(" {}x{}", b.kind(), self.iterate[bi].len());
            }
        }
        out
    }
}
