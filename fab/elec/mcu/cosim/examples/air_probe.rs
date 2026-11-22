//! Probes for compiling circuit work to native code through AIR (ESIMRT's compiled track, C0):
//!
//! - A straight-line program shaped like a sparse refactor and a device pass: `updates` updates
//!   `v[l] = 0.5 v[l] + 0.25 v[i] v[j]` over 2000 `f64` values in the state region, one in 64 adding
//!   `0.25 exp(v[i] / 1000)` and one in 64 `0.25 ln(|v[j]| + 1)` (eight plan operations an update:
//!   three loads, four arithmetic, one store; twelve with the math). Built with `AirBuilder`,
//!   compiled with `native::compile_with`, run with `Code::run_kernel` on this thread.
//! - Its compile time cold, through an in-memory kernel store (what a disk hit costs past the
//!   read), and its run time a call, against the same plan run by a Rust loop over index triples
//!   (today's shape of a refactor: two or three index loads a multiply). The Rust loop uses the
//!   portable `exp` and `ln` (`makepad_csg_math::portable`, the bits AIR's `MathFn` promises), and
//!   the two results are compared bit for bit.
//! - The portable `exp` and `ln` against the platform's: time a call and how many results differ.
//! - A device bank as a loop (C3's shape): `DIODES` junctions, each loading its two node voltages
//!   through its index pair, limiting the step as `pnjlim` does (`ln` on the limited branch only),
//!   then `exp`, conductance, current and the new limiting point stored back. The AIR loop
//!   against the same Rust loop on portable math and on the platform's, per junction and in bits.
//!
//! `cargo run --release -p fab-elec-mcu-cosim --example air_probe`

use makepad_csg_math::portable;
use makepad_script_compute::ir::{Bin, Cmp, Op, Program, Region, Stmt, Ty, Un, Val};
use makepad_script_compute::lower::{AirBuilder, MathFn};
use makepad_script_compute::{kcache, native};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

const VALUES: u32 = 2000;

#[derive(Clone, Copy)]
struct Update {
    i: u32,
    j: u32,
    l: u32,
    /// 0 plain, 1 with exp, 2 with ln.
    kind: u8,
}

fn plan(updates: usize, seed: u64) -> Vec<Update> {
    let mut s = seed;
    let mut next = |n: u32| {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((s >> 33) % n as u64) as u32
    };
    (0..updates).map(|k| Update { i: next(VALUES), j: next(VALUES), l: next(VALUES), kind: if k % 64 == 17 { 1 } else if k % 64 == 41 { 2 } else { 0 } }).collect()
}

fn plan_ops(p: &[Update]) -> usize {
    p.iter().map(|u| if u.kind == 0 { 8 } else { 12 }).sum()
}

/// The plan as a Rust loop over its index triples, in AIR's operation order.
fn rust_run(p: &[Update], v: &mut [f64]) {
    for u in p {
        let (a, b, c) = (v[u.i as usize], v[u.j as usize], v[u.l as usize]);
        let mut r = c * 0.5 + a * b * 0.25;
        match u.kind {
            1 => r += portable::exp(a * 1e-3) * 0.25,
            2 => r += portable::ln(b.abs() + 1.0) * 0.25,
            _ => {}
        }
        v[u.l as usize] = r;
    }
}

fn load(b: &mut AirBuilder, k: u32) -> Val {
    let lo = b.load(Ty::I32, Region::State, 2 * k, 1, None);
    let hi = b.load(Ty::I32, Region::State, 2 * k + 1, 1, None);
    b.def(Ty::F64, Op::Bin(Bin::MakeD, hi, lo))
}

fn store(b: &mut AirBuilder, k: u32, v: Val) {
    let lo = b.def(Ty::I32, Op::Un(Un::LoD, v));
    let hi = b.def(Ty::I32, Op::Un(Un::HiD, v));
    b.store(Region::State, 2 * k, 1, None, lo);
    b.store(Region::State, 2 * k + 1, 1, None, hi);
}

