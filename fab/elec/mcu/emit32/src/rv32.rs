//! Ported from MicroPython's py/asmrv32.{c,h}
//!
//! The MIT License (MIT)
//!
//! Copyright (c) 2024 Alessandro Gatti
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
//! Immediates are signed RV32 values, including already-shifted LUI/AUIPC
//! values. Compressed window operands are full register numbers (x8..x15).
//! Legal HINT encodings are accepted; reserved encodings and operands that
//! would select a different instruction are rejected. Store arguments retain
//! MicroPython's order: value register first, address register second.
//!
//! The source's CA-format register masks are corrected from two bits to three
//! so the full compressed register window is representable. C.ADDI16SP has no
//! encoder in the supplied source and is not added here. Helpers retain the
//! source's instruction choices: if a choice needs C.LUI with `rd == SP` or
//! C.ADD with a zero source, operand checking rejects it instead of substituting
//! a different sequence. Large-offset loads require distinct destination/base
//! registers, and scratch-using helpers reject aliases with their inputs.

use crate::asm::{AsmBase, Pass};

pub type Reg = u8;

pub mod reg {
    use super::Reg;
    pub const ZERO: Reg = 0;
    pub const RA: Reg = 1;
    pub const SP: Reg = 2;
    pub const GP: Reg = 3;
    pub const TP: Reg = 4;
    pub const T0: Reg = 5;
    pub const T1: Reg = 6;
    pub const T2: Reg = 7;
    pub const S0: Reg = 8;
    pub const S1: Reg = 9;
    pub const A0: Reg = 10;
    pub const A1: Reg = 11;
    pub const A2: Reg = 12;
    pub const A3: Reg = 13;
    pub const A4: Reg = 14;
    pub const A5: Reg = 15;
    pub const A6: Reg = 16;
    pub const A7: Reg = 17;
    pub const S2: Reg = 18;
    pub const S3: Reg = 19;
    pub const S4: Reg = 20;
    pub const S5: Reg = 21;
    pub const S6: Reg = 22;
    pub const S7: Reg = 23;
    pub const S8: Reg = 24;
    pub const S9: Reg = 25;
    pub const S10: Reg = 26;
    pub const S11: Reg = 27;
    pub const T3: Reg = 28;
    pub const T4: Reg = 29;
    pub const T5: Reg = 30;
    pub const T6: Reg = 31;
}

fn check_reg(name: &str, operand: &str, r: Reg) {
    assert!(r < 32, "{name}: {operand} must be in 0..=31");
}

fn in_window(r: Reg) -> bool { (8..=15).contains(&r) }
fn fits_signed(value: i32, bits: u32) -> bool {
    let limit = 1i32 << (bits - 1);
    value >= -limit && value < limit
}

/// Pure, checked opcode encoders. Offsets are in bytes, including compressed
/// load/store offsets. CSR addresses are unsigned 12-bit values.
pub mod enc {
    use super::{check_reg, fits_signed, in_window, reg, Reg};

    fn window(name: &str, operand: &str, r: Reg) -> u32 {
        assert!(in_window(r), "{name}: {operand} must be in 8..=15");
        u32::from(r - 8)
    }

    fn signed(name: &str, operand: &str, value: i32, bits: u32) {
        assert!(fits_signed(value, bits), "{name}: {operand} must fit signed {bits} bits");
    }

    fn unsigned(name: &str, operand: &str, value: i32, bits: u32) {
        assert!(value >= 0 && (value as u32) < (1u32 << bits),
            "{name}: {operand} must fit unsigned {bits} bits");
    }

    fn aligned(name: &str, operand: &str, value: i32, alignment: i32) {
        assert!(value & (alignment - 1) == 0,
            "{name}: {operand} must be aligned to {alignment} bytes");
    }

    // MicroPython's RV32_ENCODE_TYPE_* layouts, after operand validation.
    fn type_i(op: u32, ft3: u32, rd: Reg, rs: Reg, imm: i32) -> u32 {
        op | (u32::from(rd) << 7) | (ft3 << 12) | (u32::from(rs) << 15)
            | (((imm as u32) & 0xfff) << 20)
    }

    fn type_ci(op: u32, ft3: u32, rd: Reg, imm: i32) -> u16 {
        let imm = imm as u32;
        (op | (ft3 << 13) | (u32::from(rd) << 7)
            | ((imm & 0x20) << 7) | ((imm & 0x1f) << 2)) as u16
    }

    fn type_cb(ft3: u32, rs: u32, imm: u32) -> u16 {
        (1 | (ft3 << 13) | (rs << 7) | ((imm & 0xe0) << 5)
            | ((imm & 0x1f) << 2)) as u16
    }

