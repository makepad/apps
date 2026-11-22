//! Standalone native refactor and solve for a fixed sparse LU pivot sequence.

mod wave;
pub use wave::WaveCompiler;

use fab_elec_sim::sparse::compiled::{CompileStats, CompiledLu, LuPlan, PlanCompiler};
use relax_backend::rir::{Cls, Cond, FOp, FUn, Func, IOp, Inst, Mem, Reloc, Term, VReg, U64};
use relax_backend::standalone::{self, Alloc, Emitted, Passes};
use relax_session::Options;
use std::ops::Range;
use std::ptr;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// Compiles a sparse LU plan for the native host.
pub struct Compiler;

fn options() -> &'static Options {
    static OPTIONS: OnceLock<Options> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut o = Options::from_env();
        o.ra_quick_coalesce = cfg!(target_arch = "aarch64");
        o.nan_exact = true;
        #[cfg(target_arch = "x86_64")]
        {
            o.cpu = relax_backend::x64::detect_cpu(&o);
        }
        o
    })
}

impl PlanCompiler for Compiler {
    fn compile(&self, plan: &LuPlan) -> Option<Arc<dyn CompiledLu>> {
        if !cfg!(any(target_arch = "aarch64", target_arch = "x86_64")) || options().no_codegen {
            return None;
        }
        let ops = u32::try_from(plan.ops()).ok()?;
        let start = Instant::now();
        let mut f = build(plan)?;
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
        Some(Arc::new(NativeLu {
            emitted,
            n: plan.n as usize,
            slots: plan.slots as usize,
            vals: plan.vals as usize,
            stats,
        }))
    }
}

struct NativeLu {
    emitted: Emitted,
    n: usize,
    slots: usize,
    vals: usize,
    stats: CompileStats,
}

type Entry = relax_backend::host_fn_ptr!(fn(u64, *const f64, *mut f64, *mut f64, *mut f64, *mut f64) -> u64);

impl NativeLu {
    fn entry(&self) -> Entry {
        // SAFETY: build declares six integer-class parameters and one integer-class result.
        // The mapping uses the standalone host ABI and self owns it throughout the call.
        unsafe { std::mem::transmute(self.emitted.code.ptr()) }
    }
}

impl CompiledLu for NativeLu {
    fn refactor(&self, vals: &[f64], slots: &mut [f64], recip: &mut [f64]) -> Result<(), u32> {
        assert!(vals.len() >= self.vals);
        assert_eq!(slots.len(), self.slots);
        assert_eq!(recip.len(), self.n);
        // SAFETY: every emitted offset fits its checked slice. Op 0 reads vals, writes
        // only through the two exclusive slices, and never accesses the solve arguments.
        let status = unsafe {
            (self.entry())(0, vals.as_ptr(), slots.as_mut_ptr(), recip.as_mut_ptr(), ptr::null_mut(), ptr::null_mut())
        };
        match status {
            u64::MAX => Ok(()),
            k => Err(k as u32),
        }
    }

    fn solve(&self, slots: &[f64], recip: &[f64], b: &mut [f64], w: &mut [f64]) {
        assert_eq!(slots.len(), self.slots);
        assert_eq!(recip.len(), self.n);
        assert_eq!(b.len(), self.n);
        assert_eq!(w.len(), self.n);
        // SAFETY: op 1 ignores vals and only reads slots and recip. Its stores target
        // the exclusive b and w slices, with every offset checked at compile time.
        unsafe {
            (self.entry())(
                1, ptr::null(), slots.as_ptr().cast_mut(), recip.as_ptr().cast_mut(), b.as_mut_ptr(), w.as_mut_ptr(),
            );
        }
    }

    fn stats(&self) -> CompileStats {
        self.stats
    }
}

#[derive(Clone, Copy)]
struct Array {
    base: VReg,
    len: u32,
}

impl Array {
    fn offset(self, index: u32) -> Option<i32> {
        if index >= self.len {
            return None;
        }
        i32::try_from(u64::from(index) * 8).ok()
    }
}