fn air(p: &[Update]) -> Program {
    let mut b = AirBuilder::new();
    let (half, quarter, milli, one) = (b.f64c(0.5), b.f64c(0.25), b.f64c(1e-3), b.f64c(1.0));
    for u in p {
        let (a, bb, c) = (load(&mut b, u.i), load(&mut b, u.j), load(&mut b, u.l));
        let ch = b.def(Ty::F64, Op::Bin(Bin::MulD, c, half));
        let ab = b.def(Ty::F64, Op::Bin(Bin::MulD, a, bb));
        let abq = b.def(Ty::F64, Op::Bin(Bin::MulD, ab, quarter));
        let mut r = b.def(Ty::F64, Op::Bin(Bin::AddD, ch, abq));
        match u.kind {
            1 => {
                let x = b.def(Ty::F64, Op::Bin(Bin::MulD, a, milli));
                let e = b.math(MathFn::Exp, &[x]);
                let eq = b.def(Ty::F64, Op::Bin(Bin::MulD, e, quarter));
                r = b.def(Ty::F64, Op::Bin(Bin::AddD, r, eq));
            }
            2 => {
                let ab = b.def(Ty::F64, Op::Un(Un::AbsD, bb));
                let x = b.def(Ty::F64, Op::Bin(Bin::AddD, ab, one));
                let l = b.math(MathFn::Ln, &[x]);
                let lq = b.def(Ty::F64, Op::Bin(Bin::MulD, l, quarter));
                r = b.def(Ty::F64, Op::Bin(Bin::AddD, r, lq));
            }
            _ => {}
        }
        store(&mut b, u.l, r);
    }
    b.finish(0)
}

fn words_of(v: &[f64]) -> Vec<u32> {
    v.iter().flat_map(|x| {
        let bits = x.to_bits();
        [bits as u32, (bits >> 32) as u32]
    }).collect()
}

fn values_of(w: &[u32]) -> Vec<f64> {
    w.chunks(2).map(|c| f64::from_bits(c[0] as u64 | (c[1] as u64) << 32)).collect()
}

static STORE: Mutex<Option<HashMap<u128, Vec<u8>>>> = Mutex::new(None);

fn store_get(k: u128) -> Option<Vec<u8>> {
    STORE.lock().unwrap().as_ref().and_then(|m| m.get(&k).cloned())
}

fn store_put(k: u128, v: Vec<u8>) {
    STORE.lock().unwrap().get_or_insert_with(HashMap::new).insert(k, v);
}

fn initial() -> Vec<f64> {
    (0..VALUES).map(|k| 0.3 + 0.6 * ((k as f64 * 0.618034).fract())).collect()
}

/// One kernel call over `state` (the plan's values as words).
fn call(code: &native::Code, state: &mut [u32]) {
    let mut ctx = [0u32; 64];
    let mut shared = [0u32; 16];
    let (b0, b1) = ([0u32; 4], [0u32; 4]);
    let table = [b0.as_ptr() as u64, 4, b1.as_ptr() as u64, 4];
    // SAFETY: each program reads and writes only the state words its plan or bank holds, which
    // `state` holds (every offset access is clamped to its extent); ctx, shared and the two
    // buffers are live for the call.
    unsafe { code.run_kernel(ctx.as_mut_ptr(), state.as_mut_ptr(), shared.as_mut_ptr(), table.as_ptr(), 1) };
}

fn probe(updates: usize, cached_store: bool) {
    let p = plan(updates, 0x5eed + updates as u64);
    let ops = plan_ops(&p);
    let t = Instant::now();
    let prog = air(&p);
    let build = t.elapsed();
    let stmts = prog.body.len();
    let t = Instant::now();
    let Some(code) = native::compile_with(&prog, 0) else {
        println!("{ops} plan ops: compile_with declined the program");
        return;
    };
    let cold = t.elapsed();
    let cached = cached_store.then(|| {
        let t = Instant::now();
        let again = native::compile_with(&prog, 0).expect("the stored kernel loads");
        (t.elapsed(), again.len())
    });

    // bits: one call against the Rust loop from the same values
    let mut rust = initial();
    let mut state = words_of(&rust);
    call(&code, &mut state);
    rust_run(&p, &mut rust);
    let native_v = values_of(&state);
    let differ = rust.iter().zip(&native_v).filter(|(a, b)| a.to_bits() != b.to_bits()).count();

    // speed: calls in a loop, best of five batches
    let batch = (2_000_000 / ops).max(10);
    let mut best_native = f64::INFINITY;
    let mut best_rust = f64::INFINITY;
    for _ in 0..5 {
        let t = Instant::now();
        for _ in 0..batch {
            call(&code, &mut state);
        }
        best_native = best_native.min(t.elapsed().as_secs_f64() / batch as f64);
        let t = Instant::now();
        for _ in 0..batch {
            rust_run(&p, &mut rust);
        }
        best_rust = best_rust.min(t.elapsed().as_secs_f64() / batch as f64);
    }
    std::hint::black_box((&state, &rust));
    println!(
        "{ops:>6} plan ops ({updates} updates, {stmts} AIR statements, {} bytes of code): build {:.2} ms, compile cold {:.1} ms{}; \
         a call {:.2} us ({:.2} ns an op), the Rust loop {:.2} us ({:.2} ns an op): {:.2}x; bits {} of {VALUES} values differ",
        code.len(), build.as_secs_f64() * 1e3, cold.as_secs_f64() * 1e3,
        cached.map_or(String::new(), |(c, _)| format!(", from the store {:.2} ms", c.as_secs_f64() * 1e3)),
        best_native * 1e6, best_native * 1e9 / ops as f64, best_rust * 1e6, best_rust * 1e9 / ops as f64, best_rust / best_native, differ
    );
}

