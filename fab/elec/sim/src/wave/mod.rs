//! The wave mode's kernels: a block of a circuit run as a wave digital filter on a fixed sample
//! grid, for real time where the user picks it over Fast's error control.
//!
//! A kernel's devices split into ports and a root. Each capacitor is a port: an adapted one-port of
//! port resistance `R = T / 2C` (`T` the sample period: the bilinear transform), seen from the
//! root as a voltage `a` behind `R`. The root is everything else, resistors and sources and
//! controlled sources, joined in one multiport (an R-type adaptor) whose scattering is solved once
//! by modified nodal analysis when the kernel is built ([`build`]). Each sample the root reflects
//! `b = 2 v - a` into every port, `v` the port's voltage, and each capacitor sends back what it
//! was sent a sample before: `a[n + 1] = b[n]`.
//!
//! Per sample a kernel is a fixed list of sums of products ([`WavePlan`]): the ports' reflected
//! waves and the probed outputs, each a sum over the ports' incident waves and the sample's inputs
//! (the sources' values, and a constant one). The coefficients are data (`consts`), not part of
//! the plan: a value edit folds new numbers into them and the plan stays. Every sum starts from
//! its first product and adds the others in the plan's order, with no fused multiply-add, so any
//! other implementation of a plan that follows the same order gives the same bits.

pub mod build;

use crate::sparse::compiled::CompileStats;
use std::sync::Arc;

/// A kernel's operations per sample.
///
/// The operands of a sample are the ports' incident waves `a[0..ports]` followed by the sample's
/// inputs `u[0..inputs]`: operand `j` is `a[j]` for `j < ports`, else `u[j - ports]`. Row `r` of
/// `rows` is the sum, over its terms `t` in order, of `consts[t] * operand(terms[t])`, zero for a
/// row with no terms. The first `ports` rows are the ports' reflected waves `b`; the next
/// `outputs` rows are the sample's outputs. Then every port's incident wave becomes its reflected
/// one: `a = b`.
#[derive(Clone, Debug, PartialEq)]
pub struct WavePlan {
    pub ports: u32,
    pub inputs: u32,
    pub outputs: u32,
    /// `ports + outputs` rows, each its terms' range `(start, end)` in `terms`.
    pub rows: Vec<(u32, u32)>,
    /// Each term's operand. Term `t`'s coefficient is `consts[t]`.
    pub terms: Vec<u32>,
}

impl WavePlan {
    /// Multiply-adds a sample.
    pub fn ops(&self) -> usize {
        self.terms.len()
    }

    /// Runs `samples` samples. `a` holds the ports' incident waves and carries them on; `u` holds
    /// `inputs` values a sample, sample after sample, and `y` takes `outputs` values a sample the
    /// same way; `b` is a workspace of `ports`. O(ops) a sample.
    pub fn run(&self, consts: &[f64], a: &mut [f64], u: &[f64], y: &mut [f64], b: &mut [f64], samples: usize) {
        let (ports, inputs, outputs) = (self.ports as usize, self.inputs as usize, self.outputs as usize);
        assert_eq!(consts.len(), self.terms.len(), "one coefficient a term");
        assert_eq!(a.len(), ports);
        assert_eq!(b.len(), ports);
        assert!(u.len() >= samples * inputs && y.len() >= samples * outputs);
        for s in 0..samples {
            let us = &u[s * inputs..(s + 1) * inputs];
            for (r, &(start, end)) in self.rows.iter().enumerate() {
                let mut acc = 0.0;
                for t in start as usize..end as usize {
                    let j = self.terms[t] as usize;
                    let x = if j < ports { a[j] } else { us[j - ports] };
                    let p = consts[t] * x;
                    acc = if t == start as usize { p } else { acc + p };
                }
                if r < ports {
                    b[r] = acc;
                } else {
                    y[s * outputs + r - ports] = acc;
                }
            }
            a.copy_from_slice(b);
        }
    }
}

/// A compiled wave plan. The implementation checks the slice lengths and runs the arithmetic
/// in the same order as [`WavePlan::run`].
pub trait CompiledWave: Send + Sync {
    fn run(&self, consts: &[f64], a: &mut [f64], u: &[f64], y: &mut [f64], b: &mut [f64], samples: usize);
    /// The compile cost and code size, with [`WavePlan::ops`] operations per sample.
    fn stats(&self) -> CompileStats;
}

/// Turns a wave plan into native code. `None` keeps the plan interpreted.
pub trait WaveCompiler: Send + Sync {
    fn compile(&self, plan: &WavePlan) -> Option<Arc<dyn CompiledWave>>;
}
