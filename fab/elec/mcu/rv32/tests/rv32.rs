use fab_elec_mcu_rv32::insn::{decode, BranchOp, CsrOp, ImmOp, Insn, LoadOp, RegOp, StoreOp};
use fab_elec_mcu_rv32::sim::{cause, Bus, Cpu, Kind, Memory, Perm, Stop, TrapMode, Width};

// Test-only format encoders, transcribed from the manual, not from an encoder
// implementation. §§2.1.2–3: R/I/S/B/U/J. §7.1 Table 32 and Figures 7–9:
// CR/CI/CSS/CIW/CL/CS/CA/CB/CJ. No assembler is needed to build these programs.
fn r(opcode: u32, f7: u32, f3: u32, rd: u8, rs1: u8, rs2: u8) -> u32 {
    f7 << 25 | u32::from(rs2) << 20 | u32::from(rs1) << 15
        | f3 << 12 | u32::from(rd) << 7 | opcode
}
fn i(opcode: u32, f3: u32, rd: u8, rs1: u8, imm: i32) -> u32 {
    (imm as u32 & 0xfff) << 20 | u32::from(rs1) << 15
        | f3 << 12 | u32::from(rd) << 7 | opcode
}
fn s(f3: u32, rs1: u8, rs2: u8, imm: i32) -> u32 {
    let imm = imm as u32 & 0xfff;
    (imm >> 5) << 25 | u32::from(rs2) << 20 | u32::from(rs1) << 15
        | f3 << 12 | (imm & 31) << 7 | 0x23
}

// Pack source bits in the order printed left-to-right in the manual's fields.
// This deliberately uses a different construction from the production decoder.
fn field(value: i32, source_bits: &[u8]) -> u32 {
    source_bits.iter().fold(0, |out, &bit| (out << 1) | ((value as u32 >> bit) & 1))
}
fn b(f3: u32, rs1: u8, rs2: u8, offset: i32) -> u32 {
    field(offset, &[12, 10, 9, 8, 7, 6, 5]) << 25
        | u32::from(rs2) << 20 | u32::from(rs1) << 15 | f3 << 12
        | field(offset, &[4, 3, 2, 1, 11]) << 7 | 0x63
}
fn u(opcode: u32, rd: u8, value: u32) -> u32 {
    (value & 0xfffff000) | u32::from(rd) << 7 | opcode
}
fn j(rd: u8, offset: i32) -> u32 {
    field(offset, &[20, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 11, 19, 18, 17, 16, 15, 14, 13, 12]) << 12
        | u32::from(rd) << 7 | 0x6f
}
fn cr(f4: u32, rd: u8, rs2: u8) -> u32 {
    f4 << 12 | u32::from(rd) << 7 | u32::from(rs2) << 2 | 2
}
fn ci(f3: u32, rd: u8, imm: i32, quadrant: u32) -> u32 {
    f3 << 13 | field(imm, &[5]) << 12 | u32::from(rd) << 7
        | field(imm, &[4, 3, 2, 1, 0]) << 2 | quadrant
}
fn css(f3: u32, rs2: u8, immediate_field: u32) -> u32 {
    f3 << 13 | immediate_field << 7 | u32::from(rs2) << 2 | 2
}
fn ciw(rd: u8, offset: i32) -> u32 {
    field(offset, &[5, 4, 9, 8, 7, 6, 2, 3]) << 5 | u32::from(rd - 8) << 2
}
fn cl(rd: u8, rs1: u8, offset: i32) -> u32 {
    2 << 13 | field(offset, &[5, 4, 3]) << 10 | u32::from(rs1 - 8) << 7
        | field(offset, &[2, 6]) << 5 | u32::from(rd - 8) << 2
}
fn cs(rs1: u8, rs2: u8, offset: i32) -> u32 {
    6 << 13 | field(offset, &[5, 4, 3]) << 10 | u32::from(rs1 - 8) << 7
        | field(offset, &[2, 6]) << 5 | u32::from(rs2 - 8) << 2
}
fn ca(f6: u32, f2: u32, rd: u8, rs2: u8) -> u32 {
    f6 << 10 | u32::from(rd - 8) << 7 | f2 << 5 | u32::from(rs2 - 8) << 2 | 1
}
fn cb(f3: u32, rs1: u8, immediate_field: u32) -> u32 {
    f3 << 13 | (immediate_field >> 5) << 10 | u32::from(rs1 - 8) << 7
        | (immediate_field & 31) << 2 | 1
}
fn cb_branch(f3: u32, rs1: u8, offset: i32) -> u32 {
    cb(f3, rs1, field(offset, &[8, 4, 3, 7, 6, 2, 1, 5]))
}
fn cb_imm(sub: u32, rd: u8, imm: i32) -> u32 {
    cb(4, rd, field(imm, &[5]) << 7 | sub << 5 | field(imm, &[4, 3, 2, 1, 0]))
}
fn cj(f3: u32, offset: i32) -> u32 {
    f3 << 13 | field(offset, &[11, 4, 9, 8, 10, 6, 7, 3, 2, 1, 5]) << 2 | 1
}

const REGS: [u8; 4] = [0, 1, 15, 31];
const IMM12: [i32; 6] = [-2048, 2047, -1, 0, 0x555, -0x556];
const IMM6: [i32; 6] = [-32, 31, -1, 0, 21, -22];
const REG_OPS: [(u32, u32, RegOp); 18] = [
    (0, 0, RegOp::Add), (32, 0, RegOp::Sub), (0, 1, RegOp::Sll),
    (0, 2, RegOp::Slt), (0, 3, RegOp::Sltu), (0, 4, RegOp::Xor),
    (0, 5, RegOp::Srl), (32, 5, RegOp::Sra), (0, 6, RegOp::Or), (0, 7, RegOp::And),
    (1, 0, RegOp::Mul), (1, 1, RegOp::Mulh), (1, 2, RegOp::Mulhsu), (1, 3, RegOp::Mulhu),
    (1, 4, RegOp::Div), (1, 5, RegOp::Divu), (1, 6, RegOp::Rem), (1, 7, RegOp::Remu),
];
const BRANCHES: [(u32, BranchOp); 6] = [
    (0, BranchOp::Beq), (1, BranchOp::Bne), (4, BranchOp::Blt),
    (5, BranchOp::Bge), (6, BranchOp::Bltu), (7, BranchOp::Bgeu),
];

fn decoded(word: u32, expected: Insn, name: &str) {
    assert_eq!(decode(word), Ok((expected, 4)), "{name}: word {word:08x}");
}
fn expansion(half: u32, word: u32, expected: Insn, name: &str) {
    assert_eq!(decode(half), Ok((expected, 2)), "{name}: compressed operands {half:04x}");
    assert_eq!(decode(word), Ok((expected, 4)), "{name}: base expansion {word:08x}");
    assert_eq!(decode(half | 0xa55a0000), Ok((expected, 2)), "{name}: high half ignored");
}

#[test]
fn decode_register_and_immediate_operations() {
    for (f7, f3, op) in REG_OPS {
        for rd in REGS { for rs1 in REGS { for rs2 in REGS {
            decoded(r(0x33, f7, f3, rd, rs1, rs2), Insn::Op { op, rd, rs1, rs2 }, "R fields");
        } } }
    }
    for (f3, op) in [(0, ImmOp::Addi), (2, ImmOp::Slti), (3, ImmOp::Sltiu),
        (4, ImmOp::Xori), (6, ImmOp::Ori), (7, ImmOp::Andi)] {
        for rd in REGS { for rs1 in REGS { for imm in IMM12 {
            decoded(i(0x13, f3, rd, rs1, imm), Insn::OpImm { op, rd, rs1, imm }, "I sign extension");
        } } }
    }
    for (f3, upper, op) in [(1, 0, ImmOp::Slli), (5, 0, ImmOp::Srli), (5, 0x400, ImmOp::Srai)] {
        for rd in REGS { for rs1 in REGS { for imm in [0, 1, 15, 21, 31] {
            decoded(i(0x13, f3, rd, rs1, upper | imm), Insn::OpImm { op, rd, rs1, imm }, "RV32 shift amount");
        } } }
    }
}

