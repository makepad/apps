//! Left-looking sparse LU (Gilbert-Peierls), generic over the scalar (E3 1.4 steps 3-5).
//!
//! Column `k` of the factor is a sparse triangular solve with the columns already done: a
//! depth-first search over L finds the column's nonzero pattern in topological order, then the
//! numeric update runs over exactly that pattern. Threshold partial pivoting keeps the ordering's
//! preferred row when it is at least `PIVOT_TOL` of the column's largest candidate.
//!
//! `refactor` reuses the pattern and the pivot sequence (the normal Newton iteration); it asks for
//! a full factor when a kept pivot has become too small against its column. Each successful factor
//! packs its numeric entries into U, diagonal and L regions and resolves the refactor program once.
//!
//! The factor keeps the ordering's block upper triangular form (`Ordering::blocks`): only the
//! diagonal blocks are factored. A column's entries in an earlier block's rows stay as they are
//! in the matrix, the last part of its U range (`off`), and the solves apply them between blocks,
//! the last block first. Pivots never leave their block (an earlier block's rows are all pivoted,
//! a later block's rows have no entry in the column), so this is exact; it only skips the fill
//! and the work of the off-diagonal blocks, which neither the factor nor a refactor needs.

use super::compiled::{self, CompileStats, CompiledLu, LuPlan, PlanColumn, PlanCompiler, PlanJit};
use super::order::{Ordering, NONE};
use super::pattern::Pattern;
use crate::num::Scalar;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

/// A compile's answer, set once by the job on the host's pool.
type Answer = Arc<OnceLock<Option<Arc<dyn CompiledLu>>>>;

/// A pivot is kept while it is at least this fraction of its column's largest candidate.
pub const PIVOT_TOL: f64 = 1e-3;

/// The factor met a column with no usable pivot: the matrix is singular there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Singular {
    /// The original column (unknown index - 1).
    pub col: u32,
}

/// The diagonal slot, L range and refactor program for a column.
#[derive(Clone)]
struct Column {
    diag: u32,
    lower: Range<u32>,
    loads: Range<u32>,
    updates: Range<u32>,
}

#[derive(Clone)]
struct Load<S> {
    slot: u32,
    /// Zero means fill, not the value array's trash cell.
    value: u32,
    last: S,
}

#[derive(Clone)]
struct Update {
    u_slot: u32,
    source: u32,
    subs: Range<u32>,
}

#[derive(Clone)]
struct Subtract {
    dst: u32,
    l_slot: u32,
}

#[derive(Clone)]
pub struct SparseLu<S> {
    n: usize,
    pat: Arc<Pattern>,
    ord: Arc<Ordering>,
    /// Factor step of each original column.
    qinv: Vec<u32>,
    piv: Vec<u32>,
    pinv: Vec<u32>,
    l_ptr: Vec<u32>,
    l_row: Vec<u32>,
    u_ptr: Vec<u32>,
    u_step: Vec<u32>,
    /// Per step, where its U entries in earlier blocks' rows begin (they end at `u_ptr[k + 1]`):
    /// matrix values, never updated.
    u_off: Vec<u32>,
    /// Each step's diagonal block's first step.
    block_of: Vec<u32>,
    /// The diagonal blocks' first steps, then `n`.
    starts: Vec<u32>,
    /// U in `u_step` order, then `n` diagonals, then L in `l_row` order.
    slots: Vec<S>,
    /// One reciprocal per pivot, shared by L scaling and both solves.
    recip: Vec<S>,
    columns: Vec<Column>,
    loads: Vec<Load<S>>,
    updates: Vec<Update>,
    subs: Vec<Subtract>,
    /// A dirty refactor's dirty steps, and any refactor's recomputed ones. Bitsets make resetting
    /// the flags O(n/64), even when a dirty refactor follows one that visited every column.
    pending: Vec<u64>,
    recomputed: Vec<u64>,
    remembered: bool,
    /// The full factor's numeric columns (L, U, the diagonals) and a step's entries in earlier
    /// blocks' rows, and `build_plan`'s row of each slot and slot and value of each row: cleared
    /// for each factor at the largest sizes it has needed, not made again.
    scratch_l: Vec<S>,
    scratch_u: Vec<S>,
    scratch_diag: Vec<S>,
    scratch_off: Vec<u32>,
    scratch_slot_row: Vec<u32>,
    scratch_row_slot: Vec<u32>,
    scratch_row_value: Vec<u32>,
    /// Dense workspace used only by the full factor's pivot search.
    x: Vec<S>,
    w: Vec<S>,
    mark: Vec<u32>,
    stamp: u32,
    stack: Vec<(u32, u32)>,
    topo: Vec<u32>,
    touched: Vec<u32>,
    have: bool,
    pub factors: u64,
    /// Full factors whose pivot sequence differed from the one before.
    pub pivot_changes: u64,
    pub refactors: u64,
    /// Columns skipped by refactors, including work before a pivot fallback.
    pub columns_skipped: u64,
    /// The host's compile facility (`Options::plan_jit`); None keeps every plan interpreted.
    jit: Option<PlanJit>,
    /// Compiled plans by pivot sequence, the most recently used last (`compiled::JIT_KEEP`).
    compiled: Vec<(Arc<[u32]>, Arc<dyn CompiledLu>)>,
    /// Plans sent to compile and not taken yet: their pivot sequence and the job's answer.
    asked: Vec<(Arc<[u32]>, Answer)>,
    /// Pivot sequences whose compile came back None: they stay interpreted.
    refused: Vec<Arc<[u32]>>,
    /// The compiled plan of the present pivot sequence, when it has one.
    current: Option<Arc<dyn CompiledLu>>,
    /// `refactors` when the present pivot sequence was set, and whether it was decided (compiled,
    /// sent, refused or out of the size range).
    key_from: u64,
    key_done: bool,
    /// The thrash guard's window: `refactors` at its start, compiles asked in it, and whether
    /// the guard has logged.
    window: (u64, u32, bool),
    too_large_logged: bool,
    /// Compiles asked, and refactors and solves run by compiled plans.
    pub compiles_asked: u64,
    pub compiled_refactors: u64,
    pub compiled_solves: u64,
    /// Columns recomputed by refactors, including a column that triggers a pivot fallback.
    pub columns_recomputed: u64,
}

