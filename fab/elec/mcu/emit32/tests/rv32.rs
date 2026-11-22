//! Expectations come from the RISC-V Instruction Set Manual: RV32I integer,
//! control-transfer and memory instructions; M; Zicsr; and Zca's RV32 compressed
//! expansions. They do not use the encoder's field packing or immediate helpers.

use fab_elec_mcu_emit32::asm::{assemble, AsmBase, Pass};
use fab_elec_mcu_emit32::rv32::{enc, reg::*, Asm, Reg};
use fab_elec_mcu_rv32::insn::{decode, BranchOp, CsrOp, ImmOp, Insn, LoadOp, RegOp, StoreOp};
use fab_elec_mcu_rv32::sim::{Cpu, Memory, Perm, Stop, TrapMode};

const REGS: [Reg; 4] = [0, 1, 15, 31];
const CREGS: [Reg; 5] = [0, 1, 8, 15, 31];
const WINDOW: [Reg; 2] = [8, 15];
const I12: [i32; 6] = [-2048, 2047, -1, 0, 1, 0x555];
const I6: [i32; 6] = [-32, 31, -1, 0, 1, 0x15];
const SHIFT: [i32; 4] = [0, 1, 31, 0x15];

fn instruction(name: &str, bits: u32, expected: Insn, length: u32) {
    assert_eq!(decode(bits), Ok((expected, length)),
        "{name}: manual instruction expansion and length, bits={bits:#010x}");
}

#[test]
fn integer_register_encodings() {
    let cases: &[(&str, fn(Reg, Reg, Reg) -> u32, RegOp)] = &[
        ("add", enc::add, RegOp::Add), ("sub", enc::sub, RegOp::Sub),
        ("sll", enc::sll, RegOp::Sll), ("slt", enc::slt, RegOp::Slt),
        ("sltu", enc::sltu, RegOp::Sltu), ("xor", enc::xor, RegOp::Xor),
        ("srl", enc::srl, RegOp::Srl), ("sra", enc::sra, RegOp::Sra),
        ("or", enc::or, RegOp::Or), ("and", enc::and, RegOp::And),
        ("mul", enc::mul, RegOp::Mul), ("mulh", enc::mulh, RegOp::Mulh),
        ("mulhsu", enc::mulhsu, RegOp::Mulhsu), ("mulhu", enc::mulhu, RegOp::Mulhu),
        ("div", enc::div, RegOp::Div), ("divu", enc::divu, RegOp::Divu),
        ("rem", enc::rem, RegOp::Rem), ("remu", enc::remu, RegOp::Remu),
    ];
    for &(name, encode, op) in cases {
        for rd in REGS { for rs1 in REGS { for rs2 in REGS {
            instruction(name, encode(rd, rs1, rs2), Insn::Op { op, rd, rs1, rs2 }, 4);
        } } }
    }
}

#[test]
fn integer_immediate_encodings() {
    let cases: &[(&str, fn(Reg, Reg, i32) -> u32, ImmOp, &[i32])] = &[
        ("addi", enc::addi, ImmOp::Addi, &I12),
        ("slti", enc::slti, ImmOp::Slti, &I12),
        ("sltiu", enc::sltiu, ImmOp::Sltiu, &I12),
        ("xori", enc::xori, ImmOp::Xori, &I12),
        ("ori", enc::ori, ImmOp::Ori, &I12),
        ("andi", enc::andi, ImmOp::Andi, &I12),
        ("slli", enc::slli, ImmOp::Slli, &SHIFT),
        ("srli", enc::srli, ImmOp::Srli, &SHIFT),
        ("srai", enc::srai, ImmOp::Srai, &SHIFT),
    ];
    for &(name, encode, op, immediates) in cases {
        for rd in REGS { for rs1 in REGS { for &imm in immediates {
            instruction(name, encode(rd, rs1, imm), Insn::OpImm { op, rd, rs1, imm }, 4);
        } } }
    }
    for rd in REGS {
        // U-immediates are the actual 32-bit values, not a 20-bit field.
        for imm in [0x80000000u32, 0x7ffff000, 0xfffff000, 0, 0x1000, 0xaaaaa000] {
            instruction("lui", enc::lui(rd, imm as i32), Insn::Lui { rd, imm }, 4);
            instruction("auipc", enc::auipc(rd, imm as i32), Insn::Auipc { rd, imm }, 4);
        }
    }
}