#[test]
fn decode_memory_and_control_formats() {
    for rd in REGS { for rs1 in REGS { for offset in IMM12 {
        for (f3, op) in [(0, LoadOp::Lb), (1, LoadOp::Lh), (2, LoadOp::Lw), (4, LoadOp::Lbu), (5, LoadOp::Lhu)] {
            decoded(i(3, f3, rd, rs1, offset), Insn::Load { op, rd, rs1, offset }, "load operands");
        }
        for (f3, op) in [(0, StoreOp::Sb), (1, StoreOp::Sh), (2, StoreOp::Sw)] {
            decoded(s(f3, rs1, rd, offset), Insn::Store { op, rs1, rs2: rd, offset }, "S immediate scatter");
        }
        decoded(i(0x67, 0, rd, rs1, offset), Insn::Jalr { rd, rs1, offset }, "JALR signed byte offset");
    } } }
    // B/J offsets cannot be -1: the low bit is implicitly zero; use -2.
    for (f3, op) in BRANCHES { for rs1 in REGS { for rs2 in REGS {
        for offset in [-4096, 4094, -2, 0, 0xaaa, -0xaac] {
            decoded(b(f3, rs1, rs2, offset), Insn::Branch { op, rs1, rs2, offset }, "B signed even offset");
        }
    } } }
    for rd in REGS {
        for offset in [-1048576, 1048574, -2, 0, 0xaaaaa, -0xaaaac] {
            decoded(j(rd, offset), Insn::Jal { rd, offset }, "J signed even offset");
        }
        for imm in [0, 0x80000000, 0x7ffff000, 0xfffff000, 0xaaaaa000] {
            decoded(u(0x37, rd, imm), Insn::Lui { rd, imm }, "LUI value includes zero low bits");
            decoded(u(0x17, rd, imm), Insn::Auipc { rd, imm }, "AUIPC U immediate");
        }
    }
}

#[test]
fn decode_system_and_fences() {
    for (imm, expected) in [(0, Insn::Ecall), (1, Insn::Ebreak), (0x302, Insn::Mret), (0x105, Insn::Wfi)] {
        decoded(i(0x73, 0, 0, 0, imm), expected, "exact SYSTEM encoding");
    }
    for (f3, op) in [(1, CsrOp::Rw), (2, CsrOp::Rs), (3, CsrOp::Rc),
        (5, CsrOp::Rwi), (6, CsrOp::Rsi), (7, CsrOp::Rci)] {
        for rd in REGS { for src in REGS { for csr in [0, 1, 0x7ff, 0xfff, 0xaaa] {
            decoded(i(0x73, f3, rd, src, csr as i32), Insn::Csr { op, rd, src, csr }, "CSR fields are unsigned");
        } } }
    }
    for rd in REGS { for rs1 in REGS { for imm in IMM12 {
        decoded(i(0x0f, 0, rd, rs1, imm), Insn::Fence, "FENCE ignores fm/pred/succ/rs1/rd");
        decoded(i(0x0f, 1, rd, rs1, imm), Insn::FenceI, "FENCE.I ignores unused fields");
    } } }
    for fm in 0..16 { for pred in [0, 1, 5, 10, 15] { for succ in [0, 1, 5, 10, 15] {
        decoded(i(0x0f, 0, 31, 15, fm << 8 | pred << 4 | succ), Insn::Fence, "all FENCE modes accepted with varied predecessor/successor sets");
    } } }
}

#[test]
fn compressed_ci_cr_and_stack_expansions() {
    for rd in [0, 1, 2, 15, 31] {
        for imm in IMM6 {
            expansion(ci(0, rd, imm, 1), i(0x13, 0, rd, rd, imm),
                Insn::OpImm { op: ImmOp::Addi, rd, rs1: rd, imm }, "C.ADDI/C.NOP including HINTs");
            expansion(ci(2, rd, imm, 1), i(0x13, 0, rd, 0, imm),
                Insn::OpImm { op: ImmOp::Addi, rd, rs1: 0, imm }, "C.LI including rd=0 HINT");
            if rd != 2 && imm != 0 {
                let value = (imm as u32) << 12;
                expansion(ci(3, rd, imm, 1), u(0x37, rd, value),
                    Insn::Lui { rd, imm: value }, "C.LUI including rd=0 HINT");
            }
        }
        for imm in [0, 1, 15, 21, 31] {
            expansion(ci(0, rd, imm, 2), i(0x13, 1, rd, rd, imm),
                Insn::OpImm { op: ImmOp::Slli, rd, rs1: rd, imm }, "C.SLLI includes zero and rd=0 HINTs");
        }
        for rs2 in [1, 2, 3, 4, 5, 15, 31] {
            expansion(cr(8, rd, rs2), r(0x33, 0, 0, rd, 0, rs2),
                Insn::Op { op: RegOp::Add, rd, rs1: 0, rs2 }, "C.MV expands to ADD");
            expansion(cr(9, rd, rs2), r(0x33, 0, 0, rd, rd, rs2),
                Insn::Op { op: RegOp::Add, rd, rs1: rd, rs2 }, "C.ADD including HINT");
        }
        if rd != 0 {
            for (f4, link) in [(8, 0), (9, 1)] {
                expansion(cr(f4, rd, 0), i(0x67, 0, link, rd, 0),
                    Insn::Jalr { rd: link, rs1: rd, offset: 0 }, "C.JR/C.JALR");
            }
        }
        for offset in [0, 4, 252, 84, 168] {
            if rd != 0 {
                expansion(ci(2, rd, field(offset, &[5, 4, 3, 2, 7, 6]) as i32, 2), i(3, 2, rd, 2, offset),
                    Insn::Load { op: LoadOp::Lw, rd, rs1: 2, offset }, "C.LWSP unsigned stack offset");
            }
            expansion(css(6, rd, field(offset, &[5, 4, 3, 2, 7, 6])), s(2, 2, rd, offset),
                Insn::Store { op: StoreOp::Sw, rs1: 2, rs2: rd, offset }, "C.SWSP unsigned stack offset");
        }
    }
    for imm in [-512, 496, -16, 16, 336, -352] {
        expansion(ci(3, 2, field(imm, &[9, 4, 6, 8, 7, 5]) as i32, 1), i(0x13, 0, 2, 2, imm),
            Insn::OpImm { op: ImmOp::Addi, rd: 2, rs1: 2, imm }, "C.ADDI16SP scaled signed immediate");
    }
    expansion(cr(9, 0, 0), i(0x73, 0, 0, 0, 1), Insn::Ebreak, "C.EBREAK special CR case");
}

