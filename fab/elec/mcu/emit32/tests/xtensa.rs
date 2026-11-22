//! Expectations are from the Cadence Xtensa ISA Summary: chapter 8 instruction
//! operations and chapter 9 formats/opcode maps (pp. 656–680). Fields below are
//! the ISA's r/s/t fields, including fixed opcode fields, not semantic operands.
//! CALL/BRI12/RI16/RI7 lack some of those fields; those are zero per the model API.
//! No decoder, disassembler or emulator source is used as an oracle.

use fab_elec_mcu_emit32::asm::{AsmBase, Pass};
use fab_elec_mcu_emit32::xtensa::{assemble, cc, ccz, enc, reg::*, Asm, Reg};
use fab_elec_mcu_emu::bus::{Memory, Perm};
use fab_elec_mcu_xtensa::insn::{decode, target, Insn, Op};
use fab_elec_mcu_xtensa::sim::{Config, Cpu, IntType, Stop, Unaligned, Vectors};

const REGS: [Reg; 3] = [0, 1, 15];
const I8: [i32; 6] = [-128, 127, -1, 0, 1, 0x55];
const I12: [i32; 6] = [-2048, 2047, -1, 0, 1, 0x555];
// Conditional branch offsets: -3..=0 are reserved (ISA Summary p. 62).
const B8: [i32; 6] = [-128, 127, -4, 1, 2, 0x55];
const B12: [i32; 6] = [-2048, 2047, -4, 1, 2, 0x555];
const I18: [i32; 6] = [-131072, 131071, -1, 0, 1, 0x15555];
const CONDITIONS: [u8; 12] = [
    cc::NONE, cc::EQ, cc::LT, cc::LTU, cc::ALL, cc::BC,
    cc::ANY, cc::NE, cc::GE, cc::GEU, cc::NALL, cc::BS,
];

fn insn(op: Op, r: u8, s: u8, t: u8, imm: i32) -> Insn { Insn { op, r, s, t, imm, bits: 0 } }

fn decoded(name: &str, bits: u32) -> (Insn, u32) {
    match decode(bits) {
        Ok(result) => result,
        Err(()) => panic!("{name}: instruction must decode, bits={bits:#08x}"),
    }
}

fn instruction(name: &str, bits: u32, expected: Insn, length: u32) {
    let (actual, actual_length) = decoded(name, bits);
    // The supplied API does not promise Debug/PartialEq/Copy on Op or Insn.
    assert_eq!(std::mem::discriminant(&actual.op), std::mem::discriminant(&expected.op),
        "{name}: ISA mnemonic, bits={bits:#08x}");
    assert_eq!((actual.r, actual.s, actual.t), (expected.r, expected.s, expected.t),
        "{name}: ISA r/s/t fields, bits={bits:#08x}");
    assert_eq!(actual.imm, expected.imm, "{name}: ISA expanded immediate, bits={bits:#08x}");
    assert_eq!(actual_length, length, "{name}: instruction length, bits={bits:#08x}");
}

#[test]
fn register_instruction_encodings() {
    // Chapter 8: ADDX2/4, AND/OR/XOR, MULL and SUB write AR[r] from AR[s], AR[t].
    let cases: &[(&str, fn(Reg, Reg, Reg) -> u32, fn() -> Op)] = &[
        ("addx2", enc::addx2, || Op::Addx2), ("addx4", enc::addx4, || Op::Addx4),
        ("and", enc::and, || Op::And), ("or", enc::or, || Op::Or),
        ("xor", enc::xor, || Op::Xor), ("mull", enc::mull, || Op::Mull),
        // ISA Summary pp. 539, 557–558, 562–563; RST2 map p. 667.
        ("muluh", enc::muluh, || Op::Muluh), ("quou", enc::quou, || Op::Quou),
        ("remu", enc::remu, || Op::Remu),
        ("sub", enc::sub, || Op::Sub),
        // MIN/MAX diagrams pp. 489–492 and RST3 p. 668.
        ("min", enc::min, || Op::Min), ("max", enc::max, || Op::Max),
        ("minu", enc::minu, || Op::Minu), ("maxu", enc::maxu, || Op::Maxu),
        ("salt", enc::salt, || Op::Salt),
    ];
    for &(name, encode, op) in cases {
        for rd in REGS { for rs in REGS { for rt in REGS {
            instruction(name, encode(rd, rs, rt), insn(op(), rd, rs, rt, 0), 3);
        } } }
    }
    for rd in REGS { for rs in REGS {
        for rt in REGS {
            instruction("add_n", enc::add_n(rd, rs, rt).into(), insn(Op::AddN, rd, rs, rt, 0), 2);
        }
        instruction("mov_n", enc::mov_n(rd, rs).into(), insn(Op::MovN, 0, rs, rd, 0), 2);
        instruction("neg", enc::neg(rd, rs), insn(Op::Neg, rd, 0, rs, 0), 3);
        instruction("sll", enc::sll(rd, rs), insn(Op::Sll, rd, rs, 0, 0), 3);
        instruction("srl", enc::srl(rd, rs), insn(Op::Srl, rd, 0, rs, 0), 3);
        instruction("sra", enc::sra(rd, rs), insn(Op::Sra, rd, 0, rs, 0), 3);
        for shift in [0u8, 1, 15, 16, 31] {
            instruction("srai", enc::srai(rd, rs, shift),
                insn(Op::Srai, rd, shift & 15, rs, i32::from(shift)), 3);
            if shift < 16 {
                instruction("srli", enc::srli(rd, rs, shift),
                    insn(Op::Srli, rd, shift, rs, i32::from(shift)), 3);
            }
            if shift != 0 {
                instruction("slli", enc::slli(rd, rs, shift),
                    insn(Op::Slli, rd, rs, (32 - shift) & 15, i32::from(32 - shift)), 3);
            }
        }
    } }
    for rs in REGS {
        instruction("ssl", enc::ssl(rs), insn(Op::Ssl, 1, rs, 0, 0), 3);
        instruction("ssr", enc::ssr(rs), insn(Op::Ssr, 0, rs, 0, 0), 3);
        instruction("callx0", enc::callx0(rs), insn(Op::Callx0, 0, rs, 0, 0), 3);
        instruction("jx", enc::jx(rs), insn(Op::Jx, 0, rs, 0, 0), 3);
    }
    instruction("ret_n", enc::ret_n().into(), insn(Op::RetN, 15, 0, 0, 0), 2);
}