fn build(plan: &LuPlan) -> Option<Func> {
    let n = plan.n as usize;
    let ptr_len = n.checked_add(1)?;
    if plan.columns.len() != n
        || plan.piv.len() != n
        || plan.q.len() != n
        || plan.l_ptr.len() != ptr_len
        || plan.u_ptr.len() != ptr_len
        || plan.u_off.len() != n
        || plan.starts.first() != Some(&0)
        || plan.starts.last() != Some(&plan.n)
        || plan.starts.windows(2).any(|p| p[0] > p[1])
    {
        return None;
    }

    let mut f = Func::new("sparse_lu".into(), 0);
    let [op, vals, slots, recip, b, w] = std::array::from_fn(|_| f.vreg(Cls::I));
    f.params.extend([op, vals, slots, recip, b, w]);
    f.rets.push(Cls::I);
    let entry = f.block();
    let refactor = f.block();
    let solve = f.block();
    f.blocks[entry as usize].term = Term::Branch(op, solve, refactor);

    let slots = Array { base: slots, len: plan.slots };
    let recip = Array { base: recip, len: plan.n };
    let mut ir = Builder { f, cur: refactor };
    ir.refactor(plan, Array { base: vals, len: plan.vals }, slots, recip)?;
    ir.cur = solve;
    ir.solve(plan, slots, recip, Array { base: b, len: plan.n }, Array { base: w, len: plan.n })?;
    Some(ir.f)
}

struct Builder {
    f: Func,
    cur: u32,
}

impl Builder {
    fn emit(&mut self, inst: Inst) {
        let block = &mut self.f.blocks[self.cur as usize];
        block.insts.push(inst);
        block.pos.push(0);
    }

    fn value(&mut self, class: Cls, inst: impl FnOnce(VReg) -> Inst) -> VReg {
        let dst = self.f.vreg(class);
        self.emit(inst(dst));
        dst
    }

    fn term(&mut self, term: Term) {
        self.f.blocks[self.cur as usize].term = term;
    }

    fn ret(&mut self, result: i64) {
        let v = self.value(Cls::I, |d| Inst::Iconst(d, result));
        self.term(Term::Ret(vec![v]));
    }

    fn float(&mut self, x: f64) -> VReg {
        self.value(Cls::F64, |d| Inst::Fconst(d, x.to_bits(), true))
    }

    fn bin(&mut self, op: FOp, a: VReg, b: VReg) -> VReg {
        self.value(Cls::F64, |d| Inst::FBin(op, true, d, a, b))
    }

    fn abs(&mut self, a: VReg) -> VReg {
        self.value(Cls::F64, |d| Inst::FUnary(FUn::Abs, true, d, a))
    }

    fn cmp(&mut self, cond: Cond, a: VReg, b: VReg) -> VReg {
        self.value(Cls::I, |d| Inst::FCmp(cond, true, d, a, b))
    }

    fn load(&mut self, array: Array, i: u32) -> Option<VReg> {
        let offset = array.offset(i)?;
        Some(self.value(Cls::F64, |d| Inst::Load(Mem::F64, d, array.base, offset)))
    }

    fn store(&mut self, array: Array, i: u32, v: VReg) -> Option<()> {
        self.emit(Inst::Store(Mem::F64, array.base, array.offset(i)?, v));
        Some(())
    }

    fn sub_product(&mut self, dst: Array, i: u32, left: VReg, right: VReg) -> Option<()> {
        let old = self.load(dst, i)?;
        let product = self.bin(FOp::Mul, left, right);
        let new = self.bin(FOp::Sub, old, product);
        self.store(dst, i, new)
    }