#[test]
fn compressed_compact_register_window() {
    for rd in 8..=15 {
        for imm in [4, 1020, 340, 680] {
            expansion(ciw(rd, imm), i(0x13, 0, rd, 2, imm),
                Insn::OpImm { op: ImmOp::Addi, rd, rs1: 2, imm }, "C.ADDI4SPN full compact window");
        }
        for rs1 in 8..=15 {
            for offset in [0, 4, 124, 84, 40] {
                expansion(cl(rd, rs1, offset), i(3, 2, rd, rs1, offset),
                    Insn::Load { op: LoadOp::Lw, rd, rs1, offset }, "C.LW full compact windows");
                expansion(cs(rs1, rd, offset), s(2, rs1, rd, offset),
                    Insn::Store { op: StoreOp::Sw, rs1, rs2: rd, offset }, "C.SW full compact windows");
            }
            for (f2, f7, f3, op) in [(0, 32, 0, RegOp::Sub), (1, 0, 4, RegOp::Xor),
                (2, 0, 6, RegOp::Or), (3, 0, 7, RegOp::And)] {
                expansion(ca(0b100011, f2, rd, rs1), r(0x33, f7, f3, rd, rd, rs1),
                    Insn::Op { op, rd, rs1: rd, rs2: rs1 }, "CA full compact windows");
            }
        }
        for (sub, upper, op) in [(0, 0, ImmOp::Srli), (1, 0x400, ImmOp::Srai)] {
            for imm in [0, 1, 15, 21, 31] {
                expansion(cb_imm(sub, rd, imm), i(0x13, 5, rd, rd, upper | imm),
                    Insn::OpImm { op, rd, rs1: rd, imm }, "CB shift including zero HINT");
            }
        }
        for imm in IMM6 {
            expansion(cb_imm(2, rd, imm), i(0x13, 7, rd, rd, imm),
                Insn::OpImm { op: ImmOp::Andi, rd, rs1: rd, imm }, "C.ANDI signed immediate");
        }
        for (cf3, f3, op) in [(6, 0, BranchOp::Beq), (7, 1, BranchOp::Bne)] {
            for offset in [-256, 254, -2, 0, 170, -172] {
                expansion(cb_branch(cf3, rd, offset), b(f3, rd, 0, offset),
                    Insn::Branch { op, rs1: rd, rs2: 0, offset }, "CB signed even offsets");
            }
        }
    }
    for (f3, rd) in [(1, 1), (5, 0)] {
        for offset in [-2048, 2046, -2, 0, 1364, -1366] {
            expansion(cj(f3, offset), j(rd, offset), Insn::Jal { rd, offset }, "CJ signed even offsets");
        }
    }
}

#[test]
fn reject_reserved_and_unimplemented_encodings() {
    for word in [0, u32::MAX, 0xffff, 0x1f, 0x3f, 0x7f] {
        assert_eq!(decode(word), Err(()), "zero/ones/long encoding is illegal: {word:08x}");
    }
    for opcode in [0x07, 0x27, 0x2f, 0x43, 0x47, 0x4b, 0x4f, 0x53, 0x1b, 0x3b, 0x0b, 0x2b, 0x5b, 0x7b] {
        assert_eq!(decode(r(opcode, 0, 2, 1, 15, 31)), Err(()), "FP/atomic/RV64/custom opcode {opcode:02x}");
    }
    for opcode in (3..128).step_by(4) {
        if ![0x03, 0x0f, 0x13, 0x17, 0x23, 0x33, 0x37, 0x63, 0x67, 0x6f, 0x73].contains(&opcode) {
            assert_eq!(decode(opcode), Err(()), "unimplemented major opcode {opcode:02x}");
        }
    }
    for f7 in 0..128 { for f3 in 0..8 {
        if !REG_OPS.iter().any(|&(a, b, _)| (a, b) == (f7, f3)) {
            assert_eq!(decode(r(0x33, f7, f3, 1, 15, 31)), Err(()), "unimplemented OP funct7={f7} funct3={f3}, including Zba/Zbb");
        }
    } }
    for f3 in [1, 5] { for f7 in 0..128 {
        if !(f7 == 0 || (f3 == 5 && f7 == 32)) {
            assert_eq!(decode(r(0x13, f7, f3, 1, 15, 31)), Err(()), "shift reserved funct7={f7}, including shamt[5]");
        }
    } }
    for f3 in 0..8 {
        if ![0, 1, 2, 4, 5].contains(&f3) {
            assert_eq!(decode(i(3, f3, 1, 15, 0)), Err(()), "unimplemented LOAD width {f3}");
        }
        if f3 > 2 {
            assert_eq!(decode(s(f3, 15, 31, 0)), Err(()), "unimplemented STORE width {f3}");
        }
        if f3 == 2 || f3 == 3 {
            assert_eq!(decode(b(f3, 1, 15, 0)), Err(()), "reserved BRANCH condition {f3}");
        }
        if f3 != 0 {
            assert_eq!(decode(i(0x67, f3, 1, 15, 0)), Err(()), "reserved JALR funct3 {f3}");
        }
        if f3 > 1 {
            assert_eq!(decode(i(0x0f, f3, 0, 0, 0)), Err(()), "unsupported MISC-MEM funct3 {f3}");
        }
    }
    for imm in [2, 0x102, 0x202, 0x7b2, 0x120] {
        assert_eq!(decode(i(0x73, 0, 0, 0, imm)), Err(()), "unsupported privileged instruction {imm:03x}");
    }
    for imm in [0, 1, 0x105, 0x302] {
        for (rd, rs1) in [(1, 0), (0, 1), (31, 15)] {
            assert_eq!(decode(i(0x73, 0, rd, rs1, imm)), Err(()), "SYSTEM requires zero rd/rs1");
        }
    }
    assert_eq!(decode(i(0x73, 4, 0, 0, 0)), Err(()), "reserved SYSTEM funct3");
    for q in [0, 2] { for f3 in [1, 3, 5, 7] { for payload in [0, 0x555, 0x1ffc] {
        assert_eq!(decode(f3 << 13 | (payload & !3) | q), Err(()), "compressed FP slots including Zcmp/Zcmt q={q} f3={f3}");
    } } }
    for payload in [0, 4, 0x555, 0x1ffc] {
        assert_eq!(decode(4 << 13 | (payload & !3)), Err(()), "quadrant 0 reserved/Zcb slot");
    }
    for rd in 8..=15 {
        assert_eq!(decode(ciw(rd, 0)), Err(()), "C.ADDI4SPN zero immediate reserved");
        for sub in [0, 1] {
            assert_eq!(decode(cb_imm(sub, rd, 32)), Err(()), "RV32 compressed right shift shamt[5] unsupported");
        }
        for f2 in 0..4 {
            assert_eq!(decode(ca(0b100111, f2, rd, 15)), Err(()), "RV64 CA or reserved/Zcb form");
        }
    }
    for rd in [0, 1, 2, 15, 31] {
        assert_eq!(decode(ci(3, rd, 0, 1)), Err(()), "C.LUI/C.ADDI16SP zero immediate reserved");
        assert_eq!(decode(ci(0, rd, 32, 2)), Err(()), "RV32 C.SLLI shamt[5] unsupported");
    }
    for imm in IMM6 {
        assert_eq!(decode(ci(2, 0, imm, 2)), Err(()), "C.LWSP rd=0 reserved");
    }
    assert_eq!(decode(cr(8, 0, 0)), Err(()), "C.JR rs1=0 reserved");
}

const DATA: u32 = 0x3fc80000;
const CODE: u32 = 0x40380000;
const FLASH: u32 = 0x42000000;
const RW: Perm = Perm { read: true, write: true, exec: false };
const RX: Perm = Perm { read: true, write: false, exec: true };

