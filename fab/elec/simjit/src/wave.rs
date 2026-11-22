use super::{options, Array, Builder};
use fab_elec_sim::sparse::compiled::CompileStats;
use fab_elec_sim::wave::{CompiledWave, WavePlan};
use relax_backend::rir::{Cls, FOp, Func, IOp, Inst, Reloc, Term, U64};
use relax_backend::standalone::{self, Alloc, Emitted, Passes};
use std::sync::Arc;
use std::time::Instant;

/// Compiles a wave plan for the native host.
pub struct WaveCompiler;

impl fab_elec_sim::wave::WaveCompiler for WaveCompiler {
    fn compile(&self, plan: &WavePlan) -> Option<Arc<dyn CompiledWave>> {
        if !cfg!(any(target_arch = "aarch64", target_arch = "x86_64")) || options().no_codegen {
            return None;
        }
        let ops = u32::try_from(plan.ops()).ok()?;
        let start = Instant::now();
        let mut f = build(plan, ops)?;
        let resolve = |r| match r {
            Reloc::Value(v) => Some(v),
            _ => None,
        };
        let emitted = standalone::compile(options(), &mut f, Passes::Blocks, Alloc::Quick, &resolve)?;
        let stats = CompileStats {
            compile_ns: u64::try_from(start.elapsed().as_nanos()).ok()?,
            code_bytes: emitted.text_len,
            ops,
        };
        Some(Arc::new(NativeWave {
            emitted,
            ports: plan.ports as usize,
            inputs: plan.inputs as usize,
            outputs: plan.outputs as usize,
            stats,
        }))
    }
}

struct NativeWave {
    emitted: Emitted,
    ports: usize,
    inputs: usize,
    outputs: usize,
    stats: CompileStats,
}

type Entry = relax_backend::host_fn_ptr!(fn(u64, *const f64, *mut f64, *const f64, *mut f64, *mut f64) -> u64);

impl NativeWave {
    fn entry(&self) -> Entry {
        // SAFETY: build declares six integer-class parameters and one integer-class result.
        // The mapping uses the standalone host ABI and self owns it throughout the call.
        unsafe { std::mem::transmute(self.emitted.code.ptr()) }
    }
}

impl CompiledWave for NativeWave {
    fn run(&self, consts: &[f64], a: &mut [f64], u: &[f64], y: &mut [f64], b: &mut [f64], samples: usize) {
        assert_eq!(consts.len(), self.stats.ops as usize, "one coefficient a term");
        assert_eq!(a.len(), self.ports);
        assert_eq!(b.len(), self.ports);
        assert!(samples.checked_mul(self.inputs).is_some_and(|n| n <= u.len())
            && samples.checked_mul(self.outputs).is_some_and(|n| n <= y.len()));
        // SAFETY: every offset fits its array, and u and y cover all samples at their
        // emitted strides. Stores only target the exclusive a, y and b slices.
        unsafe {
            (self.entry())(samples as u64, consts.as_ptr(), a.as_mut_ptr(), u.as_ptr(), y.as_mut_ptr(), b.as_mut_ptr());
        }
    }

    fn stats(&self) -> CompileStats {
        self.stats
    }
}

fn build(plan: &WavePlan, ops: u32) -> Option<Func> {
    let ports = plan.ports as usize;
    let operands = ports.checked_add(plan.inputs as usize)?;
    if plan.rows.len() != ports.checked_add(plan.outputs as usize)?
        || plan.terms.iter().any(|&j| j as usize >= operands)
    {
        return None;
    }

    let mut f = Func::new("wave".into(), 0);
    let [samples, consts, a, u, y, b] = std::array::from_fn(|_| f.vreg(Cls::I));
    f.params.extend([samples, consts, a, u, y, b]);
    f.rets.push(Cls::I);
    let entry = f.block();
    let sample = f.block();
    let done = f.block();
    let mut ir = Builder { f, cur: entry };
    let u_stride = ir.value(Cls::I, |d| Inst::Iconst(d, i64::from(plan.inputs) * 8));
    let y_stride = ir.value(Cls::I, |d| Inst::Iconst(d, i64::from(plan.outputs) * 8));
    ir.term(Term::Branch(samples, sample, done));

    ir.cur = sample;
    let consts = Array { base: consts, len: ops };
    let a = Array { base: a, len: plan.ports };
    let u = Array { base: u, len: plan.inputs };
    let y = Array { base: y, len: plan.outputs };
    let b = Array { base: b, len: plan.ports };
    let zero = ir.float(0.0);
    for (r, &(start, end)) in plan.rows.iter().enumerate() {
        let terms = plan.terms.get(start as usize..end as usize)?;
        let mut acc = zero;
        for (t, &j) in (start..end).zip(terms) {
            let x = if j < plan.ports { ir.load(a, j)? } else { ir.load(u, j - plan.ports)? };
            let c = ir.load(consts, t)?;
            let p = ir.bin(FOp::Mul, c, x);
            acc = if t == start { p } else { ir.bin(FOp::Add, acc, p) };
        }
        if r < ports {
            ir.store(b, r as u32, acc)?;
        } else {
            ir.store(y, (r - ports) as u32, acc)?;
        }
    }
    for k in 0..plan.ports {
        let reflected = ir.load(b, k)?;
        ir.store(a, k, reflected)?;
    }
    ir.emit(Inst::IBin(IOp::Add, U64, u.base, u.base, u_stride));
    ir.emit(Inst::IBin(IOp::Add, U64, y.base, y.base, y_stride));
    ir.emit(Inst::IBinI(IOp::Sub, U64, samples, samples, 1));
    ir.term(Term::Branch(samples, sample, done));
    ir.cur = done;
    ir.ret(0);
    Some(ir.f)
}
