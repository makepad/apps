//! Windowed leaf text for the island's pure libc and compiler imports.
//! Clean-room implementation from the C definitions, IEEE 754 and the Xtensa ISA.
//! `__fixdfsi` follows Rust's `as i32`: truncate toward zero, saturate at i32::MIN/MAX, and return 0 for NaN.
//! `sprintf` copies unsupported specifications literally from '%' through the conversion character,
//! or up to the format's NUL for an incomplete specification. These cases consume no argument.

use fab_elec_mcu_emit32::xtensa::{assemble_relaxed, cc, ccz, enc, reg::*, Asm};
use fab_elec_mcu_island::{Glue, GlueRegion};

pub const HELPERS: [&str; 27] = [
    "memcpy", "memset", "memcmp", "memmove", "strlen", "strncmp", "strncpy", "strnlen", "strcpy",
    "hexstr2bin", "__divdi3", "__moddi3", "__udivdi3", "__umoddi3", "__bswapsi2",
    "__extendsfdf2", "__truncdfsf2", "__fixdfsi", "__floatsidf", "__floatunsidf", "__floatundisf",
    "__adddf3", "__subdf3", "__muldf3", "__divdf3", "__divsf3", "sprintf",
];

fn leaf(name: &str, labels: usize, body: impl Fn(&mut Asm)) -> Glue {
    framed_leaf(name, labels, 32, body)
}

fn framed_leaf(name: &str, labels: usize, frame: u32, body: impl Fn(&mut Asm)) -> Glue {
    let bytes = assemble_relaxed(&mut Asm::new(labels, A8), |a| {
        a.entry(A1, frame);
        body(a);
        a.retw_n();
    });
    Glue { name: name.into(), region: GlueRegion::Text, align: 4, entry: 0, bytes, relocations: Vec::new() }
}

#[derive(Clone, Copy, PartialEq)]
enum Transfer { Forward, Backward, Fill }

// a2 holds the original destination throughout. a5 is its cursor, a3 the
// source cursor or fill byte, and a4 the remaining length. Backward cursors
// point one past the next unit, so decrement both before touching memory.
fn transfer_unit(a: &mut Asm, kind: Transfer, word: bool) {
    let width = if word { 4 } else { 1 };
    if kind == Transfer::Backward {
        a.addi(A3, A3, -width);
        a.addi(A5, A5, -width);
    }
    if word {
        if kind != Transfer::Fill { a.l32i_n(A6, A3, 0); }
        a.s32i_n(A6, A5, 0);
    } else if kind == Transfer::Fill {
        a.s8i(A3, A5, 0);
    } else {
        a.l8ui(A6, A3, 0);
        a.s8i(A6, A5, 0);
    }
    if kind == Transfer::Forward { a.addi(A3, A3, width); }
    if kind != Transfer::Backward { a.addi(A5, A5, width); }
    a.addi(A4, A4, -width);
}

fn transfer(a: &mut Asm, kind: Transfer, first_label: usize) {
    let [head, aligned, words, tail, bytes, done] = std::array::from_fn(|i| first_label + i);
    if kind != Transfer::Fill {
        a.xor(A7, A5, A3);
        a.extui(A7, A7, 0, 2);
        a.bccz_reg_label(ccz::NE, A7, tail);
    }
    a.base.label_assign(head);
    a.bcci_reg_label(cc::LTU, A4, 4, tail);
    a.extui(A7, A5, 0, 2);
    a.bccz_reg_label(ccz::EQ, A7, aligned);
    transfer_unit(a, kind, false);
    a.j_label(head);

    a.base.label_assign(aligned);
    if kind == Transfer::Fill {
        a.extui(A6, A3, 0, 8);
        a.slli(A7, A6, 8);
        a.or(A6, A6, A7);
        a.slli(A7, A6, 16);
        a.or(A6, A6, A7);
    }
    a.base.label_assign(words);
    transfer_unit(a, kind, true);
    a.bcci_reg_label(cc::GEU, A4, 4, words);

    a.base.label_assign(tail);
    a.bccz_reg_label(ccz::EQ, A4, done);
    a.base.label_assign(bytes);
    transfer_unit(a, kind, false);
    a.bccz_reg_label(ccz::NE, A4, bytes);
    a.base.label_assign(done);
}

fn memmove(a: &mut Asm) {
    let (forward, done) = (12, 13);
    a.sub(A7, A2, A3);
    a.bccz_reg_label(ccz::EQ, A7, done);
    // Only a destination strictly inside the source needs a backward copy.
    a.bcc_reg_reg_label(cc::GEU, A7, A4, forward);
    a.add_n(A5, A2, A4);
    a.add_n(A3, A3, A4);
    transfer(a, Transfer::Backward, 0);
    a.retw_n();

    a.base.label_assign(forward);
    a.mov_n(A5, A2);
    transfer(a, Transfer::Forward, 6);
    a.base.label_assign(done);
}

fn compare(a: &mut Asm, stop_at_nul: bool) {
    let (next, done) = (0, 1);
    a.mov_n(A5, A2);
    a.movi_n(A2, 0);
    a.bccz_reg_label(ccz::EQ, A4, done);
    a.base.label_assign(next);
    a.l8ui(A6, A5, 0);
    a.l8ui(A7, A3, 0);
    a.sub(A2, A6, A7);
    a.bccz_reg_label(ccz::NE, A2, done);
    if stop_at_nul { a.bccz_reg_label(ccz::EQ, A6, done); }
    a.addi(A5, A5, 1);
    a.addi(A3, A3, 1);
    a.addi(A4, A4, -1);
    a.bccz_reg_label(ccz::NE, A4, next);
    a.base.label_assign(done);
}

fn length(a: &mut Asm, bounded: bool) {
    let (next, done) = (0, 1);
    a.mov_n(A4, A2);
    if bounded { a.add_n(A3, A2, A3); }
    a.base.label_assign(next);
    if bounded { a.bcc_reg_reg_label(cc::EQ, A4, A3, done); }
    a.l8ui(A5, A4, 0);
    a.bccz_reg_label(ccz::EQ, A5, done);
    a.addi(A4, A4, 1);
    a.j_label(next);
    a.base.label_assign(done);
    a.sub(A2, A4, A2);
}