#[test]
fn handler_instruction_round_trips() {
    // ISA Summary pp. 328–329, 431–432, 514–515, 522–523, 547–551,
    // 557, 562, 573–574, 628–629, 632–633, 646–647, 656, 667–671.
    let cases: &[(&str, fn(Reg, Reg, Reg) -> u32, Op)] = &[
        ("quos", enc::quos, Op::Quos), ("rems", enc::rems, Op::Rems),
        ("movnez", enc::movnez, Op::Movnez), ("movt", enc::movt, Op::Movt),
        ("add_s", enc::add_s, Op::AddS), ("sub_s", enc::sub_s, Op::SubS),
        ("mul_s", enc::mul_s, Op::MulS), ("oeq_s", enc::oeq_s, Op::OeqS),
        ("olt_s", enc::olt_s, Op::OltS), ("ole_s", enc::ole_s, Op::OleS),
        ("float_s", enc::float_s, Op::FloatS), ("trunc_s", enc::trunc_s, Op::TruncS),
    ];
    for &(name, encode, op) in cases {
        for r in REGS { for s in REGS { for t in REGS {
            let imm = if matches!(op, Op::FloatS | Op::TruncS) { i32::from(t) } else { 0 };
            instruction(name, encode(r, s, t), insn(op, r, s, t, imm), 3);
        } } }
    }
    for r in REGS { for s in REGS {
        instruction("rfr", enc::rfr(r, s), insn(Op::Rfr, r, s, 4, 0), 3);
        instruction("wfr", enc::wfr(r, s), insn(Op::Wfr, r, s, 5, 0), 3);
    } }
}

#[test]
fn windowed_instruction_round_trips() {
    // ISA Summary Tables 198–204 (pp. 661–663), 207 (p. 663),
    // 211, 213 (p. 665), 216 (p. 666), 221 (p. 668), 223 (p. 669),
    // 249–253 (p. 678), 257–258 (pp. 679–680).
    for s in 0..=15 {
        // ENTRY pp. 426–427: unsigned imm12 in units of eight bytes.
        // All s field values encode; only a0..a3 have defined execution.
        for frame_bytes in [0, 8, 16, 120, 16376, 16384, 32760] {
            instruction("entry", enc::entry(s, frame_bytes),
                insn(Op::Entry, 0, s, 0, frame_bytes as i32), 3);
        }
        instruction("callx4", enc::callx4(s), insn(Op::Callx4, 0, s, 0, 0), 3);
        instruction("callx8", enc::callx8(s), insn(Op::Callx8, 0, s, 0, 0), 3);
        instruction("callx12", enc::callx12(s), insn(Op::Callx12, 0, s, 0, 0), 3);
        for t in 0..=15 {
            instruction("movsp", enc::movsp(t, s), insn(Op::Movsp, 1, s, t, 0), 3);
            // L32E p. 456, S32E pp. 591–592: one-extend r, then scale by four.
            for (r, offset) in [(0, -64), (1, -60), (7, -36), (8, -32), (14, -8), (15, -4)] {
                instruction("l32e", enc::l32e(t, s, offset),
                    insn(Op::L32e, r, s, t, offset), 3);
                instruction("s32e", enc::s32e(t, s, offset),
                    insn(Op::S32e, r, s, t, offset), 3);
            }
        }
    }
    for n in -8..=7 {
        instruction("rotw", enc::rotw(n), insn(Op::Rotw, 8, 0, (n as u8) & 15, n), 3);
    }
    instruction("retw", enc::retw(), insn(Op::Retw, 0, 0, 0, 0), 3);
    instruction("retw_n", enc::retw_n().into(), insn(Op::RetwN, 15, 0, 1, 0), 2);
    instruction("rfwo", enc::rfwo(), insn(Op::Rfwo, 3, 4, 0, 0), 3);
    instruction("rfwu", enc::rfwu(), insn(Op::Rfwu, 3, 5, 0, 0), 3);
    instruction("rfe", enc::rfe(), insn(Op::Rfe, 3, 0, 0, 0), 3);

    let calls: &[(&str, fn(i32) -> u32, fn(&mut Asm, usize), Op, u8)] = &[
        ("call4", enc::call4, Asm::call4, Op::Call4, 1),
        ("call8", enc::call8, Asm::call8, Op::Call8, 2),
        ("call12", enc::call12, Asm::call12, Op::Call12, 3),
    ];
    for &(name, encode, emit_label, op, n) in calls {
        for rel in I18 {
            let bits = encode(rel);
            // The model retains CALL's n field in Insn::t.
            instruction(name, bits, insn(op, 0, 0, n, rel * 4), 3);
            // CALL4/8/12 pp. 373–377: clear PC's low two bits, then add
            // four and the signed word displacement multiplied by four.
            for pc in [0x80000, 0x80001, 0x80002, 0x80003] {
                assert_eq!(target(&decoded(name, bits).0, pc), Some((0x80004 + rel * 4) as u32),
                    "{name}: target is aligned PC + 4 + signed word displacement");
            }
        }
        for backwards in [false, true] {
            let mut a = Asm::new(1, A15);
            let mut call_at = 0;
            let code = assemble(&mut a, |a| {
                if backwards {
                    a.base.label_assign(0);
                    a.entry(A1, 16);
                    a.retw_n();
                }
                a.base.align(4);
                a.base.reserve(1); // Match call0's instruction location, 1 mod 4.
                call_at = a.base.offset();
                emit_label(a, 0);
                if !backwards {
                    a.base.label_assign(0);
                    a.entry(A1, 16);
                    a.retw_n();
                }
            });
            let at = call_at as usize;
            let bits = u32::from_le_bytes([code[at], code[at + 1], code[at + 2], 0]);
            let dest = a.base.label_offset(0).unwrap();
            instruction(name, bits, insn(op, 0, 0, n, dest as i32 - call_at as i32 - 3), 3);
            assert_eq!(target(&decoded(name, bits).0, CODE + call_at), Some(CODE + dest),
                "{name} backwards={backwards}: label displacement converted from bytes to words");
        }
        for rel in [-131073, 131072] {
            panics(name, || { encode(rel); });
        }
    }

    // RSR/WSR: Table 221 p. 668; XSR: Table 216 p. 666, RSR format p. 657.
    // insn::special_access in the model's src/insn.rs restricts decoding to
    // configured SRs. XSR p. 654 permits an illegal-instruction exception for
    // unconfigured SRs, so check field endpoints without decoding them.
    let special_ops: &[(&str, fn(Reg, u32) -> u32, Op)] = &[
        ("rsr", |t, special| enc::rsr(0, 3, 0, special, t), Op::Rsr),
        ("wsr", enc::wsr, Op::Wsr),
        ("xsr", enc::xsr, Op::Xsr),
    ];
    for &(name, encode, op) in special_ops {
        for t in 0..=15 {
            // WINDOWBASE, WINDOWSTART, EPC1, EXCSAVE1, PS, EXCCAUSE.
            for special in [72, 73, 177, 209, 230, 232] {
                instruction(name, encode(t, special), insn(op, 0, 0, t, special as i32), 3);
            }
            for special in [0, 255] {
                assert_eq!((encode(t, special) >> 8) & 255, special,
                    "{name}: full unsigned eight-bit SR field");
            }
        }
        panics(name, || { encode(A1, 256); });
    }
    for frame_bytes in [1, 32768] {
        panics("entry invalid frame", || { enc::entry(A1, frame_bytes); });
    }
    for n in [-9, 8] {
        panics("rotw outside signed four-bit range", || { enc::rotw(n); });
    }
    for offset in [-68, -63, 0] {
        panics("l32e invalid byte offset", || { enc::l32e(A2, A1, offset); });
        panics("s32e invalid byte offset", || { enc::s32e(A2, A1, offset); });
    }
}