impl<S: Scalar> SparseLu<S> {
    pub fn new(pat: Arc<Pattern>, ord: Arc<Ordering>) -> Self {
        let n = pat.n;
        let mut qinv = vec![0; n];
        for (k, &c) in ord.q.iter().enumerate() {
            qinv[c as usize] = k as u32;
        }
        // one block over every step when the ordering names none
        let starts = if ord.blocks.len() >= 2 && ord.blocks[0] == 0 && ord.blocks[ord.blocks.len() - 1] as usize == n {
            ord.blocks.clone()
        } else {
            vec![0, n as u32]
        };
        let mut block_of = vec![0u32; n];
        for w in starts.windows(2) {
            block_of[w[0] as usize..w[1] as usize].fill(w[0]);
        }
        SparseLu {
            n,
            pat,
            ord,
            qinv,
            piv: vec![NONE; n],
            pinv: vec![NONE; n],
            l_ptr: Vec::new(),
            l_row: Vec::new(),
            u_ptr: Vec::new(),
            u_step: Vec::new(),
            u_off: Vec::new(),
            block_of,
            starts,
            slots: Vec::new(),
            recip: vec![S::ZERO; n],
            columns: Vec::new(),
            loads: Vec::new(),
            updates: Vec::new(),
            subs: Vec::new(),
            pending: vec![0; n.div_ceil(64)],
            recomputed: vec![0; n.div_ceil(64)],
            remembered: false,
            scratch_l: Vec::new(),
            scratch_u: Vec::new(),
            scratch_diag: Vec::new(),
            scratch_off: Vec::new(),
            scratch_slot_row: Vec::new(),
            scratch_row_slot: Vec::new(),
            scratch_row_value: Vec::new(),
            x: vec![S::ZERO; n],
            w: vec![S::ZERO; n],
            mark: vec![0; n],
            stamp: 0,
            stack: Vec::new(),
            topo: Vec::new(),
            touched: Vec::new(),
            have: false,
            factors: 0,
            pivot_changes: 0,
            refactors: 0,
            columns_skipped: 0,
            jit: None,
            compiled: Vec::new(),
            asked: Vec::new(),
            refused: Vec::new(),
            current: None,
            key_from: 0,
            key_done: false,
            window: (0, 0, false),
            too_large_logged: false,
            compiles_asked: 0,
            compiled_refactors: 0,
            compiled_solves: 0,
            columns_recomputed: 0,
        }
    }

    pub fn n(&self) -> usize {
        self.n
    }

    /// Nonzeros of L and U (diagonal included once).
    pub fn fill(&self) -> usize {
        self.l_row.len() + self.u_step.len() + self.n
    }

