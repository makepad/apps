//! Constant-cycle i32 arithmetic for the ATmega328P.
//!
//! A = r16..r19, B = r20..r23, result = A; unsigned division also returns
//! its remainder in r24..r27. Only r0, r1, A, B, r24..r27, Z and arithmetic
//! flags/T may change. r2 must be zero; r3..r15, Y and the interrupt bit stay
//! untouched. Cycle totals include the caller's CALL and the helper's RET,
//! excluding time spent servicing interrupts.

use fab_elec_mcu_avr::asm::{Asm, Label, Target};
use fab_elec_mcu_avr::insn::{cycles, Insn, Reg};
use Insn::*;

/// CALL through RET, inclusive, for every input.
pub const MUL32: u32 = 54;
/// CALL through RET, inclusive, for every input.
pub const UDIVMOD32: u32 = 694;
/// CALL through RET, inclusive, for every input.
pub const SDIV32: u32 = 737;
/// CALL through RET, inclusive, for every input.
pub const SREM32: u32 = 738;
/// CALL through RET, inclusive, for every input.
pub const SHL32: u32 = 58;
/// CALL through RET, inclusive, for every input.
pub const SHR32: u32 = 58;
/// CALL through RET, inclusive, for every input.
pub const SAR32: u32 = 61;

/// Maximum bytes beyond the caller's return address: one sign byte and the
/// unsigned division call's two-byte return address. Other helpers use zero.
pub const STACK_BYTES: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper { Mul32, Udivmod32, Sdiv32, Srem32, Shl32, Shr32, Sar32 }

impl Helper {
    pub const ALL: [Self; 7] = [Self::Mul32, Self::Udivmod32, Self::Sdiv32, Self::Srem32,
        Self::Shl32, Self::Shr32, Self::Sar32];

    /// CALL through RET, including any nested helper call.
    pub fn cycles(self) -> u32 {
        match self {
            Self::Mul32 => MUL32, Self::Udivmod32 => UDIVMOD32, Self::Sdiv32 => SDIV32,
            Self::Srem32 => SREM32, Self::Shl32 => SHL32, Self::Shr32 => SHR32, Self::Sar32 => SAR32,
        }
    }

    pub(crate) fn label(self, h: &Helpers) -> Label {
        match self {
            Self::Mul32 => h.mul32, Self::Udivmod32 => h.udivmod32, Self::Sdiv32 => h.sdiv32,
            Self::Srem32 => h.srem32, Self::Shl32 => h.shl32, Self::Shr32 => h.shr32, Self::Sar32 => h.sar32,
        }
    }