// All program tests use this one memory setup: 64 KiB shared SRAM, plus flash.
fn memory() -> (Memory, usize, usize) {
    let mut mem = Memory::new();
    let ram = mem.store(65536);
    let flash = mem.store(4096);
    mem.map(DATA, ram, 0, 65536, RW);
    mem.map(CODE, ram, 0, 65536, RX);
    mem.map(FLASH, flash, 0, 4096, RX);
    (mem, ram, flash)
}
fn put(mem: &mut Memory, store: usize, offset: usize, bits: u32, len: usize) {
    mem.bytes_mut(store)[offset..offset + len].copy_from_slice(&bits.to_le_bytes()[..len]);
}
fn step_word(cpu: &mut Cpu, mem: &mut Memory, ram: usize, word: u32) {
    cpu.pc = CODE;
    put(mem, ram, 0, word, 4);
    assert_eq!(cpu.step(mem), Ok(()), "instruction completes: {word:08x}");
}

#[test]
fn arithmetic_execution_corners() {
    let (mut mem, ram, _) = memory();
    let mut cpu = Cpu::new(CODE);
    for (f7, f3, a, b, expected, property) in [
        (0, 0, u32::MAX, 1, 0, "ADD wraps"),
        (32, 0, 0, 1, u32::MAX, "SUB wraps"),
        (0, 2, 0x80000000, 0, 1, "SLT signed negative"),
        (0, 3, 0x80000000, 0, 0, "SLTU unsigned high bit"),
        (0, 2, 0x7fffffff, u32::MAX, 0, "SLT positive versus -1"),
        (0, 3, 0x7fffffff, u32::MAX, 1, "SLTU positive versus MAX"),
        (0, 1, 1, 0xffffffe1, 2, "SLL masks rs2 to five bits"),
        (0, 5, 0x80000000, 0xffffffe1, 0x40000000, "SRL masks rs2"),
        (32, 5, 0x80000000, 0xffffffe1, 0xc0000000, "SRA masks and extends sign"),
        (0, 1, 0x12345678, 32, 0x12345678, "register shift by 32 is shift by zero"),
        (32, 5, 0x80000000, 31, u32::MAX, "SRA maximal amount"),
        (0, 4, 0xaaaa5555, 0x55555555, 0xffff0000, "XOR bits"),
        (0, 6, 0xaaaa0000, 0x55550000, 0xffff0000, "OR bits"),
        (0, 7, 0xaaaa5555, 0xffff0000, 0xaaaa0000, "AND bits"),
    ] {
        cpu.x[1] = a; cpu.x[15] = b;
        step_word(&mut cpu, &mut mem, ram, r(0x33, f7, f3, 31, 1, 15));
        assert_eq!(cpu.x[31], expected, "{property}");
    }
    for (f3, a, imm, expected, property) in [
        (0, u32::MAX, 1, 0, "ADDI wraps"),
        (0, 0, -1, u32::MAX, "ADDI negative immediate"),
        (2, 0x80000000, -1, 1, "SLTI signed immediate"),
        (3, 0, -1, 1, "SLTIU sign-extends before unsigned comparison"),
        (3, u32::MAX, -1, 0, "SLTIU equal unsigned operands"),
        (4, 0xaaaa5555, -1, 0x5555aaaa, "XORI sign extension"),
        (6, 0, -1, u32::MAX, "ORI sign extension"),
        (7, 0x12345678, -16, 0x12345670, "ANDI sign extension"),
        (1, 1, 31, 0x80000000, "SLLI maximal amount"),
        (5, 0x80000000, 31, 1, "SRLI zero fill"),
        (5, 0x80000000, 0x41f, u32::MAX, "SRAI sign fill"),
    ] {
        cpu.x[1] = a;
        step_word(&mut cpu, &mut mem, ram, i(0x13, f3, 31, 1, imm));
        assert_eq!(cpu.x[31], expected, "{property}");
    }
    cpu.x[0] = u32::MAX;
    step_word(&mut cpu, &mut mem, ram, i(0x13, 0, 1, 0, 7));
    assert_eq!(cpu.x[1], 7, "instructions read x0 as zero despite external slot mutation");
    step_word(&mut cpu, &mut mem, ram, i(0x13, 0, 0, 1, 9));
    assert_eq!(cpu.x[0], 0, "writes to x0 discarded");
    step_word(&mut cpu, &mut mem, ram, u(0x37, 31, 0xfffff000));
    assert_eq!(cpu.x[31], 0xfffff000, "LUI writes full U immediate");
    step_word(&mut cpu, &mut mem, ram, u(0x17, 31, 0xfffff000));
    assert_eq!(cpu.x[31], CODE - 4096, "AUIPC uses instruction address");
}

#[test]
fn multiplication_and_division_table() {
    let (mut mem, ram, _) = memory();
    let mut cpu = Cpu::new(CODE);
    // Expected high halves are literal 64-bit product corners (§4.5.1).
    for (a, b, low, signed, mixed, unsigned) in [
        (0, u32::MAX, 0, 0, 0, 0),
        (u32::MAX, u32::MAX, 1, 0, u32::MAX, 0xfffffffe),
        (0x80000000, u32::MAX, 0x80000000, 0, 0x80000000, 0x7fffffff),
        (u32::MAX, 0x80000000, 0x80000000, 0, u32::MAX, 0x7fffffff),
        (0x80000000, 0x80000000, 0, 0x40000000, 0xc0000000, 0x40000000),
        (0x7fffffff, 0x7fffffff, 1, 0x3fffffff, 0x3fffffff, 0x3fffffff),
        (0x7fffffff, u32::MAX, 0x80000001, u32::MAX, 0x7ffffffe, 0x7ffffffe),
    ] {
        cpu.x[1] = a; cpu.x[15] = b;
        for (f3, expected) in [(0, low), (1, signed), (2, mixed), (3, unsigned)] {
            step_word(&mut cpu, &mut mem, ram, r(0x33, 1, f3, 31, 1, 15));
            assert_eq!(cpu.x[31], expected, "M multiply f3={f3}, a={a:08x}, b={b:08x}");
        }
    }
    // §4.5.2 Table 22 and truncation toward zero for negative operands.
    for (a, b, div, divu, rem, remu) in [
        (0, 0, u32::MAX, u32::MAX, 0, 0),
        (0x80000000, 0, u32::MAX, u32::MAX, 0x80000000, 0x80000000),
        (u32::MAX, 0, u32::MAX, u32::MAX, u32::MAX, u32::MAX),
        (0x80000000, u32::MAX, 0x80000000, 0, 0, 0x80000000),
        (0xfffffff9, 3, 0xfffffffe, 1431655763, u32::MAX, 0),
        (7, 0xfffffffd, 0xfffffffe, 0, 1, 7),
        (0xfffffff9, 0xfffffffd, 2, 0, u32::MAX, 0xfffffff9),
    ] {
        cpu.x[1] = a; cpu.x[15] = b;
        for (f3, expected) in [(4, div), (5, divu), (6, rem), (7, remu)] {
            step_word(&mut cpu, &mut mem, ram, r(0x33, 1, f3, 31, 1, 15));
            assert_eq!(cpu.x[31], expected, "M divide/remainder f3={f3}, a={a:08x}, b={b:08x}");
        }
    }
}

