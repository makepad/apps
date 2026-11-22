//! Emitted ports of rt::fmath, specialized to binary32. Inputs a2/a3,
//! result a2; a2..a11 and f0/f1/b0 are scratch. No FSR access or FCR write.
//! Arithmetic guards implement fmath::binary_nan_f32 before the FPU sees
//! an operand (ISA Summary pp. 108–110). Tininess affects only FSR.U (p. 109),
//! which handlers never observe; the caller supplies FCR=0 and enables CP0.
//! Only div/sqrt have internal backward branches: normalize's shift loop and
//! sqrt's correction loops. Their extra visits are annotated beside the code
//! for insns::bound; all other helpers need only their static instruction span.
use fab_elec_mcu_emit32::xtensa::{cc, ccz, Reg};
use fab_elec_mcu_rt::format::*;
use crate::emit::Body;

const SIGN: u32 = 0x8000_0000;
const MAG: u32 = 0x7fff_ffff;
const INF: u32 = 0x7f80_0000;
const HIDDEN: u32 = 0x0080_0000;
const QUIET: u32 = 0x0040_0000;
const NAN: u32 = INF | QUIET;

pub(crate) fn unary(b: &mut Body<'_>, op: u32) {
    match op {
        // Sign operations preserve even signaling NaN payloads, as exec does.
        UN_NEG_F => { b.imm(3, SIGN); b.a.xor(2, 2, 3); }
        UN_ABS_F => { b.imm(3, MAG); b.a.and(2, 2, 3); }
        UN_I2F => { b.a.float_s(0, 2, 0); b.a.rfr(2, 0); }
        UN_F2I => {
            let nan = b.label(); let done = b.label();
            b.imm(3, MAG); b.a.and(3, 2, 3); b.imm(4, INF);
            b.branch(cc::LTU, 4, 3, nan);
            // TRUNC.S saturates finite overflow and +/-infinity to i32's
            // endpoints (pp. 632–633); Rust's NaN-to-zero rule is the guard.
            b.a.wfr(0, 2); b.a.trunc_s(2, 0, 0); b.jump(done);
            b.bind(nan); b.imm(2, 0); b.bind(done);
        }
        UN_SQRT_F => sqrt(b),
        UN_FLOOR_F | UN_CEIL_F | UN_TRUNC_F | UN_ROUND_F => integral(b, op),
        _ => unreachable!("verified f32 unary operation"),
    }
}

// Leaves the original operands alone; returns magnitudes a4/a5, infinity a6.
fn magnitudes(b: &mut Body<'_>) {
    b.imm(6, MAG); b.a.and(4, 2, 6); b.a.and(5, 3, 6); b.imm(6, INF);
}

pub(crate) fn compare(b: &mut Body<'_>, op: u32) {
    let nan = b.label(); let done = b.label();
    magnitudes(b);
    b.branch(cc::LTU, 6, 4, nan); b.branch(cc::LTU, 6, 5, nan);
    b.a.wfr(0, 2); b.a.wfr(1, 3);
    match op {
        CMP_LT => b.a.olt_s(0, 0, 1), CMP_LE => b.a.ole_s(0, 0, 1),
        CMP_GT => b.a.olt_s(0, 1, 0), CMP_GE => b.a.ole_s(0, 1, 0),
        _ => b.a.oeq_s(0, 0, 1),
    }
    b.imm(2, u32::from(op == CMP_NE)); b.imm(7, u32::from(op != CMP_NE));
    b.a.movt(2, 7, 0); b.jump(done);
    b.bind(nan); b.imm(2, u32::from(op == CMP_NE)); b.bind(done);
}