fn strncpy(a: &mut Asm) {
    let (next, pad, done) = (0, 1, 2);
    a.mov_n(A5, A2);
    a.bccz_reg_label(ccz::EQ, A4, done);
    a.base.label_assign(next);
    a.l8ui(A6, A3, 0);
    a.bccz_reg_label(ccz::EQ, A6, pad);
    a.s8i(A6, A5, 0);
    a.addi(A3, A3, 1);
    a.addi(A5, A5, 1);
    a.addi(A4, A4, -1);
    a.bccz_reg_label(ccz::NE, A4, next);
    a.retw_n();

    a.base.label_assign(pad);
    a.s8i(A6, A5, 0);
    a.addi(A5, A5, 1);
    a.addi(A4, A4, -1);
    a.bccz_reg_label(ccz::NE, A4, pad);
    a.base.label_assign(done);
}

fn strcpy(a: &mut Asm) {
    a.mov_n(A5, A2);
    a.base.label_assign(0);
    a.l8ui(A6, A3, 0);
    a.s8i(A6, A5, 0);
    a.addi(A3, A3, 1);
    a.addi(A5, A5, 1);
    a.bccz_reg_label(ccz::NE, A6, 0);
}

fn hexstr2bin(a: &mut Asm) {
    let (next, high_ok, low_ok, success, invalid) = (0, 1, 2, 3, 4);
    a.bccz_reg_label(ccz::EQ, A4, success);
    a.movi_n(A8, 0x20);
    a.base.label_assign(next);
    for (offset, nibble, valid) in [(0, A5, high_ok), (1, A6, low_ok)] {
        a.l8ui(A7, A2, offset);
        a.addi(nibble, A7, -i32::from(b'0'));
        a.bcci_reg_label(cc::LTU, nibble, 10, valid);
        a.or(A7, A7, A8);
        a.addi(nibble, A7, -i32::from(b'a'));
        a.bcci_reg_label(cc::GEU, nibble, 6, invalid);
        a.addi(nibble, nibble, 10);
        a.base.label_assign(valid);
    }
    a.slli(A5, A5, 4);
    a.or(A5, A5, A6);
    a.s8i(A5, A3, 0);
    a.addi(A2, A2, 2);
    a.addi(A3, A3, 1);
    a.addi(A4, A4, -1);
    a.bccz_reg_label(ccz::NE, A4, next);
    a.base.label_assign(success);
    a.movi_n(A2, 0);
    a.retw_n();
    a.base.label_assign(invalid);
    a.movi_n(A2, -1);
}

#[derive(Clone, Copy, PartialEq)]
enum DivisionResult { Quotient, Remainder }

// Negate the pair modulo 2^64 when sign is negative. Subtracting a nonzero
// low word from zero borrows one from the high word. sign may alias hi.
fn negate_if_negative(a: &mut Asm, sign: u8, lo: u8, hi: u8, done: usize) {
    a.bccz_reg_label(ccz::GE, sign, done);
    a.neg(lo, lo);
    a.neg(hi, hi);
    a.bccz_reg_label(ccz::EQ, lo, done);
    a.addi(hi, hi, -1);
    a.base.label_assign(done);
}

fn division(name: &str, signed: bool, result: DivisionResult) -> Glue {
    let quotient = result == DivisionResult::Quotient;
    leaf(name, 10, |a| {
        let (nonzero, dividend_abs, divisor_abs, divide_high, wide) = (0, 1, 2, 3, 4);
        let (bit, subtract, advance, apply_sign, done) = (5, 6, 7, 8, 9);

        // Handle zero before taking magnitudes: x % 0 returns both original words.
        a.or(A9, A4, A5);
        a.bccz_reg_label(ccz::NE, A9, nonzero);
        if quotient {
            a.movi_n(A2, -1);
            a.movi_n(A3, -1);
        }
        a.retw_n();
        a.base.label_assign(nonzero);
        if signed {
            if quotient { a.xor(A10, A3, A5); } else { a.mov_n(A10, A3); }
            negate_if_negative(a, A3, A2, A3, dividend_abs);
            negate_if_negative(a, A5, A4, A5, divisor_abs);
        }

        // Divide the high word first. A two-word divisor exceeds any one-word
        // prefix, so its high quotient digit is zero and its remainder is that
        // prefix. For a one-word divisor hardware produces both directly.
        a.mov_n(A6, A3);
        a.movi_n(A3, 0);
        a.bccz_reg_label(ccz::NE, A5, wide);
        a.bccz_reg_label(ccz::NE, A6, divide_high);
        if quotient { a.quou(A2, A2, A4); } else { a.remu(A2, A2, A4); }
        a.j_label(apply_sign);
        a.base.label_assign(divide_high);
        if quotient { a.quou(A3, A6, A4); }
        a.remu(A6, A6, A4);

        a.base.label_assign(wide);
        a.movi_n(A7, 0);
        a.movi_n(A8, 32);
        a.base.label_assign(bit);
        // Shift the next dividend bit from a2 into the remainder (a6 low,
        // a7 high). Its value fits in the prefix consumed so far, at most
        // 64 bits. Each consumed bit frees one position in a2 for the quotient.
        a.extui(A9, A6, 31, 1);
        a.addx2(A7, A7, A9);
        a.extui(A9, A2, 31, 1);
        a.addx2(A6, A6, A9);
        a.add_n(A2, A2, A2);
        a.bcc_reg_reg_label(cc::LTU, A7, A5, advance);
        a.bcc_reg_reg_label(cc::GEU, A6, A4, subtract);
        a.bcc_reg_reg_label(cc::EQ, A7, A5, advance);
        a.addi(A7, A7, -1); // high > divisor high, low word needs a borrow
        a.base.label_assign(subtract);
        a.sub(A7, A7, A5);
        a.sub(A6, A6, A4);
        if quotient { a.addi(A2, A2, 1); }
        a.base.label_assign(advance);
        a.addi(A8, A8, -1);
        a.bccz_reg_label(ccz::NE, A8, bit);
        if !quotient {
            a.mov_n(A2, A6);
            a.mov_n(A3, A7);
        }
        a.base.label_assign(apply_sign);
        if signed { negate_if_negative(a, A10, A2, A3, done); }
    })
}

fn bswap(a: &mut Asm) {
    a.slli(A3, A2, 24);
    for (from, to) in [(8, 16), (16, 8)] {
        a.extui(A4, A2, from, 8);
        a.slli(A4, A4, to);
        a.or(A3, A3, A4);
    }
    a.extui(A2, A2, 24, 8);
    a.or(A2, A2, A3);
}

// Put the leading one of a nonzero word in bit 31. a8 receives the shift
// distance, so the same search adjusts exponents and aligns a trailing word.
fn normalize_word(a: &mut Asm, word: u8, first_label: usize) {
    a.movi_n(A8, 0);
    for (i, shift) in [16, 8, 4, 2, 1].into_iter().enumerate() {
        let skip = first_label + i;
        if shift == 1 {
            a.bccz_reg_label(ccz::LT, word, skip);
        } else {
            a.extui(A7, word, 32 - shift, shift);
            a.bccz_reg_label(ccz::NE, A7, skip);
        }
        a.slli(word, word, shift);
        a.addi(A8, A8, i32::from(shift));
        a.base.label_assign(skip);
    }
}