#[test]
fn load_extension_and_store_byte_placement() {
    let (mut mem, ram, _) = memory();
    let mut cpu = Cpu::new(CODE);
    cpu.x[1] = DATA + 0x100;
    cpu.x[15] = 0x89abcdef;
    for (f3, offset) in [(0, 0), (1, 2), (2, 4)] {
        step_word(&mut cpu, &mut mem, ram, s(f3, 1, 15, offset));
    }
    assert_eq!(&mem.bytes(ram)[0x100..0x108], &[0xef, 0, 0xef, 0xcd, 0xef, 0xcd, 0xab, 0x89], "SB/SH/SW truncate and place little-endian bytes");
    for (f3, offset, expected) in [(0, 0, 0xffffffef), (4, 0, 0xef),
        (1, 2, 0xffffcdef), (5, 2, 0xcdef), (2, 4, 0x89abcdef)] {
        step_word(&mut cpu, &mut mem, ram, i(3, f3, 31, 1, offset));
        assert_eq!(cpu.x[31], expected, "load sign/zero extension funct3={f3}");
    }
    cpu.x[1] = DATA + 0x108;
    step_word(&mut cpu, &mut mem, ram, i(3, 2, 31, 1, -4));
    assert_eq!(cpu.x[31], 0x89abcdef, "negative load displacement");
    step_word(&mut cpu, &mut mem, ram, s(2, 1, 15, -8));
    assert_eq!(&mem.bytes(ram)[0x100..0x104], &[0xef, 0xcd, 0xab, 0x89], "negative store displacement");
}

#[test]
fn branch_conditions_and_jump_links() {
    let (mut mem, ram, _) = memory();
    let mut cpu = Cpu::new(CODE);
    // Expected conditions ordered BEQ, BNE, BLT, BGE, BLTU, BGEU.
    for (a, z, expected) in [
        (0, 0, [true, false, false, true, false, true]),
        (0x80000000, 0x7fffffff, [false, true, true, false, false, true]),
        (0x7fffffff, 0x80000000, [false, true, false, true, true, false]),
        (u32::MAX, 0, [false, true, true, false, false, true]),
        (0, u32::MAX, [false, true, false, true, true, false]),
        (u32::MAX, u32::MAX, [true, false, false, true, false, true]),
    ] {
        cpu.x[1] = a; cpu.x[15] = z;
        for ((f3, _), taken) in BRANCHES.into_iter().zip(expected) {
            for offset in [-2, 10] {
                step_word(&mut cpu, &mut mem, ram, b(f3, 1, 15, offset));
                assert_eq!(cpu.pc, if taken { CODE.wrapping_add(offset as u32) } else { CODE + 4 }, "branch f3={f3} taken={taken} signed/unsigned corner");
            }
        }
    }
    step_word(&mut cpu, &mut mem, ram, j(1, 10));
    assert_eq!((cpu.pc, cpu.x[1]), (CODE + 10, CODE + 4), "JAL link and halfword-aligned target");
    cpu.x[1] = CODE + 13;
    step_word(&mut cpu, &mut mem, ram, i(0x67, 0, 1, 1, -2));
    assert_eq!((cpu.pc, cpu.x[1]), (CODE + 10, CODE + 4), "JALR reads old rs1=rd, adds signed offset, clears bit zero");
    for (half, link, target) in [(cj(1, 10), 1, CODE + 10), (cr(9, 1, 0), 1, CODE + 12),
        (cj(5, 10), 0, CODE + 10), (cr(8, 1, 0), 0, CODE + 12)] {
        cpu.pc = CODE; cpu.x[1] = CODE + 13;
        put(&mut mem, ram, 0, half, 2);
        assert_eq!(cpu.step(&mut mem), Ok(()), "compressed jump completes");
        assert_eq!(cpu.pc, target, "compressed jump target");
        assert_eq!(cpu.x[1], if link == 1 { CODE + 2 } else { CODE + 13 }, "compressed link is pc+2 or discarded");
    }
}

fn state(cpu: &Cpu) -> ([u32; 32], u32, u64, [u32; 6], TrapMode) {
    (cpu.x, cpu.pc, cpu.instret,
        [cpu.mstatus, cpu.mtvec, cpu.mepc, cpu.mcause, cpu.mtval, cpu.mscratch], cpu.traps)
}

fn fault_cases(mode: TrapMode) {
    for (word, pc, address, reason, tval, property) in [
        (0xa55a0000, CODE, DATA, cause::ILLEGAL, 0, "illegal compressed bits are zero-extended"),
        (u32::MAX, CODE, DATA, cause::ILLEGAL, u32::MAX, "illegal full word tval"),
        (i(3, 1, 31, 1, 0), CODE, DATA + 1, cause::LOAD_MISALIGNED, DATA + 1, "misaligned LH"),
        (i(3, 2, 31, 1, 0), CODE, DATA + 2, cause::LOAD_MISALIGNED, DATA + 2, "misaligned LW"),
        (s(1, 1, 15, 0), CODE, DATA + 1, cause::STORE_MISALIGNED, DATA + 1, "misaligned SH"),
        (s(2, 1, 15, 0), CODE, DATA + 2, cause::STORE_MISALIGNED, DATA + 2, "misaligned SW"),
        (s(2, 1, 15, 0), CODE, FLASH, cause::STORE_ACCESS, FLASH, "flash write protection"),
        (i(3, 2, 0, 1, 0), CODE, 0x80000000, cause::LOAD_ACCESS, 0x80000000, "load to x0 still faults"),
        (s(2, 1, 15, 0), CODE, 0x80000000, cause::STORE_ACCESS, 0x80000000, "unmapped store"),
        (0, 0x80000000, DATA, cause::INSN_ACCESS, 0x80000000, "unmapped fetch"),
        (0, DATA, DATA, cause::INSN_ACCESS, DATA, "fetch requires exec permission"),
        (0, CODE + 1, DATA, cause::INSN_MISALIGNED, CODE + 1, "externally supplied odd pc"),
        (i(0x73, 0, 0, 0, 0), CODE, DATA, cause::ECALL_M, 0, "ECALL from machine mode"),
        (i(0x73, 0, 0, 0, 1), CODE, DATA, cause::BREAKPOINT, CODE, "EBREAK"),
        (cr(9, 0, 0), CODE, DATA, cause::BREAKPOINT, CODE, "C.EBREAK"),
        (i(0x73, 1, 31, 15, 0x301), CODE, DATA, cause::ILLEGAL,
            i(0x73, 1, 31, 15, 0x301), "unimplemented CSR"),
        (i(0x73, 1, 31, 15, 0xf14), CODE, DATA, cause::ILLEGAL,
            i(0x73, 1, 31, 15, 0xf14), "read-only mhartid write"),
    ] {
        let (mut mem, ram, flash) = memory();
        put(&mut mem, ram, 0, word, 4);
        put(&mut mem, ram, 0x300, i(0x73, 0, 0, 0, 0x302), 4);
        for enabled in [0, 8] {
            let mut cpu = Cpu::new(pc);
            cpu.traps = mode;
            cpu.instret = 7;
            cpu.x[1] = address; cpu.x[15] = 0x12345678; cpu.x[31] = 0xabcdef01;
            cpu.mtvec = CODE + 0x301;
            cpu.mstatus = 0x80 | enabled;
            cpu.mepc = 0x100; cpu.mcause = 9; cpu.mtval = 0x55; cpu.mscratch = 0xaa;
            let before = state(&cpu);
            let before_ram = mem.bytes(ram).to_vec();
            let before_flash = mem.bytes(flash).to_vec();
            let outcome = cpu.step(&mut mem);
            assert_eq!(mem.bytes(ram), before_ram.as_slice(), "{property}: no memory mutation on exception");
            assert_eq!(mem.bytes(flash), before_flash.as_slice(), "{property}: flash unchanged");
            if mode == TrapMode::Stop {
                let expected = if reason == cause::BREAKPOINT { Stop::Ebreak { pc } }
                    else { Stop::Trap { cause: reason, pc, tval } };
                assert_eq!(outcome, Err(expected), "{property}: Stop outcome");
                assert_eq!(state(&cpu), before, "{property}: every CPU field unchanged in Stop mode");
            } else {
                assert_eq!(outcome, Ok(()), "{property}: Take enters handler");
                assert_eq!(cpu.x, before.0, "{property}: faulting instruction writes no registers");
                assert_eq!(cpu.instret, 7, "{property}: exception does not retire");
                assert_eq!((cpu.mepc, cpu.mcause, cpu.mtval), (pc & !1, reason, tval), "{property}: trap CSRs");
                assert_eq!(cpu.mstatus, 0x1800 | (enabled << 4), "{property}: MIE to MPIE, clear MIE, MPP=M");
                assert_eq!(cpu.pc, CODE + 0x300, "{property}: synchronous trap goes to BASE, not vectored offset");
                assert_eq!((cpu.mtvec, cpu.mscratch), (CODE + 0x301, 0xaa), "{property}: unrelated CSRs preserved");
                assert_eq!(cpu.step(&mut mem), Ok(()), "{property}: handler MRET completes");
                assert_eq!(cpu.pc, pc & !1, "{property}: MRET reads mepc with bit zero clear");
                assert_eq!(cpu.mstatus, 0x80 | enabled, "{property}: MRET restores MIE, sets MPIE, clears MPP");
                assert_eq!(cpu.instret, 8, "{property}: only MRET retires");
                cpu.pc = CODE + 0x300;
                let returned = state(&cpu);
                assert_eq!(cpu.step(&mut mem), Err(Stop::Unmodeled { pc: CODE + 0x300, what: "MRET to user mode" }),
                    "{property}: a second MRET (MPP now U) stops: only M is modeled");
                assert_eq!(state(&cpu), returned, "{property}: the unmodeled MRET changes nothing");
            }
        }
    }
}