#[test]
fn boot_stage_instruction_round_trips() {
    // ISA pp. 429,448,490,586,644,650 and opcode maps pp. 662–668.
    for reg in REGS {
        for special in [72, 73, 228, 230] {
            instruction("wsr", enc::wsr(reg, special),
                insn(Op::Wsr, 0, 0, reg, special as i32), 3);
        }
        for source in REGS {
            for (shift, width) in [(0, 1), (0, 8), (0, 12), (8, 8), (12, 12), (16, 16), (31, 1)] {
                let bits = enc::extui(reg, source, shift, width);
                instruction("extui", bits,
                    insn(Op::Extui, width - 1, shift & 15, source, i32::from(shift)), 3);
                // EXTUI's decoder keeps its destination in bits, because r
                // in the decoded structure carries the encoded mask width.
                assert_eq!((decoded("extui destination", bits).0.bits >> 12) & 15,
                    u32::from(reg), "extui: destination survives independently of mask width");
            }
        }
    }
    instruction("isync", enc::isync(), insn(Op::Isync, 2, 0, 0, 0), 3);
    instruction("rsync", enc::rsync(), insn(Op::Rsync, 2, 0, 1, 0), 3);
    instruction("memw", enc::memw(), insn(Op::Memw, 2, 0, 12, 0), 3);
    for level in [0, 1, 15] {
        instruction("waiti", enc::waiti(level), insn(Op::Waiti, 7, level, 0, i32::from(level)), 3);
    }
}

#[test]
fn immediate_and_memory_encodings() {
    // ADDI: sign-extended imm8. MOVI: sign-extended s:imm8 (pp. 332, 506).
    for rd in REGS {
        for rs in REGS { for imm in I8 {
            instruction("addi", enc::addi(rd, rs, imm), insn(Op::Addi, 12, rs, rd, imm), 3);
        } }
        // s is imm12[11:8].
        for (imm, s) in I12.into_iter().zip([8, 7, 15, 0, 0, 5]) {
            instruction("movi", enc::movi(rd, imm), insn(Op::Movi, 10, s, rd, imm), 3);
        }
        // MOVI.N sign extends with imm7[6] AND imm7[5], giving -32..95 (p. 508).
        for imm in [-32, 95, -1, 0, 1, 0x55] {
            instruction("movi_n", enc::movi_n(rd, imm).into(), insn(Op::MoviN, 0, rd, 0, imm), 2);
        }
    }
    // Offsets from the ISA's assembler syntax, with expanded byte immediates.
    let cases: &[(&str, fn(Reg, Reg, u32) -> u32, fn() -> Op, u8, &[u32])] = &[
        ("l8ui", enc::l8ui, || Op::L8ui, 0, &[0, 1, 255, 0x55]),
        ("l16ui", enc::l16ui, || Op::L16ui, 1, &[0, 2, 510, 0xaa]),
        ("l32i", enc::l32i, || Op::L32i, 2, &[0, 4, 1020, 0x154]),
        ("s8i", enc::s8i, || Op::S8i, 4, &[0, 1, 255, 0x55]),
        ("s16i", enc::s16i, || Op::S16i, 5, &[0, 2, 510, 0xaa]),
        ("s32i", enc::s32i, || Op::S32i, 6, &[0, 4, 1020, 0x154]),
    ];
    for &(name, encode, op, r, offsets) in cases {
        for value in REGS { for base in REGS { for &offset in offsets {
            instruction(name, encode(value, base, offset), insn(op(), r, base, value, offset as i32), 3);
        } } }
    }
    for value in REGS { for base in REGS {
        for (offset, r) in [(0, 0), (4, 1), (60, 15), (40, 10)] {
            instruction("l32i_n", enc::l32i_n(value, base, offset).into(),
                insn(Op::L32iN, r, base, value, offset as i32), 2);
            instruction("s32i_n", enc::s32i_n(value, base, offset).into(),
                insn(Op::S32iN, r, base, value, offset as i32), 2);
        }
    } }
    // L32R uses a one-extended imm16, NOT a signed imm16, then multiplies by 4.
    // Check all PC alignments against ((PC + 3) & ~3), pp. 461–462.
    for rd in REGS {
        for (pc, pc_base) in [(0x40000, 0x40000), (0x40001, 0x40004),
            (0x40002, 0x40004), (0x40003, 0x40004)]
        {
            for rel in [-262144, -4, -8, -0x15554] {
                let dest = (pc_base as i64 + i64::from(rel)) as u32;
                let bits = enc::l32r(rd, pc, dest);
                instruction("l32r", bits, insn(Op::L32r, 0, 0, rd, rel), 3);
                assert_eq!(target(&decoded("l32r", bits).0, pc), Some(dest),
                    "l32r: rounded PC base and negative byte displacement");
            }
        }
    }
}

