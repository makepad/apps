//! Ported from MicroPython's py/asmxtensa.{c,h}
//!
//! The MIT License (MIT)
//!
//! Copyright (c) 2016 Damien P. George
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
//! THE SOFTWARE.
//!
//! CALL0 ABI only; frames and runtime function tables belong to the caller.
//! Raw load/store instruction offsets are checked byte offsets; offset helpers
//! retain MicroPython's element units. `enc::call0` takes a signed word offset,
//! as in MicroPython. The label helper converts its byte displacement to words.
//! Raw CALL0/L32R emitting methods are named `op_call0`/`op_l32r` to distinguish
//! them from the label helpers (Rust does not overload methods).
//! Windowed instructions are also exposed as checked encoders and emitters;
//! their frames and window-management handlers remain the caller's responsibility.
//! CALL4/8/12 use the same word units and `op_call4`/`op_call8`/`op_call12`
//! naming convention, with separate label helpers.
//!
//! Use this module's [`assemble`] driver for literals: a preliminary Compute
//! traversal counts constants before the stable Compute/Emit pair. The closure
//! must generate the same program on every traversal. [`assemble_relaxed`]
//! additionally plans branch widths before the final Compute/Emit pair.
//! Page citations refer to Cadence's [Xtensa ISA Summary](https://www.cadence.com/content/dam/cadence-www/global/en_US/documents/tools/silicon-solutions/compute-ip/isa-summary.pdf).

use crate::asm::{AsmBase, Pass};

pub type Reg = u8;

pub mod reg {
    use super::Reg;
    pub const A0: Reg = 0;
    pub const A1: Reg = 1;
    pub const A2: Reg = 2;
    pub const A3: Reg = 3;
    pub const A4: Reg = 4;
    pub const A5: Reg = 5;
    pub const A6: Reg = 6;
    pub const A7: Reg = 7;
    pub const A8: Reg = 8;
    pub const A9: Reg = 9;
    pub const A10: Reg = 10;
    pub const A11: Reg = 11;
    pub const A12: Reg = 12;
    pub const A13: Reg = 13;
    pub const A14: Reg = 14;
    pub const A15: Reg = 15;
}

pub mod ccz {
    pub const EQ: u8 = 0;
    pub const NE: u8 = 1;
    pub const LT: u8 = 2;
    pub const GE: u8 = 3;
}

pub mod cc {
    pub const NONE: u8 = 0;
    pub const EQ: u8 = 1;
    pub const LT: u8 = 2;
    pub const LTU: u8 = 3;
    pub const ALL: u8 = 4;
    pub const BC: u8 = 5;
    pub const ANY: u8 = 8;
    pub const NE: u8 = 9;
    pub const GE: u8 = 10;
    pub const GEU: u8 = 11;
    pub const NALL: u8 = 12;
    pub const BS: u8 = 13;
}

fn check_reg(name: &str, operand: &str, reg: Reg) {
    assert!(reg <= 15, "{name}: {operand} must be in 0..=15");
}

fn fits_signed(value: i32, bits: u32) -> bool {
    let limit = 1i32 << (bits - 1);
    (-limit..limit).contains(&value)
}

fn check_cc(name: &str, cond: u8) {
    assert!(matches!(cond, 0..=5 | 8..=13), "{name}: cond must be a cc constant");
}

/// Pure, checked encoders. Format functions accept unsigned field bits, except
/// BRI8/BRI12, whose last operand is a signed byte displacement. CALL's raw
/// `offset` field is unsigned; the instruction functions supply signed values.
pub mod enc {
    use super::{check_cc, check_reg, fits_signed, Reg};

    fn field(name: &str, operand: &str, value: u32, bits: u32) {
        assert!(value < (1 << bits), "{name}: {operand} must fit unsigned {bits} bits");
    }

    fn signed(name: &str, operand: &str, value: i32, bits: u32) {
        assert!(fits_signed(value, bits), "{name}: {operand} must fit signed {bits} bits");
    }

    /// A conditional branch's offset: signed, and never -3..=0, which the ISA Summary (p. 62)
    /// makes illegal and reserves. The label helpers never produce one.
    fn branch(name: &str, operand: &str, value: i32, bits: u32) {
        signed(name, operand, value, bits);
        assert!(!(-3..=0).contains(&value), "{name}: {operand} -3..=0 is reserved (ISA Summary p. 62)");
    }

    fn scaled(name: &str, operand: &str, value: u32, bits: u32, shift: u32) {
        assert!(value & ((1 << shift) - 1) == 0,
            "{name}: {operand} must be aligned to {} bytes", 1 << shift);
        assert!(value >> shift < (1 << bits),
            "{name}: {operand} must fit unsigned {bits} bits after scaling by {}", 1 << shift);
    }

    pub fn rrr(op0: u8, op1: u8, op2: u8, r: Reg, s: Reg, t: Reg) -> u32 {
        field("rrr", "op0", op0.into(), 4);
        field("rrr", "op1", op1.into(), 4);
        field("rrr", "op2", op2.into(), 4);
        check_reg("rrr", "r", r);
        check_reg("rrr", "s", s);
        check_reg("rrr", "t", t);
        (u32::from(op2) << 20) | (u32::from(op1) << 16) | (u32::from(r) << 12)
            | (u32::from(s) << 8) | (u32::from(t) << 4) | u32::from(op0)
    }

    pub fn rri4(op0: u8, op1: u8, r: Reg, s: Reg, t: Reg, imm4: u8) -> u32 {
        field("rri4", "op0", op0.into(), 4);
        field("rri4", "op1", op1.into(), 4);
        check_reg("rri4", "r", r);
        check_reg("rri4", "s", s);
        check_reg("rri4", "t", t);
        field("rri4", "imm4", imm4.into(), 4);
        rrr(op0, op1, imm4, r, s, t)
    }

    pub fn rri8(op0: u8, r: Reg, s: Reg, t: Reg, imm8: u32) -> u32 {
        field("rri8", "op0", op0.into(), 4);
        check_reg("rri8", "r", r);
        check_reg("rri8", "s", s);
        check_reg("rri8", "t", t);
        field("rri8", "imm8", imm8, 8);
        (imm8 << 16) | (u32::from(r) << 12) | (u32::from(s) << 8)
            | (u32::from(t) << 4) | u32::from(op0)
    }

    pub fn ri16(op0: u8, t: Reg, imm16: u32) -> u32 {
        field("ri16", "op0", op0.into(), 4);
        check_reg("ri16", "t", t);
        field("ri16", "imm16", imm16, 16);
        (imm16 << 8) | (u32::from(t) << 4) | u32::from(op0)
    }

    pub fn rsr(op0: u8, op1: u8, op2: u8, rs: u32, t: Reg) -> u32 {
        field("rsr", "op0", op0.into(), 4);
        field("rsr", "op1", op1.into(), 4);
        field("rsr", "op2", op2.into(), 4);
        field("rsr", "rs", rs, 8);
        check_reg("rsr", "t", t);
        (u32::from(op2) << 20) | (u32::from(op1) << 16) | (rs << 8)
            | (u32::from(t) << 4) | u32::from(op0)
    }