    fn refactor(&mut self, p: &LuPlan, vals: Array, slots: Array, recip: Array) -> Option<()> {
        let zero = self.float(0.0);
        let one = self.float(1.0);
        let tol = self.float(p.pivot_tol);
        let infinity = self.float(f64::INFINITY);
        for (k, col) in (0..p.n).zip(&p.columns) {
            for &(slot, value) in p.loads.get(col.loads.start as usize..col.loads.end as usize)? {
                let v = if value == 0 {
                    zero
                } else {
                    let input = self.load(vals, value)?;
                    self.bin(FOp::Add, zero, input)
                };
                self.store(slots, slot, v)?;
            }
            for (u_slot, subs) in p.updates.get(col.updates.start as usize..col.updates.end as usize)? {
                let u = self.load(slots, *u_slot)?;
                for &(dst, l_slot) in p.subs.get(subs.start as usize..subs.end as usize)? {
                    let l = self.load(slots, l_slot)?;
                    self.sub_product(slots, dst, l, u)?;
                }
            }
            let diag = self.load(slots, col.diag)?;
            let magnitude = self.abs(diag);
            let mut max = magnitude;
            for i in col.lower.clone() {
                let l = self.load(slots, i)?;
                let x = self.abs(l);
                // max and x are magnitudes. Replace a NaN accumulator with x, and keep
                // a numeric accumulator when x is NaN (the ordered greater test is false).
                let greater = self.cmp(Cond::Gt, x, max);
                let max_nan = self.cmp(Cond::Ne, max, max);
                let take_x = self.value(Cls::I, |d| Inst::IBin(IOp::Or, U64, d, greater, max_nan));
                max = self.value(Cls::F64, |d| Inst::Sel(d, take_x, x, max));
            }
            let positive = self.cmp(Cond::Gt, magnitude, zero);
            let threshold = self.bin(FOp::Mul, tol, max);
            let large = self.cmp(Cond::Ge, magnitude, threshold);
            // A maximum of absolute values is finite exactly when it is ordered below +inf.
            let finite = self.cmp(Cond::Lt, max, infinity);
            let usable = self.value(Cls::I, |d| Inst::IBin(IOp::And, U64, d, positive, large));
            let ok = self.value(Cls::I, |d| Inst::IBin(IOp::And, U64, d, usable, finite));
            let next = self.f.block();
            let failed = self.f.block();
            self.term(Term::Branch(ok, next, failed));
            self.cur = failed;
            self.ret(i64::from(k));
            self.cur = next;

            let inv = self.bin(FOp::Div, one, diag);
            self.store(recip, k, inv)?;
            for i in col.lower.clone() {
                let l = self.load(slots, i)?;
                let scaled = self.bin(FOp::Mul, l, inv);
                self.store(slots, i, scaled)?;
            }
        }
        self.ret(-1);
        Some(())
    }

    fn nonzero_updates(
        &mut self,
        value: VReg,
        zero: VReg,
        range: Range<u32>,
        mut update: impl FnMut(&mut Self, u32) -> Option<()>,
    ) -> Option<()> {
        if !range.is_empty() {
            let is_zero = self.cmp(Cond::Eq, value, zero);
            let body = self.f.block();
            let next = self.f.block();
            self.term(Term::Branch(is_zero, next, body));
            self.cur = body;
            for i in range {
                update(self, i)?;
            }
            self.term(Term::Jump(next));
            self.cur = next;
        }
        Some(())
    }

    fn solve(&mut self, p: &LuPlan, slots: Array, recip: Array, b: Array, w: Array) -> Option<()> {
        let zero = self.float(0.0);
        for bounds in p.starts.windows(2).rev() {
            let steps = bounds[0] as usize..bounds[1] as usize;
            for k in steps.clone() {
                let y = self.load(b, p.piv[k])?;
                self.nonzero_updates(y, zero, p.l_ptr[k]..p.l_ptr[k + 1], |ir, i| {
                    let l = ir.load(slots, p.lower_base.checked_add(i)?)?;
                    ir.sub_product(b, *p.l_row.get(i as usize)?, l, y)
                })?;
            }
            for k in steps.clone() {
                let y = self.load(b, p.piv[k])?;
                self.store(w, k as u32, y)?;
            }
            for k in steps.clone().rev() {
                let y = self.load(w, k as u32)?;
                let inv = self.load(recip, k as u32)?;
                let x = self.bin(FOp::Mul, y, inv);
                self.store(w, k as u32, x)?;
                self.nonzero_updates(x, zero, p.u_ptr[k]..p.u_off[k], |ir, i| {
                    let u = ir.load(slots, i)?;
                    ir.sub_product(w, *p.u_step.get(i as usize)?, u, x)
                })?;
            }
            for k in steps {
                let x = self.load(w, k as u32)?;
                self.nonzero_updates(x, zero, p.u_off[k]..p.u_ptr[k + 1], |ir, i| {
                    let u = ir.load(slots, i)?;
                    let step = *p.u_step.get(i as usize)?;
                    ir.sub_product(b, *p.piv.get(step as usize)?, u, x)
                })?;
            }
        }
        for (k, &col) in (0..p.n).zip(&p.q) {
            let x = self.load(w, k)?;
            self.store(b, col, x)?;
        }
        self.ret(0);
        Some(())
    }
}