fn branch_op(cond: u8) -> Op {
    match cond {
        cc::NONE => Op::Bnone, cc::EQ => Op::Beq, cc::LT => Op::Blt, cc::LTU => Op::Bltu,
        cc::ALL => Op::Ball, cc::BC => Op::Bbc, cc::ANY => Op::Bany, cc::NE => Op::Bne,
        cc::GE => Op::Bge, cc::GEU => Op::Bgeu, cc::NALL => Op::Bnall, cc::BS => Op::Bbs,
        _ => panic!("branch_op: test condition must be a MicroPython cc constant"),
    }
}

fn zero_op(cond: u8) -> Op {
    match cond {
        ccz::EQ => Op::Beqz, ccz::NE => Op::Bnez, ccz::LT => Op::Bltz, ccz::GE => Op::Bgez,
        _ => panic!("zero_op: test condition must be a MicroPython ccz constant"),
    }
}

#[test]
fn branch_and_call_encodings() {
    // ISA Summary Tables 15–16 pp. 64–65; BRI8 p. 658 and instruction
    // diagrams pp. 350, 355, 357, 360–362, 365. The CPU's independent
    // decoder expands r to the table constant and keeps displacement in bits.
    for (unsigned, table) in [
        (false, [-1, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256]),
        (true, [32768, 65536, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256]),
    ] {
        let conditions: &[(u8, Op)] = if unsigned { &[(cc::LTU, Op::Bltui), (cc::GEU, Op::Bgeui)] }
            else { &[(cc::EQ, Op::Beqi), (cc::NE, Op::Bnei), (cc::LT, Op::Blti), (cc::GE, Op::Bgei)] };
        for (r, constant) in table.into_iter().enumerate() {
            assert_eq!(if unsigned { enc::b4constu(constant as u32) } else { enc::b4const(constant) },
                Some(r as u8), "branch table index: unsigned={unsigned}, constant={constant}");
            for &(cond, op) in conditions { for rs in REGS { for rel in B8 {
                let bits = enc::bcci(cond, rs, constant, rel);
                instruction("bcci", bits, insn(op, r as u8, rs, 0, constant), 3);
                assert_eq!(target(&decoded("bcci", bits).0, 0x1000), Some((0x1004 + rel) as u32),
                    "bcci: target is PC + 4 + signed imm8");
            } } }
        }
    }
    for c in [0, 9, -2, 257] {
        assert_eq!(enc::b4const(c), None, "signed table excludes {c}");
        assert_eq!(enc::b4constu(c as u32), None, "unsigned table excludes {c}");
    }
    for cond in CONDITIONS { for rs in REGS { for rt in REGS { for rel in B8 {
        let bits = enc::bcc(cond, rs, rt, rel);
        instruction("bcc", bits, insn(branch_op(cond), cond, rs, rt, rel), 3);
        assert_eq!(target(&decoded("bcc", bits).0, 0x1000), Some((0x1004 + rel) as u32),
            "bcc: target is PC + 4 + signed imm8");
    } } } }
    for cond in [ccz::EQ, ccz::NE, ccz::LT, ccz::GE] { for rs in REGS { for rel in B12 {
        let bits = enc::bccz(cond, rs, rel);
        instruction("bccz", bits, insn(zero_op(cond), 0, rs, 0, rel), 3);
        assert_eq!(target(&decoded("bccz", bits).0, 0x1000), Some((0x1004 + rel) as u32),
            "bccz: target is PC + 4 + signed imm12");
    } } }
    for rel in I18 {
        let bits = enc::j(rel);
        instruction("j", bits, insn(Op::J, 0, 0, 0, rel), 3);
        assert_eq!(target(&decoded("j", bits).0, 0x80000), Some((0x80004 + rel) as u32),
            "j: target is PC + 4 + signed byte displacement");
        let bits = enc::call0(rel);
        instruction("call0", bits, insn(Op::Call0, 0, 0, 0, rel * 4), 3);
        for pc in [0x80000, 0x80001, 0x80002, 0x80003] {
            assert_eq!(target(&decoded("call0", bits).0, pc), Some((0x80004 + rel * 4) as u32),
                "call0: target is aligned PC + 4 + signed word displacement");
        }
    }
}