    macro_rules! r_ops {
        ($($name:ident: $ft3:expr, $ft7:expr;)*) => {$(
            pub fn $name(rd: Reg, rs1: Reg, rs2: Reg) -> u32 {
                check_reg(stringify!($name), "rd", rd);
                check_reg(stringify!($name), "rs1", rs1);
                check_reg(stringify!($name), "rs2", rs2);
                0x33 | (u32::from(rd) << 7) | ($ft3 << 12)
                    | (u32::from(rs1) << 15) | (u32::from(rs2) << 20) | ($ft7 << 25)
            }
        )*};
    }
    r_ops! {
        add: 0, 0; and: 7, 0; div: 4, 1; divu: 5, 1;
        mul: 0, 1; mulh: 1, 1; mulhsu: 2, 1; mulhu: 3, 1;
        or: 6, 0; rem: 6, 1; remu: 7, 1; sll: 1, 0;
        slt: 2, 0; sltu: 3, 0; sra: 5, 0x20; srl: 5, 0;
        sub: 0, 0x20; xor: 4, 0;
    }

    macro_rules! i_ops {
        ($($name:ident: $op:expr, $ft3:expr, $operand:ident;)*) => {$(
            pub fn $name(rd: Reg, rs: Reg, $operand: i32) -> u32 {
                check_reg(stringify!($name), "rd", rd);
                check_reg(stringify!($name), "rs", rs);
                signed(stringify!($name), stringify!($operand), $operand, 12);
                type_i($op, $ft3, rd, rs, $operand)
            }
        )*};
    }
    i_ops! {
        addi: 0x13, 0, immediate; andi: 0x13, 7, immediate;
        ori: 0x13, 6, immediate; slti: 0x13, 2, immediate;
        sltiu: 0x13, 3, immediate; xori: 0x13, 4, immediate;
        jalr: 0x67, 0, offset;
        lb: 3, 0, offset; lbu: 3, 4, offset; lh: 3, 1, offset;
        lhu: 3, 5, offset; lw: 3, 2, offset;
    }

    macro_rules! shifts {
        ($($name:ident: $ft3:expr, $high:expr;)*) => {$(
            pub fn $name(rd: Reg, rs: Reg, immediate: i32) -> u32 {
                check_reg(stringify!($name), "rd", rd);
                check_reg(stringify!($name), "rs", rs);
                unsigned(stringify!($name), "immediate", immediate, 5);
                type_i(0x13, $ft3, rd, rs, immediate | $high)
            }
        )*};
    }
    shifts! { slli: 1, 0; srai: 5, 0x400; srli: 5, 0; }

    macro_rules! branches {
        ($($name:ident: $ft3:expr;)*) => {$(
            pub fn $name(rs1: Reg, rs2: Reg, offset: i32) -> u32 {
                check_reg(stringify!($name), "rs1", rs1);
                check_reg(stringify!($name), "rs2", rs2);
                signed(stringify!($name), "offset", offset, 13);
                aligned(stringify!($name), "offset", offset, 2);
                let imm = offset as u32;
                0x63 | ($ft3 << 12) | ((imm & 0x800) >> 4)
                    | ((imm & 0x1e) << 7) | (u32::from(rs1) << 15)
                    | (u32::from(rs2) << 20) | ((imm & 0x7e0) << 20)
                    | ((imm & 0x1000) << 19)
            }
        )*};
    }
    branches! { beq: 0; bge: 5; bgeu: 7; blt: 4; bltu: 6; bne: 1; }

    macro_rules! stores {
        ($($name:ident: $ft3:expr;)*) => {$(
            pub fn $name(rs2: Reg, rs1: Reg, offset: i32) -> u32 {
                check_reg(stringify!($name), "rs2", rs2);
                check_reg(stringify!($name), "rs1", rs1);
                signed(stringify!($name), "offset", offset, 12);
                let imm = offset as u32;
                0x23 | ((imm & 0x1f) << 7) | ($ft3 << 12)
                    | (u32::from(rs1) << 15) | (u32::from(rs2) << 20)
                    | ((imm & 0xfe0) << 20)
            }
        )*};
    }
    stores! { sb: 0; sh: 1; sw: 2; }

    pub fn lui(rd: Reg, immediate: i32) -> u32 {
        check_reg("lui", "rd", rd);
        aligned("lui", "immediate", immediate, 4096);
        0x37 | (u32::from(rd) << 7) | (immediate as u32)
    }

    pub fn auipc(rd: Reg, offset: i32) -> u32 {
        check_reg("auipc", "rd", rd);
        aligned("auipc", "offset", offset, 4096);
        0x17 | (u32::from(rd) << 7) | (offset as u32)
    }