fn sign_bit(a: &mut Asm, word: u8) {
    a.extui(A5, word, 31, 1);
    a.slli(A5, A5, 31);
}

// a2 holds the fraction left aligned, with its implicit bit already removed.
// Its high 20 bits go beside the exponent and its low 12 start the low word.
fn pack_double_word(a: &mut Asm, signed: bool) {
    a.srli(A3, A2, 12);
    a.slli(A2, A2, 20);
    a.slli(A4, A4, 20);
    a.or(A3, A3, A4);
    if signed { a.or(A3, A3, A5); }
}

fn extend_single(a: &mut Asm) {
    let (finite, subnormal, pack) = (0, 1, 2);
    sign_bit(a, A2);
    a.extui(A4, A2, 23, 8);
    a.slli(A2, A2, 9);
    a.movi(A6, 255);
    a.bcc_reg_reg_label(cc::NE, A4, A6, finite);
    a.movi(A4, 2047);
    a.movi_n(A6, 1);
    a.slli(A6, A6, 31);
    a.movnez(A2, A6, A2); // canonical quiet fraction for NaN, zero for infinity
    a.j_label(pack);

    a.base.label_assign(finite);
    a.movi(A6, 1023 - 127);
    a.bccz_reg_label(ccz::EQ, A4, subnormal);
    a.add_n(A4, A4, A6);
    a.j_label(pack);
    a.base.label_assign(subnormal);
    a.bccz_reg_label(ccz::EQ, A2, pack); // exponent and fraction are both zero
    normalize_word(a, A2, 3);
    a.sub(A4, A6, A8); // unbiased exponent = -127 - leading zeros
    a.slli(A2, A2, 1);
    a.base.label_assign(pack);
    pack_double_word(a, true);
}

// Top 32 bits of a normal binary64 significand, with the implicit one in
// bit 31 of a6. The remaining 21 bits are still in the original low word a2.
fn double_significand(a: &mut Asm) {
    a.slli(A6, A3, 11);
    a.extui(A7, A2, 21, 11);
    a.or(A6, A6, A7);
    a.movi_n(A7, 1);
    a.slli(A7, A7, 31);
    a.or(A6, A6, A7);
}

// Discard the low byte of a6. At exactly 0x80, a2 distinguishes a nonzero
// tail from a tie, and the retained low bit decides the tie. Adding the
// significand to the exponent base lets a carry cross either format boundary.
fn round_single(a: &mut Asm, increment: usize, done: usize) {
    a.extui(A7, A6, 0, 8);
    a.srli(A6, A6, 8);
    a.slli(A4, A4, 23);
    a.add_n(A6, A6, A4);
    a.bcci_reg_label(cc::LTU, A7, 128, done);
    a.bcci_reg_label(cc::NE, A7, 128, increment);
    a.extui(A7, A6, 0, 1);
    a.or(A7, A7, A2);
    a.bccz_reg_label(ccz::EQ, A7, done);
    a.base.label_assign(increment);
    a.addi(A6, A6, 1);
    a.base.label_assign(done);
}

fn truncate_double(a: &mut Asm) {
    let (finite, infinity, below_overflow, in_range, round, increment, done) = (0, 1, 2, 3, 4, 5, 6);
    sign_bit(a, A3);
    a.extui(A4, A3, 20, 11);
    a.movi(A8, 2047);
    a.bcc_reg_reg_label(cc::NE, A4, A8, finite);
    a.slli(A7, A3, 12);
    a.or(A7, A7, A2);
    a.bccz_reg_label(ccz::EQ, A7, infinity);
    a.movi(A6, 511);
    a.slli(A6, A6, 22); // all exponent bits and the quiet NaN bit
    a.j_label(done);

    a.base.label_assign(finite);
    a.movi(A8, 1023 + 128);
    a.bcc_reg_reg_label(cc::LTU, A4, A8, below_overflow);
    a.base.label_assign(infinity);
    a.movi(A6, 255);
    a.slli(A6, A6, 23);
    a.j_label(done);
    a.base.label_assign(below_overflow);
    a.movi(A8, 1023 - 150); // half the smallest subnormal is 2^-150
    a.bcc_reg_reg_label(cc::GEU, A4, A8, in_range);
    a.mov_n(A2, A5);
    a.retw_n();

    a.base.label_assign(in_range);
    double_significand(a);
    a.slli(A2, A2, 11); // keep all 21 trailing bits for the rounding decision
    a.movi(A8, 1023 - 127 + 1);
    a.sub(A4, A4, A8); // exponent base excludes the implicit one in a6
    a.bccz_reg_label(ccz::GE, A4, round);
    // A subnormal needs another 1..24 bits shifted out. SSR sets SAR to
    // that distance: SLL captures the lost bits while SRL aligns the result.
    a.neg(A8, A4);
    a.ssr(A8);
    a.sll(A7, A6);
    a.or(A2, A2, A7);
    a.srl(A6, A6);
    a.movi_n(A4, 0);
    a.base.label_assign(round);
    round_single(a, increment, done);
    a.or(A2, A6, A5);
}

fn fix_double(a: &mut Asm) {
    let (large, saturate, zero, done) = (0, 1, 2, 3);
    a.extui(A4, A3, 20, 11);
    a.movi(A5, 1023);
    a.sub(A4, A4, A5);
    a.bccz_reg_label(ccz::LT, A4, zero);
    a.movi_n(A5, 31);
    a.bcc_reg_reg_label(cc::GE, A4, A5, large);
    double_significand(a);
    a.sub(A5, A5, A4);
    a.ssr(A5);
    a.srl(A2, A6); // exponent 0..30: discarding fraction bits truncates |x|
    a.bccz_reg_label(ccz::GE, A3, done);
    a.neg(A2, A2);
    a.retw_n();

    a.base.label_assign(large);
    a.movi(A5, 1024);
    a.bcc_reg_reg_label(cc::NE, A4, A5, saturate);
    a.slli(A7, A3, 12);
    a.or(A7, A7, A2);
    a.bccz_reg_label(ccz::NE, A7, zero);
    a.base.label_assign(saturate);
    a.srai(A7, A3, 31);
    a.movi_n(A2, -1);
    a.srli(A2, A2, 1);
    a.xor(A2, A2, A7); // MAX for positive, its complement MIN for negative
    a.retw_n();
    a.base.label_assign(zero);
    a.movi_n(A2, 0);
    a.base.label_assign(done);
}

