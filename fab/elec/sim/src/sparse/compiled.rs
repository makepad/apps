//! Compiled LU plans (ESIMRT T9): a sparse LU's refactor and solve as straight-line code, built
//! outside the kernel. The kernel has no dependencies, so it only describes a plan (`LuPlan`) and
//! takes a compiler from its host (`PlanJit` in `Options`); the compiler runs on the host's pool,
//! never on the sim worker, and its code is adopted at a step boundary (`SparseLu::adopt`).
//!
//! A compiled plan does the interpreted plan's arithmetic in the same order, so its results are
//! the same bits: the switch between the two is invisible in the results. Everything that cannot
//! compile (no code generation on this host, a refused plan, a plan too small to pay or too large
//! to compile) stays interpreted, which is not an error.

use std::fmt;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A sparse LU's numeric program for one pattern, ordering and pivot sequence, in the order the
/// interpreted refactor and solve run it. Slots hold U in `u_step` order, then the `n` diagonals,
/// then L in `l_row` order (`SparseLu`'s layout).
///
/// Refactor, step by step (`k` in `0..n`), as `SparseLu::refactor_impl` recomputing every column:
/// 1. each load of the column: `slots[slot] = 0.0` when `value` is 0, else `0.0 + vals[value]`;
/// 2. each update of the column, in order: `u = slots[u_slot]`, then for each subtract of it, in
///    order, `slots[dst] = slots[dst] - slots[l_slot] * u` (a multiply, then a subtract: no fused
///    multiply-add);
/// 3. the pivot check: `d = slots[diag]`, `m = |d|` folded with `f64::max` over `|slots[i]|` of
///    the column's L entries in order; it fails unless `|d| > 0`, `|d| >= pivot_tol * m` and `m`
///    is finite (then the refactor stops at `k`; the caller factors afresh);
/// 4. `recip[k] = 1.0 / d`, then each L entry of the column `slots[i] = slots[i] * recip[k]`.
///
/// Solve (`b` by row in, the solution by column out; `w` a step-indexed workspace), as
/// `SparseLu::solve`: for each block from the last to the first (`starts`):
/// 1. for `k` in the block: `y = b[piv[k]]`; unless `y == 0.0`, for each L entry `i` of `k`:
///    `b[l_row[i]] = b[l_row[i]] - slots[lower_base + i] * y`;
/// 2. for `k` in the block: `w[k] = b[piv[k]]`;
/// 3. for `k` in the block, last first: `w[k] = w[k] * recip[k]`; unless that is `0.0`, for `i` in
///    `u_ptr[k]..u_off[k]`: `w[u_step[i]] = w[u_step[i]] - slots[i] * w[k]`;
/// 4. for `k` in the block: unless `w[k] == 0.0`, for `i` in `u_off[k]..u_ptr[k + 1]`:
///    `b[piv[u_step[i]]] = b[piv[u_step[i]]] - slots[i] * w[k]`;
///
/// then `b[q[k]] = w[k]` for every `k`. A zero test is an ordered compare: NaN is not zero.
#[derive(Clone, Debug, PartialEq)]
pub struct LuPlan {
    pub n: u32,
    /// Length of the slot array.
    pub slots: u32,
    /// The value arrays a refactor reads are longer than every load's `value`.
    pub vals: u32,
    /// Index of L's first slot.
    pub lower_base: u32,
    pub pivot_tol: f64,
    pub columns: Vec<PlanColumn>,
    /// (slot, value index; 0 loads +0.0)
    pub loads: Vec<(u32, u32)>,
    /// (U slot, the range of `subs` it drives)
    pub updates: Vec<(u32, Range<u32>)>,
    /// (destination slot, L slot)
    pub subs: Vec<(u32, u32)>,
    /// The diagonal blocks' first steps, then `n`.
    pub starts: Vec<u32>,
    pub piv: Vec<u32>,
    pub q: Vec<u32>,
    pub l_ptr: Vec<u32>,
    pub l_row: Vec<u32>,
    pub u_ptr: Vec<u32>,
    pub u_off: Vec<u32>,
    pub u_step: Vec<u32>,
}

/// One step of a plan's refactor.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanColumn {
    pub diag: u32,
    /// The column's L slots.
    pub lower: Range<u32>,
    /// Its loads in `LuPlan::loads`.
    pub loads: Range<u32>,
    /// Its updates in `LuPlan::updates`.
    pub updates: Range<u32>,
}

impl LuPlan {
    /// Loads, multiply-subtracts of the refactor and multiply-subtracts of the solve: the size the
    /// compile thresholds read.
    pub fn ops(&self) -> usize {
        self.loads.len() + self.subs.len() + self.l_row.len() + self.u_step.len()
    }
}

