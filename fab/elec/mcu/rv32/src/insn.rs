//! Instruction decoding from the unprivileged manual §§2.1, 4.2, 4.5, 7.1
//! and privileged manual §3.3. Compressed instructions retain their length,
//! but share execution semantics with their base instruction expansion.

pub type Reg = u8; // 0..=31

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchOp { Beq, Bne, Blt, Bge, Bltu, Bgeu }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadOp { Lb, Lh, Lw, Lbu, Lhu }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreOp { Sb, Sh, Sw }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImmOp { Addi, Slti, Sltiu, Xori, Ori, Andi, Slli, Srli, Srai }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegOp {
    Add, Sub, Sll, Slt, Sltu, Xor, Srl, Sra, Or, And,
    Mul, Mulh, Mulhsu, Mulhu, Div, Divu, Rem, Remu,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsrOp { Rw, Rs, Rc, Rwi, Rsi, Rci }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Insn {
    /// `imm` is the full value written, with the low twelve bits zero.
    Lui { rd: Reg, imm: u32 },
    /// `imm` is the full value added to this instruction's PC.
    Auipc { rd: Reg, imm: u32 },
    /// Signed byte displacement from this instruction, always even.
    Jal { rd: Reg, offset: i32 },
    /// Signed byte displacement from rs1; execution clears target bit zero.
    Jalr { rd: Reg, rs1: Reg, offset: i32 },
    Branch { op: BranchOp, rs1: Reg, rs2: Reg, offset: i32 },
    Load { op: LoadOp, rd: Reg, rs1: Reg, offset: i32 },
    /// Stores rs2 at rs1 + offset.
    Store { op: StoreOp, rs1: Reg, rs2: Reg, offset: i32 },
    /// For shifts, `imm` is the shift amount, 0..=31.
    OpImm { op: ImmOp, rd: Reg, rs1: Reg, imm: i32 },
    Op { op: RegOp, rd: Reg, rs1: Reg, rs2: Reg },
    Fence, FenceI, Ecall, Ebreak, Mret, Wfi,
    /// `src` is rs1 for register forms, or the five-bit uimm for immediate forms.
    Csr { op: CsrOp, rd: Reg, src: u8, csr: u16 },
}

fn sext(value: u32, width: u32) -> i32 {
    ((value << (32 - width)) as i32) >> (32 - width)
}

/// Decodes little-endian instruction bytes. If `bits & 3 != 3`, only the low
/// halfword is used. Returns a base instruction and its byte length (2 or 4).
/// Compressed HINTs decode to their computational expansion. Unsupported or
/// reserved encodings return `Err(())`; CSR existence is checked at execution.
pub fn decode(bits: u32) -> Result<(Insn, u32), ()> {
    if bits & 3 != 3 {
        return compressed(bits & 0xffff).map(|insn| (insn, 2));
    }
    let rd = ((bits >> 7) & 31) as Reg;
    let rs1 = ((bits >> 15) & 31) as Reg;
    let rs2 = ((bits >> 20) & 31) as Reg;
    let f3 = (bits >> 12) & 7;
    let f7 = bits >> 25;
    let imm = (bits as i32) >> 20;
    let insn = match bits & 0x7f {
        0x37 => Insn::Lui { rd, imm: bits & 0xfffff000 },
        0x17 => Insn::Auipc { rd, imm: bits & 0xfffff000 },
        0x6f => Insn::Jal {
            rd,
            offset: sext(((bits >> 31) << 20) | (bits & 0xff000)
                | ((bits >> 20) & 1) << 11 | ((bits >> 21) & 0x3ff) << 1, 21),
        },
        0x67 if f3 == 0 => Insn::Jalr { rd, rs1, offset: imm },
        0x63 => Insn::Branch {
            op: match f3 {
                0 => BranchOp::Beq, 1 => BranchOp::Bne,
                4 => BranchOp::Blt, 5 => BranchOp::Bge,
                6 => BranchOp::Bltu, 7 => BranchOp::Bgeu,
                _ => return Err(()),
            },
            rs1, rs2,
            offset: sext(((bits >> 31) << 12) | ((bits >> 7) & 1) << 11
                | ((bits >> 25) & 63) << 5 | ((bits >> 8) & 15) << 1, 13),
        },
        0x03 => Insn::Load {
            op: match f3 {
                0 => LoadOp::Lb, 1 => LoadOp::Lh, 2 => LoadOp::Lw,
                4 => LoadOp::Lbu, 5 => LoadOp::Lhu, _ => return Err(()),
            },
            rd, rs1, offset: imm,
        },
        0x23 => Insn::Store {
            op: match f3 {
                0 => StoreOp::Sb, 1 => StoreOp::Sh, 2 => StoreOp::Sw,
                _ => return Err(()),
            },
            rs1, rs2, offset: sext((bits >> 25) << 5 | ((bits >> 7) & 31), 12),
        },
        0x13 => {
            // §2.1.4.1: RV32 shamt is five bits. Match the entire funct7,
            // rejecting bit 25 as well as other extensions' shift encodings.
            let op = match (f3, f7) {
                (0, _) => ImmOp::Addi, (2, _) => ImmOp::Slti,
                (3, _) => ImmOp::Sltiu, (4, _) => ImmOp::Xori,
                (6, _) => ImmOp::Ori, (7, _) => ImmOp::Andi,
                (1, 0) => ImmOp::Slli, (5, 0) => ImmOp::Srli,
                (5, 0x20) => ImmOp::Srai, _ => return Err(()),
            };
            Insn::OpImm { op, rd, rs1, imm: if f3 == 1 || f3 == 5 { rs2 as i32 } else { imm } }
        }
        0x33 => Insn::Op {
            op: match (f7, f3) {
                (0, 0) => RegOp::Add, (0x20, 0) => RegOp::Sub,
                (0, 1) => RegOp::Sll, (0, 2) => RegOp::Slt,
                (0, 3) => RegOp::Sltu, (0, 4) => RegOp::Xor,
                (0, 5) => RegOp::Srl, (0x20, 5) => RegOp::Sra,
                (0, 6) => RegOp::Or, (0, 7) => RegOp::And,
                (1, 0) => RegOp::Mul, (1, 1) => RegOp::Mulh,
                (1, 2) => RegOp::Mulhsu, (1, 3) => RegOp::Mulhu,
                (1, 4) => RegOp::Div, (1, 5) => RegOp::Divu,
                (1, 6) => RegOp::Rem, (1, 7) => RegOp::Remu,
                _ => return Err(()),
            },
            rd, rs1, rs2,
        },
        // §§2.1.7 and 4.1: base implementations ignore unused fields,
        // including FENCE.I's funct12, rs1, and rd, for forward compatibility.
        0x0f if f3 == 0 => Insn::Fence,
        0x0f if f3 == 1 => Insn::FenceI,
        0x73 if f3 == 0 => match bits {
            0x00000073 => Insn::Ecall,
            0x00100073 => Insn::Ebreak,
            0x30200073 => Insn::Mret,
            0x10500073 => Insn::Wfi,
            _ => return Err(()),
        },
        0x73 => Insn::Csr {
            op: match f3 {
                1 => CsrOp::Rw, 2 => CsrOp::Rs, 3 => CsrOp::Rc,
                5 => CsrOp::Rwi, 6 => CsrOp::Rsi, 7 => CsrOp::Rci,
                _ => return Err(()),
            },
            rd, src: rs1, csr: (bits >> 20) as u16,
        },
        _ => return Err(()),
    };
    Ok((insn, 4))
}

fn compressed(b: u32) -> Result<Insn, ()> {
    let rd = ((b >> 7) & 31) as Reg;
    let rs2 = ((b >> 2) & 31) as Reg;
    let rp = (8 + ((b >> 7) & 7)) as Reg;
    let sp = (8 + ((b >> 2) & 7)) as Reg;
    let ci = sext(((b >> 12) & 1) << 5 | ((b >> 2) & 31), 6);
    let word_offset = (((b >> 10) & 7) << 3 | ((b >> 6) & 1) << 2
        | ((b >> 5) & 1) << 6) as i32;
    let insn = match (b & 3, b >> 13) {
        (0, 0) => {
            // §7.1.4.2: CIW immediate [5:4|9:6|2|3], zero is reserved.
            let imm = (((b >> 11) & 3) << 4 | ((b >> 7) & 15) << 6
                | ((b >> 6) & 1) << 2 | ((b >> 5) & 1) << 3) as i32;
            if imm == 0 { return Err(()); }
            Insn::OpImm { op: ImmOp::Addi, rd: sp, rs1: 2, imm }
        }
        (0, 2) => Insn::Load { op: LoadOp::Lw, rd: sp, rs1: rp, offset: word_offset },
        (0, 6) => Insn::Store { op: StoreOp::Sw, rs1: rp, rs2: sp, offset: word_offset },
        (1, 0) => Insn::OpImm { op: ImmOp::Addi, rd, rs1: rd, imm: ci },
        (1, 1) | (1, 5) => {
            // §7.1.3: CJ offset [11|4|9:8|10|6|7|3:1|5].
            let offset = sext(((b >> 12) & 1) << 11 | ((b >> 11) & 1) << 4
                | ((b >> 9) & 3) << 8 | ((b >> 8) & 1) << 10
                | ((b >> 7) & 1) << 6 | ((b >> 6) & 1) << 7
                | ((b >> 3) & 7) << 1 | ((b >> 2) & 1) << 5, 12);
            Insn::Jal { rd: if b >> 13 == 1 { 1 } else { 0 }, offset }
        }
        (1, 2) => Insn::OpImm { op: ImmOp::Addi, rd, rs1: 0, imm: ci },
        (1, 3) => {
            if ci == 0 { return Err(()); }
            if rd == 2 {
                let imm = sext(((b >> 12) & 1) << 9 | ((b >> 6) & 1) << 4
                    | ((b >> 5) & 1) << 6 | ((b >> 3) & 3) << 7
                    | ((b >> 2) & 1) << 5, 10);
                Insn::OpImm { op: ImmOp::Addi, rd: 2, rs1: 2, imm }
            } else {
                // §7.1.4.1: rd=0 is a HINT, but nzimm=0 is reserved.
                Insn::Lui { rd, imm: (ci as u32) << 12 }
            }
        }
        (1, 4) => match (b >> 10) & 3 {
            sub @ (0 | 1) => {
                // §7.1.4.2: shamt[5]=1 is RV32 custom space; zero is a HINT.
                if b & 0x1000 != 0 { return Err(()); }
                Insn::OpImm {
                    op: if sub == 0 { ImmOp::Srli } else { ImmOp::Srai },
                    rd: rp, rs1: rp, imm: ci,
                }
            }
            2 => Insn::OpImm { op: ImmOp::Andi, rd: rp, rs1: rp, imm: ci },
            _ => {
                // §7.1.7 Figure 8: bit 12 selects RV64/reserved (or Zcb).
                if b & 0x1000 != 0 { return Err(()); }
                Insn::Op {
                    op: match (b >> 5) & 3 {
                        0 => RegOp::Sub, 1 => RegOp::Xor, 2 => RegOp::Or, _ => RegOp::And,
                    },
                    rd: rp, rs1: rp, rs2: sp,
                }
            }
        },
        (1, 6) | (1, 7) => Insn::Branch {
            op: if b >> 13 == 6 { BranchOp::Beq } else { BranchOp::Bne },
            rs1: rp, rs2: 0,
            offset: sext(((b >> 12) & 1) << 8 | ((b >> 10) & 3) << 3
                | ((b >> 5) & 3) << 6 | ((b >> 3) & 3) << 1
                | ((b >> 2) & 1) << 5, 9),
        },
        (2, 0) => {
            if b & 0x1000 != 0 { return Err(()); }
            Insn::OpImm { op: ImmOp::Slli, rd, rs1: rd, imm: ci }
        }
        (2, 2) => {
            if rd == 0 { return Err(()); }
            Insn::Load {
                op: LoadOp::Lw, rd, rs1: 2,
                offset: (((b >> 12) & 1) << 5 | ((b >> 4) & 7) << 2
                    | ((b >> 2) & 3) << 6) as i32,
            }
        }
        (2, 4) => match (b & 0x1000 != 0, rd, rs2) {
            (false, 0, 0) => return Err(()),
            (false, _, 0) => Insn::Jalr { rd: 0, rs1: rd, offset: 0 },
            (false, _, _) => Insn::Op { op: RegOp::Add, rd, rs1: 0, rs2 },
            (true, 0, 0) => Insn::Ebreak,
            (true, _, 0) => Insn::Jalr { rd: 1, rs1: rd, offset: 0 },
            (true, _, _) => Insn::Op { op: RegOp::Add, rd, rs1: rd, rs2 },
        },
        (2, 6) => Insn::Store {
            op: StoreOp::Sw, rs1: 2, rs2,
            offset: (((b >> 9) & 15) << 2 | ((b >> 7) & 3) << 6) as i32,
        },
        // §7.1.7 Figures 7–9: FP, RV64-only, Zcb/Zcmp/Zcmt, reserved slots.
        _ => return Err(()),
    };
    Ok(insn)
}