#[test]
fn faults_stop_without_state_changes() { fault_cases(TrapMode::Stop); }

#[test]
fn faults_take_machine_traps_and_return() { fault_cases(TrapMode::Take); }

// Observe access ordering, width, kind, and the retired count. A bus records
// attempts even on errors, while its backing bytes obey the atomicity contract.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Access { addr: u32, width: Width, kind: Kind, now: u64 }
struct Traced {
    mem: Memory,
    accesses: Vec<Access>,
}
impl Bus for Traced {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, now: u64) -> Result<u32, ()> {
        self.accesses.push(Access { addr, width, kind, now });
        self.mem.read(addr, width, kind, now)
    }
    fn write(&mut self, addr: u32, width: Width, value: u32, now: u64) -> Result<(), ()> {
        self.accesses.push(Access { addr, width, kind: Kind::Store, now });
        self.mem.write(addr, width, value, now)
    }
}

#[test]
fn fetch_halves_fault_address_and_retirement_timestamps() {
    let (mut mem, ram, _) = memory();
    put(&mut mem, ram, 0, ci(0, 0, 0, 1), 2);
    put(&mut mem, ram, 2, i(3, 2, 0, 1, 0), 4);
    put(&mut mem, ram, 6, s(0, 1, 15, 1), 4);
    let mut bus = Traced { mem, accesses: Vec::new() };
    let mut cpu = Cpu::new(CODE);
    cpu.x[1] = DATA + 0x100; cpu.x[15] = 0x55; cpu.instret = 19;
    for _ in 0..3 { assert_eq!(cpu.step(&mut bus), Ok(()), "mixed-width program retires"); }
    assert_eq!(bus.accesses, vec![
        Access { addr: CODE, width: Width::H, kind: Kind::Fetch, now: 19 },
        Access { addr: CODE + 2, width: Width::H, kind: Kind::Fetch, now: 20 },
        Access { addr: CODE + 4, width: Width::H, kind: Kind::Fetch, now: 20 },
        Access { addr: DATA + 0x100, width: Width::W, kind: Kind::Load, now: 20 },
        Access { addr: CODE + 6, width: Width::H, kind: Kind::Fetch, now: 21 },
        Access { addr: CODE + 8, width: Width::H, kind: Kind::Fetch, now: 21 },
        Access { addr: DATA + 0x101, width: Width::B, kind: Kind::Store, now: 21 },
    ], "fetches are halfwords; load-to-x0 accesses bus; now counts prior completions");
    for mode in [TrapMode::Stop, TrapMode::Take] {
        cpu = Cpu::new(CODE + 65534); cpu.traps = mode; cpu.mtvec = CODE + 0x301;
        put(&mut bus.mem, ram, 65534, i(0x13, 0, 1, 0, 7), 2);
        bus.accesses.clear();
        let outcome = cpu.step(&mut bus);
        if mode == TrapMode::Stop {
            assert_eq!(outcome, Err(Stop::Trap { cause: cause::INSN_ACCESS, pc: CODE + 65534, tval: CODE + 65536 }), "second-half fault reports failing address");
        } else {
            assert_eq!(outcome, Ok(()), "second-half fault enters handler");
            assert_eq!((cpu.mepc, cpu.mcause, cpu.mtval, cpu.pc),
                (CODE + 65534, cause::INSN_ACCESS, CODE + 65536, CODE + 0x300), "second-half fault keeps beginning in mepc");
        }
        assert_eq!(bus.accesses.len(), 2, "second-half fault stops after two fetch attempts");
        assert_eq!((cpu.instret, cpu.x[1]), (0, 0), "partial fetch neither executes nor retires");
        cpu = Cpu::new(CODE + 65534);
        put(&mut bus.mem, ram, 65534, ci(0, 0, 0, 1), 2);
        bus.accesses.clear();
        assert_eq!(cpu.step(&mut bus), Ok(()), "compressed final halfword needs no second read");
        assert_eq!(bus.accesses.len(), 1, "compressed fetch reads exactly one halfword");
    }
}

#[test]
fn alignment_faults_precede_data_bus_access() {
    let (mem, ram, _) = memory();
    let mut bus = Traced { mem, accesses: Vec::new() };
    for mode in [TrapMode::Stop, TrapMode::Take] {
        for word in [i(3, 1, 0, 1, 0), i(3, 2, 31, 1, 0), s(1, 1, 15, 0), s(2, 1, 15, 0)] {
            let mut cpu = Cpu::new(CODE); cpu.traps = mode;
            cpu.x[1] = 0x80000001; // Misaligned AND unmapped: alignment wins.
            put(&mut bus.mem, ram, 0, word, 4);
            bus.accesses.clear();
            let outcome = cpu.step(&mut bus);
            let reason = if word & 0x7f == 3 { cause::LOAD_MISALIGNED } else { cause::STORE_MISALIGNED };
            if mode == TrapMode::Stop {
                assert_eq!(outcome, Err(Stop::Trap { cause: reason, pc: CODE, tval: 0x80000001 }), "misalignment precedes unmapped access fault");
            } else {
                assert_eq!(outcome, Ok(()), "misalignment takes trap");
                assert_eq!((cpu.mcause, cpu.mtval), (reason, 0x80000001), "Take mode alignment priority");
            }
            assert_eq!(bus.accesses.len(), 2, "misaligned access only fetches instruction halves");
            assert!(bus.accesses.iter().all(|a| a.kind == Kind::Fetch), "no data bus access on misalignment");
        }
    }
    let mut cpu = Cpu::new(CODE + 1);
    bus.accesses.clear();
    assert_eq!(cpu.step(&mut bus), Err(Stop::Trap { cause: cause::INSN_MISALIGNED, pc: CODE + 1, tval: CODE + 1 }), "external odd pc trap");
    assert!(bus.accesses.is_empty(), "odd pc rejected before bus access");
}