    pub fn jal(rd: Reg, offset: i32) -> u32 {
        check_reg("jal", "rd", rd);
        signed("jal", "offset", offset, 21);
        aligned("jal", "offset", offset, 2);
        let imm = offset as u32;
        0x6f | (u32::from(rd) << 7) | (imm & 0xff000)
            | ((imm & 0x800) << 9) | ((imm & 0x7fe) << 20)
            | ((imm & 0x100000) << 11)
    }

    macro_rules! csr_regs {
        ($($name:ident: $ft3:expr;)*) => {$(
            pub fn $name(rd: Reg, rs: Reg, immediate: i32) -> u32 {
                check_reg(stringify!($name), "rd", rd);
                check_reg(stringify!($name), "rs", rs);
                unsigned(stringify!($name), "immediate (csr)", immediate, 12);
                type_i(0x73, $ft3, rd, rs, immediate)
            }
        )*};
    }
    csr_regs! { csrrc: 3; csrrs: 2; csrrw: 1; }

    macro_rules! csr_imms {
        ($($name:ident: $ft3:expr;)*) => {$(
            pub fn $name(rd: Reg, csr: u16, immediate: i32) -> u32 {
                check_reg(stringify!($name), "rd", rd);
                unsigned(stringify!($name), "csr", i32::from(csr), 12);
                unsigned(stringify!($name), "immediate", immediate, 5);
                0x73 | (u32::from(rd) << 7) | ($ft3 << 12)
                    | (u32::from(csr) << 20) | ((immediate as u32) << 15)
            }
        )*};
    }
    csr_imms! { csrrci: 7; csrrsi: 6; csrrwi: 5; }

    pub fn ebreak() -> u32 { 0x100073 }
    pub fn ecall() -> u32 { 0x73 }
    pub fn cebreak() -> u16 { 0x9002 }
    pub fn cnop() -> u16 { 0x0001 }

    macro_rules! cr_ops {
        ($($name:ident: $ft4:expr;)*) => {$(
            pub fn $name(rd: Reg, rs: Reg) -> u16 {
                check_reg(stringify!($name), "rd", rd);
                check_reg(stringify!($name), "rs", rs);
                assert!(rs != 0, "{}: rs must be nonzero", stringify!($name));
                (2 | (u16::from(rs) << 2) | (u16::from(rd) << 7) | ($ft4 << 12)) as u16
            }
        )*};
    }
    cr_ops! { cadd: 9; cmv: 8; }

    macro_rules! ci_ops {
        ($($name:ident: $ft3:expr;)*) => {$(
            pub fn $name(rd: Reg, immediate: i32) -> u16 {
                check_reg(stringify!($name), "rd", rd);
                signed(stringify!($name), "immediate", immediate, 6);
                type_ci(1, $ft3, rd, immediate)
            }
        )*};
    }
    ci_ops! { caddi: 0; cli: 2; }

    pub fn caddi4spn(rd: Reg, immediate: i32) -> u16 {
        let rd = window("caddi4spn", "rd", rd);
        unsigned("caddi4spn", "immediate", immediate, 10);
        aligned("caddi4spn", "immediate", immediate, 4);
        assert!(immediate != 0, "caddi4spn: immediate must be nonzero");
        let imm = immediate as u32;
        ((rd << 2) | ((imm & 0x3c0) << 1) | ((imm & 0x30) << 7)
            | ((imm & 8) << 2) | ((imm & 4) << 4)) as u16
    }

    macro_rules! ca_ops {
        ($($name:ident: $ft2:expr;)*) => {$(
            pub fn $name(rd: Reg, rs: Reg) -> u16 {
                let rd = window(stringify!($name), "rd", rd);
                let rs = window(stringify!($name), "rs", rs);
                // The source's 0x03 masks lose bit 2 of these 3-bit fields.
                // Keep all three validated window bits (x8 through x15).
                (1 | (0x23 << 10) | ($ft2 << 5) | (rd << 7) | (rs << 2)) as u16
            }
        )*};
    }
    ca_ops! { cand: 3; cor: 2; csub: 0; cxor: 1; }

    pub fn candi(rd: Reg, immediate: i32) -> u16 {
        let rd = window("candi", "rd", rd);
        signed("candi", "immediate", immediate, 6);
        let imm = immediate as u32;
        type_cb(4, rd, ((imm & 0x20) << 2) | (imm & 0x1f) | 0x40)
    }

    macro_rules! cbranches {
        ($($name:ident: $ft3:expr;)*) => {$(
            pub fn $name(rs: Reg, offset: i32) -> u16 {
                let rs = window(stringify!($name), "rs", rs);
                signed(stringify!($name), "offset", offset, 9);
                aligned(stringify!($name), "offset", offset, 2);
                let imm = offset as u32;
                type_cb($ft3, rs, ((imm & 0x100) >> 1) | ((imm & 0xc0) >> 3)
                    | ((imm & 0x20) >> 5) | ((imm & 0x18) << 2) | (imm & 6))
            }
        )*};
    }
    cbranches! { cbeqz: 6; cbnez: 7; }