#[test]
fn control_flow_encodings() {
    let cases: &[(&str, fn(Reg, Reg, i32) -> u32, BranchOp)] = &[
        ("beq", enc::beq, BranchOp::Beq), ("bne", enc::bne, BranchOp::Bne),
        ("blt", enc::blt, BranchOp::Blt), ("bge", enc::bge, BranchOp::Bge),
        ("bltu", enc::bltu, BranchOp::Bltu), ("bgeu", enc::bgeu, BranchOp::Bgeu),
    ];
    for &(name, encode, op) in cases {
        for rs1 in REGS { for rs2 in REGS {
            for offset in [-4096, 4094, -2, 0, 2, 0xaaa] {
                instruction(name, encode(rs1, rs2, offset),
                    Insn::Branch { op, rs1, rs2, offset }, 4);
            }
        } }
    }
    for rd in REGS {
        for offset in [-1048576, 1048574, -2, 0, 2, 0xaaaaa] {
            instruction("jal", enc::jal(rd, offset), Insn::Jal { rd, offset }, 4);
        }
        for rs1 in REGS { for offset in I12 {
            // JALR has byte granularity, so odd immediates are legal.
            instruction("jalr", enc::jalr(rd, rs1, offset),
                Insn::Jalr { rd, rs1, offset }, 4);
        } }
    }
    instruction("ecall", enc::ecall(), Insn::Ecall, 4);
    instruction("ebreak", enc::ebreak(), Insn::Ebreak, 4);
}

#[test]
fn memory_and_csr_encodings() {
    let loads: &[(&str, fn(Reg, Reg, i32) -> u32, LoadOp)] = &[
        ("lb", enc::lb, LoadOp::Lb), ("lh", enc::lh, LoadOp::Lh),
        ("lw", enc::lw, LoadOp::Lw), ("lbu", enc::lbu, LoadOp::Lbu),
        ("lhu", enc::lhu, LoadOp::Lhu),
    ];
    let stores: &[(&str, fn(Reg, Reg, i32) -> u32, StoreOp)] = &[
        ("sb", enc::sb, StoreOp::Sb), ("sh", enc::sh, StoreOp::Sh),
        ("sw", enc::sw, StoreOp::Sw),
    ];
    for r1 in REGS { for r2 in REGS { for offset in I12 {
        for &(name, encode, op) in loads {
            instruction(name, encode(r1, r2, offset),
                Insn::Load { op, rd: r1, rs1: r2, offset }, 4);
        }
        for &(name, encode, op) in stores {
            instruction(name, encode(r1, r2, offset),
                Insn::Store { op, rs1: r2, rs2: r1, offset }, 4);
        }
    } } }
    let regs: &[(&str, fn(Reg, Reg, i32) -> u32, CsrOp)] = &[
        ("csrrw", enc::csrrw, CsrOp::Rw), ("csrrs", enc::csrrs, CsrOp::Rs),
        ("csrrc", enc::csrrc, CsrOp::Rc),
    ];
    let imms: &[(&str, fn(Reg, u16, i32) -> u32, CsrOp)] = &[
        ("csrrwi", enc::csrrwi, CsrOp::Rwi), ("csrrsi", enc::csrrsi, CsrOp::Rsi),
        ("csrrci", enc::csrrci, CsrOp::Rci),
    ];
    for rd in REGS { for csr in [0u16, 1, 0xfff, 0xaaa] {
        for src in REGS { for &(name, encode, op) in regs {
            instruction(name, encode(rd, src, i32::from(csr)),
                Insn::Csr { op, rd, src, csr }, 4);
        } }
        for src in [0u8, 1, 31, 0x15] { for &(name, encode, op) in imms {
            instruction(name, encode(rd, csr, i32::from(src)),
                Insn::Csr { op, rd, src, csr }, 4);
        } }
    } }
}