fn math() {
    let n = 1_000_000;
    let xs: Vec<f64> = (0..n).map(|k| -40.0 + 80.0 * ((k as f64 * 0.618034).fract())).collect();
    let ys: Vec<f64> = (0..n).map(|k| 1e-12 + 1e6 * ((k as f64 * 0.414214).fract())).collect();
    let time = |f: &dyn Fn(f64) -> f64, args: &[f64]| {
        let mut best = f64::INFINITY;
        for _ in 0..5 {
            let t = Instant::now();
            let mut acc = 0.0;
            for &x in args {
                acc += f(std::hint::black_box(x));
            }
            std::hint::black_box(acc);
            best = best.min(t.elapsed().as_secs_f64() * 1e9 / args.len() as f64);
        }
        best
    };
    let (pe, se) = (time(&portable::exp, &xs), time(&f64::exp, &xs));
    let (pl, sl) = (time(&portable::ln, &ys), time(&f64::ln, &ys));
    let de = xs.iter().filter(|&&x| portable::exp(x).to_bits() != x.exp().to_bits()).count();
    let dl = ys.iter().filter(|&&y| portable::ln(y).to_bits() != y.ln().to_bits()).count();
    println!("exp: portable {pe:.2} ns, platform {se:.2} ns, {de} of {n} results differ; ln: portable {pl:.2} ns, platform {sl:.2} ns, {dl} of {n} differ");
}

// -- the device bank ---------------------------------------------------------------------------

const NODES: u32 = 32;
const DIODES: u32 = 64;
/// Words an instance: p, n, then is, nvt, vcrit, vold, g, i as f64 pairs.
const STRIDE: u32 = 14;
const INST0: u32 = 2 * NODES;
const GMIN: f64 = 1e-12;

#[derive(Clone, Copy)]
struct Diode {
    p: u32,
    n: u32,
    is: f64,
    nvt: f64,
    vcrit: f64,
    vold: f64,
    g: f64,
    i: f64,
}

fn bank() -> (Vec<f64>, Vec<Diode>) {
    let x: Vec<f64> = (0..NODES).map(|k| if k == 0 { 0.0 } else { 0.9 * ((k as f64 * 0.618034).fract()) }).collect();
    let d = (0..DIODES).map(|k| {
        let (is, nvt) = (1e-14 * (1.0 + (k % 5) as f64), 0.02585 * (1.0 + 0.05 * (k % 3) as f64));
        let vcrit = nvt * (nvt / (std::f64::consts::SQRT_2 * is)).ln();
        Diode { p: 1 + (k * 7) % (NODES - 1), n: (k * 3) % NODES, is, nvt, vcrit, vold: 0.3 + 0.01 * (k % 7) as f64, g: 0.0, i: 0.0 }
    }).collect();
    (x, d)
}

/// The bank in Rust, as the AIR does it: `exp` and `ln` given.
#[inline(never)]
fn rust_bank(x: &[f64], d: &mut [Diode], exp: fn(f64) -> f64, ln: fn(f64) -> f64) {
    for j in d.iter_mut() {
        let mut vd = x[j.p as usize] - x[j.n as usize];
        let reach = (vd - j.vold).abs() - 2.0 * j.nvt;
        if (vd - j.vcrit).min(reach) > 0.0 {
            vd = j.vold + j.nvt * ln(((vd - j.vold) / j.nvt + 1.0).max(1e-300));
        }
        let e = exp(vd / j.nvt);
        j.g = j.is * e / j.nvt + GMIN;
        j.i = j.is * (e - 1.0);
        j.vold = vd;
    }
}

fn ld64(b: &mut AirBuilder, base: u32, extent: u32, off: Val) -> Val {
    let one = b.i32c(1);
    let off1 = b.def(Ty::I32, Op::Bin(Bin::AddI, off, one));
    let lo = b.load(Ty::I32, Region::State, base, extent, Some(off));
    let hi = b.load(Ty::I32, Region::State, base, extent, Some(off1));
    b.def(Ty::F64, Op::Bin(Bin::MakeD, hi, lo))
}