    pub fn call(op0: u8, n: u8, offset: u32) -> u32 {
        field("call", "op0", op0.into(), 4);
        field("call", "n", n.into(), 2);
        field("call", "offset", offset, 18);
        (offset << 6) | (u32::from(n) << 4) | u32::from(op0)
    }

    pub fn callx(op0: u8, op1: u8, op2: u8, r: Reg, s: Reg, m: u8, n: u8) -> u32 {
        field("callx", "op0", op0.into(), 4);
        field("callx", "op1", op1.into(), 4);
        field("callx", "op2", op2.into(), 4);
        check_reg("callx", "r", r);
        check_reg("callx", "s", s);
        field("callx", "m", m.into(), 2);
        field("callx", "n", n.into(), 2);
        rrr(op0, op1, op2, r, s, (m << 2) | n)
    }

    pub fn bri8(op0: u8, r: Reg, s: Reg, m: u8, n: u8, imm8: i32) -> u32 {
        field("bri8", "op0", op0.into(), 4);
        check_reg("bri8", "r", r);
        check_reg("bri8", "s", s);
        field("bri8", "m", m.into(), 2);
        field("bri8", "n", n.into(), 2);
        signed("bri8", "imm8", imm8, 8);
        rri8(op0, r, s, (m << 2) | n, (imm8 as u32) & 0xff)
    }

    pub fn bri12(op0: u8, s: Reg, m: u8, n: u8, imm12: i32) -> u32 {
        field("bri12", "op0", op0.into(), 4);
        check_reg("bri12", "s", s);
        field("bri12", "m", m.into(), 2);
        field("bri12", "n", n.into(), 2);
        signed("bri12", "imm12", imm12, 12);
        (((imm12 as u32) & 0xfff) << 12) | (u32::from(s) << 8)
            | (u32::from(m) << 6) | (u32::from(n) << 4) | u32::from(op0)
    }

    pub fn rrrn(op0: u8, r: Reg, s: Reg, t: Reg) -> u16 {
        field("rrrn", "op0", op0.into(), 4);
        check_reg("rrrn", "r", r);
        check_reg("rrrn", "s", s);
        check_reg("rrrn", "t", t);
        (u16::from(r) << 12) | (u16::from(s) << 8) | (u16::from(t) << 4) | u16::from(op0)
    }

    pub fn ri7(op0: u8, s: Reg, imm7: u32) -> u16 {
        field("ri7", "op0", op0.into(), 4);
        check_reg("ri7", "s", s);
        field("ri7", "imm7", imm7, 7);
        (((imm7 & 0xf) << 12) | (u32::from(s) << 8) | (imm7 & 0x70) | u32::from(op0)) as u16
    }

    macro_rules! r_ops {
        ($($name:ident: $op1:expr, $op2:expr;)*) => {$(
            pub fn $name(rd: Reg, rs1: Reg, rs2: Reg) -> u32 {
                check_reg(stringify!($name), "rd", rd);
                check_reg(stringify!($name), "rs1", rs1);
                check_reg(stringify!($name), "rs2", rs2);
                rrr(0, $op1, $op2, rd, rs1, rs2)
            }
        )*};
    }
    r_ops! {
        // ADDX2/4: ISA Summary pp. 335–336; RRR p. 656, RST0 Table 200 p. 662.
        addx2: 0, 9; addx4: 0, 10; and: 0, 1; mull: 2, 8;
        or: 0, 2; sub: 0, 12; xor: 0, 3;
        // ISA Summary pp. 557, 562; RST2 map p. 667.
        quos: 2, 13; rems: 2, 15;
        // MULUH p. 539, QUOU pp. 557–558, REMU pp. 562–563;
        // RRR format p. 656 and RST2 opcode map p. 667.
        muluh: 2, 10; quou: 2, 12; remu: 2, 14;
        // RST3 map p. 668; MOVT pp. 514–515.
        movnez: 3, 9; movt: 3, 13;
        // MIN/MAX option: MIN pp. 491–492, MINU p. 492, MAX/MAXU
        // pp. 489–490; RRR p. 656, RST3 map p. 668 (op1=3).
        min: 3, 4; max: 3, 5; minu: 3, 6; maxu: 3, 7;
        // SALT p. 599: signed less-than, writing 0 or 1; RST2 p. 667.
        salt: 2, 7;
    }

    // ISA Summary RRR diagram p. 656; FP opcode maps pp. 670–671.
    // ADD.S pp. 328–329, SUB.S pp. 628–629, MUL.S pp. 522–523;
    // OEQ.S pp. 547–548, OLT.S pp. 550–551, OLE.S pp. 549–550;
    // FLOAT.S pp. 431–432 and TRUNC.S pp. 632–633 (t is the scale).
    macro_rules! fp_ops {
        ($($name:ident: $op1:expr, $op2:expr;)*) => {$(
            pub fn $name(r: Reg, s: Reg, t: Reg) -> u32 {
                rrr(0, $op1, $op2, r, s, t)
            }
        )*};
    }
    fp_ops! {
        add_s: 10, 0; sub_s: 10, 1; mul_s: 10, 2;
        oeq_s: 11, 2; olt_s: 11, 4; ole_s: 11, 6;
        float_s: 10, 12; trunc_s: 10, 9;
    }

    /// RFR, ISA Summary pp. 573–574.
    pub fn rfr(rd: Reg, fs: Reg) -> u32 { rrr(0, 10, 15, rd, fs, 4) }

    /// WFR, pp. 646–647. Table 228 p. 671 resolves p. 647's duplicated
    /// t=2 diagram: WFR uses t=5, as in the independent CPU decoder.
    pub fn wfr(fd: Reg, rs: Reg) -> u32 { rrr(0, 10, 15, fd, rs, 5) }

    pub fn add_n(rd: Reg, rs1: Reg, rs2: Reg) -> u16 {
        check_reg("add_n", "rd", rd);
        check_reg("add_n", "rs1", rs1);
        check_reg("add_n", "rs2", rs2);
        rrrn(10, rd, rs1, rs2)
    }

    pub fn addi(rd: Reg, rs: Reg, imm8: i32) -> u32 {
        check_reg("addi", "rd", rd);
        check_reg("addi", "rs", rs);
        signed("addi", "imm8", imm8, 8);
        rri8(2, 12, rs, rd, (imm8 as u32) & 0xff)
    }

    /// RRI8 p. 656; BEQ p. 349, BNE pp. 364–365, BLT p. 359,
    /// BGE pp. 354–355, BLTU p. 361, BGEU p. 356. Signed imm8 from
    /// PC+4 (-128..127), with -3..=0 reserved (ISA Summary p. 62).
    pub fn bcc(cond: u8, rs1: Reg, rs2: Reg, rel8: i32) -> u32 {
        check_cc("bcc", cond);
        check_reg("bcc", "rs1", rs1);
        check_reg("bcc", "rs2", rs2);
        branch("bcc", "rel8", rel8, 8);
        rri8(7, cond, rs1, rs2, (rel8 as u32) & 0xff)
    }

