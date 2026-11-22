//! Load by block (ESIMRT C2). A few hub unknowns, the border (`Options::border`: rails, a mixer
//! sum), cut the circuit into blocks. An instance belongs to the block of the unknowns it touches
//! off the border; one that touches only the border and ground belongs to the border group.
//! Blocks of three unknowns or fewer are evaluated together as one group, so their dispatch costs
//! no more than one block's.
//!
//! A load evaluates block by block, each block's instances bank by bank in instance order.
//! - The entries only one block writes (its owned slots of `g` and `c`, its rows of `f` and `q`)
//!   therefore get exactly a whole load's sums, in the same order.
//! - The entries several blocks write (the border's rows and the slots between border unknowns)
//!   are taken per block and summed in block order. They differ from a whole load's only by that
//!   order's rounding, and do not depend on which blocks this load evaluated.
//!
//! That is what lets a resting block keep its last evaluation (C2c): its owned entries and its
//! parts of the shared ones stay as they were, bit for bit.

use crate::stamp::{Bank, EvalCx, Stamps, Unk};
use std::ops::Range;

/// Blocks of at most this many unknowns are evaluated as one group.
pub const SMALL_BLOCK: usize = 3;

#[derive(Clone)]
pub struct BlockPlan {
    /// Per block: a name (its first unknown's, "small blocks" for the group, "border" for the
    /// border group), its unknowns and its instances.
    pub names: Vec<String>,
    pub size: Vec<u32>,
    pub instances: Vec<u32>,
    /// The border unknowns.
    pub border: Vec<Unk>,
    /// Per block: (bank, range of `insts`), in bank order.
    runs: Vec<Vec<(u32, Range<u32>)>>,
    insts: Vec<u32>,
    /// Per block: the slots and rows only its instances write.
    own_slots: Vec<Vec<u32>>,
    own_rows: Vec<Vec<u32>>,
    /// Per block: the shared slots and rows it writes, and its last part of each.
    shared_slots: Vec<Vec<u32>>,
    shared_rows: Vec<Vec<u32>>,
    part_g: Vec<Vec<f64>>,
    part_c: Vec<Vec<f64>>,
    part_f: Vec<Vec<f64>>,
    part_q: Vec<Vec<f64>>,
    /// Every shared slot (row) with each part summed into it: (entry, block, position), by entry,
    /// then block.
    sum_slots: Vec<(u32, u32, u32)>,
    sum_rows: Vec<(u32, u32, u32)>,
    /// Per block: the unknowns its instances read or write, without ground (the bypass test).
    pub terminals: Vec<Vec<Unk>>,
}

fn find(parent: &mut [u32], mut u: u32) -> u32 {
    while parent[u as usize] != u {
        parent[u as usize] = parent[parent[u as usize] as usize];
        u = parent[u as usize];
    }
    u
}

