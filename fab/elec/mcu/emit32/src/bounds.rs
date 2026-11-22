//! Port of `ir::bounds` in platform/script/compute/src/ir.rs to verified AIR.
//! Like that pass, bounds describe the value's u32 bits, variables take the
//! largest bound of all their SETs (including their initial zero), and four
//! rounds propagate variable facts from an initially unknown state.
//!
//! Differences from the source:
//! - Walk verified AIR steps, including omitted operations, instead of IR blocks.
//! - C1's counted head value is in [start, limit - 1] after its exit guard.
//!   Recheck the constant limit and absence of a second counter SET below;
//!   only non-negative, nonempty ranges contribute a u32 bound.
//! - ADD_I also accepts two bounded non-negative operands, with checked sum.
//! - DIV_I by a positive constant and SHR_I by a constant accept a bounded
//!   non-negative dividend/source. A bound below 2^31 proves non-negativity.
//! - MUL_I (also the multiply in FMA) requires that same non-negative proof
//!   for its bounded operand, rather than accepting any u32 bound. Products
//!   and sums remain checked for u32 overflow, as in the source.
//! No branch-condition refinement, load bounds or compiler flags are used.

use fab_elec_mcu_rt::{format::*, module::Module};

use crate::control::{Analysis, CountedLoop, Step, pure};

fn nonnegative(bound: Option<u32>) -> Option<u32> {
    bound.filter(|&b| b <= i32::MAX as u32)
}

fn product(a: usize, b: usize, out: &[Option<u32>], constants: &[Option<u32>]) -> Option<u32> {
    let (x, c) = nonnegative(out[a]).zip(nonnegative(constants[b]))
        .or_else(|| nonnegative(out[b]).zip(nonnegative(constants[a])))?;
    x.checked_mul(c)
}

fn counted_bound(steps: &[Step<'_>], n: usize, c: CountedLoop, constants: &[Option<u32>]) -> Option<u32> {
    let Step::Op(loop_op) = steps[n] else { return None; };
    let Step::Op(cmp) = steps[n + 2] else { return None; };
    // These are safety preconditions, not just C1's code-shape preferences:
    // its constant compare operand is loop-invariant, and the sole counter
    // write is the recognised unit increment at the head. Check again so a
    // future relaxation of counted_loop cannot silently weaken this proof.
    let bound = constants[cmp.args[2] as usize]? as i32;
    let limit = match cmp.sub { CMP_GE => bound, CMP_GT => bound.checked_add(1)?, _ => return None };
    if limit != c.limit || c.start < 0 || c.start >= limit { return None; }
    if steps[n + 9..].iter().take_while(|s| !matches!(s, Step::Op(i) if i.at >= loop_op.end))
        .any(|s| matches!(s, Step::Op(i) if i.op == OP_SET && i.args[0] == c.var && i.at < loop_op.end))
    { return None; }
    Some((limit - 1) as u32)
}

/// Re-prove bounds from the verified entry, never from compiler annotations.
/// C1 guarantees the head GET has no uses outside its loop, and that its
/// only uses before the exit guard are the compare itself. The increment
/// and every other consumer execute after the guard. Thus the head fact
/// also safely bounds its SET, while later GETs use the ordinary SET union.
pub fn bounds(m: &Module<'_>, k: usize, steps: &[Step<'_>], control: &Analysis<'_>) -> Vec<Option<u32>> {
    let e = &m.entries()[k];
    let mut counted = vec![None; e.n_vals as usize];
    for (n, c) in control.counted.iter().enumerate() {
        if let Some(c) = c {
            counted[c.value as usize] = counted_bound(steps, n, *c, &control.constants);
        }
    }
    let mut var_b = vec![None; e.n_vars as usize];
    let mut out = vec![None; e.n_vals as usize];
    for _ in 0..4 {
        out.fill(None);
        let mut constants = vec![None; out.len()];
        let mut sets: Vec<Option<Option<u32>>> = vec![None; var_b.len()];
        for &step in steps {
            let Step::Op(i) = step else { continue; };
            let a = i.args;
            if i.op == OP_SET {
                let set = &mut sets[a[0] as usize];
                *set = Some(match (*set, out[a[1] as usize]) {
                    (None, b) => b,
                    (Some(Some(x)), Some(y)) => Some(x.max(y)),
                    _ => None,
                });
            }
            if !pure(i) || m.ty(k, a[0], false) != TY_I32 { continue; }
            let v = a[0] as usize;
            let x = a[1] as usize;
            out[v] = match i.op {
                OP_GET => counted[v].or(var_b[x]),
                OP_CONST => { constants[v] = Some(a[1]); nonnegative(constants[v]) }
                OP_WRAP => Some(a[2] - 1), // the verifier rejects zero length
                OP_BIN => {
                    let y = a[2] as usize;
                    match i.sub {
                        BIN_ADD_I => out[x].zip(nonnegative(constants[y]))
                            .or_else(|| out[y].zip(nonnegative(constants[x])))
                            .or_else(|| nonnegative(out[x]).zip(nonnegative(out[y])))
                            .and_then(|(x, y)| x.checked_add(y)),
                        BIN_MUL_I => product(x, y, &out, &constants),
                        BIN_AND_I => nonnegative(constants[y]).or(nonnegative(constants[x])).or(out[x]).or(out[y]),
                        BIN_SHR_UI => constants[y].map(|s| out[x].unwrap_or(u32::MAX) >> (s & 31)),
                        BIN_SHR_I => nonnegative(out[x]).zip(constants[y]).map(|(x, s)| x >> (s & 31)),
                        BIN_SHL_I => out[x].zip(constants[y]).and_then(|(x, s)| {
                            if s < 31 { x.checked_shl(s).filter(|&y| y >> s == x && y <= i32::MAX as u32) }
                            else { None }
                        }),
                        BIN_DIV_I => nonnegative(out[x]).zip(nonnegative(constants[y]).filter(|&c| c != 0))
                            .map(|(x, c)| x / c),
                        _ => None,
                    }
                }
                OP_FMA => product(x, a[2] as usize, &out, &constants)
                    .zip(out[a[3] as usize]).and_then(|(m, c)| m.checked_add(c)),
                OP_SEL => out[a[2] as usize].zip(out[a[3] as usize]).map(|(x, y)| x.max(y)),
                _ => None,
            };
        }
        let next: Vec<_> = sets.into_iter().map(|s| s.flatten()).collect();
        if next == var_b { break; }
        var_b = next;
    }
    out
}