    /// ISA Summary Table 15 p. 64: the signed immediate is a table index,
    /// not a four-bit signed integer.
    pub fn b4const(value: i32) -> Option<u8> {
        [-1, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256]
            .iter().position(|&c| c == value).map(|n| n as u8)
    }

    /// ISA Summary Table 16 p. 65, unsigned branch immediate encodings.
    pub fn b4constu(value: u32) -> Option<u8> {
        [32768, 65536, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256]
            .iter().position(|&c| c == value).map(|n| n as u8)
    }

    /// BEQI pp. 350–351, BNEI pp. 365–366, BLTI p. 360, BGEI p. 355;
    /// BLTUI pp. 361–362, BGEUI p. 357. BRI8 p. 658: op0=6,
    /// n=2 (signed) or 3 (unsigned), m=EQ:0/NE:1/LT:2/GE:3, r=b4const.
    /// Displacement is signed imm8 from PC+4; reserved -3..=0: p. 62.
    pub fn bcci(cond: u8, rs: Reg, constant: i32, rel8: i32) -> u32 {
        let (m, n, r) = match cond {
            super::cc::EQ => (0, 2, b4const(constant)),
            super::cc::NE => (1, 2, b4const(constant)),
            super::cc::LT => (2, 2, b4const(constant)),
            super::cc::GE => (3, 2, b4const(constant)),
            super::cc::LTU => (2, 3, b4constu(constant as u32)),
            super::cc::GEU => (3, 3, b4constu(constant as u32)),
            _ => panic!("bcci: condition has no immediate form"),
        };
        branch("bcci", "rel8", rel8, 8);
        bri8(6, r.expect("bcci: constant is not encodable"), rs, m, n, rel8)
    }

    /// BRI12 p. 658; BEQZ pp. 351–352, BNEZ pp. 366–367, BLTZ
    /// pp. 362–363, BGEZ p. 358. Signed imm12 from PC+4 (-2048..2047),
    /// with -3..=0 reserved (ISA Summary p. 62).
    pub fn bccz(cond: u8, rs: Reg, rel12: i32) -> u32 {
        field("bccz", "cond", cond.into(), 2);
        check_reg("bccz", "rs", rs);
        branch("bccz", "rel12", rel12, 12);
        bri12(6, rs, cond, 1, rel12)
    }

    /// Signed 18-bit word displacement, before CALL0's implicit multiplication by four.
    pub fn call0(rel18: i32) -> u32 {
        signed("call0", "rel18", rel18, 18);
        call(5, 0, (rel18 as u32) & 0x3ffff)
    }

    /// ISA Summary Tables 198 (p. 661), 249 (p. 678); CALL4 pp. 373–374.
    /// Signed 18-bit word displacement, before implicit multiplication by four.
    pub fn call4(rel18: i32) -> u32 {
        signed("call4", "rel18", rel18, 18);
        call(5, 1, (rel18 as u32) & 0x3ffff)
    }

    /// ISA Summary Tables 198 (p. 661), 249 (p. 678); CALL8 pp. 375–376.
    /// Signed 18-bit word displacement, before implicit multiplication by four.
    pub fn call8(rel18: i32) -> u32 {
        signed("call8", "rel18", rel18, 18);
        call(5, 2, (rel18 as u32) & 0x3ffff)
    }

    /// ISA Summary Tables 198 (p. 661), 249 (p. 678); CALL12 pp. 376–377.
    /// Signed 18-bit word displacement, before implicit multiplication by four.
    pub fn call12(rel18: i32) -> u32 {
        signed("call12", "rel18", rel18, 18);
        call(5, 3, (rel18 as u32) & 0x3ffff)
    }

    pub fn callx0(reg: Reg) -> u32 {
        check_reg("callx0", "reg", reg);
        callx(0, 0, 0, 0, reg, 3, 0)
    }

    /// ISA Summary Tables 198–202 (pp. 661–662), 204 (p. 663); CALLX4 pp. 378–379.
    pub fn callx4(reg: Reg) -> u32 {
        check_reg("callx4", "reg", reg);
        callx(0, 0, 0, 0, reg, 3, 1)
    }

    /// ISA Summary Tables 198–202 (pp. 661–662), 204 (p. 663); CALLX8 pp. 379–380.
    pub fn callx8(reg: Reg) -> u32 {
        check_reg("callx8", "reg", reg);
        callx(0, 0, 0, 0, reg, 3, 2)
    }

    /// ISA Summary Tables 198–202 (pp. 661–662), 204 (p. 663); CALLX12 pp. 380–382.
    pub fn callx12(reg: Reg) -> u32 {
        check_reg("callx12", "reg", reg);
        callx(0, 0, 0, 0, reg, 3, 3)
    }

    /// ISA Summary Tables 198 (p. 661), 250, 253 (p. 678); ENTRY pp. 426–427.
    /// Encodes all four bits of s; execution is defined only for a0..a3.
    pub fn entry(s: Reg, frame_bytes: u32) -> u32 {
        check_reg("entry", "s", s);
        scaled("entry", "frame_bytes", frame_bytes, 12, 3);
        let imm12 = (frame_bytes >> 3) as i32;
        // ENTRY's unsigned imm12 uses the same bits as BRI12's signed field.
        bri12(6, s, 0, 3, (imm12 << 20) >> 20)
    }

    /// ISA Summary Tables 198–203 (pp. 661–662); RETW pp. 565–567.
    pub fn retw() -> u32 { callx(0, 0, 0, 0, 0, 2, 1) }

    /// ISA Summary Tables 198 (p. 661), 257–258 (pp. 679–680); RETW.N pp. 567–568.
    pub fn retw_n() -> u16 { rrrn(13, 15, 0, 1) }

    /// ISA Summary Tables 198–201 (pp. 661–662); MOVSP pp. 513–514.
    pub fn movsp(t: Reg, s: Reg) -> u32 {
        check_reg("movsp", "t", t);
        check_reg("movsp", "s", s);
        rrr(0, 0, 0, 1, s, t)
    }

    /// ISA Summary Tables 198–200 (pp. 661–662), 213 (p. 665); ROTW pp. 578–579.
    pub fn rotw(n: i32) -> u32 {
        signed("rotw", "n", n, 4);
        rrr(0, 0, 4, 8, 0, (n as u8) & 15)
    }

    /// ISA Summary Tables 198–199 (p. 661), 223 (p. 669); L32E pp. 455–457.
    /// The r field is a one-extended word offset, supplied here in bytes.
    pub fn l32e(t: Reg, s: Reg, offset: i32) -> u32 {
        check_reg("l32e", "t", t);
        check_reg("l32e", "s", s);
        assert!(offset & 3 == 0, "l32e: offset must be aligned to 4 bytes");
        assert!((-64..=-4).contains(&offset), "l32e: offset must be in -64..=-4");
        rrr(0, 9, 0, (offset / 4 + 16) as u8, s, t)
    }