    /// Full factor with pivot search. `vals` is the value array (index 0 is the trash cell).
    pub fn factor(&mut self, vals: &[S]) -> Result<(), Singular> {
        self.have = false;
        self.current = None;
        self.factors += 1;
        let n = self.n;
        let pat = self.pat.clone();
        let ord = self.ord.clone();
        // Only pivot search needs the separate numeric columns. Successful factors pack them
        // into slots; refactor and both solves use those slots exclusively. They are the LU's
        // scratch, taken for the factor and given back after it.
        let mut l_val = std::mem::take(&mut self.scratch_l);
        let mut u_val = std::mem::take(&mut self.scratch_u);
        let mut u_diag = std::mem::take(&mut self.scratch_diag);
        let mut off = std::mem::take(&mut self.scratch_off);
        l_val.clear();
        l_val.reserve(self.l_row.len());
        u_val.clear();
        u_val.reserve(self.u_step.len());
        u_diag.clear();
        u_diag.resize(n, S::ZERO);
        self.pinv.iter_mut().for_each(|p| *p = NONE);
        self.l_ptr.clear();
        self.l_row.clear();
        self.u_ptr.clear();
        self.u_step.clear();
        self.u_off.clear();
        self.l_ptr.push(0);
        self.u_ptr.push(0);
        // the first factor sets the sequence; a later one counts when it changes it
        let mut moved = false;
        let before = self.factors > 1;
        for k in 0..n {
            let c = ord.q[k] as usize;
            let first = self.block_of[k];
            off.clear();
            self.stamp = self.stamp.wrapping_add(1);
            if self.stamp == 0 {
                self.mark.iter_mut().for_each(|m| *m = 0);
                self.stamp = 1;
            }
            let stamp = self.stamp;
            self.touched.clear();
            self.topo.clear();
            for i in pat.col_ptr[c] as usize..pat.col_ptr[c + 1] as usize {
                let r = pat.row[i] as usize;
                self.x[r] += vals[i + 1];
                if self.mark[r] != stamp {
                    self.mark[r] = stamp;
                    self.touched.push(r as u32);
                    let p = self.pinv[r];
                    if p != NONE && p < first {
                        // an earlier block's row: the entry stays as it is
                        off.push(p);
                    } else if p != NONE {
                        self.dfs(p, stamp);
                    }
                }
            }
            // numeric: steps in topological order (reverse postorder)
            for t in (0..self.topo.len()).rev() {
                let p = self.topo[t] as usize;
                let ukp = self.x[self.piv[p] as usize];
                for i in self.l_ptr[p] as usize..self.l_ptr[p + 1] as usize {
                    let r = self.l_row[i] as usize;
                    let l = l_val[i];
                    self.x[r] -= l * ukp;
                }
            }
            for t in (0..self.topo.len()).rev() {
                let p = self.topo[t] as usize;
                self.u_step.push(p as u32);
                u_val.push(self.x[self.piv[p] as usize]);
            }
            self.u_off.push(self.u_step.len() as u32);
            off.sort_unstable();
            for &p in &off {
                self.u_step.push(p);
                u_val.push(self.x[self.piv[p as usize] as usize]);
            }
            self.u_ptr.push(self.u_step.len() as u32);
            // pivot among the unpivoted touched rows
            let pref = ord.pref[k] as usize;
            let mut best = NONE;
            let mut best_mag = 0.0f64;
            let mut pref_ok = false;
            for &r in &self.touched {
                let r = r as usize;
                if self.pinv[r] != NONE {
                    continue;
                }
                let m = self.x[r].mag();
                if m > best_mag || (m == best_mag && best != NONE && (r as u32) < best) {
                    best_mag = m;
                    best = r as u32;
                }
                if r == pref {
                    pref_ok = true;
                }
            }
            if best == NONE || best_mag == 0.0 || !best_mag.is_finite() {
                for &r in &self.touched {
                    self.x[r as usize] = S::ZERO;
                }
                (self.scratch_l, self.scratch_u, self.scratch_diag, self.scratch_off) = (l_val, u_val, u_diag, off);
                return Err(Singular { col: c as u32 });
            }
            let prow = if pref_ok && self.x[pref].mag() >= PIVOT_TOL * best_mag { pref } else { best as usize };
            let diag = self.x[prow];
            moved |= self.piv[k] != prow as u32;
            self.piv[k] = prow as u32;
            self.pinv[prow] = k as u32;
            u_diag[k] = diag;
            let inv = S::ONE / diag;
            self.recip[k] = inv;
            for &r in &self.touched {
                let r = r as usize;
                if self.pinv[r] == NONE {
                    self.l_row.push(r as u32);
                    l_val.push(self.x[r] * inv);
                }
                self.x[r] = S::ZERO;
            }
            self.l_ptr.push(self.l_row.len() as u32);
        }
        self.pivot_changes += u64::from(before && moved);
        self.build_plan(&l_val, &u_val, &u_diag);
        (self.scratch_l, self.scratch_u, self.scratch_diag, self.scratch_off) = (l_val, u_val, u_diag, off);
        self.have = true;
        self.key_from = self.refactors;
        self.key_done = false;
        self.current = self.lookup();
        Ok(())
    }