fn st64(b: &mut AirBuilder, base: u32, extent: u32, off: Val, v: Val) {
    let one = b.i32c(1);
    let off1 = b.def(Ty::I32, Op::Bin(Bin::AddI, off, one));
    let lo = b.def(Ty::I32, Op::Un(Un::LoD, v));
    let hi = b.def(Ty::I32, Op::Un(Un::HiD, v));
    b.store(Region::State, base, extent, Some(off), lo);
    b.store(Region::State, base, extent, Some(off1), hi);
}

fn air_bank() -> Program {
    let mut b = AirBuilder::new();
    let ext = DIODES * STRIDE;
    let j = b.var(Ty::I32);
    let zero = b.i32c(0);
    b.set(j, zero);
    b.open_loop();
    let jv = b.get(j);
    let count = b.i32c(DIODES as i32);
    let done = b.def(Ty::Bool, Op::CmpI(Cmp::Ge, jv, count));
    b.push(Stmt::If(done, vec![Stmt::Break(0)], Vec::new()));
    let stride = b.i32c(STRIDE as i32);
    let o = b.def(Ty::I32, Op::Bin(Bin::MulI, jv, stride));
    let p = b.load(Ty::I32, Region::State, INST0, ext, Some(o));
    let one = b.i32c(1);
    let o1 = b.def(Ty::I32, Op::Bin(Bin::AddI, o, one));
    let n = b.load(Ty::I32, Region::State, INST0, ext, Some(o1));
    let two = b.i32c(2);
    let (p2, n2) = (b.def(Ty::I32, Op::Bin(Bin::MulI, p, two)), b.def(Ty::I32, Op::Bin(Bin::MulI, n, two)));
    let (xp, xn) = (ld64(&mut b, 0, 2 * NODES, p2), ld64(&mut b, 0, 2 * NODES, n2));
    let is = ld64(&mut b, INST0 + 2, ext, o);
    let nvt = ld64(&mut b, INST0 + 4, ext, o);
    let vcrit = ld64(&mut b, INST0 + 6, ext, o);
    let vold = ld64(&mut b, INST0 + 8, ext, o);
    let vd0 = b.def(Ty::F64, Op::Bin(Bin::SubD, xp, xn));
    let vdv = b.var(Ty::F64);
    b.set(vdv, vd0);
    let step = b.def(Ty::F64, Op::Bin(Bin::SubD, vd0, vold));
    let astep = b.def(Ty::F64, Op::Un(Un::AbsD, step));
    let two_d = b.f64c(2.0);
    let twonvt = b.def(Ty::F64, Op::Bin(Bin::MulD, two_d, nvt));
    let reach = b.def(Ty::F64, Op::Bin(Bin::SubD, astep, twonvt));
    let over = b.def(Ty::F64, Op::Bin(Bin::SubD, vd0, vcrit));
    let m = b.def(Ty::F64, Op::Bin(Bin::MinD, over, reach));
    let z = b.f64c(0.0);
    let lim = b.def(Ty::Bool, Op::CmpD(Cmp::Gt, m, z));
    b.open();
    {
        let q = b.def(Ty::F64, Op::Bin(Bin::DivD, step, nvt));
        let onef = b.f64c(1.0);
        let a = b.def(Ty::F64, Op::Bin(Bin::AddD, q, onef));
        let tiny = b.f64c(1e-300);
        let a = b.def(Ty::F64, Op::Bin(Bin::MaxD, a, tiny));
        let l = b.math(MathFn::Ln, &[a]);
        let nl = b.def(Ty::F64, Op::Bin(Bin::MulD, nvt, l));
        let v = b.def(Ty::F64, Op::Bin(Bin::AddD, vold, nl));
        b.set(vdv, v);
    }
    let then = b.close();
    b.push(Stmt::If(lim, then, Vec::new()));
    let vd = b.get(vdv);
    let arg = b.def(Ty::F64, Op::Bin(Bin::DivD, vd, nvt));
    let e = b.math(MathFn::Exp, &[arg]);
    let ise = b.def(Ty::F64, Op::Bin(Bin::MulD, is, e));
    let g0 = b.def(Ty::F64, Op::Bin(Bin::DivD, ise, nvt));
    let gmin = b.f64c(GMIN);
    let g = b.def(Ty::F64, Op::Bin(Bin::AddD, g0, gmin));
    let onef = b.f64c(1.0);
    let em1 = b.def(Ty::F64, Op::Bin(Bin::SubD, e, onef));
    let i = b.def(Ty::F64, Op::Bin(Bin::MulD, is, em1));
    st64(&mut b, INST0 + 10, ext, o, g);
    st64(&mut b, INST0 + 12, ext, o, i);
    st64(&mut b, INST0 + 8, ext, o, vd);
    let jn = b.def(Ty::I32, Op::Bin(Bin::AddI, jv, one));
    b.set(j, jn);
    let body = b.close();
    b.push(Stmt::Loop { cap: DIODES + 1, body });
    b.finish(0)
}