    /// ISA Summary Tables 198–199 (p. 661), 223 (p. 669); S32E pp. 591–592.
    /// The r field is a one-extended word offset, supplied here in bytes.
    pub fn s32e(t: Reg, s: Reg, offset: i32) -> u32 {
        check_reg("s32e", "t", t);
        check_reg("s32e", "s", s);
        assert!(offset & 3 == 0, "s32e: offset must be aligned to 4 bytes");
        assert!((-64..=-4).contains(&offset), "s32e: offset must be in -64..=-4");
        rrr(0, 9, 4, (offset / 4 + 16) as u8, s, t)
    }

    /// ISA Summary Tables 198–201 (pp. 661–662), 207 (p. 663), 211 (p. 665); RFWO p. 575.
    pub fn rfwo() -> u32 { rrr(0, 0, 0, 3, 4, 0) }

    /// ISA Summary Tables 198–201 (pp. 661–662), 207 (p. 663), 211 (p. 665); RFWU p. 576.
    pub fn rfwu() -> u32 { rrr(0, 0, 0, 3, 5, 0) }

    /// ISA Summary Tables 198–201 (pp. 661–662), 207 (p. 663), 211 (p. 665); RFE pp. 570–571.
    pub fn rfe() -> u32 { rrr(0, 0, 0, 3, 0, 0) }

    pub fn j(rel18: i32) -> u32 {
        signed("j", "rel18", rel18, 18);
        call(6, 0, (rel18 as u32) & 0x3ffff)
    }

    pub fn jx(reg: Reg) -> u32 {
        check_reg("jx", "reg", reg);
        callx(0, 0, 0, 0, reg, 2, 2)
    }

    macro_rules! memory {
        ($($name:ident($value:ident): $r:expr, $shift:expr;)*) => {$(
            pub fn $name($value: Reg, reg_base: Reg, byte_offset: u32) -> u32 {
                check_reg(stringify!($name), stringify!($value), $value);
                check_reg(stringify!($name), "reg_base", reg_base);
                scaled(stringify!($name), "byte_offset", byte_offset, 8, $shift);
                rri8(2, $r, reg_base, $value, byte_offset >> $shift)
            }
        )*};
    }
    memory! {
        l8ui(rd): 0, 0; l16ui(rd): 1, 1; l32i(rd): 2, 2;
        s8i(rs): 4, 0; s16i(rs): 5, 1; s32i(rs): 6, 2;
    }

    macro_rules! memory_n {
        ($($name:ident($value:ident): $op:expr;)*) => {$(
            pub fn $name($value: Reg, reg_base: Reg, byte_offset: u32) -> u16 {
                check_reg(stringify!($name), stringify!($value), $value);
                check_reg(stringify!($name), "reg_base", reg_base);
                scaled(stringify!($name), "byte_offset", byte_offset, 4, 2);
                rrrn($op, (byte_offset >> 2) as u8, reg_base, $value)
            }
        )*};
    }
    memory_n! { l32i_n(rd): 8; s32i_n(rs): 9; }

    pub fn l32r(rd: Reg, op_off: u32, dest_off: u32) -> u32 {
        check_reg("l32r", "rd", rd);
        assert!(dest_off & 3 == 0, "l32r: dest_off must be aligned to 4 bytes");
        let pc = (u64::from(op_off) + 3) & !3;
        let rel = i64::from(dest_off) - pc as i64;
        assert!((-262144..=-4).contains(&rel),
            "l32r: dest_off must be 4..=262144 bytes behind aligned (op_off + 3)");
        ri16(1, rd, ((rel >> 2) as u32) & 0xffff)
    }

    pub fn mov_n(rd: Reg, rs: Reg) -> u16 {
        check_reg("mov_n", "rd", rd);
        check_reg("mov_n", "rs", rs);
        rrrn(13, 0, rs, rd)
    }

    pub fn movi(rd: Reg, imm12: i32) -> u32 {
        check_reg("movi", "rd", rd);
        signed("movi", "imm12", imm12, 12);
        rri8(2, 10, ((imm12 >> 8) & 0xf) as u8, rd, (imm12 as u32) & 0xff)
    }

    pub fn movi_n(rd: Reg, imm7: i32) -> u16 {
        check_reg("movi_n", "rd", rd);
        assert!((-32..=95).contains(&imm7), "movi_n: imm7 must be in -32..=95");
        ri7(12, rd, (imm7 as u32) & 0x7f)
    }

    macro_rules! unary {
        ($($name:ident: $op1:expr, $op2:expr, $s:expr, $t:expr;)*) => {$(
            pub fn $name(rd: Reg, rs: Reg) -> u32 {
                check_reg(stringify!($name), "rd", rd);
                check_reg(stringify!($name), "rs", rs);
                rrr(0, $op1, $op2, rd, if $s { rs } else { 0 }, if $t { rs } else { 0 })
            }
        )*};
    }
    unary! {
        neg: 0, 6, false, true;
        sll: 1, 10, true, false;
        srl: 1, 9, false, true;
        sra: 1, 11, false, true;
    }

    pub fn ret_n() -> u16 { rrrn(13, 15, 0, 0) }

    /// SLLI, ISA Summary pp. 610–611; RST1 map pp. 666–667.
    /// Zero is a move at the caller: the encoded zero shift is undefined.
    pub fn slli(rd: Reg, rs: Reg, shift: u8) -> u32 {
        assert!((1..=31).contains(&shift), "slli: shift must be in 1..=31");
        let sa = 32 - shift;
        rrr(0, 1, sa >> 4, rd, rs, sa & 15)
    }

    /// SRLI, ISA Summary p. 616. Shifts of 16..31 use EXTUI instead.
    pub fn srli(rd: Reg, rs: Reg, shift: u8) -> u32 {
        assert!(shift < 16, "srli: shift must be in 0..=15");
        rrr(0, 1, 4, rd, shift, rs)
    }

    /// SRAI, ISA Summary pp. 613–614; RST1 map pp. 666–667.
    pub fn srai(rd: Reg, rs: Reg, shift: u8) -> u32 {
        assert!(shift < 32, "srai: shift must be in 0..=31");
        rrr(0, 1, 2 | (shift >> 4), rd, shift & 15, rs)
    }

    pub fn ssl(rs: Reg) -> u32 {
        check_reg("ssl", "rs", rs);
        rrr(0, 0, 4, 1, rs, 0)
    }

    pub fn ssr(rs: Reg) -> u32 {
        check_reg("ssr", "rs", rs);
        rrr(0, 0, 4, 0, rs, 0)
    }

    // Cadence Xtensa ISA Summary pp. 650,657,668: WSR, RSR format,
    // RST3 opcode map. The SR number is an unsigned eight-bit field.
    pub fn wsr(rs: Reg, special: u32) -> u32 {
        rsr(0, 3, 1, special, rs)
    }

    /// ISA Summary Tables 198–199 (p. 661), 216 (p. 666); XSR pp. 653–654,
    /// RSR format p. 657. The special-register number is an unsigned eight-bit field.
    pub fn xsr(t: Reg, special: u32) -> u32 {
        rsr(0, 1, 6, special, t)
    }