#[test]
fn compressed_arithmetic_encodings() {
    for rd in CREGS {
        for imm in I6 {
            instruction("caddi", u32::from(enc::caddi(rd, imm)),
                Insn::OpImm { op: ImmOp::Addi, rd, rs1: rd, imm }, 2);
            instruction("cli", u32::from(enc::cli(rd, imm)),
                Insn::OpImm { op: ImmOp::Addi, rd, rs1: ZERO, imm }, 2);
        }
        for imm in SHIFT {
            instruction("cslli", u32::from(enc::cslli(rd, imm)),
                Insn::OpImm { op: ImmOp::Slli, rd, rs1: rd, imm }, 2);
        }
        for imm in [-131072, 126976, -4096, 4096, 0x15000] {
            instruction("clui", u32::from(enc::clui(rd, imm)),
                Insn::Lui { rd, imm: imm as u32 }, 2);
        }
        for rs2 in [1, 8, 15, 31] {
            instruction("cadd", u32::from(enc::cadd(rd, rs2)),
                Insn::Op { op: RegOp::Add, rd, rs1: rd, rs2 }, 2);
            instruction("cmv", u32::from(enc::cmv(rd, rs2)),
                Insn::Op { op: RegOp::Add, rd, rs1: ZERO, rs2 }, 2);
        }
    }
    let ops: &[(&str, fn(Reg, Reg) -> u16, RegOp)] = &[
        ("cand", enc::cand, RegOp::And), ("cor", enc::cor, RegOp::Or),
        ("csub", enc::csub, RegOp::Sub), ("cxor", enc::cxor, RegOp::Xor),
    ];
    for rd in WINDOW {
        for rs2 in WINDOW { for &(name, encode, op) in ops {
            instruction(name, u32::from(encode(rd, rs2)),
                Insn::Op { op, rd, rs1: rd, rs2 }, 2);
        } }
        for imm in I6 {
            instruction("candi", u32::from(enc::candi(rd, imm)),
                Insn::OpImm { op: ImmOp::Andi, rd, rs1: rd, imm }, 2);
        }
        for imm in SHIFT {
            instruction("csrai", u32::from(enc::csrai(rd, imm)),
                Insn::OpImm { op: ImmOp::Srai, rd, rs1: rd, imm }, 2);
            instruction("csrli", u32::from(enc::csrli(rd, imm)),
                Insn::OpImm { op: ImmOp::Srli, rd, rs1: rd, imm }, 2);
        }
        for imm in [4, 1020, 0x154, 0x2a8] {
            instruction("caddi4spn", u32::from(enc::caddi4spn(rd, imm)),
                Insn::OpImm { op: ImmOp::Addi, rd, rs1: SP, imm }, 2);
        }
    }
    instruction("cnop", u32::from(enc::cnop()),
        Insn::OpImm { op: ImmOp::Addi, rd: ZERO, rs1: ZERO, imm: 0 }, 2);
}