impl BlockPlan {
    /// The plan for `banks` (after the layout) over `n` unknowns cut at `border`; `None` when
    /// the border leaves one block, which a whole load serves as well.
    pub fn build(banks: &[Box<dyn Bank>], n: usize, slots: usize, names: &[String], border: Vec<Unk>) -> Option<BlockPlan> {
        let mut is_border = vec![false; n + 1];
        for &u in &border {
            is_border[u as usize] = true;
        }
        // components of the unknowns off the border, joined by the instances that touch them
        let mut parent: Vec<u32> = (0..=n as u32).collect();
        let mut unks = Vec::new();
        for b in banks {
            for i in 0..b.len() {
                unks.clear();
                b.inst_unks(i, &mut unks);
                let mut first = None;
                for &u in &unks {
                    if u == 0 || is_border[u as usize] {
                        continue;
                    }
                    match first {
                        None => first = Some(u),
                        Some(f) => {
                            let (a, c) = (find(&mut parent, f), find(&mut parent, u));
                            if a != c {
                                parent[a.max(c) as usize] = a.min(c);
                            }
                        }
                    }
                }
            }
        }
        // blocks by their smallest unknown; the small ones grouped, then the border group
        const NONE: u32 = u32::MAX;
        let mut size = vec![0u32; n + 1];
        for u in 1..=n as u32 {
            if !is_border[u as usize] {
                let r = find(&mut parent, u);
                size[r as usize] += 1;
            }
        }
        let mut block_of_root = vec![NONE; n + 1];
        let mut plan_names = Vec::new();
        let mut plan_size = Vec::new();
        let mut small = NONE;
        let mut small_size = 0;
        for u in 1..=n {
            if is_border[u] || find(&mut parent, u as u32) as usize != u {
                continue;
            }
            if size[u] as usize <= SMALL_BLOCK {
                small_size += size[u];
                continue;
            }
            block_of_root[u] = plan_names.len() as u32;
            plan_names.push(names[u].clone());
            plan_size.push(size[u]);
        }
        if small_size > 0 {
            small = plan_names.len() as u32;
            plan_names.push("small blocks".to_string());
            plan_size.push(small_size);
        }
        if plan_names.len() < 2 {
            return None;
        }
        let border_group = plan_names.len() as u32;
        plan_names.push("border".to_string());
        plan_size.push(0);
        let nb = plan_names.len();
        let block_of = |parent: &mut [u32], unks: &[Unk]| -> u32 {
            match unks.iter().find(|&&u| u != 0 && !is_border[u as usize]) {
                None => border_group,
                Some(&u) => {
                    let r = find(parent, u) as usize;
                    if block_of_root[r] == NONE { small } else { block_of_root[r] }
                }
            }
        };
        // instances per block and bank; what each block touches
        let mut lists: Vec<Vec<Vec<u32>>> = vec![vec![Vec::new(); banks.len()]; nb];
        let mut terminals: Vec<Vec<Unk>> = vec![Vec::new(); nb];
        let mut block_slots: Vec<Vec<u32>> = vec![Vec::new(); nb];
        let mut slot_list = Vec::new();
        for (bi, b) in banks.iter().enumerate() {
            for i in 0..b.len() {
                unks.clear();
                b.inst_unks(i, &mut unks);
                let k = block_of(&mut parent, &unks) as usize;
                lists[k][bi].push(i as u32);
                terminals[k].extend(unks.iter().copied().filter(|&u| u != 0));
                slot_list.clear();
                b.inst_slots(i, &mut slot_list);
                block_slots[k].extend(slot_list.iter().copied().filter(|&s| s != 0));
            }
        }
        for t in terminals.iter_mut().chain(block_slots.iter_mut()) {
            t.sort_unstable();
            t.dedup();
        }
        // an entry is owned when one block writes it, shared when several do
        let mut slot_writers = vec![(NONE, 0u32); slots];
        let mut row_writers = vec![(NONE, 0u32); n + 1];
        for k in 0..nb {
            for &s in &block_slots[k] {
                let w = &mut slot_writers[s as usize];
                *w = (if w.0 == NONE { k as u32 } else { w.0 }, w.1 + 1);
            }
            for &u in &terminals[k] {
                let w = &mut row_writers[u as usize];
                *w = (if w.0 == NONE { k as u32 } else { w.0 }, w.1 + 1);
            }
        }
        let mut plan = BlockPlan {
            names: plan_names,
            size: plan_size,
            instances: lists.iter().map(|l| l.iter().map(|v| v.len() as u32).sum()).collect(),
            border,
            runs: vec![Vec::new(); nb],
            insts: Vec::new(),
            own_slots: vec![Vec::new(); nb],
            own_rows: vec![Vec::new(); nb],
            shared_slots: vec![Vec::new(); nb],
            shared_rows: vec![Vec::new(); nb],
            part_g: vec![Vec::new(); nb],
            part_c: vec![Vec::new(); nb],
            part_f: vec![Vec::new(); nb],
            part_q: vec![Vec::new(); nb],
            sum_slots: Vec::new(),
            sum_rows: Vec::new(),
            terminals: Vec::new(),
        };
        for k in 0..nb {
            for (bi, list) in lists[k].iter().enumerate() {
                if !list.is_empty() {
                    let start = plan.insts.len() as u32;
                    plan.insts.extend_from_slice(list);
                    plan.runs[k].push((bi as u32, start..plan.insts.len() as u32));
                }
            }
            for &s in &block_slots[k] {
                if slot_writers[s as usize].1 == 1 {
                    plan.own_slots[k].push(s);
                } else {
                    plan.sum_slots.push((s, k as u32, plan.shared_slots[k].len() as u32));
                    plan.shared_slots[k].push(s);
                }
            }
            for &u in &terminals[k] {
                if row_writers[u as usize].1 == 1 {
                    plan.own_rows[k].push(u);
                } else {
                    plan.sum_rows.push((u, k as u32, plan.shared_rows[k].len() as u32));
                    plan.shared_rows[k].push(u);
                }
            }
            plan.part_g[k] = vec![0.0; plan.shared_slots[k].len()];
            plan.part_c[k] = vec![0.0; plan.shared_slots[k].len()];
            plan.part_f[k] = vec![0.0; plan.shared_rows[k].len()];
            plan.part_q[k] = vec![0.0; plan.shared_rows[k].len()];
        }
        plan.sum_slots.sort_unstable();
        plan.sum_rows.sort_unstable();
        plan.terminals = terminals;
        Some(plan)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Evaluates the blocks `active` marks (every block when it is `None`) into `g`, `c`, `f`
    /// and `q`, and sums the shared entries from every block's last parts. A block left out keeps
    /// its entries and parts from its last evaluation. Returns the non-converged count and the
    /// last such (bank, instance).
    #[allow(clippy::too_many_arguments)]
    pub fn load(&mut self, banks: &mut [Box<dyn Bank>], x: &[f64], cx: &EvalCx, active: Option<&[bool]>,
        g: &mut [f64], c: &mut [f64], f: &mut [f64], q: &mut [f64]) -> (u32, Option<(usize, u32)>) {
        let mut total = 0;
        let mut at = None;
        // the trash slot and the ground row take every block's writes to ground
        g[0] = 0.0;
        c[0] = 0.0;
        f[0] = 0.0;
        q[0] = 0.0;
        for k in 0..self.names.len() {
            if active.is_some_and(|a| !a[k]) {
                continue;
            }
            for &s in self.own_slots[k].iter().chain(&self.shared_slots[k]) {
                g[s as usize] = 0.0;
                c[s as usize] = 0.0;
            }
            for &u in self.own_rows[k].iter().chain(&self.shared_rows[k]) {
                f[u as usize] = 0.0;
                q[u as usize] = 0.0;
            }
            for (bi, range) in &self.runs[k] {
                let mut st = Stamps { g: &mut *g, c: &mut *c, f: &mut *f, q: &mut *q, noncon: 0, noncon_at: 0 };
                banks[*bi as usize].eval_some(x, cx, &mut st, &self.insts[range.start as usize..range.end as usize]);
                if st.noncon > 0 {
                    total += st.noncon;
                    at = Some((*bi as usize, st.noncon_at));
                }
            }
            for (j, &s) in self.shared_slots[k].iter().enumerate() {
                self.part_g[k][j] = g[s as usize];
                self.part_c[k][j] = c[s as usize];
            }
            for (j, &u) in self.shared_rows[k].iter().enumerate() {
                self.part_f[k][j] = f[u as usize];
                self.part_q[k][j] = q[u as usize];
            }
        }
        let mut last = u32::MAX;
        for &(s, k, j) in &self.sum_slots {
            let (k, j, s) = (k as usize, j as usize, s as usize);
            if s as u32 != last {
                g[s] = 0.0;
                c[s] = 0.0;
                last = s as u32;
            }
            g[s] += self.part_g[k][j];
            c[s] += self.part_c[k][j];
        }
        last = u32::MAX;
        for &(u, k, j) in &self.sum_rows {
            let (k, j, u) = (k as usize, j as usize, u as usize);
            if u as u32 != last {
                f[u] = 0.0;
                q[u] = 0.0;
                last = u as u32;
            }
            f[u] += self.part_f[k][j];
            q[u] += self.part_q[k][j];
        }
        (total, at)
    }

    /// One line per block: name, unknowns, instances, owned and shared entries.
    pub fn report(&self) -> String {
        let shared: usize = self.shared_slots.iter().map(Vec::len).sum::<usize>() + self.shared_rows.iter().map(Vec::len).sum::<usize>();
        let mut out = format!("blocks: {} over a border of {} unknowns; {} shared entry parts\n", self.len(), self.border.len(), shared);
        for k in 0..self.len() {
            out += &format!("  {} ({} unknowns, {} instances): {} slots and {} rows owned, {} and {} shared\n",
                self.names[k], self.size[k], self.instances[k], self.own_slots[k].len(), self.own_rows[k].len(),
                self.shared_slots[k].len(), self.shared_rows[k].len());
        }
        out
    }
}