#[test]
fn format_field_layouts() {
    // Literal little-endian field placements from chapter 9. These are raw
    // formats, so not every field combination denotes a legal instruction.
    for (a, b, c, d, e, f, word) in [
        (0, 0, 0, 0, 0, 0, 0), (15, 15, 15, 15, 15, 15, 0xffffff),
        (1, 2, 3, 4, 5, 6, 0x324561), (5, 10, 5, 10, 5, 10, 0x5aa5a5),
    ] {
        assert_eq!(enc::rrr(a, b, c, d, e, f), word, "rrr: six four-bit fields");
        assert_eq!(enc::rri4(a, b, d, e, f, c), word, "rri4: immediate replaces op2");
        assert_eq!(enc::rri8(a, d, e, f, u32::from(c) * 16 + u32::from(b)), word,
            "rri8: immediate occupies high byte");
        assert_eq!(enc::rsr(a, b, c, u32::from(d) * 16 + u32::from(e), f), word,
            "rsr: eight-bit special register field");
        assert_eq!(u32::from(enc::rrrn(a, d, e, f)), word & 0xffff,
            "rrrn: low four fields in sixteen bits");
    }
    for (imm, expected) in [(0, 0x0000f1), (1, 0x0001f1), (65535, 0xfffff1), (0x5555, 0x5555f1)] {
        assert_eq!(enc::ri16(1, 15, imm), expected, "ri16: sixteen-bit immediate at bit eight");
    }
    for (imm, expected) in [(0, 0x000005), (1, 0x000045), (0x3ffff, 0xffffc5), (0x15555, 0x555545)] {
        assert_eq!(enc::call(5, 0, imm), expected, "call: eighteen-bit offset at bit six");
    }
    assert_eq!(enc::callx(0, 0, 0, 0, 15, 3, 0), 0x000fc0, "callx: separate m/n fields");
    for (rel, expected) in [(-128, 0x80ffc6), (127, 0x7fffc6), (-1, 0xffffc6),
        (0, 0x00ffc6), (1, 0x01ffc6), (0x55, 0x55ffc6)]
    {
        assert_eq!(enc::bri8(6, 15, 15, 3, 0, rel), expected, "bri8: signed byte field");
    }
    for (rel, expected) in [(-2048, 0x800fd6), (2047, 0x7fffd6), (-1, 0xffffd6),
        (0, 0x000fd6), (1, 0x001fd6), (0x555, 0x555fd6)]
    {
        assert_eq!(enc::bri12(6, 15, 3, 1, rel), expected, "bri12: signed twelve-bit field");
    }
    for (imm, expected) in [(0, 0x0f0c), (1, 0x1f0c), (127, 0xff7c), (0x55, 0x5f5c)] {
        assert_eq!(enc::ri7(12, 15, imm), expected, "ri7: split immediate around register s");
    }
}

fn panics(name: &str, f: impl FnOnce() + std::panic::UnwindSafe) {
    assert!(std::panic::catch_unwind(f).is_err(), "{name}: invalid operand must panic");
}

#[test]
fn invalid_operands_are_not_masked() {
    for rel in -3..=0 {
        panics("bcc reserved offset", move || { enc::bcc(cc::EQ, A2, A3, rel); });
        panics("bccz reserved offset", move || { enc::bccz(ccz::EQ, A2, rel); });
        panics("bcci reserved offset", move || { enc::bcci(cc::EQ, A2, 1, rel); });
    }
    panics("l32i unaligned byte offset", || { enc::l32i(A0, A1, 2); });
    panics("l32i offset beyond imm8", || { enc::l32i(A0, A1, 1024); });
    panics("l16ui unaligned byte offset", || { enc::l16ui(A0, A1, 1); });
    panics("l32i_n offset beyond imm4", || { enc::l32i_n(A0, A1, 64); });
    panics("bri8 positive overflow", || { enc::bri8(6, 0, A1, 0, 2, 128); });
    panics("bri8 negative overflow", || { enc::bri8(6, 0, A1, 0, 2, -129); });
    panics("addx2 register sixteen", || { enc::addx2(16, A0, A1); });
    panics("movi_n below asymmetric range", || { enc::movi_n(A0, -33); });
    panics("movi_n above asymmetric range", || { enc::movi_n(A0, 96); });
    panics("movi positive overflow", || { enc::movi(A0, 2048); });
    panics("bcc reserved condition", || { enc::bcc(6, A0, A1, 0); });
    panics("l32r unaligned destination", || { enc::l32r(A0, 0x100, 1); });
    panics("l32r forward reference", || { enc::l32r(A0, 0x100, 0x104); });
    panics("l32r zero displacement", || { enc::l32r(A0, 0x100, 0x100); });
    panics("l32r beyond 256 KB", || { enc::l32r(A0, 0x40004, 0); });
    panics("raw call field overflow", || { enc::call(5, 0, 0x40000); });
    panics("raw rrr opcode overflow", || { enc::rrr(16, 0, 0, 0, 0, 0); });
}

const RAM: u32 = 0x3fc88000;
const CODE: u32 = 0x40378000;

fn config() -> Config {
    Config {
        // The ESP32-S3 core (core-isa.h): unaligned accesses in hardware, EXCM level 3.
        mul32_high: true, div32: true, unaligned: Unaligned::Hardware, excm_level: 3,
        mac16: true, s32c1i: true, threadptr: true,
        vecbase_reset: 0x40000000, vectors: Vectors { kernel: 0x300, user: 0x340, window: [0, 0x40, 0x80, 0xc0, 0x100, 0x140] },
        int_type: [IntType::None; 32], int_level: [0; 32], prid: Some(0),
    }
}

fn machine(code: &[u8], registers: &[(Reg, u32)]) -> (Cpu, Memory, usize) {
    assert!(code.len() <= 65536, "model program must fit the shared 64 KB store");
    let mut mem = Memory::new();
    let store = mem.store(65536);
    mem.map(RAM, store, 0, 65536, Perm { read: true, write: true, exec: false });
    mem.map(CODE, store, 0, 65536, Perm { read: true, write: true, exec: true });
    mem.bytes_mut(store)[..code.len()].copy_from_slice(code);
    let mut cpu = Cpu::new(config(), CODE);
    for &(reg, value) in registers { cpu.ar[reg as usize] = value; }
    (cpu, mem, store)
}

fn run(name: &str, cpu: &mut Cpu, mem: &mut Memory) {
    // Every scenario skips its padding. Even the largest branch-distance case
    // executes only a handful of instructions; a loop is a failure, not work.
    match cpu.run(mem, 100) {
        Stop::Break { pc } => assert_eq!(cpu.pc, pc, "{name}: BREAK reports the stopped PC"),
        stop => panic!("{name}: program must end at BREAK, pc={:#010x}, stop={:?}",
            cpu.pc, std::mem::discriminant(&stop)),
    }
}

fn stop(a: &mut Asm) {
    // BREAK 0,0: chapter 9, QRST/RST0/ST0, r=4, s=t=0 (p. 662).
    // The reference has no BREAK inline, so this is deliberately a raw opcode.
    a.op24(0x004000);
}