    fn build_plan(&mut self, l_val: &[S], u_val: &[S], u_diag: &[S]) {
        self.slots.clear();
        self.columns.clear();
        self.loads.clear();
        self.updates.clear();
        self.subs.clear();
        self.pending.fill(0);
        self.recomputed.fill(0);
        self.remembered = false;
        self.slots.extend_from_slice(u_val);
        let diag_base = self.slots.len() as u32;
        self.slots.extend_from_slice(u_diag);
        let lower_base = self.slots.len() as u32;
        self.slots.extend_from_slice(l_val);
        let mut slot_row = std::mem::take(&mut self.scratch_slot_row);
        slot_row.clear();
        slot_row.extend(self.u_step.iter().map(|&p| self.piv[p as usize]).chain(self.piv.iter().copied()).chain(self.l_row.iter().copied()));
        for k in 0..self.n {
            self.columns.push(Column {
                diag: diag_base + k as u32,
                lower: lower_base + self.l_ptr[k]..lower_base + self.l_ptr[k + 1],
                loads: 0..0,
                updates: 0..0,
            });
        }

        // Row lookups happen once here, never in the numeric refactor. Every reached row has
        // exactly one slot in the column, including structurally present numerical zeros.
        let (mut row_slot, mut row_value) = (std::mem::take(&mut self.scratch_row_slot), std::mem::take(&mut self.scratch_row_value));
        row_slot.clear();
        row_slot.resize(self.n, NONE);
        row_value.clear();
        row_value.resize(self.n, 0);
        for k in 0..self.n {
            let column = &self.columns[k];
            let upper = self.u_ptr[k]..self.u_ptr[k + 1];
            // Keep each column's load order: U, diagonal, then L.
            let column_slots = upper.clone().chain(std::iter::once(column.diag)).chain(column.lower.clone());
            for slot in column_slots.clone() {
                let r = slot_row[slot as usize] as usize;
                row_slot[r] = slot;
                row_value[r] = 0;
            }
            let c = self.ord.q[k] as usize;
            for i in self.pat.col_ptr[c]..self.pat.col_ptr[c + 1] {
                let r = self.pat.row[i as usize] as usize;
                debug_assert!(upper.contains(&row_slot[r]) || row_slot[r] == column.diag || column.lower.contains(&row_slot[r]));
                row_value[r] = i + 1;
            }
            let loads = self.loads.len() as u32;
            for slot in column_slots {
                self.loads.push(Load { slot, value: row_value[slot_row[slot as usize] as usize], last: S::ZERO });
            }
            let updates = self.updates.len() as u32;
            // only the column's own block's steps update it; the rest of U are matrix values
            for u_slot in upper.start..self.u_off[k] {
                let p = self.u_step[u_slot as usize] as usize;
                let prior = &self.columns[p];
                let subs = self.subs.len() as u32;
                for l_slot in prior.lower.clone() {
                    let dst = row_slot[slot_row[l_slot as usize] as usize];
                    debug_assert!(upper.contains(&dst) || dst == column.diag || column.lower.contains(&dst));
                    self.subs.push(Subtract { dst, l_slot });
                }
                self.updates.push(Update { u_slot, source: p as u32, subs: subs..self.subs.len() as u32 });
            }
            self.columns[k].loads = loads..self.loads.len() as u32;
            self.columns[k].updates = updates..self.updates.len() as u32;
        }
        (self.scratch_slot_row, self.scratch_row_slot, self.scratch_row_value) = (slot_row, row_slot, row_value);
    }