    pub(crate) fn emit(self, a: &mut Asm, h: &Helpers) {
        match self {
            Self::Mul32 => emit_mul(a, h.mul32),
            Self::Udivmod32 => emit_udivmod(a, h.udivmod32),
            Self::Sdiv32 => emit_signed(a, h.sdiv32, h.udivmod32, false),
            Self::Srem32 => emit_signed(a, h.srem32, h.udivmod32, true),
            Self::Shl32 => emit_shift(a, h.shl32, Shift::Left),
            Self::Shr32 => emit_shift(a, h.shr32, Shift::Right),
            Self::Sar32 => emit_shift(a, h.sar32, Shift::Arithmetic),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Helpers {
    pub mul32: Label,
    pub udivmod32: Label,
    pub sdiv32: Label,
    pub srem32: Label,
    pub shl32: Label,
    pub shr32: Label,
    pub sar32: Label,
}

impl Helpers {
    pub(crate) fn new(a: &mut Asm) -> Self {
        Self {
            mul32: a.label(), udivmod32: a.label(), sdiv32: a.label(), srem32: a.label(),
            shl32: a.label(), shr32: a.label(), sar32: a.label(),
        }
    }
}

/// Creates, binds and emits every helper; each ends in RET.
pub fn emit(a: &mut Asm) -> Helpers {
    let h = Helpers::new(a);
    for helper in Helper::ALL { helper.emit(a, &h); }
    h
}

struct Code<'a> {
    a: &'a mut Asm,
    t: u32,
}

impl<'a> Code<'a> {
    fn new(a: &'a mut Asm, label: Label) -> Self {
        a.bind(label);
        Self { a, t: 4 } // Caller uses the fixed-size CALL.
    }

    fn i(&mut self, i: Insn) {
        let (base, extra) = cycles(&i);
        assert_eq!(extra, 0, "conditional instructions need balanced accounting");
        self.t += u32::from(base);
        self.a.emit(i);
    }

    fn seq(&mut self, insns: &[Insn]) {
        for &i in insns {
            self.i(i);
        }
    }

    fn pad(&mut self, n: u32) {
        for _ in 0..n {
            self.i(Nop);
        }
    }

    fn select(&mut self, bit: u8, yes: &[Insn], no: &[Insn]) {
        let cost = |insns: &[Insn]| -> u32 {
            insns.iter().map(|i| {
                let (base, extra) = cycles(i);
                assert_eq!(extra, 0);
                u32::from(base)
            }).sum()
        };
        let yes_cycles = cost(yes);
        let no_cycles = cost(no);
        // Taken: BRBS (2). Not taken: BRBS (1), no arm, JMP (3).
        let total = (2 + yes_cycles).max(4 + no_cycles);
        let yes_label = self.a.label();
        let done = self.a.label();
        let start = self.t;
        self.a.jump(Target::Brbs(bit), yes_label);
        self.seq(no);
        self.pad(total - 4 - no_cycles);
        self.a.jump(Target::Jmp, done);
        self.a.bind(yes_label);
        self.seq(yes);
        self.pad(total - 2 - yes_cycles);
        self.a.bind(done);
        self.t = start + total;
    }

    fn call(&mut self, label: Label, total: u32) {
        self.a.jump(Target::Call, label);
        self.t += total; // The published total already includes CALL and RET.
    }

    fn ret(mut self, total: u32) {
        self.i(Ret);
        assert_eq!(self.t, total, "helper cycle accounting changed");
    }
}

fn emit_mul(a: &mut Asm, label: Label) {
    let mut c = Code::new(a, label);
    // Ten byte products with i+j < 4; all higher terms vanish modulo 2^32.
    for r in 24..28 {
        c.i(Mov(r, 2));
    }
    for i in 0..4 {
        for j in 0..4 - i {
            let offset = i + j;
            c.i(Mul(16 + i, 20 + j));
            c.i(Add(24 + offset, 0));
            if offset < 3 {
                c.i(Adc(25 + offset, 1));
                for r in 26 + offset..28 {
                    c.i(Adc(r, 2));
                }
            }
        }
    }
    c.seq(&[Movw(16, 24), Movw(18, 26)]);
    c.ret(MUL32);
}

fn emit_udivmod(a: &mut Asm, label: Label) {
    let mut c = Code::new(a, label);
    for r in 24..28 {
        c.i(Mov(r, 2));
    }
    c.i(Ldi(30, 32));
    let again = c.a.label();
    c.a.bind(again);
    let start = c.t;
    // A supplies one dividend bit and receives one quotient bit each round.
    // The shifted remainder is at most the consumed dividend prefix, so it
    // fits in 32 bits even when the divisor has its high bit set.
    c.seq(&[
        Add(16, 16), Adc(17, 17), Adc(18, 18), Adc(19, 19),
        Adc(24, 24), Adc(25, 25), Adc(26, 26), Adc(27, 27),
        Sub(24, 20), Sbc(25, 21), Sbc(26, 22), Sbc(27, 23),
    ]);
    c.select(0,
        &[Add(24, 20), Adc(25, 21), Adc(26, 22), Adc(27, 23)],
        &[Ori(16, 1)]);
    c.i(Dec(30));
    c.a.jump(Target::Brbc(1), again);
    // Exactly 32 rounds; the final BRNE takes one cycle instead of two.
    c.t = start + 32 * (c.t - start + 2) - 1;
    // Dividing by zero naturally leaves remainder A and quotient all ones.
    // Mask only the quotient: NEG sets carry iff any divisor byte is nonzero.
    c.seq(&[Mov(30, 20), Or(30, 21), Or(30, 22), Or(30, 23), Neg(30), Sbc(30, 30)]);
    for r in 16..20 {
        c.i(And(r, 30));
    }
    c.ret(UDIVMOD32);
}

fn sign_mask(c: &mut Code<'_>, mask: Reg, high: Reg) {
    c.seq(&[Mov(mask, high), Add(mask, mask), Sbc(mask, mask)]);
}

fn apply_sign(c: &mut Code<'_>, low: Reg, mask: Reg) {
    // (x ^ m) - m, with m sign-extended across all four bytes.
    for r in low..low + 4 {
        c.i(Eor(r, mask));
    }
    c.i(Sub(low, mask));
    for r in low + 1..low + 4 {
        c.i(Sbc(r, mask));
    }
}

fn emit_signed(a: &mut Asm, label: Label, unsigned: Label, remainder: bool) {
    let mut c = Code::new(a, label);
    sign_mask(&mut c, 0, 19);
    sign_mask(&mut c, 1, 23);
    apply_sign(&mut c, 16, 0);
    apply_sign(&mut c, 20, 1);
    if !remainder {
        c.i(Eor(0, 1));
    }
    c.i(Push(0));
    c.call(unsigned, UDIVMOD32);
    c.i(Pop(0));
    if remainder {
        c.seq(&[Movw(16, 24), Movw(18, 26)]);
    }
    apply_sign(&mut c, 16, 0);
    c.ret(if remainder { SREM32 } else { SDIV32 });
}

#[derive(Clone, Copy)]
enum Shift {
    Left,
    Right,
    Arithmetic,
}

fn emit_shift(a: &mut Asm, label: Label, kind: Shift) {
    let mut c = Code::new(a, label);
    let fill = if matches!(kind, Shift::Arithmetic) {
        sign_mask(&mut c, 0, 19);
        0
    } else {
        2
    };
    // Only B's low five bits are sampled, in stages 16, 8, 4, 2, 1.
    for bit in (0..5).rev() {
        let mut yes = Vec::new();
        match (kind, bit) {
            (Shift::Left, 4) => yes.extend([Movw(18, 16), Mov(16, 2), Mov(17, 2)]),
            (Shift::Left, 3) => yes.extend([Mov(19, 18), Mov(18, 17), Mov(17, 16), Mov(16, 2)]),
            (_, 4) => yes.extend([Movw(16, 18), Mov(18, fill), Mov(19, fill)]),
            (_, 3) => yes.extend([Mov(16, 17), Mov(17, 18), Mov(18, 19), Mov(19, fill)]),
            _ => {
                for _ in 0..1 << bit {
                    match kind {
                        Shift::Left => yes.extend([Add(16, 16), Adc(17, 17), Adc(18, 18), Adc(19, 19)]),
                        Shift::Right => yes.extend([Lsr(19), Ror(18), Ror(17), Ror(16)]),
                        Shift::Arithmetic => yes.extend([Asr(19), Ror(18), Ror(17), Ror(16)]),
                    }
                }
            }
        }
        c.i(Bst(20, bit));
        c.select(6, &yes, &[]);
    }
    c.ret(match kind {
        Shift::Left => SHL32,
        Shift::Right => SHR32,
        Shift::Arithmetic => SAR32,
    });
}