#[test]
fn compressed_memory_and_control_encodings() {
    for value in WINDOW { for base in WINDOW { for offset in [0, 4, 124, 0x54, 0x28] {
        instruction("clw", u32::from(enc::clw(value, base, offset)),
            Insn::Load { op: LoadOp::Lw, rd: value, rs1: base, offset }, 2);
        instruction("csw", u32::from(enc::csw(value, base, offset)),
            Insn::Store { op: StoreOp::Sw, rs1: base, rs2: value, offset }, 2);
    } } }
    for offset in [0, 4, 252, 0x54, 0xa8] {
        for rd in [1, 8, 15, 31] {
            instruction("clwsp", u32::from(enc::clwsp(rd, offset)),
                Insn::Load { op: LoadOp::Lw, rd, rs1: SP, offset }, 2);
        }
        for rs2 in CREGS {
            instruction("cswsp", u32::from(enc::cswsp(rs2, offset)),
                Insn::Store { op: StoreOp::Sw, rs1: SP, rs2, offset }, 2);
        }
    }
    for rs1 in WINDOW { for offset in [-256, 254, -2, 0, 2, 0xaa, -86] {
        instruction("cbeqz", u32::from(enc::cbeqz(rs1, offset)),
            Insn::Branch { op: BranchOp::Beq, rs1, rs2: ZERO, offset }, 2);
        instruction("cbnez", u32::from(enc::cbnez(rs1, offset)),
            Insn::Branch { op: BranchOp::Bne, rs1, rs2: ZERO, offset }, 2);
    } }
    for offset in [-2048, 2046, -2, 0, 2, 0x2aa, -1366] {
        instruction("cj", u32::from(enc::cj(offset)), Insn::Jal { rd: ZERO, offset }, 2);
        instruction("cjal", u32::from(enc::cjal(offset)), Insn::Jal { rd: RA, offset }, 2);
    }
    for rs1 in [1, 8, 15, 31] {
        instruction("cjr", u32::from(enc::cjr(rs1)),
            Insn::Jalr { rd: ZERO, rs1, offset: 0 }, 2);
        instruction("cjalr", u32::from(enc::cjalr(rs1)),
            Insn::Jalr { rd: RA, rs1, offset: 0 }, 2);
    }
    instruction("cebreak", u32::from(enc::cebreak()), Insn::Ebreak, 2);
}

#[test]
fn operand_checks_reject_lossy_or_reserved_encodings() {
    let cases: &[(&str, fn())] = &[
        ("beq: odd offset", || { enc::beq(1, 15, 3); }),
        ("beq: signed offset range", || { enc::beq(1, 15, 4096); }),
        ("clw: word alignment", || { enc::clw(8, 15, 2); }),
        ("clw: unsigned offset range", || { enc::clw(8, 15, -4); }),
        ("cand: compressed window", || { enc::cand(8, 16); }),
        ("addi: register range", || { enc::addi(32, 1, 0); }),
        ("addi: signed immediate range", || { enc::addi(1, 1, 2048); }),
        ("slli: RV32 shift range", || { enc::slli(1, 1, 32); }),
        ("lui: upper immediate alignment", || { enc::lui(1, 1); }),
        ("csrrw: CSR address range", || { enc::csrrw(1, 1, 4096); }),
        ("csrrwi: immediate range", || { enc::csrrwi(1, 0, 32); }),
        ("caddi4spn: nonzero immediate", || { enc::caddi4spn(8, 0); }),
        ("clui: nonzero immediate", || { enc::clui(1, 0); }),
        ("clui: SP selects a different instruction", || { enc::clui(SP, 4096); }),
        ("clwsp: reserved zero destination", || { enc::clwsp(ZERO, 0); }),
        ("cjr: reserved zero source", || { enc::cjr(ZERO); }),
        ("cmv: zero source selects a different instruction", || { enc::cmv(1, ZERO); }),
    ];
    for &(property, f) in cases {
        assert!(std::panic::catch_unwind(f).is_err(), "{property} must panic");
    }
}

const RAM: u32 = 0x3fc80000;
const CODE: u32 = 0x40380000;
const DATA: usize = 0x8000;

fn memory(code: &[u8]) -> (Memory, usize) {
    let mut mem = Memory::new();
    let store = mem.store(0x10000);
    mem.bytes_mut(store)[..code.len()].copy_from_slice(code);
    mem.map(RAM, store, 0, 0x10000, Perm { read: true, write: true, exec: false });
    mem.map(CODE, store, 0, 0x10000, Perm { read: true, write: false, exec: true });
    (mem, store)
}