#[test]
fn optimised_immediates_execute_and_literals_are_patched() {
    let mut a = Asm::new(0, A15);
    for value in [0, 1, u32::MAX, (-32i32) as u32, 95, 96, 2047, 2048,
        (-2048i32) as u32, (-2049i32) as u32, 0x7fffffff, 0x80000000, 0x12345678]
    {
        let mut start = 0;
        let mut length = 0;
        let code = assemble(&mut a, |a| {
            a.literal_table();
            start = a.base.offset();
            a.mov_reg_i32_optimised(A2, value);
            length = a.base.offset() - start;
            stop(a);
        });
        let signed = value as i32;
        let (expected_op, expected_length, has_literal) = if (-32..=95).contains(&signed) {
            (Op::MoviN, 2, false)
        } else if (-2048..=2047).contains(&signed) {
            (Op::Movi, 3, false)
        } else {
            (Op::L32r, 3, true)
        };
        assert_eq!(length, expected_length, "optimised immediate {value:#x}: source's form length");
        let bits = u32::from_le_bytes([code[start as usize], code[start as usize + 1],
            code[start as usize + 2], 0]);
        assert_eq!(std::mem::discriminant(&decoded("optimised immediate", bits).0.op),
            std::mem::discriminant(&expected_op), "optimised immediate {value:#x}: source's form");
        if has_literal {
            assert_eq!(start, 8, "literal table: J, one padding byte, one word");
            assert_eq!(code[3], 0, "literal table: padding is zero");
            assert_eq!(&code[4..8], &value.to_le_bytes(), "literal table: value patched little-endian");
            let bits = u32::from_le_bytes([code[0], code[1], code[2], 0]);
            assert_eq!(target(&decoded("literal table jump", bits).0, CODE), Some(CODE + 8),
                "literal table: J skips padding and all constants");
        } else {
            assert_eq!(start, 0, "empty literal table: no J or padding emitted");
        }
        let (mut cpu, mut mem, _) = machine(&code, &[]);
        run("optimised immediate", &mut cpu, &mut mem);
        assert_eq!(cpu.ar[A2 as usize], value, "optimised immediate {value:#x}: register value");
    }
}

// Chapter 8 branch predicates, independent of the encoder's condition packing.
fn condition(cond: u8, x: u32, y: u32) -> bool {
    match cond {
        cc::NONE => x & y == 0, cc::EQ => x == y,
        cc::LT => (x as i32) < (y as i32), cc::LTU => x < y,
        cc::ALL => !x & y == 0, cc::BC => x & (1 << (y & 31)) == 0,
        cc::ANY => x & y != 0, cc::NE => x != y,
        cc::GE => (x as i32) >= (y as i32), cc::GEU => x >= y,
        cc::NALL => !x & y != 0, cc::BS => x & (1 << (y & 31)) != 0,
        _ => panic!("condition: test condition must be a MicroPython cc constant"),
    }
}

fn zero_condition(cond: u8, value: u32) -> bool {
    match cond {
        ccz::EQ => value == 0, ccz::NE => value != 0,
        ccz::LT => (value as i32) < 0, ccz::GE => (value as i32) >= 0,
        _ => panic!("zero_condition: test condition must be a MicroPython ccz constant"),
    }
}

#[derive(Clone, Copy, Debug)]
enum Branch {
    Jump,
    Zero(u8),
    Pair(u8),
    Bit(u32, u8),
}

fn branch_program(branch: Branch, backward_distance: Option<u32>) -> Vec<u8> {
    let mut widths = Vec::new();
    let code = assemble(&mut Asm::new(2, A15), |a| {
        if let Some(distance) = backward_distance {
            // Start beyond the taken path, then branch backwards into it.
            a.j_label(1);
            a.base.label_assign(0);
            a.movi_n(A4, 1);
            stop(a);
            assert!(distance >= 5, "branch fixture: backwards target block is five bytes");
            a.base.reserve(distance - 5);
            a.base.label_assign(1);
        }
        let before = a.base.offset();
        match branch {
            Branch::Jump => a.j_label(0),
            Branch::Zero(cond) => a.bccz_reg_label(cond, A2, 0),
            Branch::Pair(cond) => a.bcc_reg_reg_label(cond, A2, A3, 0),
            Branch::Bit(bit, cond) => a.bit_branch(A2, bit, 0, cond),
        }
        widths.push(a.base.offset() - before);
        a.movi_n(A4, 2);
        stop(a);
        if backward_distance.is_none() {
            a.base.label_assign(0);
            a.movi_n(A4, 1);
            stop(a);
        }
    });
    let expected = match (branch, backward_distance) {
        (Branch::Jump | Branch::Bit(..), _) => 3,
        (Branch::Zero(_), Some(distance)) if distance <= 2044 => 3,
        (Branch::Pair(_), Some(distance)) if distance <= 124 => 3,
        _ => 6,
    };
    assert_eq!(widths, [expected; 3],
        "{branch:?} distance {backward_distance:?}: only a known backwards label selects short form");
    code
}

#[test]
fn jump_and_conditional_helpers_reach_labels() {
    for distance in [None, Some(5), Some(125), Some(2045)] {
        let code = branch_program(Branch::Jump, distance);
        let (mut cpu, mut mem, _) = machine(&code, &[]);
        run("j_label", &mut cpu, &mut mem);
        assert_eq!(cpu.ar[A4 as usize], 1, "j_label {distance:?}: target path marker");
    }
    for cond in [ccz::EQ, ccz::NE, ccz::LT, ccz::GE] {
        // rel = -distance - 4: -2048 fits, -2049 requires inverted branch + J.
        for distance in [None, Some(5), Some(2044), Some(2045)] {
            let code = branch_program(Branch::Zero(cond), distance);
            for taken in [false, true] {
                let value = [0, 1, u32::MAX].into_iter()
                    .find(|&v| zero_condition(cond, v) == taken)
                    .expect("bccz fixture must contain both outcomes");
                let (mut cpu, mut mem, _) = machine(&code, &[(A2, value)]);
                run("bccz_reg_label", &mut cpu, &mut mem);
                assert_eq!(cpu.ar[A4 as usize], if taken { 1 } else { 2 },
                    "bccz {cond} {distance:?} taken={taken}: target/fallthrough marker");
            }
        }
    }
    for cond in CONDITIONS {
        // rel = -distance - 4: -128 fits, -129 requires inverted branch + J.
        for distance in [None, Some(5), Some(124), Some(125)] {
            let code = branch_program(Branch::Pair(cond), distance);
            for taken in [false, true] {
                let (x, y) = [(0, 0), (0, 1), (1, 0), (1, 1), (u32::MAX, 0), (0, u32::MAX)]
                    .into_iter().find(|&(x, y)| condition(cond, x, y) == taken)
                    .expect("bcc fixture must contain both outcomes");
                let (mut cpu, mut mem, _) = machine(&code, &[(A2, x), (A3, y)]);
                run("bcc_reg_reg_label", &mut cpu, &mut mem);
                assert_eq!(cpu.ar[A4 as usize], if taken { 1 } else { 2 },
                    "bcc {cond} {distance:?} taken={taken}: target/fallthrough marker");
            }
        }
    }
}