    macro_rules! cjumps {
        ($($name:ident: $ft3:expr;)*) => {$(
            pub fn $name(offset: i32) -> u16 {
                signed(stringify!($name), "offset", offset, 12);
                aligned(stringify!($name), "offset", offset, 2);
                let imm = offset as u32;
                (1 | ($ft3 << 13) | ((imm & 0x0e) << 2)
                    | ((imm & 0x300) << 1) | ((imm & 0x800) << 1)
                    | ((imm & 0x400) >> 2) | ((imm & 0x80) >> 1)
                    | ((imm & 0x40) << 1) | ((imm & 0x20) >> 3)
                    | ((imm & 0x10) << 7)) as u16
            }
        )*};
    }
    cjumps! { cj: 5; cjal: 1; }

    macro_rules! cjregs {
        ($($name:ident: $ft4:expr;)*) => {$(
            pub fn $name(rs: Reg) -> u16 {
                check_reg(stringify!($name), "rs", rs);
                assert!(rs != 0, "{}: rs must be nonzero", stringify!($name));
                2 | (u16::from(rs) << 7) | ($ft4 << 12)
            }
        )*};
    }
    cjregs! { cjalr: 9; cjr: 8; }

    pub fn clui(rd: Reg, immediate: i32) -> u16 {
        check_reg("clui", "rd", rd);
        assert!(rd != reg::SP, "clui: rd must not be SP (C.ADDI16SP encoding)");
        signed("clui", "immediate", immediate, 18);
        aligned("clui", "immediate", immediate, 4096);
        assert!(immediate != 0, "clui: immediate must be nonzero");
        type_ci(1, 3, rd, immediate >> 12)
    }

    macro_rules! cl_ops {
        ($($name:ident: $ft3:expr, $value:ident, $base:ident;)*) => {$(
            pub fn $name($value: Reg, $base: Reg, offset: i32) -> u16 {
                let rd = window(stringify!($name), stringify!($value), $value);
                let rs = window(stringify!($name), stringify!($base), $base);
                unsigned(stringify!($name), "offset", offset, 7);
                aligned(stringify!($name), "offset", offset, 4);
                let imm = offset as u32;
                (($ft3 << 13) | (rd << 2) | (rs << 7) | ((imm & 0x40) >> 1)
                    | ((imm & 0x38) << 7) | ((imm & 4) << 4)) as u16
            }
        )*};
    }
    cl_ops! { clw: 2, rd, rs; csw: 6, rs1, rs2; }

    pub fn clwsp(rd: Reg, offset: i32) -> u16 {
        check_reg("clwsp", "rd", rd);
        assert!(rd != 0, "clwsp: rd must be nonzero");
        unsigned("clwsp", "offset", offset, 8);
        aligned("clwsp", "offset", offset, 4);
        type_ci(2, 2, rd, ((offset & 0xc0) >> 6) | (offset & 0x3c))
    }

    pub fn cswsp(rs: Reg, offset: i32) -> u16 {
        check_reg("cswsp", "rs", rs);
        unsigned("cswsp", "offset", offset, 8);
        aligned("cswsp", "offset", offset, 4);
        let imm = ((offset & 0xc0) >> 6) | (offset & 0x3c);
        2 | (6 << 13) | (u16::from(rs) << 2) | ((imm as u16) << 7)
    }

    pub fn cslli(rd: Reg, immediate: i32) -> u16 {
        check_reg("cslli", "rd", rd);
        unsigned("cslli", "immediate", immediate, 5);
        type_ci(2, 0, rd, immediate)
    }

    macro_rules! cshifts {
        ($($name:ident: $high:expr;)*) => {$(
            pub fn $name(rd: Reg, immediate: i32) -> u16 {
                let rd = window(stringify!($name), "rd", rd);
                unsigned(stringify!($name), "immediate", immediate, 5);
                type_cb(4, rd, (immediate as u32) | $high)
            }
        )*};
    }
    cshifts! { csrai: 0x20; csrli: 0; }
}