fn run(cpu: &mut Cpu, mem: &mut Memory, expected_break: u32, property: &str) {
    cpu.traps = TrapMode::Stop;
    match cpu.run(mem, 64) {
        Stop::Ebreak { pc } => assert_eq!(pc, expected_break, "{property}: reached intended ebreak"),
        _ => panic!("{property}: program did not stop at ebreak (pc={:#x})", cpu.pc),
    }
}

fn program(f: impl FnMut(&mut Asm)) -> Vec<u8> {
    assemble(&mut Asm::new(0, T6), f)
}

#[test]
fn immediate_helpers_execute_all_rv32_bit_patterns() {
    let values = [0u32, 1, u32::MAX, 31, (-32i32) as u32, 2047, 2048,
        (-2048i32) as u32, (-2049i32) as u32, 0x7fffffff, 0x80000000,
        0x12345678, 0xfffff800, 0x00000800, 0x7ffff800];
    for value in values {
        let code = program(|a| {
            a.emit_optimised_load_immediate(A0, value as i32);
            a.ebreak();
        });
        let (mut mem, _) = memory(&code);
        let mut cpu = Cpu::new(CODE);
        cpu.x[A0 as usize] = 0xdeadbeef;
        run(&mut cpu, &mut mem, CODE + code.len() as u32 - 4, "load immediate");
        assert_eq!(cpu.x[A0 as usize], value, "load immediate preserves {value:#010x}");
    }
    // Preserve MicroPython's form choices, including its unsigned lower part
    // and conservative signed-17-bit upper test, rather than re-optimising them.
    for (value, bytes) in [(31, 2), (32, 4), (4097, 4), (4095, 6), (0x10000, 4)] {
        let code = program(|a| a.emit_optimised_load_immediate(A0, value));
        assert_eq!(code.len(), bytes, "load immediate {value:#x}: MicroPython form size");
    }
}

#[derive(Clone, Copy, Debug)]
enum Jump { Always, Equal, Nonzero(Reg) }

fn jump(a: &mut Asm, kind: Jump, label: usize) {
    match kind {
        Jump::Always => a.emit_jump(label),
        Jump::Equal => a.emit_jump_if_reg_eq(A0, A1, label),
        Jump::Nonzero(rs) => a.emit_jump_if_reg_nonzero(rs, label),
    }
}

#[test]
fn backward_jumps_preserve_reach_and_take_both_paths() {
    let cases = [
        (Jump::Always, 16, 2), (Jump::Always, 2048, 2), (Jump::Always, 2050, 8),
        (Jump::Equal, 16, 4), (Jump::Equal, 4096, 4), (Jump::Equal, 4098, 12),
        (Jump::Nonzero(A0), 16, 2), (Jump::Nonzero(A0), 128, 2),
        (Jump::Nonzero(A0), 130, 4), (Jump::Nonzero(A0), 256, 4),
        (Jump::Nonzero(A0), 258, 4), (Jump::Nonzero(A0), 4096, 4),
        (Jump::Nonzero(A0), 4098, 10),
        (Jump::Nonzero(T0), 16, 4), (Jump::Nonzero(T0), 4098, 12),
    ];
    for (kind, distance, size) in cases {
        let mut a = Asm::new(1, T6);
        let code = assemble(&mut a, |a| {
            a.base.label_assign(0);
            a.addi(A2, ZERO, 1); // Taken-path marker.
            a.ebreak();
            while a.base.offset() < distance { a.cnop(); }
            let start = a.base.offset();
            jump(a, kind, 0);
            assert_eq!(a.base.offset() - start, size, "{kind:?} at -{distance}: source form size");
            a.addi(A2, ZERO, 2); // Fallthrough marker.
            a.ebreak();
        });
        for taken in [false, true] {
            if matches!(kind, Jump::Always) && !taken { continue; }
            let (mut mem, _) = memory(&code);
            let mut cpu = Cpu::new(CODE + distance);
            cpu.x[A0 as usize] = 7;
            cpu.x[A1 as usize] = if taken { 7 } else { 8 };
            if let Jump::Nonzero(rs) = kind { cpu.x[rs as usize] = u32::from(taken); }
            let expected_break = if taken { CODE + 4 } else { CODE + distance + size + 4 };
            run(&mut cpu, &mut mem, expected_break, "backward jump");
            assert_eq!(cpu.x[A2 as usize], if taken { 1 } else { 2 },
                "{kind:?} at -{distance}: path marker for taken={taken}");
        }
    }
}