pub(crate) fn binary(b: &mut Body<'_>, op: u32) {
    if op == BIN_DIV_F { div(b); return; }
    if matches!(op, BIN_MIN_F | BIN_MAX_F) {
        // exec selects its second operand on ties (including signed zero)
        // and on unordered comparisons. Do not quiet a selected NaN.
        b.a.mov_n(8, 2); b.a.mov_n(9, 3);
        compare(b, if op == BIN_MIN_F { CMP_LT } else { CMP_GT });
        b.a.mov_n(4, 2); b.a.mov_n(2, 9); b.a.movnez(2, 8, 4);
        return;
    }
    let nan_a = b.label(); let nan_b = b.label(); let invalid = b.label();
    let valid = b.label(); let done = b.label();
    magnitudes(b);
    b.branch(cc::LTU, 6, 4, nan_a); b.branch(cc::LTU, 6, 5, nan_b);
    match op {
        BIN_ADD_F | BIN_SUB_F => {
            b.branch(cc::NE, 4, 6, valid); b.branch(cc::NE, 5, 6, valid);
            b.a.xor(7, 2, 3);
            b.zero(if op == BIN_ADD_F { ccz::LT } else { ccz::GE }, 7, invalid);
        }
        BIN_MUL_F => {
            let second = b.label();
            b.branch(cc::NE, 4, 6, second); b.zero(ccz::EQ, 5, invalid);
            b.bind(second); b.branch(cc::NE, 5, 6, valid); b.zero(ccz::EQ, 4, invalid);
        }
        _ => unreachable!("verified f32 binary operation"),
    }
    b.bind(valid); b.a.wfr(0, 2); b.a.wfr(1, 3);
    match op { BIN_ADD_F => b.a.add_s(0, 0, 1), BIN_SUB_F => b.a.sub_s(0, 0, 1), _ => b.a.mul_s(0, 0, 1) }
    b.a.rfr(2, 0); b.jump(done);
    b.bind(invalid); b.imm(2, NAN); b.jump(done);
    b.bind(nan_b); b.a.mov_n(2, 3);
    b.bind(nan_a); b.imm(6, QUIET); b.a.or(2, 2, 6);
    b.bind(done);
}

// rt::fmath::integral with fraction=23, bias=127. Every operation fits u32.
fn integral(b: &mut Body<'_>, op: u32) {
    let nan = b.label(); let small = b.label(); let pack = b.label();
    let one = b.label(); let small_pack = b.label(); let done = b.label();
    b.imm(3, SIGN); b.a.and(3, 2, 3); b.imm(4, MAG); b.a.and(4, 2, 4);
    b.imm(5, INF); b.branch(cc::LTU, 5, 4, nan); b.branch(cc::EQ, 4, 5, done);
    b.zero(ccz::EQ, 4, done);
    b.shr(5, 4, 23); b.a.addi(5, 5, -127);
    b.imm(6, 23); b.branch(cc::GE, 5, 6, done); b.zero(ccz::LT, 5, small);
    b.a.sub(6, 6, 5); b.imm(7, 1); b.a.ssl(6); b.a.sll(6, 7);
    b.a.addi(7, 6, -1); b.a.and(7, 4, 7); b.a.sub(4, 4, 7);
    match op {
        UN_FLOOR_F | UN_CEIL_F => {
            b.zero(ccz::EQ, 7, pack);
            b.zero(if op == UN_FLOOR_F { ccz::EQ } else { ccz::NE }, 3, pack);
            b.a.add_n(4, 4, 6);
        }
        UN_ROUND_F => {
            b.shr(8, 6, 1); b.branch(cc::LTU, 7, 8, pack); b.a.add_n(4, 4, 6);
        }
        _ => {}
    }
    b.bind(pack); b.a.or(2, 3, 4); b.jump(done);
    b.bind(small); b.imm(4, 0);
    match op {
        UN_FLOOR_F => b.zero(ccz::NE, 3, one),
        UN_CEIL_F => b.zero(ccz::EQ, 3, one),
        UN_ROUND_F => { b.imm(6, u32::MAX); b.branch(cc::EQ, 5, 6, one); }
        _ => {}
    }
    b.jump(small_pack); b.bind(one); b.imm(4, 0x3f80_0000);
    b.bind(small_pack); b.a.or(2, 3, 4); b.jump(done);
    b.bind(nan); b.imm(3, QUIET); b.a.or(2, 2, 3); b.bind(done);
}

// fmath's normalization, with a shift loop standing in for leading_zeros.
// Input magnitude is nonzero and finite. Uses only mag, exponent and a11.
fn normalize(b: &mut Body<'_>, mag: Reg, exponent: Reg) {
    let normal = b.label(); let again = b.label(); let done = b.label();
    b.shr(exponent, mag, 23); b.a.addi(exponent, exponent, -127);
    b.imm(11, HIDDEN); b.branch(cc::GEU, mag, 11, normal);
    b.imm(exponent, (-126i32) as u32); b.bind(again);
    let repeated = b.a.base.offset();
    b.branch(cc::GEU, mag, 11, done);
    b.a.add_n(mag, mag, mag); b.a.addi(exponent, exponent, -1); b.jump(again);
    // A nonzero binary32 significand reaches HIDDEN after at most 23
    // shifts. Charge 24 visits, including the final exit test, conservatively
    // charging the shift/backedge on that last visit too. The op owns visit 1.
    b.repeat_extra(repeated, b.a.base.offset(), 23);
    b.bind(normal); b.imm(11, HIDDEN - 1); b.a.and(mag, mag, 11);
    b.imm(11, HIDDEN); b.a.or(mag, mag, 11); b.bind(done);
}