#[test]
fn setcc_corners_and_bit_branches_execute() {
    for cond in CONDITIONS {
        let code = assemble(&mut Asm::new(0, A15), |a| {
            a.setcc_reg_reg_reg(cond, A4, A2, A3);
            stop(a);
        });
        for (x, y) in [(0, 0), (0, 1), (1, 0), (u32::MAX, u32::MAX),
            (u32::MAX, 0), (0, u32::MAX), (0x80000000, 0x7fffffff),
            (0x7fffffff, 0x80000000), (0x80000000, 0x80000000),
            (0x80000000, 31), (0x7fffffff, 31)]
        {
            let (mut cpu, mut mem, _) = machine(&code, &[(A2, x), (A3, y)]);
            run("setcc_reg_reg_reg", &mut cpu, &mut mem);
            assert_eq!(cpu.ar[A4 as usize], u32::from(condition(cond, x, y)),
                "setcc {cond}: corner x={x:#x}, y={y:#x}");
            assert_eq!((cpu.ar[A2 as usize], cpu.ar[A3 as usize]), (x, y),
                "setcc: source registers survive");
        }
    }
    for bit in [0, 31] { for cond in [6, 14] { for distance in [None, Some(5)] {
        let code = branch_program(Branch::Bit(bit, cond), distance);
        for is_set in [false, true] {
            // Other bits are set so testing the wrong bit fails for the clear case.
            let value = if is_set { 1 << bit } else { !(1 << bit) };
            let taken = is_set == (cond == 14);
            let (mut cpu, mut mem, _) = machine(&code, &[(A2, value)]);
            run("bit_branch", &mut cpu, &mut mem);
            assert_eq!(cpu.ar[A4 as usize], if taken { 1 } else { 2 },
                "bit_branch bit={bit} cond={cond} {distance:?}: tests selected bit");
        }
    } } }
}

#[test]
fn offset_loads_and_stores_execute_for_each_width() {
    for size in 0..=2 {
        for offset in [0, 15, 16, 255, 256, 2048] {
            let code = assemble(&mut Asm::new(0, A15), |a| {
                a.literal_table();
                a.store_reg_reg_offset(A2, A3, offset, size);
                a.load_reg_reg_offset(A4, A3, offset, size);
                if size == 2 { a.l32i_optimised(A5, A3, offset); }
                stop(a);
            });
            let value: u32 = 0x89abcdef;
            let (mut cpu, mut mem, store) = machine(&code, &[(A2, value), (A3, RAM + 0x4000)]);
            mem.bytes_mut(store)[0x3ff0..].fill(0xa5);
            run("load/store offset", &mut cpu, &mut mem);
            let width = 1usize << size;
            let at = 0x4000 + ((offset as usize) << size);
            assert_eq!(&mem.bytes(store)[at..at + width], &value.to_le_bytes()[..width],
                "store size={size} offset={offset}: correct little-endian bytes at scaled address");
            assert_eq!((mem.bytes(store)[at - 1], mem.bytes(store)[at + width]), (0xa5, 0xa5),
                "store size={size} offset={offset}: adjacent bytes survive");
            let expected = match size { 0 => value & 0xff, 1 => value & 0xffff, _ => value };
            assert_eq!(cpu.ar[A4 as usize], expected,
                "load size={size} offset={offset}: zero extension and effective address");
            if size == 2 {
                assert_eq!(cpu.ar[A5 as usize], value,
                    "l32i_optimised offset={offset}: narrow, wide and materialised address paths");
            }
            assert_eq!((cpu.ar[A2 as usize], cpu.ar[A3 as usize]), (value, RAM + 0x4000),
                "load/store offset: source and base survive scratch use");
        }
    }
}