    // ISA pp. 448,490,586,662–663: RST0/SYNC opcode maps.
    pub fn isync() -> u32 { rrr(0, 0, 0, 2, 0, 0) }
    pub fn rsync() -> u32 { rrr(0, 0, 0, 2, 0, 1) }
    pub fn memw() -> u32 { rrr(0, 0, 0, 2, 0, 12) }

    // ISA pp. 644,662: WAITI's s field is the new INTLEVEL.
    pub fn waiti(level: u8) -> u32 {
        field("waiti", "level", level.into(), 4);
        rrr(0, 0, 0, 7, level, 0)
    }

    // ISA p. 429: sa is bit 16 plus bits 11:8; op2 is width minus one.
    pub fn extui(rd: Reg, rs: Reg, shift: u8, width: u8) -> u32 {
        field("extui", "shift", shift.into(), 5);
        assert!((1..=16).contains(&width), "extui: width must be in 1..=16");
        assert!(u32::from(shift) + u32::from(width) <= 32,
            "extui: field must end at or before bit 31");
        rrr(0, 4 | (shift >> 4), width - 1, rd, shift & 15, rs)
    }
}

pub struct Asm {
    pub base: AsmBase,
    /// Scratch register. Large-offset stores destroy its previous value.
    pub temp: Reg,
    cur_const: u32,
    num_const: u32,
    const_table: Option<u32>,
    table_seen: bool,
    relaxation: Option<Relaxation>,
}

#[derive(Clone, Copy)]
enum BranchForm { Jump, Pair(u8, Reg, Reg), Zero(u8, Reg), Immediate(u8, Reg, i32) }
struct BranchSite { at: u32, label: usize, bits: u32, width: u32 }
#[derive(Default)]
struct Relaxation { widths: Vec<u32>, sites: Vec<BranchSite> }

impl AsMut<AsmBase> for Asm {
    fn as_mut(&mut self) -> &mut AsmBase { &mut self.base }
}

macro_rules! emitters {
    ($write:ident; $($name:ident($($arg:ident: $ty:ty),*);)*) => {$(
        pub fn $name(&mut self, $($arg: $ty),*) {
            self.$write(enc::$name($($arg),*));
        }
    )*};
}

impl Asm {
    pub fn new(labels: usize, temp: Reg) -> Self {
        check_reg("Asm::new", "temp", temp);
        Self {
            base: AsmBase::new(labels), temp, cur_const: 0, num_const: 0,
            const_table: None, table_seen: false, relaxation: None,
        }
    }

    pub fn op16(&mut self, op: u16) { self.base.write(&op.to_le_bytes()); }

    pub fn op24(&mut self, op: u32) {
        assert!(op <= 0xffffff, "op24: op must fit unsigned 24 bits");
        self.base.write(&op.to_le_bytes()[..3]);
    }

    emitters! { op24;
        addi(rd: Reg, rs: Reg, imm8: i32);
        addx2(rd: Reg, rs1: Reg, rs2: Reg); addx4(rd: Reg, rs1: Reg, rs2: Reg);
        and(rd: Reg, rs1: Reg, rs2: Reg);
        bcc(cond: u8, rs1: Reg, rs2: Reg, rel8: i32);
        bccz(cond: u8, rs: Reg, rel12: i32);
        bcci(cond: u8, rs: Reg, constant: i32, rel8: i32);
        callx0(reg: Reg); j(rel18: i32); jx(reg: Reg);
        callx4(reg: Reg); callx8(reg: Reg); callx12(reg: Reg);
        entry(s: Reg, frame_bytes: u32); retw();
        movsp(t: Reg, s: Reg); rotw(n: i32);
        l32e(t: Reg, s: Reg, offset: i32); s32e(t: Reg, s: Reg, offset: i32);
        rfwo(); rfwu(); rfe(); xsr(t: Reg, special: u32);
        l8ui(rd: Reg, reg_base: Reg, byte_offset: u32);
        l16ui(rd: Reg, reg_base: Reg, byte_offset: u32);
        l32i(rd: Reg, reg_base: Reg, byte_offset: u32);
        movi(rd: Reg, imm12: i32); mull(rd: Reg, rs1: Reg, rs2: Reg);
        quos(rd: Reg, rs1: Reg, rs2: Reg); rems(rd: Reg, rs1: Reg, rs2: Reg);
        muluh(rd: Reg, rs1: Reg, rs2: Reg);
        quou(rd: Reg, rs1: Reg, rs2: Reg); remu(rd: Reg, rs1: Reg, rs2: Reg);
        movnez(rd: Reg, rs: Reg, test: Reg); movt(rd: Reg, rs: Reg, bit: Reg);
        min(rd: Reg, rs: Reg, rt: Reg); max(rd: Reg, rs: Reg, rt: Reg);
        minu(rd: Reg, rs: Reg, rt: Reg); maxu(rd: Reg, rs: Reg, rt: Reg);
        salt(rd: Reg, rs: Reg, rt: Reg);
        add_s(fd: Reg, fs: Reg, ft: Reg); sub_s(fd: Reg, fs: Reg, ft: Reg);
        mul_s(fd: Reg, fs: Reg, ft: Reg);
        oeq_s(bit: Reg, fs: Reg, ft: Reg); olt_s(bit: Reg, fs: Reg, ft: Reg);
        ole_s(bit: Reg, fs: Reg, ft: Reg);
        float_s(fd: Reg, rs: Reg, scale: Reg); trunc_s(rd: Reg, fs: Reg, scale: Reg);
        rfr(rd: Reg, fs: Reg); wfr(fd: Reg, rs: Reg);
        neg(rd: Reg, rs: Reg); or(rd: Reg, rs1: Reg, rs2: Reg);
        s8i(rs: Reg, reg_base: Reg, byte_offset: u32);
        s16i(rs: Reg, reg_base: Reg, byte_offset: u32);
        s32i(rs: Reg, reg_base: Reg, byte_offset: u32);
        sll(rd: Reg, rs: Reg); srl(rd: Reg, rs: Reg); sra(rd: Reg, rs: Reg);
        ssl(rs: Reg); ssr(rs: Reg);
        slli(rd: Reg, rs: Reg, shift: u8); srli(rd: Reg, rs: Reg, shift: u8);
        srai(rd: Reg, rs: Reg, shift: u8);
        sub(rd: Reg, rs1: Reg, rs2: Reg); xor(rd: Reg, rs1: Reg, rs2: Reg);
        wsr(rs: Reg, special: u32); rsync(); isync(); memw(); waiti(level: u8);
        extui(rd: Reg, rs: Reg, shift: u8, width: u8);
    }
    emitters! { op16;
        add_n(rd: Reg, rs1: Reg, rs2: Reg);
        l32i_n(rd: Reg, reg_base: Reg, byte_offset: u32);
        mov_n(rd: Reg, rs: Reg); movi_n(rd: Reg, imm7: i32); ret_n();
        s32i_n(rs: Reg, reg_base: Reg, byte_offset: u32);
        retw_n();
    }

    pub fn op_call0(&mut self, rel18: i32) { self.op24(enc::call0(rel18)); }