// Direct port of fmath::div_f32: exact quotient and remainder;
// round once, including subnormal output and ties-to-even at the normal edge.
fn div(b: &mut Body<'_>) {
    let nan_a = b.label(); let nan_b = b.label(); let invalid = b.label();
    let infinity = b.label(); let zero = b.label(); let done = b.label();
    let nonzero = b.label(); let finite = b.label(); let normalized = b.label();
    let subnormal = b.label();
    let round = b.label(); let pack = b.label(); let sub_round = b.label(); let sub_pack = b.label();
    magnitudes(b); b.a.xor(8, 2, 3); b.imm(9, SIGN); b.a.and(8, 8, 9);
    b.branch(cc::LTU, 6, 4, nan_a); b.branch(cc::LTU, 6, 5, nan_b);
    b.zero(ccz::NE, 4, nonzero); b.zero(ccz::EQ, 5, invalid); b.jump(zero);
    b.bind(nonzero); b.branch(cc::NE, 4, 6, finite);
    b.branch(cc::EQ, 5, 6, invalid); b.jump(infinity);
    b.bind(finite); b.zero(ccz::EQ, 5, infinity); b.branch(cc::EQ, 5, 6, zero);
    normalize(b, 4, 6); normalize(b, 5, 7); b.a.sub(6, 6, 7);
    b.branch(cc::GEU, 4, 5, normalized); b.a.add_n(4, 4, 4); b.a.addi(6, 6, -1);
    b.bind(normalized); b.imm(7, 127); b.branch(cc::LT, 7, 6, infinity);
    b.imm(7, (-150i32) as u32); b.branch(cc::LT, 6, 7, zero);
    // Long division of ma << 23 by mb: the leading digit is one, then
    // append 8, 8 and 7 zero bits. rem < mb < 2^24, so rem << bits fits
    // u32 at every step. a2/a4 finish with the exact fmath quotient/remainder.
    b.a.quou(2, 4, 5); b.a.remu(4, 4, 5);
    for bits in [8, 8, 7] {
        b.imm(11, bits); b.a.ssl(11); b.a.sll(4, 4); b.a.sll(2, 2);
        b.a.quou(7, 4, 5); b.a.remu(4, 4, 5); b.a.or(2, 2, 7);
    }
    b.imm(7, (-126i32) as u32); b.branch(cc::LT, 6, 7, subnormal);
    b.a.add_n(4, 4, 4); b.branch(cc::LTU, 5, 4, round); b.branch(cc::NE, 4, 5, pack);
    b.a.extui(7, 2, 0, 1); b.zero(ccz::EQ, 7, pack);
    b.bind(round); b.a.addi(2, 2, 1);
    b.bind(pack); b.a.addi(6, 6, 127); b.shl(6, 6, 23);
    b.imm(7, HIDDEN); b.a.sub(2, 2, 7); b.a.add_n(2, 2, 6); b.a.or(2, 2, 8); b.jump(done);
    b.bind(subnormal); b.a.sub(6, 7, 6); // shift = -126 - exponent, 1..24.
    b.a.mov_n(9, 4); b.imm(7, 1); b.a.ssl(6); b.a.sll(7, 7);
    b.a.addi(10, 7, -1); b.a.and(4, 2, 10); b.shr(7, 7, 1);
    b.a.ssr(6); b.a.srl(2, 2);
    b.branch(cc::LTU, 7, 4, sub_round); b.branch(cc::NE, 7, 4, sub_pack);
    b.zero(ccz::NE, 9, sub_round); b.a.extui(10, 2, 0, 1); b.zero(ccz::EQ, 10, sub_pack);
    b.bind(sub_round); b.a.addi(2, 2, 1);
    b.bind(sub_pack); b.a.or(2, 2, 8); b.jump(done);
    b.bind(infinity); b.imm(2, INF); b.a.or(2, 2, 8); b.jump(done);
    b.bind(zero); b.a.mov_n(2, 8); b.jump(done);
    b.bind(invalid); b.imm(2, NAN); b.jump(done);
    b.bind(nan_b); b.a.mov_n(2, 3);
    b.bind(nan_a); b.imm(6, QUIET); b.a.or(2, 2, 6); b.bind(done);
}