#[test]
fn csr_reset_and_warl_masks() {
    let (mut mem, ram, _) = memory();
    let mut cpu = Cpu::new(CODE);
    assert_eq!(state(&cpu), ([0; 32], CODE, 0, [0, 1, 0, 0, 0, 0], TrapMode::Stop), "C3 reset values");
    cpu.x[1] = u32::MAX;
    for (csr, reset, expected) in [(0x300, 0, 0x1888), (0x305, 1, 0xffffff01),
        (0x340, 0, u32::MAX), (0x341, 0, 0xfffffffe), (0x342, 0, 0x8000001f), (0x343, 0, u32::MAX)] {
        step_word(&mut cpu, &mut mem, ram, i(0x73, 1, 15, 1, csr));
        assert_eq!(cpu.x[15], reset, "CSRRW returns pre-write CSR {csr:03x}");
        step_word(&mut cpu, &mut mem, ram, i(0x73, 2, 31, 0, csr));
        assert_eq!(cpu.x[31], expected, "all-ones CSR WARL readback {csr:03x}");
    }
    assert_eq!([cpu.mstatus, cpu.mtvec, cpu.mscratch, cpu.mepc, cpu.mcause, cpu.mtval],
        [0x1888, 0xffffff01, u32::MAX, 0xfffffffe, 0x8000001f, u32::MAX], "CSR storage holds masked values");
    for (value, expected) in [(0, 0), (0x800, 0x1800), (0x1000, 0), (0x1800, 0x1800)] {
        cpu.x[1] = value;
        step_word(&mut cpu, &mut mem, ram, i(0x73, 1, 0, 1, 0x300));
        step_word(&mut cpu, &mut mem, ram, i(0x73, 2, 31, 0, 0x300));
        assert_eq!(cpu.x[31], expected, "C3 MPP upper bit follows lower bit for input {value:04x}");
    }
    cpu.x[1] = 3;
    step_word(&mut cpu, &mut mem, ram, i(0x73, 1, 0, 1, 0x341));
    assert_eq!(cpu.mepc, 2, "mepc retains bit 1 with IALIGN=16");
    cpu.x[1] = 0xff;
    step_word(&mut cpu, &mut mem, ram, i(0x73, 1, 0, 1, 0x305));
    assert_eq!(cpu.mtvec, 1, "mtvec BASE low byte cleared and MODE forced to 1");
}

#[test]
fn csr_read_write_suppression_and_atomic_source() {
    let (mut mem, ram, _) = memory();
    let mut cpu = Cpu::new(CODE);
    for (f3, src, value, expected) in [
        (1, 1, 0x0f, 0x0f), (2, 1, 0x0f, 0xaf), (3, 1, 0x0f, 0xa0),
        (1, 0, 0, 0), (2, 0, 0, 0xa5), (3, 0, 0, 0xa5),
        (5, 15, 0xffff, 15), (6, 15, 0xffff, 0xaf), (7, 15, 0xffff, 0xa0),
        (5, 0, 0, 0), (6, 0, 0, 0xa5), (7, 0, 0, 0xa5),
    ] {
        for rd in [0, 31] {
            cpu.mscratch = 0xa5; cpu.x[src as usize] = value;
            step_word(&mut cpu, &mut mem, ram, i(0x73, f3, rd, src, 0x340));
            assert_eq!(cpu.mscratch, expected, "CSR f3={f3} src={src} rd={rd} update/suppression");
            if rd != 0 { assert_eq!(cpu.x[rd as usize], 0xa5, "CSR instruction returns old value"); }
            assert_eq!(cpu.x[0], 0, "CSR rd=0 discards result");
        }
    }
    cpu.mscratch = 0x123; cpu.x[1] = 0x456;
    step_word(&mut cpu, &mut mem, ram, i(0x73, 1, 1, 1, 0x340));
    assert_eq!((cpu.x[1], cpu.mscratch), (0x123, 0x456), "CSRRW rd=rs1 swaps using original source");
    for f3 in [1, 2, 3, 5, 6, 7] { for src in [0, 1] { for rd in [0, 31] {
        cpu.pc = CODE; cpu.x[1] = 0; // Nonzero register number, zero value.
        let word = i(0x73, f3, rd, src, 0xf14);
        put(&mut mem, ram, 0, word, 4);
        let before = state(&cpu);
        let outcome = cpu.step(&mut mem);
        if [2, 3, 6, 7].contains(&f3) && src == 0 {
            assert_eq!(outcome, Ok(()), "CSR read-only mhartid legal when write suppressed");
            assert_eq!(cpu.x[rd as usize], 0, "mhartid reads zero");
        } else {
            assert_eq!(outcome, Err(Stop::Trap { cause: cause::ILLEGAL, pc: CODE, tval: word }), "CSR write to mhartid illegal even with zero source value");
            assert_eq!(state(&cpu), before, "read-only violation does not modify destination or CSRs");
        }
    } } }
    for csr in [0x301, 0x304, 0x344, 0x7ff, 0xb02, 0xc00, 0xc02] {
        for f3 in [1, 2, 3, 5, 6, 7] {
            cpu.pc = CODE;
            let word = i(0x73, f3, 0, 0, csr);
            put(&mut mem, ram, 0, word, 4);
            let before = state(&cpu);
            assert_eq!(cpu.step(&mut mem), Err(Stop::Trap { cause: cause::ILLEGAL, pc: CODE, tval: word }), "unimplemented CSR {csr:03x} remains illegal with suppressed read or write");
            assert_eq!(state(&cpu), before, "unknown CSR changes no state");
        }
    }
}

#[test]
fn memory_permissions_bounds_offsets_and_aliases() {
    let (mut mem, ram, _) = memory();
    assert!(mem.bytes(ram).iter().all(|&v| v == 0), "new stores are zero-filled");
    assert_eq!(mem.write(DATA, Width::W, 0x12345678, 0), Ok(()), "data mapping is writable");
    assert_eq!(mem.read(CODE, Width::W, Kind::Fetch, 0), Ok(0x12345678), "instruction alias sees data write");
    assert_eq!(mem.read(DATA, Width::H, Kind::Load, 0), Ok(0x5678), "halfword read zero-extends");
    assert_eq!(mem.read(DATA + 1, Width::B, Kind::Load, 0), Ok(0x56), "byte read at interior address");
    assert_eq!(mem.read(DATA, Width::B, Kind::Fetch, 0), Err(()), "data mapping lacks exec");
    assert_eq!(mem.write(CODE, Width::B, 0xff, 0), Err(()), "instruction alias lacks write");
    assert_eq!(mem.write(FLASH, Width::B, 0xff, 0), Err(()), "flash lacks write");
    assert_eq!(mem.read(DATA + 65535, Width::H, Kind::Load, 0), Err(()), "read cannot cross mapping end");
    assert_eq!(mem.write(DATA + 65535, Width::W, u32::MAX, 0), Err(()), "write cannot cross mapping end");
    assert_eq!(mem.bytes(ram)[65535], 0, "crossing write is atomic");
    assert_eq!(mem.read(0x80000000, Width::B, Kind::Load, 0), Err(()), "unmapped read faults");
    let exec_only = Perm { read: false, write: false, exec: true };
    mem.map(0x1000, ram, 1, 2, exec_only);
    mem.map(0x1002, ram, 3, 2, exec_only);
    assert_eq!(mem.read(0x1000, Width::H, Kind::Fetch, 0), Ok(0x3456), "nonzero store offset honored");
    assert_eq!(mem.read(0x1000, Width::H, Kind::Load, 0), Err(()), "execute permission does not imply read");
    assert_eq!(mem.read(0x1000, Width::W, Kind::Fetch, 0), Err(()), "access cannot span adjacent mappings");
    mem.map(u32::MAX, ram, 0, 1, RW);
    assert_eq!(mem.read(u32::MAX, Width::B, Kind::Load, 0), Ok(0x78), "last 32-bit address can be mapped");
    assert_eq!(mem.read(u32::MAX, Width::H, Kind::Load, 0), Err(()), "memory accesses do not wrap at address-space end");
    // Empty maps consume no address space, including inside another mapping.
    mem.map(DATA, ram, 65536, 0, RW);
}