/// What a compile cost and made.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CompileStats {
    /// Bytes of machine code.
    pub code_bytes: u32,
    /// Wall time of the compile on the pool, nanoseconds.
    pub compile_ns: u64,
    /// `LuPlan::ops` of the plan it compiled.
    pub ops: u32,
}

/// A plan's compiled refactor and solve. The slices have the plan's lengths (`LuPlan`: `vals`,
/// `slots`, `n`); an implementation checks them before it runs.
pub trait CompiledLu: Send + Sync {
    /// The refactor; `Err(k)` when step `k`'s pivot fails its check (the slots are then partly
    /// written, and the caller factors afresh).
    fn refactor(&self, vals: &[f64], slots: &mut [f64], recip: &mut [f64]) -> Result<(), u32>;
    /// The solve, in place in `b`; `w` is a workspace of `n`.
    fn solve(&self, slots: &[f64], recip: &[f64], b: &mut [f64], w: &mut [f64]);
    fn stats(&self) -> CompileStats;
}

/// Turns plans into code. None keeps the plan interpreted (no code generation on this host, or a
/// plan it refuses); that is not an error.
pub trait PlanCompiler: Send + Sync {
    fn compile(&self, plan: &LuPlan) -> Option<Arc<dyn CompiledLu>>;
}

/// The host's compile facility for live runs (`Options::plan_jit`): the compiler, and the pool its
/// jobs run on (the host's workpool, never the sim worker).
#[derive(Clone)]
pub struct PlanJit {
    pub post: crate::rounds::Post,
    pub compiler: Arc<dyn PlanCompiler>,
    /// The code compiled so far, for every LU that runs with this facility.
    pub codes: Arc<CodeCache>,
}

impl fmt::Debug for PlanJit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlanJit")
    }
}

impl PartialEq for PlanJit {
    fn eq(&self, other: &PlanJit) -> bool {
        Arc::ptr_eq(&self.compiler, &other.compiler)
    }
}

/// Refactors on one pivot sequence before its plan is sent to compile: a sequence that changes
/// again within a few steps is not worth the compile.
pub const JIT_AFTER: u32 = 64;
/// Compiled plans an LU keeps, the most recently used last (drum16's LUs return to at most 6
/// pivot sequences in Fast).
pub const JIT_KEEP: usize = 4;
/// Plans below this many ops stay interpreted (the call costs more than compiling saves).
pub const JIT_MIN_OPS: usize = 64;
/// Plans above this many ops stay interpreted, logged once: straight-line code of that size costs
/// more in compile time and instruction cache than it saves.
pub const JIT_MAX_OPS: usize = 16384;
/// The thrash guard: an LU that asks for `JIT_WINDOW_ASKS` compiles within `JIT_WINDOW`
/// refactors stops asking until the window ends, logged once; it runs interpreted meanwhile.
pub const JIT_WINDOW: u64 = 65536;
pub const JIT_WINDOW_ASKS: u32 = 8;

/// Compiled plans across LUs, blocks and plans (ESIMRT A-R3): a block cut again in a shape it had
/// has LUs of the same pattern, ordering and pivot sequence, so the same `LuPlan`, and a factor
/// that sets a sequence takes its code here at once instead of after `JIT_AFTER` refactors and a
/// compile (a press and its release move between two cuts: the second time each has its code).
/// Code changes no bit, so what this holds is no part of a run's state. The least recently used
/// beyond `CODE_KEEP` go, logged once: one compile when they come back.
#[derive(Default)]
pub struct CodeCache {
    entries: Mutex<Vec<(Arc<LuPlan>, Arc<dyn CompiledLu>)>>,
    dropped: AtomicBool,
}

/// Compiled plans a `CodeCache` keeps: a few times a board's blocks' shapes.
pub const CODE_KEEP: usize = 256;

impl CodeCache {
    /// The code of `plan` if it was compiled, marked most recently used. O(entries) compares of
    /// sizes and pivots, a whole compare at a match; the lock is held for that alone.
    pub fn get(&self, plan: &LuPlan) -> Option<Arc<dyn CompiledLu>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let i = entries.iter().position(|(p, _)| p.n == plan.n && p.slots == plan.slots && p.piv == plan.piv && **p == *plan)?;
        let entry = entries.remove(i);
        let code = entry.1.clone();
        entries.push(entry);
        Some(code)
    }

    /// Keeps `code` as `plan`'s, the most recently used.
    pub fn put(&self, plan: Arc<LuPlan>, code: Arc<dyn CompiledLu>) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|(p, _)| **p != *plan);
        entries.push((plan, code));
        if entries.len() > CODE_KEEP {
            entries.remove(0);
            if !self.dropped.swap(true, Ordering::Relaxed) {
                eprintln!("fab-elec sim: over {CODE_KEEP} compiled LU plans: the least recently used go, compiled again when they come back");
            }
        }
    }

    /// The plans it holds.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