#[test]
fn forward_jumps_use_long_forms_in_both_passes() {
    for (kind, size) in [(Jump::Always, 8), (Jump::Equal, 12),
        (Jump::Nonzero(A0), 12), (Jump::Nonzero(T0), 12)] {
        let mut sizes = Vec::new();
        let code = assemble(&mut Asm::new(1, T6), |a| {
            jump(a, kind, 0);
            assert_eq!(a.base.offset(), size, "{kind:?}: forward form remains long");
            a.addi(A2, ZERO, 2);
            a.ebreak();
            a.base.label_assign(0);
            a.addi(A2, ZERO, 1);
            a.ebreak();
            sizes.push(a.base.offset());
        });
        assert_eq!(sizes, [size + 16, size + 16], "{kind:?}: Compute and Emit sizes agree");
        for taken in [false, true] {
            if matches!(kind, Jump::Always) && !taken { continue; }
            let (mut mem, _) = memory(&code);
            let mut cpu = Cpu::new(CODE);
            cpu.x[A0 as usize] = 7;
            cpu.x[A1 as usize] = if taken { 7 } else { 8 };
            if let Jump::Nonzero(rs) = kind { cpu.x[rs as usize] = u32::from(taken); }
            let end = CODE + size + if taken { 12 } else { 4 };
            run(&mut cpu, &mut mem, end, "forward jump");
            assert_eq!(cpu.x[A2 as usize], if taken { 1 } else { 2 },
                "{kind:?}: forward path marker for taken={taken}");
        }
    }
}

#[test]
fn memory_helpers_scale_offsets_and_zero_extend() {
    const VALUE: u32 = 0x89abcdef;
    for size in 0..=2 {
        let width = 1usize << size;
        for offset in [0i32, 3, 15, 16, -1, 2044 >> size, -2048 >> size,
            2048 >> size, 4096 >> size, -4096 >> size] {
            let target = (DATA as i32 + (offset << size)) as usize;
            let code = program(|a| {
                a.emit_store_reg_reg_offset(A0, A1, offset, size);
                a.emit_load_reg_reg_offset(A2, A1, offset, size);
                a.ebreak();
            });
            let (mut mem, store) = memory(&code);
            mem.bytes_mut(store)[target - 1..target + width + 1].fill(0x55);
            let mut cpu = Cpu::new(CODE);
            cpu.x[A0 as usize] = VALUE;
            cpu.x[A1 as usize] = RAM + DATA as u32;
            run(&mut cpu, &mut mem, CODE + code.len() as u32 - 4, "offset memory helpers");
            assert_eq!(&mem.bytes(store)[target..target + width], &VALUE.to_le_bytes()[..width],
                "store: correct little-endian bytes for size={size}, offset={offset}");
            assert_eq!(mem.bytes(store)[target - 1], 0x55, "store preserves preceding byte");
            assert_eq!(mem.bytes(store)[target + width], 0x55, "store preserves following byte");
            let expected = match size { 0 => 0xef, 1 => 0xcdef, _ => VALUE };
            assert_eq!(cpu.x[A2 as usize], expected,
                "load: zero extension and scaling for size={size}, offset={offset}");
            assert_eq!(cpu.x[A1 as usize], RAM + DATA as u32, "memory helpers preserve base");
            assert_eq!(cpu.x[A0 as usize], VALUE, "memory helpers preserve stored value");
        }
    }
    for (offset, length) in [(15, 2), (16, 4)] {
        let code = program(|a| a.emit_load_reg_reg_offset(A0, A1, offset, 2));
        assert_eq!(code.len(), length, "word load offset {offset}: MicroPython compressed cutoff");
        let code = program(|a| a.emit_store_reg_reg_offset(A0, A1, offset, 2));
        assert_eq!(code.len(), length, "word store offset {offset}: MicroPython compressed cutoff");
    }
}