// fmath::sqrt/isqrt specialized to N = m << 23, 2^46 <= N < 2^48.
// Seed with four integer Newton steps from x0 = 2^24. The first simplifies
// to x1 = 2^23 + (m >> 2); the next three compute floor((x + N/x)/2).
// For real Newton, relative error evolves as e' = e^2 / (2*(1+e)):
// e0 <= 1, e1 <= 1/4, e2 <= 1/40, e3 <= 1/3280, e4 <= 1/21523360.
// Thus the real fourth iterate is less than sqrt(N) + 1 (sqrt(N) < 2^24).
// Integer Newton never falls below s = floor(sqrt(N)). Above sqrt(N) its
// monotonic update is bounded by the real iterate; at s it can only give
// s or s+1. Consequently x4 is s or s+1: at most one decrement, no increments.
// Both exact correction loops remain, so seed accuracy affects only speed.
fn sqrt(b: &mut Body<'_>) {
    let nan = b.label(); let invalid = b.label(); let even = b.label();
    let lower = b.label(); let decrement = b.label(); let upper = b.label();
    let increment = b.label(); let remainder = b.label(); let pack = b.label(); let done = b.label();
    b.imm(4, MAG); b.a.and(4, 2, 4); b.imm(5, INF);
    b.branch(cc::LTU, 5, 4, nan); b.zero(ccz::EQ, 4, done);
    b.zero(ccz::LT, 2, invalid); b.branch(cc::EQ, 4, 5, done);
    normalize(b, 4, 6); b.a.extui(3, 6, 0, 1); b.zero(ccz::EQ, 3, even);
    b.a.add_n(4, 4, 4); b.a.addi(6, 6, -1); b.bind(even);
    b.shr(2, 4, 2); b.imm(7, HIDDEN); b.a.add_n(2, 2, 7);
    b.shl(9, 4, 7); // N >> 16 fits u32; the low 16 bits of N are zero.
    b.shr(5, 4, 9); b.shl(4, 4, 23); // Preserve N as a5:a4.
    for _ in 0..3 {
        // Exact N/x by a leading 32-bit digit and two zero octets.
        // x <= 2^24, hence each partial remainder << 8 fits u32.
        b.a.quou(3, 9, 2); b.a.remu(7, 9, 2);
        b.imm(11, 8); b.a.ssl(11);
        b.a.sll(7, 7); b.a.quou(10, 7, 2); b.a.remu(7, 7, 2);
        b.a.sll(3, 3); b.a.or(3, 3, 10);
        b.a.sll(7, 7); b.a.quou(10, 7, 2);
        b.a.sll(3, 3); b.a.or(3, 3, 10);
        b.a.add_n(2, 2, 3); b.shr(2, 2, 1);
    }
    // Compare full products, high word first, unsigned in both halves.
    // Keep a3 = low(r*r), updating it after every accepted increment.
    b.bind(lower); let repeated = b.a.base.offset();
    b.a.mull(3, 2, 2); b.a.muluh(7, 2, 2);
    b.branch(cc::LTU, 5, 7, decrement); b.branch(cc::LTU, 7, 5, upper);
    b.branch(cc::GEU, 4, 3, upper);
    b.bind(decrement); b.a.addi(2, 2, -1); b.jump(lower);
    // The seed proof above permits one decrement: at most two visits to
    // lower, including its exit. The op's static span owns the first visit.
    b.repeat_extra(repeated, b.a.base.offset(), 1);
    b.bind(upper); b.a.addi(9, 2, 1); b.a.mull(10, 9, 9); b.a.muluh(7, 9, 9);
    b.branch(cc::LTU, 5, 7, remainder); b.branch(cc::LTU, 7, 5, increment);
    b.branch(cc::LTU, 4, 10, remainder);
    b.bind(increment); b.a.mov_n(2, 9); b.a.mov_n(3, 10); b.jump(upper);
    // The same proof permits no increments. upper is visited at most once,
    // already charged by the enclosing op, despite its backward branch.
    // 0 <= N-r*r <= 2*r < 2^25: low-word subtraction gives the complete
    // remainder, including when the subtraction borrows from N's high word.
    b.bind(remainder); b.a.sub(3, 4, 3);
    // Integer radicands cannot tie: rem > root iff sqrt rounds upward.
    b.branch(cc::GEU, 2, 3, pack); b.a.addi(2, 2, 1);
    b.bind(pack); b.imm(11, 1); b.a.ssr(11); b.a.sra(6, 6); b.a.addi(6, 6, 127);
    b.shl(6, 6, 23); b.imm(7, HIDDEN); b.a.sub(2, 2, 7); b.a.add_n(2, 2, 6); b.jump(done);
    b.bind(invalid); b.imm(2, NAN); b.jump(done);
    b.bind(nan); b.imm(3, QUIET); b.a.or(2, 2, 3); b.bind(done);
}