#[test]
fn invalid_mappings_panic() {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let (mut mem, ram, _) = memory();
    for (base, offset, len, property) in [
        (DATA, 0, 1, "overlapping mapping"),
        (DATA - 1, 0, 2, "overlap from below"),
        (0, 65535, 2, "range past store"),
        (0, usize::MAX, 1, "offset overflow"),
        (0, 1, usize::MAX, "length overflow"),
        (u32::MAX, 0, 2, "mapping beyond 32-bit address space"),
    ] {
        assert!(catch_unwind(AssertUnwindSafe(|| mem.map(base, ram, offset, len, RW))).is_err(), "{property} must panic");
    }
    assert!(catch_unwind(AssertUnwindSafe(|| mem.map(0, usize::MAX, 0, 0, RW))).is_err(), "invalid store index must panic");
}

#[test]
fn store_generated_instruction_through_data_alias_and_execute() {
    let (mut mem, ram, _) = memory();
    let program = [s(2, 1, 15, 0), i(0x0f, 1, 0, 0, 0), i(0x67, 0, 0, 2, 0)];
    for (n, word) in program.into_iter().enumerate() { put(&mut mem, ram, n * 4, word, 4); }
    put(&mut mem, ram, 0x104, i(0x73, 0, 0, 0, 1), 4);
    let mut cpu = Cpu::new(CODE);
    cpu.x[1] = DATA + 0x100;
    cpu.x[2] = CODE + 0x100;
    cpu.x[15] = i(0x13, 0, 31, 0, 42);
    assert_eq!(cpu.run(&mut mem, 10), Stop::Ebreak { pc: CODE + 0x104 }, "generated code reaches EBREAK through instruction alias");
    assert_eq!(cpu.x[31], 42, "instruction stored through data alias executes");
    assert_eq!(cpu.instret, 4, "store, fence, jump, generated instruction retire; EBREAK does not");
}

#[test]
fn mixed_compressed_program_and_fences() {
    let (mut mem, ram, _) = memory();
    // C.LI x8,3; C.ADDI x8,-1; C.BNEZ x8,-2; C.SWSP x8,0;
    // C.LWSP x9,0; C.ADDI4SPN x10,4; C.SLLI x9,0; C.ADDI x9,0;
    // FENCE with ignored fields; FENCE.I with ignored fields; WFI.
    let halves = [ci(2, 8, 3, 1), ci(0, 8, -1, 1), cb_branch(7, 8, -2),
        css(6, 8, 0), ci(2, 9, 0, 2), ciw(10, 4), ci(0, 9, 0, 2), ci(0, 9, 0, 1)];
    for (n, half) in halves.into_iter().enumerate() { put(&mut mem, ram, n * 2, half, 2); }
    for (n, word) in [i(0x0f, 0, 31, 15, -1), i(0x0f, 1, 31, 15, -1), i(0x73, 0, 0, 0, 0x105)].into_iter().enumerate() {
        put(&mut mem, ram, 16 + n * 4, word, 4);
    }
    let mut cpu = Cpu::new(CODE); cpu.x[2] = DATA + 0x100; cpu.x[31] = 0x55;
    assert_eq!(cpu.run(&mut mem, 30), Stop::Wfi { pc: CODE + 28 }, "compressed loop falls through to retired WFI");
    assert_eq!((cpu.x[8], cpu.x[9], cpu.x[10], cpu.x[31]), (0, 0, DATA + 0x104, 0x55), "compressed arithmetic/memory/HINT expansions execute, fences do not write rd");
    assert_eq!(cpu.instret, 15, "compressed loop and WFI each count one retired instruction");
}

#[test]
fn run_limits_retirement_wrap_and_stop_precedence() {
    let (mut mem, ram, _) = memory();
    put(&mut mem, ram, 0, j(0, 0), 4);
    let mut cpu = Cpu::new(CODE);
    assert_eq!(cpu.run(&mut mem, 0), Stop::Limit, "zero budget stops immediately");
    assert_eq!((cpu.pc, cpu.instret), (CODE, 0), "zero budget performs no instruction");
    assert_eq!(cpu.run(&mut mem, 37), Stop::Limit, "loop stops at retirement limit");
    assert_eq!((cpu.pc, cpu.instret), (CODE, 37), "exactly max instructions retire");
    assert_eq!(cpu.run(&mut mem, 3), Stop::Limit, "budget relative to run entry");
    assert_eq!(cpu.instret, 40, "successive budgets accumulate");
    cpu.instret = u64::MAX - 1;
    assert_eq!(cpu.run(&mut mem, 3), Stop::Limit, "run limit survives counter wrap");
    assert_eq!(cpu.instret, 1, "retired counter wraps modulo 2^64");
    for mode in [TrapMode::Stop, TrapMode::Take] {
        cpu = Cpu::new(CODE); cpu.traps = mode;
        put(&mut mem, ram, 0, i(0x73, 0, 0, 0, 0x105), 4);
        assert_eq!(cpu.run(&mut mem, 1), Stop::Wfi { pc: CODE + 4 }, "WFI stop takes precedence when it retires last budget instruction");
        assert_eq!((cpu.pc, cpu.instret), (CODE + 4, 1), "WFI completes in either trap mode");
    }
    cpu = Cpu::new(CODE); cpu.traps = TrapMode::Take; cpu.mtvec = CODE + 0x101;
    put(&mut mem, ram, 0, i(0x73, 0, 0, 0, 0), 4);
    put(&mut mem, ram, 0x100, i(0x13, 0, 31, 0, 7), 4);
    assert_eq!(cpu.run(&mut mem, 1), Stop::Limit, "trap entry does not consume retirement budget");
    assert_eq!((cpu.pc, cpu.instret, cpu.x[31]), (CODE + 0x104, 1, 7), "handler instruction alone consumes budget");
}

#[test]
fn address_arithmetic_wraps_at_32_bits() {
    let (mut mem, ram, _) = memory();
    // Additional aliases expose the address boundary without a giant store.
    mem.map(0xfffffffe, ram, 0x200, 2, RX);
    mem.map(0, ram, 0x202, 16, Perm { read: true, write: true, exec: true });
    let word = i(0x13, 0, 1, 0, 7);
    put(&mut mem, ram, 0x200, word, 4);
    let mut cpu = Cpu::new(0xfffffffe);
    assert_eq!(cpu.step(&mut mem), Ok(()), "second instruction half wraps from MAX-1 to zero");
    assert_eq!((cpu.pc, cpu.x[1]), (2, 7), "sequential pc addition wraps");
    cpu.x[1] = u32::MAX; cpu.x[15] = 0x12345678;
    step_word(&mut cpu, &mut mem, ram, s(2, 1, 15, 1));
    step_word(&mut cpu, &mut mem, ram, i(3, 2, 31, 1, 1));
    assert_eq!(cpu.x[31], 0x12345678, "effective address addition wraps before alignment");
    put(&mut mem, ram, 0x200, cj(1, 4), 2);
    cpu.pc = 0xfffffffe;
    assert_eq!(cpu.step(&mut mem), Ok(()), "compressed JAL wraps target and link");
    assert_eq!((cpu.pc, cpu.x[1]), (2, 0), "jump target and pc+2 link wrap independently");
}