#[test]
fn register_indexed_memory_helpers_execute_without_zba() {
    for size in 0..=2 {
        for temp in [S0, T6] {
            for index in [3i32, -3] {
                let code = assemble(&mut Asm::new(0, temp), |a| {
                    a.emit_store_reg_reg_reg(A0, A1, A2, size);
                    a.emit_load_reg_reg_reg(A3, A1, A2, size);
                    a.ebreak();
                });
                let (mut mem, store) = memory(&code);
                let mut cpu = Cpu::new(CODE);
                cpu.x[A0 as usize] = 0xfedcba98;
                cpu.x[A1 as usize] = RAM + DATA as u32;
                cpu.x[A2 as usize] = index as u32;
                run(&mut cpu, &mut mem, CODE + code.len() as u32 - 4, "indexed memory helpers");
                let target = (DATA as i32 + (index << size)) as usize;
                let width = 1usize << size;
                assert_eq!(&mem.bytes(store)[target..target + width], &0xfedcba98u32.to_le_bytes()[..width],
                    "indexed store scales index={index}, size={size}, temp={temp}");
                let expected = match size { 0 => 0x98, 1 => 0xba98, _ => 0xfedcba98 };
                assert_eq!(cpu.x[A3 as usize], expected,
                    "indexed load scales and zero extends size={size}, temp={temp}");
                assert_eq!(cpu.x[A1 as usize], RAM + DATA as u32, "indexed helpers preserve base");
                assert_eq!(cpu.x[A2 as usize], index as u32, "indexed helpers preserve index");
            }
        }
    }
}

#[test]
fn pcrel_and_xor_helpers_execute() {
    let mut a = Asm::new(2, T6);
    let code = assemble(&mut a, |a| {
        a.base.label_assign(0);
        a.emit_mov_reg_pcrel(A0, 1);
        a.emit_mov_reg_pcrel(A1, 0);
        a.emit_optimised_xor(A2, A3);
        a.emit_optimised_xor(A4, A4);
        a.ebreak();
        // Exercise the sign-adjusted low half of a forward PC-relative value.
        while a.base.offset() < 0x805 { a.base.data(1, 0); }
        a.base.label_assign(1);
    });
    let (mut mem, _) = memory(&code);
    let mut cpu = Cpu::new(CODE);
    cpu.x[A2 as usize] = 0x12345678;
    cpu.x[A3 as usize] = 0xaaaaaaaa;
    cpu.x[A4 as usize] = u32::MAX;
    run(&mut cpu, &mut mem, CODE + 22, "PC-relative and xor helpers");
    assert_eq!(cpu.x[A0 as usize], CODE + 0x805, "PC-relative forward data label address");
    assert_eq!(cpu.x[A1 as usize], CODE, "PC-relative backward label address");
    assert_eq!(cpu.x[A2 as usize], 0xb89efcd2, "distinct-register optimised xor");
    assert_eq!(cpu.x[A4 as usize], 0, "same-register optimised xor clears register");
}