    fn dfs(&mut self, start: u32, stamp: u32) {
        self.stack.clear();
        self.stack.push((start, self.l_ptr[start as usize]));
        while let Some(top) = self.stack.last_mut() {
            let p = top.0 as usize;
            if top.1 < self.l_ptr[p + 1] {
                let r = self.l_row[top.1 as usize] as usize;
                top.1 += 1;
                if self.mark[r] != stamp {
                    self.mark[r] = stamp;
                    self.touched.push(r as u32);
                    let pr = self.pinv[r];
                    if pr != NONE {
                        self.stack.push((pr, self.l_ptr[pr as usize]));
                    }
                }
            } else {
                self.stack.pop();
                self.topo.push(p as u32);
            }
        }
    }

    /// The factor's size and the work its numeric passes do, on the last pattern and pivot order:
    /// (entries of L and U with the diagonal, multiply-subtracts a full refactor does, multiply-subtracts
    /// a solve does). None before the first factor.
    pub fn work(&self) -> Option<(usize, usize, usize)> {
        if !self.have {
            return None;
        }
        let (l, u) = (self.l_row.len(), self.u_step.len());
        Some((l + u + self.n, self.subs.len(), l + u))
    }

    /// Numeric factor on the last pattern and pivot order; falls back to a full factor when a
    /// pivot has become too small (or no factor exists yet). Unchanged columns keep their slots.
    pub fn refactor(&mut self, vals: &[S]) -> Result<(), Singular> {
        if self.have && self.current.is_some() {
            return self.refactor_compiled(vals);
        }
        self.refactor_impl(vals, false)
    }

    /// Recomputes every column: the reference against which skipping is measured. Remembers
    /// the loads too, so a following `refactor` can skip unchanged columns.
    pub fn refactor_all(&mut self, vals: &[S]) -> Result<(), Singular> {
        self.refactor_impl(vals, true)
    }

    /// Numeric refactor told which columns changed: `dirty` lists original columns (0-based,
    /// as `Singular::col` and `ord.q` number them, duplicates allowed) whose values in `vals`
    /// may differ from the ones the last factor or refactor loaded. Every column not listed
    /// must hold exactly the values it last loaded (the caller's guarantee). Recomputes the
    /// listed columns and every column that reads a recomputed one, in step order, and nothing
    /// else; it compares no values. The first refactor after a full factor visits every column
    /// to remember its loads; an unusable kept pivot falls back to a full factor.
    pub fn refactor_dirty(&mut self, vals: &[S], dirty: &[u32]) -> Result<(), Singular> {
        if !self.have {
            return self.factor(vals);
        }
        if self.current.is_some() {
            return self.refactor_compiled(vals);
        }
        if !self.remembered {
            return self.refactor_impl(vals, true);
        }
        self.refactors += 1;
        self.pending.fill(0);
        self.recomputed.fill(0);
        let mut first = self.n;
        for &c in dirty {
            let k = self.qinv[c as usize] as usize;
            self.pending[k / 64] |= 1u64 << (k % 64);
            first = first.min(k);
        }
        self.columns_skipped += first as u64;
        // From the first dirty step on, a column is recomputed when it is dirty or reads a
        // recomputed one: its sources precede it, so their bits already describe this refactor.
        for k in first..self.n {
            let column = &self.columns[k];
            let bit = 1u64 << (k % 64);
            let updates = &self.updates[column.updates.start as usize..column.updates.end as usize];
            let recompute = self.pending[k / 64] & bit != 0 || updates.iter().any(|update| {
                let p = update.source as usize;
                self.recomputed[p / 64] & (1u64 << (p % 64)) != 0
            });
            if !recompute {
                self.columns_skipped += 1;
                continue;
            }
            self.columns_recomputed += 1;
            self.recomputed[k / 64] |= bit;
            for load in &mut self.loads[column.loads.start as usize..column.loads.end as usize] {
                let value = if load.value == 0 { S::ZERO } else { S::ZERO + vals[load.value as usize] };
                load.last = value;
                self.slots[load.slot as usize] = value;
            }
            for update in updates {
                let ukp = self.slots[update.u_slot as usize];
                for sub in &self.subs[update.subs.start as usize..update.subs.end as usize] {
                    let l = self.slots[sub.l_slot as usize];
                    self.slots[sub.dst as usize] -= l * ukp;
                }
            }
            let diag = self.slots[column.diag as usize];
            let lower = &mut self.slots[column.lower.start as usize..column.lower.end as usize];
            let mut colmax = diag.mag();
            for l in lower.iter() {
                colmax = colmax.max(l.mag());
            }
            let ok = diag.mag() > 0.0 && diag.mag() >= PIVOT_TOL * colmax && colmax.is_finite();
            if !ok {
                return self.factor(vals);
            }
            let inv = S::ONE / diag;
            self.recip[k] = inv;
            for l in lower {
                *l = *l * inv;
            }
        }
        Ok(())
    }

