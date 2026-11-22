//! The linear solver (E3 1.4): one value layout per topology, a dense LU below `DENSE_MAX`
//! unknowns and the sparse LU above, both generic over `Scalar` so AC reuses them with `C64`.

pub mod compiled;
pub mod dense;
pub mod lu;
pub mod order;
pub mod pattern;

use crate::num::Scalar;
pub use lu::Singular;
use std::sync::Arc;

/// Below this many unknowns the dense LU is used. On circuit matrices the sparse LU, which keeps
/// its pivots across refactors, is as fast at 6 unknowns and pulls ahead from there (per Newton
/// iteration: 1.4x at 10, 1.5x at 19, 2.3x at 35, 4x at 50; `fab-elec-sim`'s `voice` example,
/// `crossover`), so the dense one keeps only the tiny systems and stays the sparse one's oracle.
pub const DENSE_MAX: usize = 8;

/// Where each matrix entry's value lives. Index 0 of every value array is the trash cell.
#[derive(Clone, Debug)]
pub enum Layout {
    Dense { n: usize },
    Sparse { pat: Arc<pattern::Pattern>, ord: Arc<order::Ordering> },
}

impl Layout {
    /// The layout for `n` unknowns with these (row, col) entries (0-based); returns it with the
    /// value index of each entry. `force` picks a layout regardless of size (tests, the oracle).
    pub fn build(n: usize, entries: &[(u32, u32)], force: Option<bool>) -> (Layout, Vec<u32>) {
        let dense = force.unwrap_or(n < DENSE_MAX);
        if dense {
            let map = entries.iter().map(|&(r, c)| 1 + r * n as u32 + c).collect();
            (Layout::Dense { n }, map)
        } else {
            let (pat, map) = pattern::Pattern::from_entries(n, entries);
            let ord = order::order(&pat);
            (Layout::Sparse { pat: Arc::new(pat), ord: Arc::new(ord) }, map)
        }
    }

    pub fn n(&self) -> usize {
        match self {
            Layout::Dense { n } => *n,
            Layout::Sparse { pat, .. } => pat.n,
        }
    }

    /// Length of a value array, trash cell included.
    pub fn len(&self) -> usize {
        match self {
            Layout::Dense { n } => n * n + 1,
            Layout::Sparse { pat, .. } => pat.nnz() + 1,
        }
    }

    pub fn is_dense(&self) -> bool {
        matches!(self, Layout::Dense { .. })
    }

    /// Row and column (0-based) of each value index; the trash maps to `u32::MAX`.
    pub fn coords(&self) -> (Vec<u32>, Vec<u32>) {
        match self {
            Layout::Dense { n } => {
                let mut rows = vec![u32::MAX; n * n + 1];
                let mut cols = vec![u32::MAX; n * n + 1];
                for r in 0..*n {
                    for c in 0..*n {
                        rows[1 + r * n + c] = r as u32;
                        cols[1 + r * n + c] = c as u32;
                    }
                }
                (rows, cols)
            }
            Layout::Sparse { pat, .. } => pat.coords(),
        }
    }

    pub fn lu<S: Scalar>(&self) -> Lu<S> {
        match self {
            Layout::Dense { n } => Lu::Dense(dense::DenseLu::new(*n)),
            Layout::Sparse { pat, ord } => Lu::Sparse(lu::SparseLu::new(pat.clone(), ord.clone())),
        }
    }
}

#[derive(Clone)]
pub enum Lu<S> {
    Dense(dense::DenseLu<S>),
    Sparse(lu::SparseLu<S>),
}

impl<S: Scalar> Lu<S> {
    pub fn factor(&mut self, vals: &[S]) -> Result<(), Singular> {
        match self {
            Lu::Dense(d) => d.factor(vals),
            Lu::Sparse(s) => s.factor(vals),
        }
    }
    /// Same pattern and pivots, new values (the dense LU has no symbolic part: it factors).
    pub fn refactor(&mut self, vals: &[S]) -> Result<(), Singular> {
        match self {
            Lu::Dense(d) => d.factor(vals),
            Lu::Sparse(s) => s.refactor(vals),
        }
    }
    /// Refactors the listed original columns (0-based, duplicates allowed) and their
    /// dependents. Unlisted columns must retain their last loaded values. The dense LU factors
    /// every column, or none when none is listed and it holds a factor (of those same values);
    /// see `SparseLu::refactor_dirty` for the sparse fallback and first pass.
    pub fn refactor_dirty(&mut self, vals: &[S], dirty: &[u32]) -> Result<(), Singular> {
        match self {
            Lu::Dense(d) if dirty.is_empty() && d.holds_factor() => Ok(()),
            Lu::Dense(d) => d.factor(vals),
            Lu::Sparse(s) => s.refactor_dirty(vals, dirty),
        }
    }
    pub fn solve(&mut self, b: &mut [S]) {
        match self {
            Lu::Dense(d) => d.solve(b),
            Lu::Sparse(s) => s.solve(b),
        }
    }
    pub fn solve_transposed(&mut self, b: &mut [S]) {
        match self {
            Lu::Dense(d) => d.solve_transposed(b),
            Lu::Sparse(s) => s.solve_transposed(b),
        }
    }
    /// How many times a pivot sequence was chosen and kept for later refactors: the sparse LU's
    /// full factors. The dense LU keeps none (every factor pivots afresh from the values), so 0.
    /// Equal counts mean the refactors in between used one pivot sequence.
    pub fn kept_pivots(&self) -> u64 {
        match self {
            Lu::Dense(_) => 0,
            Lu::Sparse(s) => s.factors,
        }
    }
    /// The sparse factor's size and work (`SparseLu::work`); None for the dense LU.
    pub fn work(&self) -> Option<(usize, usize, usize)> {
        match self {
            Lu::Dense(_) => None,
            Lu::Sparse(s) => s.work(),
        }
    }
    /// (full factors, refactors) so far.
    pub fn counts(&self) -> (u64, u64) {
        match self {
            Lu::Dense(d) => (d.factors, 0),
            Lu::Sparse(s) => (s.factors, s.refactors),
        }
    }
    /// Gives a sparse LU the host's plan compiler (`compiled::PlanJit`); the dense LU has no plan.
    pub fn set_jit(&mut self, jit: Option<compiled::PlanJit>) {
        if let Lu::Sparse(s) = self {
            s.set_jit(jit);
        }
    }
    /// At a step boundary of a Fast run: a sparse LU takes its finished compiles and sends its
    /// plan to compile when due (`SparseLu::adopt`).
    #[inline]
    pub fn adopt(&mut self) {
        if let Lu::Sparse(s) = self {
            s.adopt();
        }
    }
    /// What the compiled plan in use cost and made, if any.
    pub fn compiled_stats(&self) -> Option<compiled::CompileStats> {
        match self {
            Lu::Dense(_) => None,
            Lu::Sparse(s) => s.compiled_stats(),
        }
    }
    /// Full sparse factors whose pivot sequence differed from the factor before (none for the
    /// dense LU).
    pub fn pivot_changes(&self) -> u64 {
        match self {
            Lu::Dense(_) => 0,
            Lu::Sparse(s) => s.pivot_changes,
        }
    }
    /// (columns skipped, columns recomputed) by sparse refactors so far, including work before
    /// a pivot fallback. Full factors are not counted; (0, 0) for the dense LU.
    pub fn column_counts(&self) -> (u64, u64) {
        match self {
            Lu::Dense(_) => (0, 0),
            Lu::Sparse(s) => (s.columns_skipped, s.columns_recomputed),
        }
    }
}