#[test]
fn comparisons_execute_signed_and_unsigned_corners() {
    let code = program(|a| {
        a.meta_comparison_eq(A0, A1, A2);
        a.meta_comparison_ne(A0, A1, A3);
        a.meta_comparison_lt(A0, A1, A4, false);
        a.meta_comparison_le(A0, A1, A5, false);
        a.meta_comparison_lt(A0, A1, A6, true);
        a.meta_comparison_le(A0, A1, A7, true);
        a.ebreak();
    });
    for lhs in [0u32, 1, 0x7fffffff, 0x80000000, u32::MAX] {
        for rhs in [0u32, 1, 0x7fffffff, 0x80000000, u32::MAX] {
            let (mut mem, _) = memory(&code);
            let mut cpu = Cpu::new(CODE);
            cpu.x[A0 as usize] = lhs;
            cpu.x[A1 as usize] = rhs;
            run(&mut cpu, &mut mem, CODE + code.len() as u32 - 4, "comparison helpers");
            for (register, expected, name) in [
                (A2, lhs == rhs, "eq"), (A3, lhs != rhs, "ne"),
                (A4, (lhs as i32) < (rhs as i32), "signed lt"),
                (A5, (lhs as i32) <= (rhs as i32), "signed le"),
                (A6, lhs < rhs, "unsigned lt"), (A7, lhs <= rhs, "unsigned le"),
            ] {
                assert_eq!(cpu.x[register as usize], u32::from(expected),
                    "{name}: boolean result for {lhs:#x}, {rhs:#x}");
            }
        }
    }
}

#[test]
fn two_pass_base_and_alias_guards() {
    let mut base = AsmBase::new(1);
    assert_eq!(base.label_offset(0), None, "new label is unknown");
    let bytes = assemble(&mut base, |a| {
        a.data(1, 0xaa);
        a.align(4);
        a.label_assign(0);
        a.data(4, 0x12345678);
        if a.pass() == Pass::Compute {
            assert!(a.code().is_empty(), "Compute advances without storing code");
        }
    });
    assert_eq!(bytes, [0xaa, 0, 0, 0, 0x78, 0x56, 0x34, 0x12], "alignment and little-endian data");
    assert_eq!(base.label_offset(0), Some(4), "label retains computed offset");
    let drift = std::panic::catch_unwind(|| {
        let mut base = AsmBase::new(1);
        base.start_pass(Pass::Compute);
        base.label_assign(0);
        base.write(&[0, 0]);
        base.start_pass(Pass::Emit);
        base.write(&[0, 0]);
        base.label_assign(0);
    });
    assert!(drift.is_err(), "label assigned at a different Emit offset panics");
    let short_emit = std::panic::catch_unwind(|| {
        assemble(&mut AsmBase::new(0), |a| {
            if a.pass() == Pass::Compute { a.write(&[0]); }
        });
    });
    assert!(short_emit.is_err(), "assemble rejects a shorter Emit pass");
    let long_emit = std::panic::catch_unwind(|| {
        assemble(&mut AsmBase::new(0), |a| {
            if a.pass() == Pass::Emit { a.write(&[0]); }
        });
    });
    assert!(long_emit.is_err(), "assemble rejects a longer Emit pass");
    let undefined = std::panic::catch_unwind(|| {
        assemble(&mut Asm::new(1, T6), |a| a.emit_jump(0));
    });
    assert!(undefined.is_err(), "undefined jump label is rejected in Emit");
    let aliases: &[(&str, fn(&mut Asm))] = &[
        ("large-offset store value", |a| a.emit_store_reg_reg_offset(T6, A0, 4096, 0)),
        ("large-offset store base", |a| a.emit_store_reg_reg_offset(A0, T6, 4096, 0)),
        ("indexed store value", |a| a.emit_store_reg_reg_reg(T6, A0, A1, 2)),
        ("indexed load base", |a| a.emit_load_reg_reg_reg(A0, T6, A1, 2)),
        ("indexed load index", |a| a.emit_load_reg_reg_reg(A0, A1, T6, 2)),
        ("large-offset load destination/base", |a| a.emit_load_reg_reg_offset(A0, A0, 4096, 0)),
        ("long conditional branch", |a| a.emit_jump_if_reg_eq(T6, A0, 0)),
    ];
    for &(name, f) in aliases {
        assert!(std::panic::catch_unwind(|| f(&mut Asm::new(1, T6))).is_err(),
            "{name}: destructive operand alias is rejected");
    }
}
