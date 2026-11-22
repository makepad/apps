//! Dense LU with partial pivoting: the path below `DENSE_MAX` unknowns, and the test oracle for
//! the sparse LU (E3 1.4 step 6).
//!
//! The value array is row-major `n x n` after the trash cell: entry (r, c) is at `1 + r*n + c`.

use super::lu::Singular;
use crate::num::Scalar;

#[derive(Clone)]
pub struct DenseLu<S> {
    n: usize,
    a: Vec<S>,
    perm: Vec<u32>,
    have: bool,
    w: Vec<S>,
    pub factors: u64,
}

impl<S: Scalar> DenseLu<S> {
    pub fn new(n: usize) -> Self {
        DenseLu { n, a: vec![S::ZERO; n * n], perm: (0..n as u32).collect(), have: false, w: vec![S::ZERO; n], factors: 0 }
    }

    pub fn n(&self) -> usize {
        self.n
    }

    /// The last factor succeeded: it holds the LU of the values it loaded.
    pub fn holds_factor(&self) -> bool {
        self.have
    }

    pub fn factor(&mut self, vals: &[S]) -> Result<(), Singular> {
        self.have = false;
        self.factors += 1;
        let n = self.n;
        self.a.copy_from_slice(&vals[1..1 + n * n]);
        for (i, p) in self.perm.iter_mut().enumerate() {
            *p = i as u32;
        }
        let a = &mut self.a;
        for k in 0..n {
            let mut best = k;
            let mut best_mag = a[k * n + k].mag();
            for r in k + 1..n {
                let m = a[r * n + k].mag();
                if m > best_mag {
                    best = r;
                    best_mag = m;
                }
            }
            if best_mag == 0.0 || !best_mag.is_finite() {
                // the column of the original matrix that lost its pivot
                return Err(Singular { col: k as u32 });
            }
            if best != k {
                for c in 0..n {
                    a.swap(k * n + c, best * n + c);
                }
                self.perm.swap(k, best);
            }
            let d = a[k * n + k];
            for r in k + 1..n {
                let l = a[r * n + k] / d;
                if l != S::ZERO {
                    a[r * n + k] = l;
                    for c in k + 1..n {
                        let u = a[k * n + c];
                        a[r * n + c] -= l * u;
                    }
                } else {
                    a[r * n + k] = S::ZERO;
                }
            }
        }
        self.have = true;
        Ok(())
    }

    /// Solves `A x = b` in place (row-indexed in, column-indexed out).
    pub fn solve(&mut self, b: &mut [S]) {
        debug_assert!(self.have);
        let n = self.n;
        let a = &self.a;
        for k in 0..n {
            self.w[k] = b[self.perm[k] as usize];
        }
        for r in 0..n {
            let mut s = self.w[r];
            for c in 0..r {
                s -= a[r * n + c] * self.w[c];
            }
            self.w[r] = s;
        }
        for r in (0..n).rev() {
            let mut s = self.w[r];
            for c in r + 1..n {
                s -= a[r * n + c] * self.w[c];
            }
            self.w[r] = s / a[r * n + r];
        }
        b[..n].copy_from_slice(&self.w);
    }

    /// Solves `A^T x = b` in place.
    pub fn solve_transposed(&mut self, b: &mut [S]) {
        debug_assert!(self.have);
        let n = self.n;
        let a = &self.a;
        // P A = L U, so A^T = U^T L^T P: solve U^T z = b, L^T y = z, x = P^T y
        for r in 0..n {
            let mut s = b[r];
            for c in 0..r {
                s -= a[c * n + r] * self.w[c];
            }
            self.w[r] = s / a[r * n + r];
        }
        for r in (0..n).rev() {
            let mut s = self.w[r];
            for c in r + 1..n {
                s -= a[c * n + r] * self.w[c];
            }
            self.w[r] = s;
        }
        for k in 0..n {
            b[self.perm[k] as usize] = self.w[k];
        }
    }
}