fn word_to_double(a: &mut Asm, signed: bool) {
    let (magnitude, done) = (0, 1);
    a.movi_n(A3, 0);
    a.bccz_reg_label(ccz::EQ, A2, done);
    if signed {
        sign_bit(a, A2);
        a.bccz_reg_label(ccz::GE, A2, magnitude);
        a.neg(A2, A2); // i32::MIN becomes the unsigned magnitude 2^31
        a.base.label_assign(magnitude);
    }
    normalize_word(a, A2, 2);
    a.movi(A4, 1023 + 31);
    a.sub(A4, A4, A8);
    a.slli(A2, A2, 1);
    pack_double_word(a, signed);
    a.base.label_assign(done);
}

fn u64_to_single(a: &mut Asm) {
    let (normalize, increment, rounded, done) = (0, 1, 2, 3);
    a.mov_n(A6, A3);
    a.movi(A4, 127 + 63 - 1);
    a.bccz_reg_label(ccz::NE, A6, normalize);
    a.mov_n(A6, A2);
    a.bccz_reg_label(ccz::EQ, A6, done);
    a.movi_n(A2, 0);
    a.movi(A4, 127 + 31 - 1);
    a.base.label_assign(normalize);
    normalize_word(a, A6, 4);
    a.sub(A4, A4, A8);
    // SSL sets SAR = 32 - shift. Bring the low word's top bits into a6
    // and keep its remaining bits in a2, including when shift is zero.
    a.ssl(A8);
    a.srl(A7, A2);
    a.or(A6, A6, A7);
    a.sll(A2, A2);
    round_single(a, increment, rounded);
    a.mov_n(A2, A6);
    a.base.label_assign(done);
}

// Arithmetic keeps a nonzero significand left aligned in two words. a10/a11
// are the biased exponents, extended below 1 for normalized subnormal inputs.
// a12 holds the result sign. The final pack rounds once, after denormalization.
const NAN: usize = 0;
const INFINITY: usize = 1;
const ZERO: usize = 2;
const PACK: usize = 3;
const ZERO_EXP: i32 = -2048; // below every normalized input exponent, even for binary64

// RRR fields from the Xtensa ISA: SALTU (RST2, op2=6), SRC (RST1, op2=8)
// and SSAI (ST1, sa[4] in t). SRC concatenates high:low and shifts by SAR.
fn saltu(a: &mut Asm, dest: u8, x: u8, y: u8) { a.op24(enc::rrr(0, 2, 6, dest, x, y)); }
fn src(a: &mut Asm, dest: u8, high: u8, low: u8) { a.op24(enc::rrr(0, 1, 8, dest, high, low)); }
fn ssai(a: &mut Asm, shift: u8) { a.op24(enc::rrr(0, 0, 4, 4, shift & 15, shift >> 4)); }

// Six labels. Move a nonzero pair's leading one to bit 63; adjust its exponent
// by the same shift. a7/a8 are scratch, as in normalize_word.
fn normalize_pair(a: &mut Asm, lo: u8, hi: u8, exp: u8, first: usize) {
    a.bccz_reg_label(ccz::NE, hi, first);
    a.mov_n(hi, lo);
    a.movi_n(lo, 0);
    a.addi(exp, exp, -32);
    a.base.label_assign(first);
    normalize_word(a, hi, first + 1);
    a.sub(exp, exp, A8);
    a.ssl(A8);
    a.srl(A7, lo);
    a.or(hi, hi, A7);
    a.sll(lo, lo);
}

// Ten labels per operand. The fraction is first left aligned without its
// hidden bit: this tests every NaN payload bit and gives subnormals the same
// leading-zero search as cancellation. Normal inputs skip the search.
fn unpack(a: &mut Asm, lo: u8, hi: u8, exp: u8, single: bool, first: usize) {
    let (finite, small, nonzero, done) = (first, first + 1, first + 2, first + 3);
    if single {
        a.extui(exp, hi, 23, 8);
        a.slli(hi, hi, 9);
        a.movi_n(lo, 0);
    } else {
        a.extui(exp, hi, 20, 11);
        a.slli(hi, hi, 12);
        a.extui(A7, lo, 20, 12);
        a.or(hi, hi, A7);
        a.slli(lo, lo, 12);
    }
    a.movi(A7, if single { 255 } else { 2047 });
    a.bcc_reg_reg_label(cc::NE, exp, A7, finite);
    a.or(A7, hi, lo);
    a.bccz_reg_label(ccz::NE, A7, NAN);
    a.j_label(done); // infinity retains its all-ones exponent
    a.base.label_assign(finite);
    a.bccz_reg_label(ccz::EQ, exp, small);
    ssai(a, 1);
    src(a, lo, hi, lo);
    a.srli(hi, hi, 1);
    a.movi_n(A7, 1);
    a.slli(A7, A7, 31);
    a.or(hi, hi, A7);
    a.j_label(done);
    a.base.label_assign(small);
    a.or(A7, hi, lo);
    a.bccz_reg_label(ccz::NE, A7, nonzero);
    a.movi(exp, ZERO_EXP);
    a.j_label(done);
    a.base.label_assign(nonzero);
    normalize_pair(a, lo, hi, exp, first + 4);
    a.base.label_assign(done);
}

// Right shift by a6, OR-ing all lost bits into bit 0. Splitting at 32
// preserves both the whole discarded word and the partial one. For a shift
// of 64 or more the entire result is a single sticky bit.
fn shift_right_jam(a: &mut Asm, lo: u8, hi: u8, first: usize) {
    let (partial, jam) = (first, first + 1);
    a.movi_n(A7, 0);
    a.bcci_reg_label(cc::LTU, A6, 32, partial);
    a.mov_n(A7, lo);
    a.mov_n(lo, hi);
    a.movi_n(hi, 0);
    a.addi(A6, A6, -32);
    a.bcci_reg_label(cc::LTU, A6, 32, partial);
    a.or(A7, A7, lo);
    a.movi_n(lo, 0);
    a.j_label(jam);
    a.base.label_assign(partial);
    a.ssr(A6);
    a.sll(A8, lo);
    a.or(A7, A7, A8);
    src(a, lo, hi, lo);
    a.srl(hi, hi);
    a.base.label_assign(jam);
    a.movi_n(A8, 1);
    a.movnez(A7, A8, A7);
    a.or(lo, lo, A7);
}