#[test]
fn pcrel_call0_and_label_l32r_execute() {
    for alignment in 0..4 { for backwards in [false, true] {
        let mut a = Asm::new(2, A15);
        let mut start = 0;
        let code = assemble(&mut a, |a| {
            if backwards { a.base.label_assign(0); }
            a.j_label(1);
            a.base.reserve((alignment + 4 - 3) & 3);
            a.base.label_assign(1);
            start = a.base.offset();
            a.mov_reg_pcrel(A2, 0);
            stop(a);
            if !backwards { a.base.label_assign(0); }
        });
        let (mut cpu, mut mem, _) = machine(&code, &[]);
        run("mov_reg_pcrel", &mut cpu, &mut mem);
        assert_eq!(cpu.ar[A2 as usize], CODE + a.base.label_offset(0).unwrap(),
            "mov_reg_pcrel alignment={alignment} backwards={backwards}: label's address");
        assert_eq!(cpu.ar[A0 as usize], CODE + start + 6,
            "mov_reg_pcrel: A0 holds the unpadded return PC used in the relative sum");
    } }
    for backwards in [false, true] {
        let mut a = Asm::new(2, A15);
        let mut call_at = 0;
        let code = assemble(&mut a, |a| {
            a.j_label(0);
            if backwards {
                a.base.align(4);
                a.base.label_assign(1);
                a.addi(A2, A2, 1);
                a.ret_n();
            } else {
                a.base.reserve(2);
            }
            a.base.label_assign(0); // Both layouts put CALL0 at 1 mod 4.
            call_at = a.base.offset();
            a.call0(1);
            stop(a);
            if !backwards {
                a.base.align(4);
                a.base.label_assign(1);
                a.addi(A2, A2, 1);
                a.ret_n();
            }
        });
        let at = call_at as usize;
        let bits = u32::from_le_bytes([code[at], code[at + 1], code[at + 2], 0]);
        assert_eq!(target(&decoded("call0 label", bits).0, CODE + call_at),
            Some(CODE + a.base.label_offset(1).unwrap()),
            "call0 backwards={backwards}: label displacement converted from bytes to words");
        let (mut cpu, mut mem, _) = machine(&code, &[(A2, 41)]);
        run("call0/ret_n", &mut cpu, &mut mem);
        assert_eq!(cpu.ar[A2 as usize], 42, "call0/ret_n: function executed and returned");
        assert_eq!(cpu.ar[A0 as usize], CODE + call_at + 3, "call0: return address in A0");
    }
    let code = assemble(&mut Asm::new(2, A15), |a| {
        a.j_label(1);
        a.base.align(4);
        a.base.label_assign(0);
        a.base.data(4, 0xdeadbeef);
        a.base.label_assign(1);
        a.l32r(A2, 0);
        stop(a);
    });
    let (mut cpu, mut mem, _) = machine(&code, &[]);
    run("l32r label", &mut cpu, &mut mem);
    assert_eq!(cpu.ar[A2 as usize], 0xdeadbeef, "l32r label: aligned backwards literal load");
}

#[test]
fn literal_counting_precedes_equal_compute_and_emit_layouts() {
    let mut a = Asm::new(1, A15);
    let mut layouts = Vec::new();
    let code = assemble(&mut a, |a| {
        a.literal_table();
        let first = a.mov_reg_i32(A2, 0x12345678);
        a.j_label(0);
        a.movi_n(A4, 2);
        a.base.label_assign(0);
        let second = a.mov_reg_i32(A3, 0x87654321);
        stop(a);
        layouts.push((a.base.pass(), a.base.offset(), a.base.label_offset(0), first, second));
    });
    assert_eq!(layouts.len(), 3, "Xtensa driver: count traversal followed by Compute and Emit");
    assert_eq!((layouts[0].0, layouts[1].0, layouts[2].0), (Pass::Compute, Pass::Compute, Pass::Emit),
        "Xtensa driver: counting never emits code");
    assert_eq!((layouts[1].1, layouts[1].2, layouts[1].3, layouts[1].4),
        (layouts[2].1, layouts[2].2, layouts[2].3, layouts[2].4),
        "two passes: final Compute and Emit sizes, labels and literal locations agree");
    assert_eq!(code.len(), layouts[1].1 as usize, "two passes: emitted byte count equals Compute");
    assert_eq!((layouts[2].3, layouts[2].4), (4, 8), "literal cursor: consecutive table words");
    assert_eq!(&code[4..12], &[0x78, 0x56, 0x34, 0x12, 0x21, 0x43, 0x65, 0x87],
        "literal patching: each word is little-endian and neither overwrites its neighbour");
    let (mut cpu, mut mem, _) = machine(&code, &[(A4, 7)]);
    run("two-pass program", &mut cpu, &mut mem);
    assert_eq!((cpu.ar[A2 as usize], cpu.ar[A3 as usize], cpu.ar[A4 as usize]),
        (0x12345678, 0x87654321, 7), "two passes: literals and forward jump execute correctly");
}

#[test]
fn scratch_aliases_and_table_misuse_are_rejected() {
    panics("large load destination/base alias", || {
        Asm::new(0, A15).load_reg_reg_offset(A2, A2, 256, 2);
    });
    panics("large l32i destination/base alias", || {
        Asm::new(0, A15).l32i_optimised(A2, A2, 256);
    });
    for scratch in [A2, A3] {
        panics("large store scratch/source or scratch/base alias", || {
            Asm::new(0, scratch).store_reg_reg_offset(A2, A3, 256, 2);
        });
    }
    panics("setcc destination/source alias", || {
        Asm::new(0, A15).setcc_reg_reg_reg(cc::LT, A2, A2, A3);
    });
    panics("pcrel destination is implicit A0 scratch", || {
        Asm::new(1, A15).mov_reg_pcrel(A0, 0);
    });
    panics("literal before table", || { Asm::new(0, A15).mov_reg_i32(A2, 123); });
    panics("unaligned literal table", || {
        let mut a = Asm::new(0, A15);
        a.base.reserve(1);
        a.literal_table();
    });
    panics("two literal tables in one pass", || {
        let mut a = Asm::new(0, A15);
        a.literal_table();
        a.literal_table();
    });
    panics("missing label in Emit", || {
        assemble(&mut Asm::new(1, A15), |a| a.j_label(0));
    });
    panics("invalid operation size", || {
        Asm::new(0, A15).store_reg_reg_offset(A2, A3, 0, 3);
    });
}

#[test]
fn reserve_and_patch_obey_the_base_pass_contract() {
    let mut base = AsmBase::new(0);
    base.start_pass(Pass::Compute);
    let start = base.reserve(4);
    base.patch(u32::MAX, &[1, 2]); // Entirely a no-op during Compute.
    assert_eq!((start, base.offset()), (0, 4), "reserve: returns start and advances Compute offset");
    assert!(base.code().is_empty(), "reserve/patch: Compute allocates no emitted bytes");
    base.start_pass(Pass::Emit);
    base.reserve(4);
    assert_eq!(base.code(), &[0, 0, 0, 0], "reserve: Emit fills reserved bytes with zeros");
    base.patch(1, &[0xaa, 0x55]);
    assert_eq!(base.code(), &[0, 0xaa, 0x55, 0], "patch: overwrites only its reserved byte range");
    assert_eq!(base.offset(), 4, "patch: leaves the current offset unchanged");
    panics("patch beyond already emitted bytes", || {
        let mut base = AsmBase::new(0);
        base.reserve(4);
        base.start_pass(Pass::Emit);
        base.reserve(2);
        base.patch(2, &[1]);
    });
}