    pub fn op_call4(&mut self, rel18: i32) { self.op24(enc::call4(rel18)); }
    pub fn op_call8(&mut self, rel18: i32) { self.op24(enc::call8(rel18)); }
    pub fn op_call12(&mut self, rel18: i32) { self.op24(enc::call12(rel18)); }

    pub fn op_l32r(&mut self, rd: Reg, op_off: u32, dest_off: u32) {
        self.op24(enc::l32r(rd, op_off, dest_off));
    }

    /// Place the constants exactly as MicroPython's entry code does. Call once
    /// per pass, at a 4-byte aligned offset, before any `mov_reg_i32`.
    /// L32R reaches back at most 256 KB. No bytes are emitted for an empty table.
    /// Use [`assemble`] to count literals before computing the final layout.
    pub fn literal_table(&mut self) {
        assert!(!self.table_seen, "literal_table: table must occur only once per pass");
        assert!(self.base.offset() & 3 == 0, "literal_table: offset must be aligned to 4 bytes");
        self.table_seen = true;
        self.const_table = None;
        if self.num_const > 0 {
            let bytes = self.num_const.checked_mul(4).expect("literal_table: count exceeds u32");
            assert!(bytes < 131072, "literal_table: count exceeds J's signed 18-bit range");
            // jump over the constants
            self.j(bytes as i32);
            self.base.reserve(1); // padding/alignment byte
            self.const_table = Some(self.base.reserve(bytes));
        }
    }

    /// Save this pass's constant count and reset its cursor. The Xtensa
    /// [`assemble`] driver calls this automatically after each traversal.
    pub fn end_pass(&mut self) {
        if self.base.pass() == Pass::Emit {
            assert_eq!(self.cur_const, self.num_const, "end_pass: Emit literal count differs from Compute");
        }
        self.num_const = self.cur_const;
        self.cur_const = 0;
        self.const_table = None;
        self.table_seen = false;
    }

    fn get_label_dest(&self, label: usize) -> Option<u32> {
        let dest = self.base.label_offset(label);
        assert!(dest.is_some() || self.base.pass() == Pass::Compute,
            "get_label_dest: label {label} is undefined in Emit");
        dest
    }

    fn calculate_branch_displacement(&self, label: usize) -> (bool, i64) {
        let dest = self.get_label_dest(label);
        let rel = i64::from(dest.unwrap_or(0)) - i64::from(self.base.offset()) - 4;
        (dest.is_some() && rel < 0, rel)
    }

    // Compute only needs instruction lengths. Forward destinations and literal
    // addresses are not yet known, so give checked encoders a valid placeholder:
    // -4 fits every width, is word-aligned and is not a reserved branch offset (p. 62).
    fn displacement(&self, name: &str, rel: i64, bits: u32) -> i32 {
        if self.base.pass() == Pass::Compute { return -4; }
        let limit = 1i64 << (bits - 1);
        assert!((-limit..limit).contains(&rel), "{name}: displacement must fit signed {bits} bits");
        rel as i32
    }

    pub fn j_label(&mut self, label: usize) {
        if self.relaxation.is_some() { self.relaxed_branch(BranchForm::Jump, label); return; }
        let (_, rel) = self.calculate_branch_displacement(label);
        // we assume rel, as a signed int, fits in 18-bits
        self.j(self.displacement("j_label", rel, 18));
    }

    pub fn bccz_reg_label(&mut self, cond: u8, reg: Reg, label: usize) {
        assert!(cond <= 3, "bccz_reg_label: cond must be a ccz constant");
        if self.relaxation.is_some() { self.relaxed_branch(BranchForm::Zero(cond, reg), label); return; }
        let (can_emit_short_jump, rel) = self.calculate_branch_displacement(label);
        if can_emit_short_jump && (-2048..2048).contains(&rel) {
            // Backwards BCCZ opcodes with an offset that fits in 12 bits can
            // be emitted without any change.
            self.bccz(cond, reg, self.displacement("bccz_reg_label", rel, 12));
            return;
        }
        // Range is effectively extended to 18 bits, as a more complex jump code
        // sequence is emitted.
        self.displacement("bccz_reg_label", rel - 6, 18);
        // ~BCCZ skip ; +0  <- Condition is flipped here (EQ -> NE, etc.)
        //  J    addr ; +3
        // skip:      ; +6
        self.bccz(cond ^ 1, reg, 6 - 4);
        self.j(self.displacement("bccz_reg_label", rel - 3, 18));
    }

    pub fn bcc_reg_reg_label(&mut self, cond: u8, reg1: Reg, reg2: Reg, label: usize) {
        check_cc("bcc_reg_reg_label", cond);
        if self.relaxation.is_some() { self.relaxed_branch(BranchForm::Pair(cond, reg1, reg2), label); return; }
        let (can_emit_short_jump, rel) = self.calculate_branch_displacement(label);
        if can_emit_short_jump && (-128..128).contains(&rel) {
            // Backwards BCC opcodes with an offset that fits in 8 bits can
            // be emitted without any change.
            self.bcc(cond, reg1, reg2, self.displacement("bcc_reg_reg_label", rel, 8));
            return;
        }
        // Range is effectively extended to 18 bits, as a more complex jump code
        // sequence is emitted.
        self.displacement("bcc_reg_reg_label", rel - 6, 18);
        // ~BCC skip ; +0  <- Condition is flipped here (EQ -> NE, etc.)
        //  J   addr ; +3
        // skip:     ; +6
        self.bcc(cond ^ 8, reg1, reg2, 6 - 4);
        self.j(self.displacement("bcc_reg_reg_label", rel - 3, 18));
    }

    pub fn bcci_reg_label(&mut self, cond: u8, reg: Reg, constant: i32, label: usize) {
        if self.relaxation.is_some() {
            self.relaxed_branch(BranchForm::Immediate(cond, reg, constant), label); return;
        }
        let (backward, rel) = self.calculate_branch_displacement(label);
        if backward && (-128..128).contains(&rel) {
            self.bcci(cond, reg, constant, self.displacement("bcci_reg_label", rel, 8));
        } else {
            self.bcci(cond ^ 8, reg, constant, 2);
            self.j(self.displacement("bcci_reg_label", rel - 3, 18));
        }
    }

    fn relaxed_branch(&mut self, form: BranchForm, label: usize) {
        let at = self.base.offset();
        let bits = match form { BranchForm::Jump => 18, BranchForm::Zero(..) => 12, _ => 8 };
        let relaxation = self.relaxation.as_mut().unwrap();
        let n = relaxation.sites.len();
        let width = relaxation.widths.get(n).copied().unwrap_or(if bits == 18 { 3 } else { 6 });
        relaxation.sites.push(BranchSite { at, label, bits, width });
        if width == 0 { return; }
        let (_, rel) = self.calculate_branch_displacement(label);
        if let BranchForm::Jump = form {
            self.j(self.displacement("relaxed J", rel, 18)); return;
        }
        let short = width == 3;
        let displacement = if short { self.displacement("relaxed branch", rel, bits) } else { 2 };
        match form {
            BranchForm::Pair(cond, x, y) => self.bcc(if short { cond } else { cond ^ 8 }, x, y, displacement),
            BranchForm::Zero(cond, x) => self.bccz(if short { cond } else { cond ^ 1 }, x, displacement),
            BranchForm::Immediate(cond, x, c) => self.bcci(if short { cond } else { cond ^ 8 }, x, c, displacement),
            BranchForm::Jump => unreachable!(),
        }
        if !short { self.j(self.displacement("relaxed long branch", rel - 3, 18)); }
    }