    fn refactor_impl(&mut self, vals: &[S], all: bool) -> Result<(), Singular> {
        if !self.have {
            return self.factor(vals);
        }
        self.refactors += 1;
        // A rebuilt plan has no remembered inputs; its first refactor visits every column.
        let force = all || !self.remembered;
        self.recomputed.fill(0);
        for k in 0..self.n {
            let column = &self.columns[k];
            let loads = &mut self.loads[column.loads.start as usize..column.loads.end as usize];
            let updates = &self.updates[column.updates.start as usize..column.updates.end as usize];
            let mut changed = force;
            for load in loads.iter_mut() {
                // Preserve the scatter's addition to +0, including for an input of -0.
                let value = if load.value == 0 { S::ZERO } else { S::ZERO + vals[load.value as usize] };
                changed |= value != load.last;
                load.last = value;
            }
            // Sources precede k, so their bits already describe this refactor.
            let recomputed = changed || updates.iter().any(|update| {
                let p = update.source as usize;
                self.recomputed[p / 64] & (1u64 << (p % 64)) != 0
            });
            if !recomputed {
                self.columns_skipped += 1;
                continue;
            }
            self.columns_recomputed += 1;
            self.recomputed[k / 64] |= 1u64 << (k % 64);
            for load in loads.iter() {
                self.slots[load.slot as usize] = load.last;
            }
            for update in updates {
                let ukp = self.slots[update.u_slot as usize];
                for sub in &self.subs[update.subs.start as usize..update.subs.end as usize] {
                    let l = self.slots[sub.l_slot as usize];
                    self.slots[sub.dst as usize] -= l * ukp;
                }
            }
            let diag = self.slots[column.diag as usize];
            let lower = &mut self.slots[column.lower.start as usize..column.lower.end as usize];
            let mut colmax = diag.mag();
            for l in lower.iter() {
                colmax = colmax.max(l.mag());
            }
            let ok = diag.mag() > 0.0 && diag.mag() >= PIVOT_TOL * colmax && colmax.is_finite();
            if !ok {
                // Refactor never touches the full factor's zeroed workspace.
                return self.factor(vals);
            }
            let inv = S::ONE / diag;
            self.recip[k] = inv;
            for l in lower {
                *l = *l * inv;
            }
        }
        self.remembered = true;
        Ok(())
    }

    /// Solves `A x = b` in place: `b` by row in, `x` by column out. Block by block, the last
    /// first: a block's L and U, then its columns' entries in earlier blocks' rows move to their
    /// right-hand sides.
    pub fn solve(&mut self, b: &mut [S]) {
        debug_assert!(self.have);
        if let (Some(code), Some(slots), Some(recip), Some(b), Some(w)) =
            (self.current.as_deref(), S::as_f64(&self.slots), S::as_f64(&self.recip), S::as_f64_mut(&mut *b), S::as_f64_mut(&mut self.w))
        {
            code.solve(slots, recip, b, w);
            self.compiled_solves += 1;
            return;
        }
        let n = self.n;
        let diag_base = self.u_step.len();
        let lower_base = diag_base + n;
        for bi in (0..self.starts.len() - 1).rev() {
            let (first, end) = (self.starts[bi] as usize, self.starts[bi + 1] as usize);
            for k in first..end {
                let yk = b[self.piv[k] as usize];
                if yk != S::ZERO {
                    for i in self.l_ptr[k] as usize..self.l_ptr[k + 1] as usize {
                        let l = self.slots[lower_base + i];
                        b[self.l_row[i] as usize] -= l * yk;
                    }
                }
            }
            for k in first..end {
                self.w[k] = b[self.piv[k] as usize];
            }
            for k in (first..end).rev() {
                let wk = self.w[k] * self.recip[k];
                self.w[k] = wk;
                if wk != S::ZERO {
                    for i in self.u_ptr[k] as usize..self.u_off[k] as usize {
                        let u = self.slots[i];
                        self.w[self.u_step[i] as usize] -= u * wk;
                    }
                }
            }
            for k in first..end {
                let xk = self.w[k];
                if xk != S::ZERO {
                    for i in self.u_off[k] as usize..self.u_ptr[k + 1] as usize {
                        let u = self.slots[i];
                        b[self.piv[self.u_step[i] as usize] as usize] -= u * xk;
                    }
                }
            }
        }
        for k in 0..n {
            b[self.ord.q[k] as usize] = self.w[k];
        }
    }