fn pack_arithmetic(a: &mut Asm, single: bool, first: usize) {
    let (normal, increment, rounded) = (first + 2, first + 3, first + 4);
    a.base.label_assign(PACK);
    a.movi(A6, if single { 255 } else { 2047 });
    a.bcc_reg_reg_label(cc::GE, A10, A6, INFINITY);
    a.bcci_reg_label(cc::GE, A10, 1, normal);
    a.movi_n(A6, 1);
    a.sub(A6, A6, A10);
    shift_right_jam(a, A2, A3, first);
    a.movi_n(A10, 1);
    a.base.label_assign(normal);
    a.addi(A4, A10, -1); // the retained leading bit supplies one exponent unit
    if single {
        a.mov_n(A6, A3);
        round_single(a, increment, rounded);
        a.or(A2, A6, A12);
    } else {
        a.extui(A6, A2, 0, 11);
        ssai(a, 11);
        src(a, A2, A3, A2);
        a.srli(A3, A3, 11);
        a.slli(A4, A4, 20);
        a.add_n(A3, A3, A4);
        a.movi(A7, 1024);
        a.bcc_reg_reg_label(cc::LTU, A6, A7, rounded);
        a.bcc_reg_reg_label(cc::NE, A6, A7, increment);
        a.extui(A7, A2, 0, 1);
        a.bccz_reg_label(ccz::EQ, A7, rounded);
        a.base.label_assign(increment);
        a.addi(A2, A2, 1);
        a.bccz_reg_label(ccz::NE, A2, rounded);
        a.addi(A3, A3, 1);
        a.base.label_assign(rounded);
        a.or(A3, A3, A12);
    }
}

fn special_results(a: &mut Asm, single: bool) {
    let high = if single { A2 } else { A3 };
    a.base.label_assign(NAN);
    a.movi(high, if single { 511 } else { -1 });
    // Shifting all ones leaves binary64's exponent and quiet bit set.
    a.slli(high, high, if single { 22 } else { 19 });
    if !single { a.movi_n(A2, 0); }
    a.retw_n();
    a.base.label_assign(INFINITY);
    a.movi(high, if single { 255 } else { 2047 });
    a.slli(high, high, if single { 23 } else { 20 });
    a.or(high, high, A12);
    if !single { a.movi_n(A2, 0); }
    a.retw_n();
    a.base.label_assign(ZERO);
    a.mov_n(high, A12);
    if !single { a.movi_n(A2, 0); }
    a.retw_n();
}

// Compare raw magnitudes before unpacking. This leaves a's sign on the
// larger operand even on a deep cancellation; NaNs are detected in unpack.
fn order_addends(a: &mut Asm, first: usize) {
    let (swap, ordered) = (first, first + 1);
    a.slli(A6, A3, 1);
    a.slli(A7, A5, 1);
    a.bcc_reg_reg_label(cc::LTU, A6, A7, swap);
    a.bcc_reg_reg_label(cc::NE, A6, A7, ordered);
    a.bcc_reg_reg_label(cc::GEU, A2, A4, ordered);
    a.base.label_assign(swap);
    for (x, y) in [(A2, A4), (A3, A5)] {
        a.mov_n(A6, x);
        a.mov_n(x, y);
        a.mov_n(y, A6);
    }
    a.base.label_assign(ordered);
}

// The inputs have at least eleven trailing zeros. Halving them exactly
// leaves room for a sum. If alignment later loses any bits, the exponents
// differ by at least eleven. Subtraction can cancel at most one leading bit;
// restoring that bit and the headroom leaves the sticky bit below the guard.
fn add_significands(a: &mut Asm, first: usize) {
    let (finite, both_nonzero, plus_zero, minus, normalize) =
        (first, first + 1, first + 2, first + 3, first + 4);
    a.movi(A6, 2047);
    a.bcc_reg_reg_label(cc::NE, A10, A6, finite);
    a.bcc_reg_reg_label(cc::NE, A11, A6, INFINITY);
    a.bccz_reg_label(ccz::LT, A13, NAN);
    a.j_label(INFINITY);
    a.base.label_assign(finite);
    a.movi(A6, ZERO_EXP);
    a.bcc_reg_reg_label(cc::NE, A11, A6, both_nonzero);
    a.bcc_reg_reg_label(cc::NE, A10, A6, PACK);
    a.bccz_reg_label(ccz::GE, A13, ZERO); // two zeros with the same sign
    a.base.label_assign(plus_zero);
    a.movi_n(A12, 0);
    a.j_label(ZERO);
    a.base.label_assign(both_nonzero);
    ssai(a, 1);
    for (lo, hi) in [(A2, A3), (A4, A5)] {
        src(a, lo, hi, lo);
        a.srli(hi, hi, 1);
    }
    a.sub(A6, A10, A11);
    shift_right_jam(a, A4, A5, first + 5);
    a.addi(A10, A10, 1);
    a.bccz_reg_label(ccz::LT, A13, minus);
    a.add_n(A2, A2, A4);
    saltu(a, A6, A2, A4);
    a.add_n(A3, A3, A5);
    a.add_n(A3, A3, A6);
    a.j_label(normalize);
    a.base.label_assign(minus);
    saltu(a, A6, A2, A4);
    a.sub(A2, A2, A4);
    a.sub(A3, A3, A5);
    a.sub(A3, A3, A6);
    a.or(A6, A2, A3);
    a.bccz_reg_label(ccz::EQ, A6, plus_zero);
    a.base.label_assign(normalize);
    normalize_pair(a, A2, A3, A10, first + 7);
}

// The exact 53-bit input integers are left shifted by eleven. Accumulate
// their four 32x32 partial products into a9:a8:a7:a6 before discarding
// anything. Each middle-column addition propagates its own carry.
fn multiply_significands(a: &mut Asm, first: usize) {
    a.add_n(A10, A10, A11);
    a.movi(A6, 1023);
    a.sub(A10, A10, A6);
    a.mull(A6, A2, A4);
    a.muluh(A7, A2, A4);
    a.mull(A8, A3, A5);
    a.muluh(A9, A3, A5);
    for (x, y) in [(A2, A5), (A3, A4)] {
        a.mull(A13, x, y);
        a.muluh(A14, x, y);
        a.add_n(A7, A7, A13);
        saltu(a, A13, A7, A13);
        a.add_n(A8, A8, A14);
        saltu(a, A14, A8, A14);
        a.add_n(A9, A9, A14);
        a.add_n(A8, A8, A13);
        saltu(a, A13, A8, A13);
        a.add_n(A9, A9, A13);
    }
    let (shift, sticky) = (first, first + 1);
    a.bccz_reg_label(ccz::GE, A9, shift);
    a.addi(A10, A10, 1);
    a.j_label(sticky);
    a.base.label_assign(shift);
    ssai(a, 31);
    src(a, A9, A9, A8);
    src(a, A8, A8, A7);
    a.add_n(A7, A7, A7);
    a.base.label_assign(sticky);
    a.or(A6, A6, A7);
    a.movi_n(A2, 1);
    a.movnez(A6, A2, A6);
    a.or(A2, A8, A6);
    a.mov_n(A3, A9);
}