    // convenience function; reg_dest must be different from reg_src[12]
    pub fn setcc_reg_reg_reg(&mut self, cond: u8, reg_dest: Reg, reg_src1: Reg, reg_src2: Reg) {
        check_cc("setcc_reg_reg_reg", cond);
        assert!(reg_dest != reg_src1 && reg_dest != reg_src2,
            "setcc_reg_reg_reg: reg_dest aliases a source register");
        self.movi_n(reg_dest, 1);
        self.bcc(cond, reg_src1, reg_src2, 1);
        self.movi_n(reg_dest, 0);
    }

    /// Load through the literal table, returning its entry's byte offset.
    /// During the preliminary counting traversal the returned offset is provisional.
    pub fn mov_reg_i32(&mut self, reg_dest: Reg, value: u32) -> u32 {
        check_reg("mov_reg_i32", "reg_dest", reg_dest);
        assert!(self.table_seen, "mov_reg_i32: literal_table must precede constants");
        let entry = self.cur_const.checked_mul(4).expect("mov_reg_i32: literal index exceeds u32");
        let loc = self.const_table.unwrap_or(0).checked_add(entry)
            .expect("mov_reg_i32: literal offset exceeds u32");
        // load the constant
        if self.base.pass() == Pass::Emit {
            assert!(self.const_table.is_some(), "mov_reg_i32: constants table was not built");
            assert!(self.cur_const < self.num_const, "mov_reg_i32: literal count exceeds Compute");
            self.op_l32r(reg_dest, self.base.offset(), loc);
        } else {
            self.op24(enc::ri16(1, reg_dest, 0));
        }
        // store the constant in the table
        self.base.patch(loc, &value.to_le_bytes());
        self.cur_const = self.cur_const.checked_add(1).expect("mov_reg_i32: count exceeds u32");
        loc
    }

    pub fn mov_reg_i32_optimised(&mut self, reg_dest: Reg, value: u32) {
        if (-32..=95).contains(&(value as i32)) {
            self.movi_n(reg_dest, value as i32);
        } else if fits_signed(value as i32, 12) {
            self.movi(reg_dest, value as i32);
        } else {
            self.mov_reg_i32(reg_dest, value);
        }
    }

    /// Get a label address using MicroPython's MOVI/CALL0/ADD.N sequence.
    /// Clobbers A0; `reg_dest` must differ from A0. The relative MOVI is 12-bit.
    pub fn mov_reg_pcrel(&mut self, reg_dest: Reg, label: usize) {
        assert!(reg_dest != reg::A0, "mov_reg_pcrel: reg_dest must differ from A0");
        // Get relative offset from PC
        let dest = self.get_label_dest(label).unwrap_or(0);
        let mut rel = i64::from(dest) - i64::from(self.base.offset());
        rel -= 3 + 3; // account for 3 bytes of movi instruction, 3 bytes call0 adjustment
        self.movi(reg_dest, self.displacement("mov_reg_pcrel", rel, 12)); // imm has 12-bit range

        // Use call0 to get PC+3 into a0
        // call0 destination must be aligned on 4 bytes:
        //  - code_offset&3=0: off=0, pad=1
        //  - code_offset&3=1: off=0, pad=0
        //  - code_offset&3=2: off=1, pad=3
        //  - code_offset&3=3: off=1, pad=2
        let off = (self.base.offset() >> 1) & 1;
        let pad = 5u32.wrapping_sub(self.base.offset()) & 3;
        self.op_call0(off as i32);
        self.base.reserve(pad);

        // Add PC to relative offset
        self.add_n(reg_dest, reg_dest, reg::A0);
    }

    /// `word_offset` is in words, as in MicroPython.
    pub fn l32i_optimised(&mut self, reg_dest: Reg, reg_base: Reg, word_offset: u32) {
        let bytes = Self::element_bytes("l32i_optimised", word_offset, 2);
        if word_offset < 16 {
            self.l32i_n(reg_dest, reg_base, bytes);
        } else if word_offset < 256 {
            self.l32i(reg_dest, reg_base, bytes);
        } else {
            assert!(reg_dest != reg_base, "l32i_optimised: reg_dest aliases reg_base");
            self.mov_reg_i32_optimised(reg_dest, bytes);
            self.add_n(reg_dest, reg_base, reg_dest);
            self.l32i_n(reg_dest, reg_dest, 0);
        }
    }

    fn element_bytes(name: &str, offset: u32, operation_size: u32) -> u32 {
        assert!(operation_size <= 2, "{name}: operation_size must be in 0..=2");
        assert!(offset <= u32::MAX >> operation_size, "{name}: offset scaling exceeds u32");
        offset << operation_size
    }

    /// `offset` is in elements; `operation_size` is log2(bytes per element).
    pub fn load_reg_reg_offset(&mut self, reg_dest: Reg, reg_base: Reg, offset: u32, operation_size: u32) {
        let bytes = Self::element_bytes("load_reg_reg_offset", offset, operation_size);
        if operation_size == 2 && offset < 16 {
            self.l32i_n(reg_dest, reg_base, bytes);
            return;
        }
        if offset < 256 {
            self.op24(enc::rri8(2, operation_size as u8, reg_base, reg_dest, offset));
            return;
        }
        assert!(reg_dest != reg_base, "load_reg_reg_offset: reg_dest aliases reg_base");
        self.mov_reg_i32_optimised(reg_dest, bytes);
        self.add_n(reg_dest, reg_base, reg_dest);
        if operation_size == 2 {
            self.l32i_n(reg_dest, reg_dest, 0);
        } else {
            self.op24(enc::rri8(2, operation_size as u8, reg_dest, reg_dest, 0));
        }
    }

    /// `offset` is in elements; `operation_size` is log2(bytes per element).
    pub fn store_reg_reg_offset(&mut self, reg_src: Reg, reg_base: Reg, offset: u32, operation_size: u32) {
        let bytes = Self::element_bytes("store_reg_reg_offset", offset, operation_size);
        if operation_size == 2 && offset < 16 {
            self.s32i_n(reg_src, reg_base, bytes);
            return;
        }
        if offset < 256 {
            self.op24(enc::rri8(2, 4 | operation_size as u8, reg_base, reg_src, offset));
            return;
        }
        check_reg("store_reg_reg_offset", "temp", self.temp);
        assert!(self.temp != reg_src && self.temp != reg_base,
            "store_reg_reg_offset: temp aliases reg_src or reg_base");
        self.mov_reg_i32_optimised(self.temp, bytes);
        self.add_n(self.temp, reg_base, self.temp);
        if operation_size == 2 {
            self.s32i_n(reg_src, self.temp, 0);
        } else {
            self.op24(enc::rri8(2, 4 | operation_size as u8, self.temp, reg_src, 0));
        }
    }