pub struct Asm {
    pub base: AsmBase,
    /// Scratch register. Helpers that need it destroy its previous value.
    pub temp: Reg,
}

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
        assert!(temp != reg::ZERO, "Asm::new: temp must be nonzero");
        Self { base: AsmBase::new(labels), temp }
    }

    pub fn word(&mut self, word: u32) { self.base.write(&word.to_le_bytes()); }
    pub fn halfword(&mut self, halfword: u16) { self.base.write(&halfword.to_le_bytes()); }

    emitters! { word;
        add(rd: Reg, rs1: Reg, rs2: Reg); addi(rd: Reg, rs: Reg, immediate: i32);
        and(rd: Reg, rs1: Reg, rs2: Reg); andi(rd: Reg, rs: Reg, immediate: i32);
        auipc(rd: Reg, offset: i32);
        beq(rs1: Reg, rs2: Reg, offset: i32); bge(rs1: Reg, rs2: Reg, offset: i32);
        bgeu(rs1: Reg, rs2: Reg, offset: i32); blt(rs1: Reg, rs2: Reg, offset: i32);
        bltu(rs1: Reg, rs2: Reg, offset: i32); bne(rs1: Reg, rs2: Reg, offset: i32);
        csrrc(rd: Reg, rs: Reg, immediate: i32); csrrs(rd: Reg, rs: Reg, immediate: i32);
        csrrw(rd: Reg, rs: Reg, immediate: i32);
        csrrci(rd: Reg, csr: u16, immediate: i32); csrrsi(rd: Reg, csr: u16, immediate: i32);
        csrrwi(rd: Reg, csr: u16, immediate: i32);
        div(rd: Reg, rs1: Reg, rs2: Reg); divu(rd: Reg, rs1: Reg, rs2: Reg);
        ebreak(); ecall(); jal(rd: Reg, offset: i32); jalr(rd: Reg, rs: Reg, offset: i32);
        lb(rd: Reg, rs: Reg, offset: i32); lbu(rd: Reg, rs: Reg, offset: i32);
        lh(rd: Reg, rs: Reg, offset: i32); lhu(rd: Reg, rs: Reg, offset: i32);
        lui(rd: Reg, immediate: i32); lw(rd: Reg, rs: Reg, offset: i32);
        mul(rd: Reg, rs1: Reg, rs2: Reg); mulh(rd: Reg, rs1: Reg, rs2: Reg);
        mulhsu(rd: Reg, rs1: Reg, rs2: Reg); mulhu(rd: Reg, rs1: Reg, rs2: Reg);
        or(rd: Reg, rs1: Reg, rs2: Reg); ori(rd: Reg, rs: Reg, immediate: i32);
        rem(rd: Reg, rs1: Reg, rs2: Reg); remu(rd: Reg, rs1: Reg, rs2: Reg);
        sll(rd: Reg, rs1: Reg, rs2: Reg); slli(rd: Reg, rs: Reg, immediate: i32);
        slt(rd: Reg, rs1: Reg, rs2: Reg); slti(rd: Reg, rs: Reg, immediate: i32);
        sltiu(rd: Reg, rs: Reg, immediate: i32); sltu(rd: Reg, rs1: Reg, rs2: Reg);
        sra(rd: Reg, rs1: Reg, rs2: Reg); srai(rd: Reg, rs: Reg, immediate: i32);
        srl(rd: Reg, rs1: Reg, rs2: Reg); srli(rd: Reg, rs: Reg, immediate: i32);
        sub(rd: Reg, rs1: Reg, rs2: Reg);
        sb(rs2: Reg, rs1: Reg, offset: i32); sh(rs2: Reg, rs1: Reg, offset: i32);
        sw(rs2: Reg, rs1: Reg, offset: i32);
        xor(rd: Reg, rs1: Reg, rs2: Reg); xori(rd: Reg, rs: Reg, immediate: i32);
    }
    emitters! { halfword;
        cadd(rd: Reg, rs: Reg); caddi(rd: Reg, immediate: i32);
        caddi4spn(rd: Reg, immediate: i32); cand(rd: Reg, rs: Reg);
        candi(rd: Reg, immediate: i32); cbeqz(rs: Reg, offset: i32);
        cbnez(rs: Reg, offset: i32); cebreak(); cj(offset: i32); cjal(offset: i32);
        cjalr(rs: Reg); cjr(rs: Reg); cli(rd: Reg, immediate: i32);
        clui(rd: Reg, immediate: i32); clw(rd: Reg, rs: Reg, offset: i32);
        clwsp(rd: Reg, offset: i32); cmv(rd: Reg, rs: Reg); cnop();
        cor(rd: Reg, rs: Reg); cslli(rd: Reg, immediate: i32);
        csrai(rd: Reg, immediate: i32); csrli(rd: Reg, immediate: i32);
        csub(rd: Reg, rs: Reg); csw(rs1: Reg, rs2: Reg, offset: i32);
        cswsp(rs: Reg, offset: i32); cxor(rd: Reg, rs: Reg);
    }

    fn check_temp(&self, name: &str, operands: &[Reg]) {
        check_reg(name, "temp", self.temp);
        assert!(self.temp != reg::ZERO, "{name}: temp must be nonzero");
        assert!(!operands.contains(&self.temp), "{name}: temp aliases an operand register");
    }

    fn load_upper_immediate(&mut self, rd: Reg, immediate: i32) {
        // if immediate fits in 17 bits and is ≠ 0:
        //   c.lui rd, HI(immediate)
        // else:
        //   lui   rd, HI(immediate)
        // Keep MicroPython's 17-bit test, although C.LUI admits 18 bits.
        if fits_signed(immediate, 17) && (immediate >> 12) != 0 {
            self.clui(rd, immediate);
        } else {
            self.lui(rd, immediate);
        }
    }

    fn load_lower_immediate(&mut self, rd: Reg, immediate: u32) {
        // WARNING: This must be executed on a register that has either been
        //          previously cleared or was the target of a LUI/C.LUI or
        //          AUIPC opcode.
        if immediate == 0 { return; }

        // if LO(immediate) fits in 6 bits:
        //   c.addi rd, LO(immediate)
        // else:
        //   addi   rd, rd, LO(immediate)
        // split_immediate leaves the low part unsigned in MicroPython. Test
        // that raw value BEFORE sign extension to preserve its choice of form.
        if fits_signed(immediate as i32, 6) {
            self.caddi(rd, signed_lower(immediate));
        } else {
            self.addi(rd, rd, signed_lower(immediate));
        }
    }

    fn load_full_immediate(&mut self, rd: Reg, immediate: i32) {
        let (upper, lower) = split_immediate(immediate);
        // if immediate fits in 17 bits:
        //   c.lui  rd, HI(immediate)
        // else:
        //   lui    rd, HI(immediate)
        // if LO(immediate) fits in 6 bits && LO(immediate) != 0:
        //   c.addi rd, LO(immediate)
        // else:
        //   addi   rd, rd, LO(immediate)
        self.load_upper_immediate(rd, upper);
        self.load_lower_immediate(rd, lower);
    }

    pub fn emit_optimised_load_immediate(&mut self, rd: Reg, immediate: i32) {
        if fits_signed(immediate, 6) {
            // c.li rd, immediate
            self.cli(rd, immediate);
        } else if fits_signed(immediate, 12) {
            // addi rd, zero, immediate
            self.addi(rd, reg::ZERO, immediate);
        } else {
            self.load_full_immediate(rd, immediate);
        }
    }

    fn calculate_displacement_for_label(&self, label: usize) -> (bool, i32) {
        match self.base.label_offset(label) {
            Some(offset) => {
                let displacement = i64::from(offset) - i64::from(self.base.offset());
                let displacement = i32::try_from(displacement)
                    .expect("label displacement must fit signed 32 bits");
                assert!(displacement & 1 == 0, "label displacement must be halfword aligned");
                (displacement < 0, displacement)
            }
            None => {
                assert_eq!(self.base.pass(), Pass::Compute, "label {label} is undefined in Emit");
                // Only the size matters in Compute; no unknown label is short.
                (false, 0)
            }
        }
    }

    pub fn emit_jump(&mut self, label: usize) {
        let (can_emit_short_jump, displacement) = self.calculate_displacement_for_label(label);
        if can_emit_short_jump && fits_signed(displacement, 12) {
            // c.j displacement
            self.cj(displacement);
            return;
        }
        self.check_temp("emit_jump", &[]);
        let (upper, lower) = split_immediate(displacement);
        // auipc temporary, HI(displacement)
        // jalr  zero, temporary, LO(displacement)
        self.auipc(self.temp, upper);
        self.jalr(reg::ZERO, self.temp, signed_lower(lower));
    }

    pub fn emit_jump_if_reg_eq(&mut self, rs1: Reg, rs2: Reg, label: usize) {
        let (can_emit_short_jump, displacement) = self.calculate_displacement_for_label(label);
        if can_emit_short_jump && fits_signed(displacement, 13) {
            // beq rs1, rs2, displacement
            self.beq(rs1, rs2, displacement);
            return;
        }
        self.check_temp("emit_jump_if_reg_eq", &[rs1, rs2]);
        // Compensate for the initial BNE opcode.
        let (upper, lower) = split_immediate(displacement.wrapping_sub(4));
        // bne   rs1, rs2, 12                      ; PC + 0
        // auipc temporary, HI(displacement)       ; PC + 4
        // jalr  zero, temporary, LO(displacement) ; PC + 8
        // ...                                     ; PC + 12
        self.bne(rs1, rs2, 12);
        self.auipc(self.temp, upper);
        self.jalr(reg::ZERO, self.temp, signed_lower(lower));
    }

    pub fn emit_jump_if_reg_nonzero(&mut self, rs: Reg, label: usize) {
        let (can_emit_short_jump, mut displacement) = self.calculate_displacement_for_label(label);
        if can_emit_short_jump && fits_signed(displacement, 8) && in_window(rs) {
            // c.bnez rs', displacement
            self.cbnez(rs, displacement);
            return;
        }
        if can_emit_short_jump && fits_signed(displacement, 13) {
            // bne rs, zero, displacement
            self.bne(rs, reg::ZERO, displacement);
            return;
        }
        self.check_temp("emit_jump_if_reg_nonzero", &[rs]);
        // if rs1 in C window and displacement is negative:
        //    c.beqz rs', 10                           ; PC + 0
        //    auipc  temporary, HI(displacement)       ; PC + 2
        //    jalr   zero, temporary, LO(displacement) ; PC + 6
        //    ...                                      ; PC + 10
        // else:
        //    beq    rs, zero, 12                      ; PC + 0
        //    auipc  temporary, HI(displacement)       ; PC + 4
        //    jalr   zero, temporary, LO(displacement) ; PC + 8
        //    ...                                      ; PC + 12
        if can_emit_short_jump && in_window(rs) {
            self.cbeqz(rs, 10);
            // Compensate for the C.BEQZ opcode.
            displacement = displacement.wrapping_sub(2);
        } else {
            self.beq(rs, reg::ZERO, 12);
            // Compensate for the BEQ opcode.
            displacement = displacement.wrapping_sub(4);
        }
        let (upper, lower) = split_immediate(displacement);
        self.auipc(self.temp, upper);
        self.jalr(reg::ZERO, self.temp, signed_lower(lower));
    }

    fn load_by_size(&mut self, rd: Reg, rs: Reg, offset: i32, operation_size: u32) {
        // RV32_LOAD_OPCODE_FT3: lbu, lhu, lw.
        match operation_size {
            0 => self.lbu(rd, rs, offset),
            1 => self.lhu(rd, rs, offset),
            2 => self.lw(rd, rs, offset),
            _ => panic!("load_by_size: operation_size must be in 0..=2"),
        }
    }

    fn store_by_size(&mut self, rd: Reg, rs: Reg, offset: i32, operation_size: u32) {
        match operation_size {
            0 => self.sb(rd, rs, offset),
            1 => self.sh(rd, rs, offset),
            2 => self.sw(rd, rs, offset),
            _ => panic!("store_by_size: operation_size must be in 0..=2"),
        }
    }

    /// `offset` counts elements; `operation_size` is log2(bytes): 0, 1, or 2.
    /// A large offset uses `rd` to form the address, so `rd` must differ from `rs`.
    pub fn emit_load_reg_reg_offset(&mut self, rd: Reg, rs: Reg, offset: i32, operation_size: u32) {
        let scaled_offset = scaled_offset("emit_load_reg_reg_offset", offset, operation_size);
        if (0..64).contains(&scaled_offset) && operation_size == 2 && in_window(rd) && in_window(rs) {
            // c.lw rd', offset(rs')
            self.clw(rd, rs, scaled_offset);
            return;
        }
        if fits_signed(scaled_offset, 12) {
            // lbu|lhu|lw rd, offset(rs)
            self.load_by_size(rd, rs, scaled_offset, operation_size);
            return;
        }
        assert!(rd != rs && rd != reg::ZERO,
            "emit_load_reg_reg_offset: rd must be nonzero and distinct from rs for a large offset");
        let (upper, lower) = split_immediate(scaled_offset);
        // lui        rd, HI(offset) ; Or c.lui if possible
        // c.add      rd, rs
        // lbu|lhu|lw rd, LO(offset)(rd)
        self.load_upper_immediate(rd, upper);
        self.cadd(rd, rs);
        self.load_by_size(rd, rd, signed_lower(lower), operation_size);
    }

    /// Store `rd` at `rs + (offset << operation_size)`; sizes are 0, 1, 2.
    pub fn emit_store_reg_reg_offset(&mut self, rd: Reg, rs: Reg, offset: i32, operation_size: u32) {
        let scaled_offset = scaled_offset("emit_store_reg_reg_offset", offset, operation_size);
        if (0..64).contains(&scaled_offset) && operation_size == 2 && in_window(rd) && in_window(rs) {
            // c.sw rd', offset(rs')
            self.csw(rd, rs, scaled_offset);
            return;
        }
        if fits_signed(scaled_offset, 12) {
            // sb|sh|sw rd, offset(rs)
            self.store_by_size(rd, rs, scaled_offset, operation_size);
            return;
        }
        self.check_temp("emit_store_reg_reg_offset", &[rd, rs]);
        let (upper, lower) = split_immediate(scaled_offset);
        // lui      temporary, HI(offset) ; Or c.lui if possible
        // c.add    temporary, rs
        // sb|sh|sw rd, LO(offset)(temporary)
        self.load_upper_immediate(self.temp, upper);
        self.cadd(self.temp, rs);
        self.store_by_size(rd, self.temp, signed_lower(lower), operation_size);
    }

    pub fn emit_mov_reg_pcrel(&mut self, rd: Reg, label: usize) {
        // Unlike a jump target, a PC-relative data label can be byte aligned.
        let displacement = match self.base.label_offset(label) {
            Some(offset) => offset.wrapping_sub(self.base.offset()) as i32,
            None => {
                assert_eq!(self.base.pass(), Pass::Compute, "label {label} is undefined in Emit");
                0
            }
        };
        let (upper, lower) = split_immediate(displacement);
        // auipc rd, HI(relative)
        // addi  rd, rd, LO(relative)
        self.auipc(rd, upper);
        self.addi(rd, rd, signed_lower(lower));
    }

    pub fn emit_optimised_xor(&mut self, rd: Reg, rs: Reg) {
        if rd == rs {
            // c.li rd, 0
            self.cli(rd, 0);
        } else {
            // xor rd, rd, rs
            self.xor(rd, rd, rs);
        }
    }

    // WARNING: The scaled offset will be stored in self.temp.
    fn fix_up_scaled_reg_reg_reg(&mut self, rs1: Reg, rs2: Reg, operation_size: u32) {
        assert!(operation_size <= 2, "fix_up_scaled_reg_reg_reg: operation_size must be in 0..=2");
        self.check_temp("fix_up_scaled_reg_reg_reg", &[rs1, rs2]);
        if operation_size > 0 {
            self.slli(self.temp, rs2, operation_size as i32);
            self.cadd(self.temp, rs1);
        } else {
            self.add(self.temp, rs1, rs2);
        }
    }

    pub fn emit_load_reg_reg_reg(&mut self, rd: Reg, rs1: Reg, rs2: Reg, operation_size: u32) {
        self.fix_up_scaled_reg_reg_reg(rs1, rs2, operation_size);
        self.emit_load_reg_reg_offset(rd, self.temp, 0, operation_size);
    }

    pub fn emit_store_reg_reg_reg(&mut self, rd: Reg, rs1: Reg, rs2: Reg, operation_size: u32) {
        self.check_temp("emit_store_reg_reg_reg", &[rd, rs1, rs2]);
        self.fix_up_scaled_reg_reg_reg(rs1, rs2, operation_size);
        self.emit_store_reg_reg_offset(rd, self.temp, 0, operation_size);
    }

    pub fn meta_comparison_eq(&mut self, rs1: Reg, rs2: Reg, rd: Reg) {
        // sub   rd, rs1, rs2
        // sltiu rd, rd, 1
        self.sub(rd, rs1, rs2);
        self.sltiu(rd, rd, 1);
    }

    pub fn meta_comparison_ne(&mut self, rs1: Reg, rs2: Reg, rd: Reg) {
        // sub  rd, rs1, rs2
        // sltu rd, zero, rd
        self.sub(rd, rs1, rs2);
        self.sltu(rd, reg::ZERO, rd);
    }

    pub fn meta_comparison_lt(&mut self, rs1: Reg, rs2: Reg, rd: Reg, unsigned_comparison: bool) {
        // slt|sltu rd, rs1, rs2
        if unsigned_comparison { self.sltu(rd, rs1, rs2); }
        else { self.slt(rd, rs1, rs2); }
    }

    pub fn meta_comparison_le(&mut self, rs1: Reg, rs2: Reg, rd: Reg, unsigned_comparison: bool) {
        // slt[u] rd, rs2, rs1
        // xori   rd, rd, 1
        self.meta_comparison_lt(rs2, rs1, rd, unsigned_comparison);
        self.xori(rd, rd, 1);
    }
}

fn split_immediate(immediate: i32) -> (i32, u32) {
    let unsigned_immediate = immediate as u32;
    let mut upper = unsigned_immediate & 0xfffff000;
    let lower = unsigned_immediate & 0xfff;
    // Turn the lower half from unsigned to signed.
    if lower & 0x800 != 0 { upper = upper.wrapping_add(0x1000); }
    (upper as i32, lower)
}

fn signed_lower(lower: u32) -> i32 { ((lower << 20) as i32) >> 20 }

fn scaled_offset(name: &str, offset: i32, operation_size: u32) -> i32 {
    assert!(operation_size <= 2, "{name}: operation_size must be in 0..=2");
    let scaled = i64::from(offset) << operation_size;
    assert!(i32::try_from(scaled).is_ok(), "{name}: offset scaled by operation_size exceeds i32");
    scaled as i32
}