// Shift a normalized dividend into [divisor, 2*divisor), then subtract its
// implicit quotient bit. The remainder stays below the divisor. The spare
// high bit makes each doubling exact, even for nearly equal large inputs.
fn divide_significands(a: &mut Asm, single: bool, first: usize) {
    let (shift, leading, bit, subtract, advance) =
        (first, first + 1, first + 2, first + 3, first + 4);
    a.sub(A10, A10, A11);
    a.movi(A6, if single { 127 } else { 1023 });
    a.add_n(A10, A10, A6);
    ssai(a, 1);
    for (lo, hi) in [(A2, A3), (A4, A5)] {
        src(a, lo, hi, lo);
        a.srli(hi, hi, 1);
    }
    ssai(a, 31);
    a.bcc_reg_reg_label(cc::LTU, A3, A5, shift);
    a.bcc_reg_reg_label(cc::NE, A3, A5, leading);
    a.bcc_reg_reg_label(cc::GEU, A2, A4, leading);
    a.base.label_assign(shift);
    src(a, A3, A3, A2);
    a.add_n(A2, A2, A2);
    a.addi(A10, A10, -1);
    a.base.label_assign(leading);
    saltu(a, A6, A2, A4);
    a.sub(A2, A2, A4);
    a.sub(A3, A3, A5);
    a.sub(A3, A3, A6);
    a.movi_n(A13, 1);
    if !single { a.movi_n(A14, 0); }
    a.movi_n(A15, if single { 24 } else { 53 }); // fraction bits plus one guard
    a.base.label_assign(bit);
    src(a, A3, A3, A2);
    a.add_n(A2, A2, A2);
    if !single { src(a, A14, A14, A13); }
    a.add_n(A13, A13, A13);
    a.bcc_reg_reg_label(cc::LTU, A3, A5, advance);
    a.bcc_reg_reg_label(cc::NE, A3, A5, subtract);
    a.bcc_reg_reg_label(cc::LTU, A2, A4, advance);
    a.base.label_assign(subtract);
    saltu(a, A6, A2, A4);
    a.sub(A2, A2, A4);
    a.sub(A3, A3, A5);
    a.sub(A3, A3, A6);
    a.addi(A13, A13, 1);
    a.base.label_assign(advance);
    a.addi(A15, A15, -1);
    a.bccz_reg_label(ccz::NE, A15, bit);
    a.or(A6, A2, A3);
    a.movi_n(A2, 1);
    a.movnez(A6, A2, A6); // all quotient bits beyond the guard
    if single {
        a.slli(A3, A13, 7);
        a.mov_n(A2, A6);
    } else {
        ssai(a, 22);
        src(a, A3, A14, A13);
        a.slli(A2, A13, 10);
        a.or(A2, A2, A6);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Arithmetic { Add, Subtract, Multiply, Divide }

fn arithmetic(name: &str, op: Arithmetic, single: bool) -> Glue {
    // Four shared exits and two ordering labels precede the two unpackers.
    // Addition uses thirteen labels; division uses three dispatch labels and five in its core.
    let left = 6;
    let right = left + 10;
    let core = right + 10;
    let pack = core + 13;
    let addition = matches!(op, Arithmetic::Add | Arithmetic::Subtract);
    leaf(name, pack + 5, |a| {
        if single { a.mov_n(A5, A3); a.mov_n(A3, A2); }
        if op == Arithmetic::Subtract {
            a.movi_n(A6, 1);
            a.slli(A6, A6, 31);
            a.xor(A5, A5, A6);
        }
        if addition { order_addends(a, 4); }
        a.xor(A13, A3, A5);
        a.extui(A12, if addition { A3 } else { A13 }, 31, 1);
        a.slli(A12, A12, 31);
        unpack(a, A2, A3, A10, single, left);
        unpack(a, A4, A5, A11, single, right);
        if addition {
            add_significands(a, core);
        } else {
            let (a_inf, a_zero, finite) = (core, core + 1, core + 2);
            let division = op == Arithmetic::Divide;
            a.movi(A6, if single { 255 } else { 2047 });
            a.movi(A7, ZERO_EXP);
            a.bcc_reg_reg_label(cc::EQ, A10, A6, a_inf);
            a.bcc_reg_reg_label(cc::EQ, A10, A7, a_zero);
            a.bcc_reg_reg_label(cc::EQ, A11, A6, if division { ZERO } else { INFINITY });
            a.bcc_reg_reg_label(cc::NE, A11, A7, finite);
            a.j_label(if division { INFINITY } else { ZERO });
            a.base.label_assign(a_inf);
            a.bcc_reg_reg_label(cc::EQ, A11, if division { A6 } else { A7 }, NAN);
            a.j_label(INFINITY);
            a.base.label_assign(a_zero);
            a.bcc_reg_reg_label(cc::EQ, A11, if division { A7 } else { A6 }, NAN);
            a.j_label(ZERO);
            a.base.label_assign(finite);
            if division { divide_significands(a, single, core + 3); }
            else { multiply_significands(a, core + 3); }
        }
        a.j_label(PACK);
        special_results(a, single);
        pack_arithmetic(a, single, pack);
    })
}

// a3 points at the current format byte in a8 while parsing a specification.
fn fmt_advance(a: &mut Asm) {
    a.addi(A3, A3, 1);
    a.l8ui(A8, A3, 0);
}

fn fmt_byte_branch(a: &mut Asm, cond: u8, byte: u8, label: usize) {
    a.addi(A14, A8, -i32::from(byte));
    a.bccz_reg_label(cond, A14, label);
}

// Keep parsing the width, precision and length even after seeing an unsupported
// flag. a9 marks the original '%' so the entire span can then be copied verbatim.
fn sprintf_spec(a: &mut Asm, first: usize, literal: usize, fetch: usize) {
    let [flags, left, zero, bad_flag, next_flag, width, precision, precision_digits,
        length, bad_double, double_length, bad_length, bit_length, bit_width, skip_width,
        spec] = std::array::from_fn(|i| first + i);
    a.movi_n(A5, 0); // minimum width
    a.movi_n(A6, 0); // left justify
    a.movi_n(A7, 32); // leading pad byte
    a.movi_n(A13, 0); // unsupported syntax seen
    a.base.label_assign(flags);
    fmt_byte_branch(a, ccz::EQ, b'-', left);
    fmt_byte_branch(a, ccz::EQ, b'0', zero);
    for &c in b"+ #" { fmt_byte_branch(a, ccz::EQ, c, bad_flag); }
    fmt_byte_branch(a, ccz::NE, b'*', width);
    a.movi_n(A13, 1);
    fmt_advance(a);
    a.j_label(precision);
    a.base.label_assign(left);
    a.movi_n(A6, 1);
    a.j_label(next_flag);
    a.base.label_assign(zero);
    a.movi_n(A7, 48);
    a.j_label(next_flag);
    a.base.label_assign(bad_flag);
    a.movi_n(A13, 1);
    a.base.label_assign(next_flag);
    fmt_advance(a);
    a.j_label(flags);

    a.base.label_assign(width);
    a.addi(A14, A8, -48);
    a.bcci_reg_label(cc::GEU, A14, 10, precision);
    a.addx4(A5, A5, A5);
    a.addx2(A5, A5, A14); // 10 * width + digit
    fmt_advance(a);
    a.j_label(width);
    a.base.label_assign(precision);
    fmt_byte_branch(a, ccz::NE, b'.', length);
    a.movi_n(A13, 1);
    fmt_advance(a);
    fmt_byte_branch(a, ccz::NE, b'*', precision_digits);
    fmt_advance(a);
    a.j_label(length);
    a.base.label_assign(precision_digits);
    a.addi(A14, A8, -48);
    a.bcci_reg_label(cc::GEU, A14, 10, length);
    fmt_advance(a);
    a.j_label(precision_digits);

    a.base.label_assign(length);
    fmt_byte_branch(a, ccz::EQ, b'l', double_length);
    for &c in b"jztLH" { fmt_byte_branch(a, ccz::EQ, c, bad_length); }
    fmt_byte_branch(a, ccz::EQ, b'w', bit_length);
    fmt_byte_branch(a, ccz::EQ, b'D', bad_double);
    fmt_byte_branch(a, ccz::NE, b'h', spec);
    a.base.label_assign(bad_double);
    a.movi_n(A13, 1);
    a.base.label_assign(double_length);
    a.mov_n(A14, A8);
    fmt_advance(a);
    a.bcc_reg_reg_label(cc::NE, A8, A14, spec);
    a.base.label_assign(bad_length);
    a.movi_n(A13, 1);
    fmt_advance(a);
    a.j_label(spec);

    // wN and wfN are lengths only with a positive decimal N. A bare 'w'
    // ends an unknown conversion, so leave the following byte for the main loop.
    a.base.label_assign(bit_length);
    a.mov_n(A10, A3);
    fmt_advance(a);
    fmt_byte_branch(a, ccz::NE, b'f', bit_width);
    fmt_advance(a);
    a.base.label_assign(bit_width);
    a.addi(A14, A8, -i32::from(b'1'));
    a.movi_n(A15, 9);
    a.bcc_reg_reg_label(cc::LTU, A14, A15, skip_width);
    a.mov_n(A3, A10);
    a.movi(A8, i32::from(b'w'));
    a.j_label(spec);
    a.base.label_assign(skip_width);
    a.movi_n(A13, 1);
    fmt_advance(a);
    a.addi(A14, A8, -48);
    a.bcci_reg_label(cc::LTU, A14, 10, skip_width);
    a.base.label_assign(spec);
    a.bccz_reg_label(ccz::EQ, A8, literal); // leave a3 at the NUL
    a.addi(A3, A3, 1);
    a.bccz_reg_label(ccz::NE, A13, literal);
    for &c in b"Xxdiucs" { fmt_byte_branch(a, ccz::EQ, c, fetch); }
    a.j_label(literal);
}

// Build a payload at a11 with byte count a12. The 12 scratch bytes at
// sp+16..28 fit ten decimal digits and a minus sign, or the NULL spelling.
fn sprintf_value(a: &mut Asm, first: usize, field: usize) {
    let [string, character, scan, scanned, hexadecimal, digits, nibble,
        decimal_digit, store] = std::array::from_fn(|i| first + i);
    fmt_byte_branch(a, ccz::EQ, b's', string);
    fmt_byte_branch(a, ccz::EQ, b'c', character);
    a.addi(A11, A1, 28);
    a.movi_n(A12, 0);
    fmt_byte_branch(a, ccz::EQ, b'X', hexadecimal);
    fmt_byte_branch(a, ccz::EQ, b'x', hexadecimal);
    a.movi_n(A14, 10);
    a.addi(A15, A8, -i32::from(b'u'));
    a.bccz_reg_label(ccz::EQ, A15, digits);
    a.bccz_reg_label(ccz::GE, A10, digits);
    a.neg(A10, A10); // unsigned magnitude, also for i32::MIN
    a.movi_n(A13, 45);
    a.j_label(digits);
    a.base.label_assign(hexadecimal);
    a.movi_n(A14, 16);
    a.addi(A15, A8, -33); // 'X'/'x' minus 33 = 'A'/'a' minus ten
    a.base.label_assign(digits);
    a.bcci_reg_label(cc::EQ, A14, 16, nibble);
    a.remu(A8, A10, A14);
    a.quou(A10, A10, A14);
    a.j_label(decimal_digit);
    a.base.label_assign(nibble);
    a.extui(A8, A10, 0, 4);
    a.srli(A10, A10, 4);
    a.bcci_reg_label(cc::LTU, A8, 10, decimal_digit);
    a.add_n(A8, A8, A15);
    a.j_label(store);
    a.base.label_assign(decimal_digit);
    a.addi(A8, A8, 48);
    a.base.label_assign(store);
    a.addi(A11, A11, -1);
    a.s8i(A8, A11, 0);
    a.addi(A12, A12, 1);
    a.bccz_reg_label(ccz::NE, A10, digits);
    a.bccz_reg_label(ccz::EQ, A13, field);
    a.addi(A11, A11, -1);
    a.s8i(A13, A11, 0);
    a.addi(A12, A12, 1);
    a.j_label(field);

    a.base.label_assign(character);
    a.movi_n(A7, 32);
    a.addi(A11, A1, 16);
    a.s8i(A10, A11, 0);
    a.movi_n(A12, 1); // '%c' can itself write a NUL
    a.j_label(field);
    a.base.label_assign(string);
    a.movi_n(A7, 32);
    a.mov_n(A11, A10);
    a.mov_n(A12, A10);
    a.bccz_reg_label(ccz::NE, A10, scan);
    a.addi(A11, A1, 16);
    for (i, &c) in b"(null)".iter().enumerate() {
        a.movi(A8, i32::from(c));
        a.s8i(A8, A11, i as u32);
    }
    a.movi_n(A12, 6);
    a.j_label(field);
    a.base.label_assign(scan);
    a.l8ui(A8, A12, 0);
    a.bccz_reg_label(ccz::EQ, A8, scanned);
    a.addi(A12, A12, 1);
    a.j_label(scan);
    a.base.label_assign(scanned);
    a.sub(A12, A12, A11);
}

fn sprintf_field(a: &mut Asm, first: usize, next: usize) {
    let [leading, payload, copy, trailing, spaces] = std::array::from_fn(|i| first + i);
    a.maxu(A5, A5, A12);
    a.sub(A5, A5, A12);
    a.bccz_reg_label(ccz::NE, A6, payload); // '-' overrides '0'
    a.bccz_reg_label(ccz::EQ, A5, payload);
    a.bccz_reg_label(ccz::EQ, A13, leading);
    a.bcci_reg_label(cc::EQ, A7, 32, leading);
    a.s8i(A13, A2, 0); // negative sign precedes leading zeros
    a.addi(A2, A2, 1);
    a.addi(A11, A11, 1);
    a.addi(A12, A12, -1);
    a.base.label_assign(leading);
    a.s8i(A7, A2, 0);
    a.addi(A2, A2, 1);
    a.addi(A5, A5, -1);
    a.bccz_reg_label(ccz::NE, A5, leading);
    a.base.label_assign(payload);
    a.bccz_reg_label(ccz::EQ, A12, trailing);
    a.base.label_assign(copy);
    a.l8ui(A8, A11, 0);
    a.s8i(A8, A2, 0);
    a.addi(A11, A11, 1);
    a.addi(A2, A2, 1);
    a.addi(A12, A12, -1);
    a.bccz_reg_label(ccz::NE, A12, copy);
    a.base.label_assign(trailing);
    a.bccz_reg_label(ccz::EQ, A5, next);
    a.movi_n(A7, 32);
    a.base.label_assign(spaces);
    a.s8i(A7, A2, 0);
    a.addi(A2, A2, 1);
    a.addi(A5, A5, -1);
    a.bccz_reg_label(ccz::NE, A5, spaces);
    a.j_label(next);
}

fn sprintf(name: &str) -> Glue {
    let [next, put_byte, escape, percent, literal, fetch, pop, field, done] =
        std::array::from_fn(|i| i);
    const FRAME: u32 = 80; // 32 local bytes below the 48-byte window save area
    framed_leaf(name, 39, FRAME, |a| {
        for i in 0..4 { a.s32i_n(A4 + i, A1, 4 * u32::from(i)); }
        a.s32i_n(A2, A1, 28);
        a.mov_n(A4, A1); // next argument in the saved a4..a7
        a.base.label_assign(next);
        a.l8ui(A8, A3, 0);
        a.bccz_reg_label(ccz::EQ, A8, done);
        a.addi(A3, A3, 1);
        fmt_byte_branch(a, ccz::EQ, b'%', percent);
        a.base.label_assign(put_byte);
        a.s8i(A8, A2, 0);
        a.addi(A2, A2, 1);
        a.j_label(next);
        a.base.label_assign(escape);
        a.addi(A3, A3, 1);
        a.j_label(put_byte);
        a.base.label_assign(percent);
        a.addi(A9, A3, -1);
        a.l8ui(A8, A3, 0);
        fmt_byte_branch(a, ccz::EQ, b'%', escape);
        sprintf_spec(a, 9, literal, fetch);
        a.base.label_assign(literal);
        a.l8ui(A8, A9, 0);
        a.s8i(A8, A2, 0);
        a.addi(A9, A9, 1);
        a.addi(A2, A2, 1);
        a.bcc_reg_reg_label(cc::NE, A9, A3, literal);
        a.j_label(next);

        a.base.label_assign(fetch);
        // From the pack's own call sites (ESP-IDF v6.1.1 esp32s3 libpp.a, pp.o pp_create_task):
        // argument word 6 is at caller sp+0, word 7 at +4. ENTRY put caller sp at a1+FRAME.
        a.addi(A14, A1, 16);
        a.bcc_reg_reg_label(cc::NE, A4, A14, pop);
        a.addi(A4, A1, FRAME as i32);
        a.base.label_assign(pop);
        a.l32i_n(A10, A4, 0);
        a.addi(A4, A4, 4);
        sprintf_value(a, 25, field);
        a.base.label_assign(field);
        sprintf_field(a, 34, next);
        a.base.label_assign(done);
        a.s8i(A8, A2, 0);
        a.l32i_n(A14, A1, 28);
        a.sub(A2, A2, A14);
    })
}

pub fn helpers() -> Vec<Glue> {
    vec![
        leaf(HELPERS[0], 6, |a| { a.mov_n(A5, A2); transfer(a, Transfer::Forward, 0); }),
        leaf(HELPERS[1], 6, |a| { a.mov_n(A5, A2); transfer(a, Transfer::Fill, 0); }),
        leaf(HELPERS[2], 2, |a| compare(a, false)),
        leaf(HELPERS[3], 14, memmove),
        leaf(HELPERS[4], 2, |a| length(a, false)),
        leaf(HELPERS[5], 2, |a| compare(a, true)),
        leaf(HELPERS[6], 3, strncpy),
        leaf(HELPERS[7], 2, |a| length(a, true)),
        leaf(HELPERS[8], 1, strcpy),
        leaf(HELPERS[9], 5, hexstr2bin),
        division(HELPERS[10], true, DivisionResult::Quotient),
        division(HELPERS[11], true, DivisionResult::Remainder),
        division(HELPERS[12], false, DivisionResult::Quotient),
        division(HELPERS[13], false, DivisionResult::Remainder),
        leaf(HELPERS[14], 0, bswap),
        leaf(HELPERS[15], 8, extend_single),
        leaf(HELPERS[16], 7, truncate_double),
        leaf(HELPERS[17], 4, fix_double),
        leaf(HELPERS[18], 7, |a| word_to_double(a, true)),
        leaf(HELPERS[19], 7, |a| word_to_double(a, false)),
        leaf(HELPERS[20], 9, u64_to_single),
        arithmetic(HELPERS[21], Arithmetic::Add, false),
        arithmetic(HELPERS[22], Arithmetic::Subtract, false),
        arithmetic(HELPERS[23], Arithmetic::Multiply, false),
        arithmetic(HELPERS[24], Arithmetic::Divide, false),
        arithmetic(HELPERS[25], Arithmetic::Divide, true),
        sprintf(HELPERS[26]),
    ]
}