    /// Solves `A^T x = b` in place (the adjoint analyses): `b` by column in, `x` by row out.
    /// Block by block, the first first: an earlier block's solution is final when a later block's
    /// U^T reads it through the entries in its rows.
    pub fn solve_transposed(&mut self, b: &mut [S]) {
        debug_assert!(self.have);
        let n = self.n;
        let diag_base = self.u_step.len();
        let lower_base = diag_base + n;
        for bi in 0..self.starts.len() - 1 {
            let (first, end) = (self.starts[bi] as usize, self.starts[bi + 1] as usize);
            // U^T v = Q^T b
            for k in first..end {
                let mut s = b[self.ord.q[k] as usize];
                for i in self.u_ptr[k] as usize..self.u_ptr[k + 1] as usize {
                    s -= self.slots[i] * self.w[self.u_step[i] as usize];
                }
                self.w[k] = s * self.recip[k];
            }
            // L^T y = v, y in step order; then x[piv[k]] = y[k]
            for k in (first..end).rev() {
                let mut s = self.w[k];
                for i in self.l_ptr[k] as usize..self.l_ptr[k + 1] as usize {
                    s -= self.slots[lower_base + i] * self.w[self.pinv[self.l_row[i] as usize] as usize];
                }
                self.w[k] = s;
            }
        }
        for k in 0..n {
            b[self.piv[k] as usize] = self.w[k];
        }
    }

    /// The refactor through the present pivot sequence's compiled plan: the interpreted
    /// arithmetic in the same order, so the same bits. A failed pivot check factors afresh, as the
    /// interpreted refactor does.
    fn refactor_compiled(&mut self, vals: &[S]) -> Result<(), Singular> {
        let ran = match (self.current.as_deref(), S::as_f64(vals), S::as_f64_mut(&mut self.slots), S::as_f64_mut(&mut self.recip)) {
            (Some(code), Some(v), Some(slots), Some(recip)) => Some(code.refactor(v, slots, recip)),
            _ => None,
        };
        let Some(ran) = ran else {
            return self.refactor_impl(vals, false);
        };
        self.refactors += 1;
        self.compiled_refactors += 1;
        // the loads were not remembered: an interpreted refactor after this one recomputes all
        self.remembered = false;
        match ran {
            Ok(()) => Ok(()),
            Err(_) => self.factor(vals),
        }
    }

    /// The present pivot sequence's numeric program (`compiled::LuPlan`); None before a factor.
    pub fn plan(&self) -> Option<LuPlan> {
        if !self.have {
            return None;
        }
        Some(LuPlan {
            n: self.n as u32,
            slots: self.slots.len() as u32,
            vals: (self.pat.nnz() + 1) as u32,
            lower_base: (self.u_step.len() + self.n) as u32,
            pivot_tol: PIVOT_TOL,
            columns: self.columns.iter().map(|c| PlanColumn { diag: c.diag, lower: c.lower.clone(), loads: c.loads.clone(), updates: c.updates.clone() }).collect(),
            loads: self.loads.iter().map(|l| (l.slot, l.value)).collect(),
            updates: self.updates.iter().map(|u| (u.u_slot, u.subs.clone())).collect(),
            subs: self.subs.iter().map(|s| (s.dst, s.l_slot)).collect(),
            starts: self.starts.clone(),
            piv: self.piv.clone(),
            q: self.ord.q.clone(),
            l_ptr: self.l_ptr.clone(),
            l_row: self.l_row.clone(),
            u_ptr: self.u_ptr.clone(),
            u_off: self.u_off.clone(),
            u_step: self.u_step.clone(),
        })
    }

    /// Gives the LU the host's compile facility (None: every plan stays interpreted).
    pub fn set_jit(&mut self, jit: Option<PlanJit>) {
        self.jit = jit;
    }

    /// The compiled plan in use, if any: what its compile cost and made.
    pub fn compiled_stats(&self) -> Option<CompileStats> {
        self.current.as_ref().map(|c| c.stats())
    }

    /// Compiles the present plan here and now and uses it (tests and measurements; a live run
    /// compiles on the host's pool through `adopt`). False when the compiler declines.
    pub fn compile_now(&mut self, compiler: &dyn PlanCompiler) -> bool {
        let Some(code) = self.plan().and_then(|p| compiler.compile(&p)) else {
            return false;
        };
        self.keep(self.piv.as_slice().into(), code.clone());
        self.current = Some(code);
        true
    }