    /// `condition` is 6 for BBCI or 14 for BBSI, as in MicroPython.
    pub fn bit_branch(&mut self, reg: Reg, bit: u32, label: usize, condition: u8) {
        check_reg("bit_branch", "reg", reg);
        assert!(bit < 32, "bit_branch: bit must be in 0..=31");
        assert!(matches!(condition, 6 | 14), "bit_branch: condition must be 6 (clear) or 14 (set)");
        let (_, rel) = self.calculate_branch_displacement(label);
        let rel = self.displacement("bit_branch", rel, 8);
        // The ISA Summary (p. 62) reserves -3..=0: a branch to the next instruction.
        assert!(!(-3..=0).contains(&rel), "bit_branch: offset -3..=0 is reserved (ISA Summary p. 62)");
        self.op24(enc::rri8(7, condition | (bit >> 4) as u8, reg, (bit & 0xf) as u8,
            (rel as u32) & 0xff));
    }

    /// Label CALL0, retaining the source's aligned target, location and signed
    /// 18-bit byte-range checks. Call at offset 1 mod 4, with a target 0 mod 4.
    pub fn call0(&mut self, label: usize) {
        let dest = self.get_label_dest(label).unwrap_or(0);
        let rel = i64::from(dest) - i64::from(self.base.offset()) - 3;
        if self.base.pass() == Pass::Emit {
            assert!(dest & 3 == 0, "call0: target must be word-aligned");
            assert!(rel & 3 == 0, "call0: location + 3 must be word-aligned");
        }
        let rel = self.displacement("call0", rel, 18);
        // The C helper passes bytes here, but op_call0 encodes words. Convert
        // once at this boundary so the instruction actually reaches the label.
        self.op_call0(rel / 4);
    }

    // Match call0's label resolution, aligned location and signed byte range.
    // The raw CALL4/8/12 encoders accept the full signed 18-bit word range.
    fn windowed_call_label(&mut self, name: &str, label: usize, encode: fn(i32) -> u32) {
        let dest = self.get_label_dest(label).unwrap_or(0);
        let rel = i64::from(dest) - i64::from(self.base.offset()) - 3;
        if self.base.pass() == Pass::Emit {
            assert!(dest & 3 == 0, "{name}: target must be word-aligned");
            assert!(rel & 3 == 0, "{name}: location + 3 must be word-aligned");
        }
        let rel = self.displacement(name, rel, 18);
        self.op24(encode(rel / 4));
    }

    /// Label CALL4, with call0's signed 18-bit byte range and location 1 mod 4.
    pub fn call4(&mut self, label: usize) {
        self.windowed_call_label("call4", label, enc::call4);
    }

    /// Label CALL8, with call0's signed 18-bit byte range and location 1 mod 4.
    pub fn call8(&mut self, label: usize) {
        self.windowed_call_label("call8", label, enc::call8);
    }

    /// Label CALL12, with call0's signed 18-bit byte range and location 1 mod 4.
    pub fn call12(&mut self, label: usize) {
        self.windowed_call_label("call12", label, enc::call12);
    }

    /// Label L32R, retaining MicroPython's stricter signed 18-bit byte range
    /// and aligned instruction location. The raw encoder accepts the full range.
    pub fn l32r(&mut self, reg: Reg, label: usize) {
        check_reg("l32r", "reg", reg);
        let dest = self.get_label_dest(label).unwrap_or(0);
        let rel = i64::from(dest) - i64::from(self.base.offset());
        if self.base.pass() == Pass::Emit {
            assert!(dest & 3 == 0, "l32r: target must be word-aligned");
            assert!(rel & 3 == 0, "l32r: location must be word-aligned");
            assert!(rel < 0, "l32r: target must precede location");
            self.displacement("l32r", rel, 18);
            self.op_l32r(reg, self.base.offset(), dest);
        } else {
            self.op24(enc::ri16(1, reg, 0));
        }
    }
}

/// Count literals, then generate a stable Compute/Emit pair with the common
/// assembler driver. The callback runs three times and must reset any external
/// state that affects layout each time. Do not call `end_pass` in the callback.
/// This preliminary traversal is necessary because a fresh MicroPython literal
/// table has zero entries until its first `end_pass`; the base cannot insert
/// bytes before already assigned labels. No code is emitted during counting.
pub fn assemble(asm: &mut Asm, mut f: impl FnMut(&mut Asm)) -> Vec<u8> {
    asm.relaxation = None;
    asm.num_const = 0;
    asm.cur_const = 0;
    asm.const_table = None;
    asm.table_seen = false;
    asm.base.start_pass(Pass::Compute);
    f(asm);
    asm.end_pass();
    crate::asm::assemble(asm, |asm| {
        f(asm);
        asm.end_pass();
    })
}

/// Opt-in relaxation for handlers with no alignment padding between branch
/// sites and their labels. Literal tables and entry alignment may precede all
/// sites. The callback must make the same label/branch calls every traversal.
/// Widths only shrink, so convergence needs at most two changes per site.
/// The final Compute/Emit pair uses identical, frozen decisions; source maps
/// and literal addresses therefore describe the final bytes directly.
pub fn assemble_relaxed(asm: &mut Asm, mut f: impl FnMut(&mut Asm)) -> Vec<u8> {
    asm.relaxation = None;
    asm.num_const = 0;
    asm.cur_const = 0;
    asm.const_table = None;
    asm.table_seen = false;
    asm.base.start_pass(Pass::Compute);
    f(asm);
    asm.end_pass();
    asm.relaxation = Some(Relaxation::default());
    loop {
        asm.base.start_pass(Pass::Compute);
        asm.relaxation.as_mut().unwrap().sites.clear();
        f(asm);
        asm.end_pass();
        let relaxation = asm.relaxation.as_mut().unwrap();
        let mut changed = false;
        relaxation.widths = relaxation.sites.iter().map(|site| {
            let dest = asm.base.label_offset(site.label).expect("relaxation target is defined");
            let mut width = site.width;
            if dest == site.at + width { width = 0; }
            else if width == 6 {
                // Shrinking this forward branch also moves its destination
                // three bytes earlier. B/B(I): signed 8 bits; BZ: signed 12
                // bits, all from PC+4 (ISA Summary pp. 62–63, 350–366).
                let rel = i64::from(dest) - i64::from(site.at) - 4 - if dest > site.at { 3 } else { 0 };
                let limit = 1i64 << (site.bits - 1);
                if (-limit..limit).contains(&rel) && !(-3..=0).contains(&rel) { width = 3; }
            }
            changed |= width != site.width;
            width
        }).collect();
        if !changed { break; }
    }
    let size = asm.base.offset();
    asm.base.start_pass(Pass::Emit);
    asm.relaxation.as_mut().unwrap().sites.clear();
    f(asm);
    asm.end_pass();
    assert_eq!(asm.base.offset(), size, "relaxed Emit size differs from Compute");
    asm.relaxation = None;
    asm.base.code().to_vec()
}