fn bank_words(x: &[f64], d: &[Diode]) -> Vec<u32> {
    let mut w = words_of(x);
    for j in d {
        w.extend_from_slice(&[j.p, j.n]);
        w.extend(words_of(&[j.is, j.nvt, j.vcrit, j.vold, j.g, j.i]));
    }
    w
}

fn bank_back(w: &[u32]) -> Vec<(f64, f64, f64)> {
    (0..DIODES as usize).map(|k| {
        let v = values_of(&w[(INST0 as usize + k * STRIDE as usize + 2)..(INST0 as usize + (k + 1) * STRIDE as usize)]);
        (v[3], v[4], v[5])
    }).collect()
}

fn device_probe() {
    let prog = air_bank();
    let t = Instant::now();
    let Some(code) = native::compile_with(&prog, 0) else {
        println!("device bank: compile_with declined the program");
        return;
    };
    let cold = t.elapsed();
    let (x, d0) = bank();
    // bits: three passes from the same start (the limiting point moves), AIR against Rust portable
    let mut state = bank_words(&x, &d0);
    let mut d = d0.clone();
    let mut differ = 0;
    let mut limited = 0;
    for pass in 0..3 {
        let before: Vec<f64> = d.iter().map(|j| j.vold).collect();
        call(&code, &mut state);
        rust_bank(&x, &mut d, portable::exp, portable::ln);
        if pass == 0 {
            limited = d.iter().zip(&before).zip(&x).filter(|((j, _), _)| (j.vold - (x[j.p as usize] - x[j.n as usize])).abs() > 0.0).count();
        }
        for (k, (vold, g, i)) in bank_back(&state).into_iter().enumerate() {
            let j = &d[k];
            differ += [(vold, j.vold), (g, j.g), (i, j.i)].iter().filter(|(a, b)| a.to_bits() != b.to_bits()).count();
        }
    }
    let batch = 20_000;
    let (mut air_t, mut port_t, mut plat_t) = (f64::INFINITY, f64::INFINITY, f64::INFINITY);
    for _ in 0..5 {
        let fresh = bank_words(&x, &d0);
        let t = Instant::now();
        for _ in 0..batch {
            state[..fresh.len()].copy_from_slice(&fresh);
            call(&code, &mut state);
        }
        air_t = air_t.min(t.elapsed().as_secs_f64() / batch as f64);
        let t = Instant::now();
        for _ in 0..batch {
            d.copy_from_slice(&d0);
            rust_bank(&x, std::hint::black_box(&mut d), portable::exp, portable::ln);
        }
        port_t = port_t.min(t.elapsed().as_secs_f64() / batch as f64);
        let t = Instant::now();
        for _ in 0..batch {
            d.copy_from_slice(&d0);
            rust_bank(&x, std::hint::black_box(&mut d), f64::exp, f64::ln);
        }
        plat_t = plat_t.min(t.elapsed().as_secs_f64() / batch as f64);
    }
    let per = |t: f64| t * 1e9 / DIODES as f64;
    println!(
        "device bank ({DIODES} junctions, {limited} limited on the first pass, {} bytes of code, compile cold {:.1} ms): AIR {:.2} ns a junction, Rust portable {:.2} ns, Rust platform {:.2} ns (each with a {}-word state copy); AIR vs Rust portable {:.2}x, vs platform {:.2}x; bits {differ} of {} results differ",
        code.len(), cold.as_secs_f64() * 1e3, per(air_t), per(port_t), per(plat_t), state.len(), port_t / air_t, plat_t / air_t, 3 * 3 * DIODES
    );
}

fn main() {
    println!("host: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    math();
    device_probe();
    if std::env::args().nth(1).as_deref() == Some("device") {
        return;
    }
    for updates in [400, 1250] {
        probe(updates, false);
    }
    // an in-memory store from here: the second compile of a program is a store hit
    kcache::install(kcache::Hook { get: store_get, put: store_put });
    for updates in [1250, 3750] {
        probe(updates, true);
    }
}