    /// At a step boundary of a Fast run: takes the compiles that came back, uses the present pivot
    /// sequence's plan once it is compiled, and sends the present plan to compile after it has run
    /// `JIT_AFTER` refactors. A plan below `JIT_MIN_OPS` or above `JIT_MAX_OPS` stays
    /// interpreted, and so does every plan of an LU the thrash guard has stopped.
    pub fn adopt(&mut self) {
        if self.jit.is_none() {
            return;
        }
        if !self.asked.is_empty() {
            self.take_answers();
        }
        if self.current.is_some() || self.key_done || !self.have || self.refactors - self.key_from < u64::from(compiled::JIT_AFTER) {
            return;
        }
        self.key_done = true;
        self.ask();
    }

    fn take_answers(&mut self) {
        let mut i = 0;
        while i < self.asked.len() {
            let Some(answer) = self.asked[i].1.get().cloned() else {
                i += 1;
                continue;
            };
            let (piv, _) = self.asked.swap_remove(i);
            match answer {
                Some(code) => self.keep(piv, code),
                None => self.refused.push(piv),
            }
        }
        if self.current.is_none() && self.have {
            self.current = self.lookup();
        }
    }

    fn ask(&mut self) {
        let ops = self.loads.len() + self.subs.len() + self.l_row.len() + self.u_step.len();
        if ops < compiled::JIT_MIN_OPS {
            return;
        }
        if ops > compiled::JIT_MAX_OPS {
            if !self.too_large_logged {
                self.too_large_logged = true;
                eprintln!("fab-elec sim: an LU plan of {ops} ops (above {}) stays interpreted", compiled::JIT_MAX_OPS);
            }
            return;
        }
        if self.refused.iter().chain(self.asked.iter().map(|(p, _)| p)).any(|p| **p == *self.piv) {
            return;
        }
        if self.refactors - self.window.0 >= compiled::JIT_WINDOW {
            self.window = (self.refactors, 0, self.window.2);
        }
        if self.window.1 >= compiled::JIT_WINDOW_ASKS {
            if !self.window.2 {
                self.window.2 = true;
                eprintln!(
                    "fab-elec sim: an LU's pivot sequence keeps changing ({} compiles within {} refactors): it stays interpreted until that settles",
                    compiled::JIT_WINDOW_ASKS,
                    compiled::JIT_WINDOW
                );
            }
            return;
        }
        let (Some(jit), Some(plan)) = (self.jit.clone(), self.plan()) else {
            return;
        };
        self.window.1 += 1;
        self.compiles_asked += 1;
        let answer: Answer = Arc::new(OnceLock::new());
        self.asked.push((self.piv.as_slice().into(), answer.clone()));
        let (compiler, codes) = (jit.compiler, jit.codes);
        (jit.post)(Box::new(move || {
            let code = compiler.compile(&plan);
            if let Some(code) = &code {
                codes.put(Arc::new(plan), code.clone());
            }
            let _ = answer.set(code);
        }));
    }

    /// The compiled plan of the present pivot sequence, marked most recently used: this LU's own,
    /// else one its facility's `CodeCache` holds (an LU of a block cut again in a shape it had).
    fn lookup(&mut self) -> Option<Arc<dyn CompiledLu>> {
        if let Some(i) = self.compiled.iter().position(|(p, _)| **p == *self.piv) {
            let entry = self.compiled.remove(i);
            let code = entry.1.clone();
            self.compiled.push(entry);
            return Some(code);
        }
        let ops = self.loads.len() + self.subs.len() + self.l_row.len() + self.u_step.len();
        if !(compiled::JIT_MIN_OPS..=compiled::JIT_MAX_OPS).contains(&ops) {
            return None;
        }
        let code = self.jit.as_ref()?.codes.get(&self.plan()?)?;
        self.keep(self.piv.as_slice().into(), code.clone());
        Some(code)
    }

    /// Keeps a compiled plan, dropping the least recently used beyond `JIT_KEEP` (its code is
    /// unmapped when the last holder lets it go).
    fn keep(&mut self, piv: Arc<[u32]>, code: Arc<dyn CompiledLu>) {
        self.compiled.retain(|(p, _)| *p != piv);
        self.compiled.push((piv, code));
        if self.compiled.len() > compiled::JIT_KEEP {
            self.compiled.remove(0);
        }
    }
}
