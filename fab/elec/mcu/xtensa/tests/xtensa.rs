//! Independent test encoders transcribed from chapter 9's format diagrams.
//! No production encoder, external assembler, or generated opcode tables.

use fab_elec_mcu_emu::bus::{Bus, Kind, Memory, Perm, Width};
use fab_elec_mcu_xtensa::insn::{decode, target, Insn, Op};
use fab_elec_mcu_xtensa::sim::{Unaligned, cause, Config, Cpu, IntType, Stop, TrapMode, Vectors};

const RAM: u32 = 0x3fc8_8000;
const IRAM: u32 = 0x4037_8000;
const FLASH: u32 = 0x4200_0000;
const BREAK: u32 = 0x004000;
const RFE: u32 = 0x003000;
const NOP: u32 = 0x0020f0;

// The twelve formats in §9.1, PDF pp. 656–659. Parameters are encoding
// fields, not assembler operands. Signed values are truncated to field width.
fn rrr(op1: u32, op2: u32, r: u32, s: u32, t: u32) -> u32 {
    op2 << 20 | op1 << 16 | r << 12 | s << 8 | t << 4
}
fn rri4(op0: u32, op1: u32, imm4: u32, r: u32, s: u32, t: u32) -> u32 {
    imm4 << 20 | op1 << 16 | r << 12 | s << 8 | t << 4 | op0
}
fn rri8(op0: u32, r: u32, s: u32, t: u32, imm8: i32) -> u32 {
    (imm8 as u32 & 255) << 16 | r << 12 | s << 8 | t << 4 | op0
}
fn ri16(t: u32, imm16: u32) -> u32 { (imm16 & 65535) << 8 | t << 4 | 1 }
fn rsr(op1: u32, op2: u32, t: u32, sr: u32) -> u32 {
    op2 << 20 | op1 << 16 | sr << 8 | t << 4
}
fn call(n: u32, offset: i32) -> u32 { (offset as u32 & 0x3ffff) << 6 | n << 4 | 5 }
fn callx(n: u32, m: u32, s: u32) -> u32 { s << 8 | m << 6 | n << 4 }
fn entry(s: u32, bytes: u32) -> u32 { bri12(3, 0, s, (bytes / 8) as i32) }
fn retw(narrow: bool) -> (u32, u32) {
    if narrow { (rrrn(13, 15, 0, 1), 2) } else { (callx(1, 2, 0), 3) }
}
fn rotw(amount: i32) -> u32 { rrr(0, 4, 8, 0, amount as u32 & 15) }
fn window_mem(store: bool, t: u32, s: u32, offset: i32) -> u32 {
    rri4(0, 9, if store { 4 } else { 0 }, ((offset / 4) & 15) as u32, s, t)
}
fn rfw(underflow: bool) -> u32 { rrr(0, 0, 3, if underflow { 5 } else { 4 }, 0) }
fn bri8(n: u32, m: u32, s: u32, r: u32, offset: i32) -> u32 {
    (offset as u32 & 255) << 16 | r << 12 | s << 8 | m << 6 | n << 4 | 6
}
fn bri12(n: u32, m: u32, s: u32, offset: i32) -> u32 {
    (offset as u32 & 4095) << 12 | s << 8 | m << 6 | n << 4 | 6
}
fn rrrn(op0: u32, r: u32, s: u32, t: u32) -> u32 {
    r << 12 | s << 8 | t << 4 | op0
}
fn ri7(s: u32, imm7: i32) -> u32 {
    let imm7 = imm7 as u32 & 127;
    (imm7 & 15) << 12 | s << 8 | (imm7 >> 4) << 4 | 12
}
fn ri6(z: u32, s: u32, imm6: u32) -> u32 {
    (imm6 & 15) << 12 | s << 8 | 1 << 7 | z << 6 | (imm6 >> 4) << 4 | 12
}

/// The single board setup. RAM and IRAM alias the same 64 KiB byte store;
/// flash permits reads and execution only. Every test program ends in BREAK.
/// Both vector stubs contain RFE. Tests explicitly opt out of reset masking.
fn setup(program: &[(u32, u32)]) -> (Cpu, Memory, usize, usize) {
    let mut mem = Memory::new();
    let ram = mem.store(65536);
    let flash = mem.store(4096);
    mem.map(RAM, ram, 0, 65536, Perm { read: true, write: true, exec: false });
    mem.map(IRAM, ram, 0, 65536, Perm { read: true, write: true, exec: true });
    mem.map(FLASH, flash, 0, 4096, Perm { read: true, write: false, exec: true });
    let mut offset = 0;
    for &(bits, length) in program.iter().chain(std::iter::once(&(BREAK, 3))) {
        let bytes = bits.to_le_bytes();
        mem.bytes_mut(flash)[offset..offset + length as usize]
            .copy_from_slice(&bytes[..length as usize]);
        offset += length as usize;
    }
    for offset in [0x100, 0x180] {
        mem.bytes_mut(ram)[offset..offset + 3].copy_from_slice(&RFE.to_le_bytes()[..3]);
    }
    let mut int_type = [IntType::None; 32];
    let mut int_level = [0; 32];
    for (n, ty, level) in [
        (0, IntType::Level, 1), (1, IntType::Edge, 1),
        (2, IntType::Software, 1), (3, IntType::Timer(0), 1),
        (4, IntType::Timer(1), 1), (5, IntType::Timer(2), 1),
        (6, IntType::Level, 3), (31, IntType::Edge, 1),
    ] {
        int_type[n] = ty;
        int_level[n] = level;
    }
    let config = Config {
        mul32_high: true, div32: true, unaligned: Unaligned::Exception, excm_level: 1,
        mac16: true, s32c1i: true, threadptr: true,
        vecbase_reset: IRAM, vectors: Vectors { kernel: 0x100, user: 0x180, window: [0, 0x40, 0x80, 0xc0, 0x100, 0x140] },
        int_type, int_level, prid: Some(0x1234_5678),
    };
    let mut cpu = Cpu::new(config, FLASH);
    cpu.ps = 0;
    (cpu, mem, ram, flash)
}

fn decoded(bits: u32, len: u32, op: Op, r: u8, s: u8, t: u8, imm: i32) {
    let expected = Insn { op, r, s, t, imm, bits: bits & if len == 2 { 0xffff } else { 0xffffff } };
    assert_eq!(decode(bits), Ok((expected, len)), "decode fields: {op:?}, {bits:06x}");
    assert_eq!(decode(bits | 0xab00_0000), Ok((expected, len)), "high byte ignored: {op:?}");
    if len == 2 {
        assert_eq!(decode(bits | 0x005a_0000), Ok((expected, len)), "narrow ignores third byte: {op:?}");
    }
}

#[test]
fn decode_rrr_arithmetic_moves_and_multiply_divide() {
    use Op::*;
    // Tables 200, 216, 220, 221 (pp. 662, 666–668). SALT/U use the
    // unambiguous instruction diagrams pp. 599–600, not the duplicated label.
    for (op1, op2, op) in [
        (0, 1, And), (0, 2, Or), (0, 3, Xor),
        (0, 8, Add), (0, 9, Addx2), (0, 10, Addx4), (0, 11, Addx8),
        (0, 12, Sub), (0, 13, Subx2), (0, 14, Subx4), (0, 15, Subx8),
        (1, 8, Src), (2, 6, Saltu), (2, 7, Salt), (2, 8, Mull),
        (2, 10, Muluh), (2, 11, Mulsh), (2, 12, Quou), (2, 13, Quos),
        (2, 14, Remu), (2, 15, Rems), (3, 8, Moveqz), (3, 9, Movnez),
        (3, 10, Movltz), (3, 11, Movgez),
    ] {
        for r in [0, 1, 15] { for s in [0, 1, 15] { for t in [0, 1, 15] {
            decoded(rrr(op1, op2, r, s, t), 3, op, r as u8, s as u8, t as u8, 0);
        } } }
    }
    for (op, sf) in [(Neg, 0), (Abs, 1)] {
        for r in [0, 1, 15] { for t in [0, 1, 15] {
            decoded(rrr(0, 6, r, sf, t), 3, op, r as u8, sf as u8, t as u8, 0);
        } }
    }
    for (op, op2) in [(Srl, 9), (Sra, 11)] {
        for r in [0, 1, 15] { for t in [0, 1, 15] {
            decoded(rrr(1, op2, r, 0, t), 3, op, r as u8, 0, t as u8, 0);
        } }
    }
    for r in [0, 1, 15] { for s in [0, 1, 15] {
        decoded(rrr(1, 10, r, s, 0), 3, Sll, r as u8, s as u8, 0, 0);
    } }
}

#[test]
fn decode_immediates_loads_stores_and_shifts() {
    use Op::*;
    for (opcode, op, scale) in [
        (0, L8ui, 1), (1, L16ui, 2), (2, L32i, 4), (4, S8i, 1),
        (5, S16i, 2), (6, S32i, 4), (9, L16si, 2),
    ] {
        for s in [0, 1, 15] { for t in [0, 1, 15] { for imm in [0, 1, 0x55, 255] {
            decoded(rri8(2, opcode, s, t, imm), 3, op, opcode as u8, s as u8, t as u8, imm * scale);
        } } }
    }
    for (opcode, op, scale) in [(12, Addi, 1), (13, Addmi, 256)] {
        for s in [0, 1, 15] { for t in [0, 1, 15] { for imm in [-128, -1, 0, 1, 0x55, 127] {
            decoded(rri8(2, opcode, s, t, imm), 3, op, opcode as u8, s as u8, t as u8, imm * scale);
        } } }
    }
    for t in [0, 1, 15] { for imm in [-2048, -1, 0, 1, 0x555, 2047] {
        let s = (imm as u32 >> 8) & 15;
        decoded(rri8(2, 10, s, t, imm), 3, Movi, 10, s as u8, t as u8, imm);
    } }
    for t in [0, 1, 15] { for field in [0, 1, 0x5555, 0x8000, 0xffff] {
        decoded(ri16(t, field), 3, L32r, 0, 0, t as u8, (field as i32 - 65536) * 4);
    } }
    for (opcode, op) in [(0, Ssr), (1, Ssl), (2, Ssa8l), (3, Ssa8b)] {
        for s in [0, 1, 15] {
            decoded(rrr(0, 4, opcode, s, 0), 3, op, opcode as u8, s as u8, 0, 0);
        }
    }
    for sa in [0, 1, 15, 16, 21, 31] {
        decoded(rrr(0, 4, 4, sa & 15, sa >> 4), 3, Ssai, 4, (sa & 15) as u8, (sa >> 4) as u8, sa as i32);
        for r in [0, 1, 15] { for operand in [0, 1, 15] {
            decoded(rrr(1, 2 | (sa >> 4), r, sa & 15, operand), 3, Srai, r as u8, (sa & 15) as u8, operand as u8, sa as i32);
            if sa != 0 {
                decoded(rrr(1, sa >> 4, r, operand, sa & 15), 3, Slli, r as u8, operand as u8, (sa & 15) as u8, sa as i32);
            }
            if sa <= 15 {
                decoded(rrr(1, 4, r, sa, operand), 3, Srli, r as u8, sa as u8, operand as u8, sa as i32);
            }
            for mask in [0, 1, 5, 15] {
                if sa + mask <= 31 {
                    decoded(rrr(4 | (sa >> 4), mask, r, sa & 15, operand), 3, Extui, mask as u8, (sa & 15) as u8, operand as u8, sa as i32);
                }
            }
        } }
    }
}

#[test]
fn decode_narrow_instructions() {
    use Op::*;
    for r in [0, 1, 5, 15] { for s in [0, 1, 15] { for t in [0, 1, 5, 15] {
        for (opcode, op, imm) in [(8, L32iN, r * 4), (9, S32iN, r * 4), (10, AddN, 0)] {
            decoded(rrrn(opcode, r, s, t), 2, op, r as u8, s as u8, t as u8, imm as i32);
        }
        decoded(rrrn(11, r, s, t), 2, AddiN, r as u8, s as u8, t as u8, if t == 0 { -1 } else { t as i32 });
    } } }
    for s in [0, 1, 15] {
        for t in [0, 1, 15] { decoded(rrrn(13, 0, s, t), 2, MovN, 0, s as u8, t as u8, 0); }
        for imm in [-32, -1, 0, 1, 0x55, 95] { decoded(ri7(s, imm), 2, MoviN, 0, s as u8, 0, imm); }
        for (z, op) in [(0, BeqzN), (1, BnezN)] { for offset in [0, 1, 21, 63] {
            decoded(ri6(z, s, offset), 2, op, 0, s as u8, 0, offset as i32);
        } }
        decoded(rrrn(13, 15, s, 2), 2, BreakN, 15, s as u8, 2, 0);
    }
    for (t, op) in [(0, RetN), (1, RetwN), (3, NopN), (6, IllN)] {
        decoded(rrrn(13, 15, 0, t), 2, op, 15, 0, t as u8, 0);
    }
}

#[test]
fn decode_branches_calls_windowed_and_targets() {
    use Op::*;
    const SIGNED: [i32; 16] = [-1, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256];
    const UNSIGNED: [i32; 16] = [32768, 65536, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256];
    for pc in [0u32, 3, 0x4200_0023, 0xffff_fffd] {
        for (opcode, op) in [
            (0, Bnone), (1, Beq), (2, Blt), (3, Bltu), (4, Ball), (5, Bbc),
            (6, Bbci), (7, Bbci), (8, Bany), (9, Bne), (10, Bge), (11, Bgeu),
            (12, Bnall), (13, Bbs), (14, Bbsi), (15, Bbsi),
        ] {
            for s in [0, 1, 15] { for t in [0, 1, 15] { for off in [-128, -4, 1, 0x55, 127] {
                let bits = rri8(7, opcode, s, t, off);
                decoded(bits, 3, op, opcode as u8, s as u8, t as u8, off);
                assert_eq!(target(&decode(bits).unwrap().0, pc), Some(pc.wrapping_add(4).wrapping_add(off as u32)), "RRI8 branch target {op:?}");
            } } }
        }
        for (n, m, op) in [(2, 0, Beqi), (2, 1, Bnei), (2, 2, Blti), (2, 3, Bgei), (3, 2, Bltui), (3, 3, Bgeui)] {
            for r in 0..16 { for s in [0, 1, 15] { for off in [-128, -4, 1, 0x55, 127] {
                let bits = bri8(n, m, s, r, off);
                decoded(bits, 3, op, r as u8, s as u8, 0, if n == 2 { SIGNED[r as usize] } else { UNSIGNED[r as usize] });
                assert_eq!(target(&decode(bits).unwrap().0, pc), Some(pc.wrapping_add(4).wrapping_add(off as u32)), "BRI8 keeps displacement separately from table value");
            } } }
        }
        for (m, op) in [(0, Beqz), (1, Bnez), (2, Bltz), (3, Bgez)] {
            for s in [0, 1, 15] { for off in [-2048, -4, 1, 0x555, 2047] {
                let bits = bri12(1, m, s, off);
                decoded(bits, 3, op, 0, s as u8, 0, off);
                assert_eq!(target(&decode(bits).unwrap().0, pc), Some(pc.wrapping_add(4).wrapping_add(off as u32)), "BRI12 branch target");
            } }
        }
        for (z, op) in [(0, BeqzN), (1, BnezN)] { for off in [0, 1, 21, 63] {
            let bits = ri6(z, 15, off);
            decoded(bits, 2, op, 0, 15, 0, off as i32);
            assert_eq!(target(&decode(bits).unwrap().0, pc), Some(pc.wrapping_add(4).wrapping_add(off)), "narrow branch uses PC+4");
        } }
        for (n, op) in [(0, Call0), (1, Call4), (2, Call8), (3, Call12)] {
            for off in [-131072, -1, 0, 1, 0x15555, 131071] {
                let bits = call(n, off);
                decoded(bits, 3, op, 0, 0, n as u8, off * 4);
                assert_eq!(target(&decode(bits).unwrap().0, pc), Some((pc & !3).wrapping_add(4).wrapping_add((off * 4) as u32)), "CALL aligns PC before adding offset");
            }
        }
        for off in [-131072, -1, 0, 1, 0x15555, 131071] {
            let bits = (off as u32 & 0x3ffff) << 6 | 6;
            decoded(bits, 3, J, 0, 0, 0, off);
            assert_eq!(target(&decode(bits).unwrap().0, pc), Some(pc.wrapping_add(4).wrapping_add(off as u32)), "J uses unaligned PC+4");
        }
        for field in [0, 1, 0x5555, 65535] {
            let i = decode(ri16(1, field)).unwrap().0;
            assert_eq!(target(&i, pc), Some((pc.wrapping_add(3) & !3).wrapping_sub((65536 - field) * 4)), "L32R rounds PC+3 down before negative offset");
        }
    }
    for (n, op) in [(0, Callx0), (1, Callx4), (2, Callx8), (3, Callx12)] { for s in [0, 1, 15] {
        let bits = callx(n, 3, s);
        decoded(bits, 3, op, 0, s as u8, 0, 0);
        assert_eq!(target(&decode(bits).unwrap().0, FLASH), None, "register call has no PC-relative target");
    } }
    for (n, op) in [(0, Ret), (1, Retw)] { decoded(callx(n, 2, 0), 3, op, 0, 0, 0, 0); }
    for s in [0, 1, 15] {
        decoded(callx(2, 2, s), 3, Jx, 0, s as u8, 0, 0);
        for imm in [0, 1, 0x555, 4095] { decoded(bri12(3, 0, s, imm), 3, Entry, 0, s as u8, 0, imm * 8); }
        for t in [0, 1, 15] {
            decoded(rrr(0, 0, 1, s, t), 3, Movsp, 1, s as u8, t as u8, 0);
            for r in [0, 1, 5, 15] {
                decoded(rri4(0, 9, 0, r, s, t), 3, L32e, r as u8, s as u8, t as u8, (r as i32 - 16) * 4);
                decoded(rri4(0, 9, 4, r, s, t), 3, S32e, r as u8, s as u8, t as u8, (r as i32 - 16) * 4);
            }
        }
    }
    for imm in [-8i32, -1, 0, 1, 5, 7] {
        decoded(rrr(0, 4, 8, 0, imm as u32 & 15), 3, Rotw, 8, 0, (imm as u8) & 15, imm);
    }
}

#[test]
fn decode_control_and_special_register_rules() {
    use Op::*;
    decoded(0, 3, Ill, 0, 0, 0, 0);
    decoded(0x005000, 3, Syscall, 5, 0, 0, 0);
    for (t, op) in [(0, Isync), (1, Rsync), (2, Esync), (3, Dsync), (8, Excw), (12, Memw), (13, Extw), (15, Nop)] {
        decoded(rrr(0, 0, 2, 0, t), 3, op, 2, 0, t as u8, 0);
        assert_eq!(target(&decode(rrr(0, 0, 2, 0, t)).unwrap().0, FLASH), None, "synchronization has no target");
    }
    for s in 8..16 { decoded(rrr(0, 0, 2, s, 0), 3, Fsync, 2, s as u8, 0, (s & 7) as i32); }
    for s in [0, 1, 15] { for t in [0, 1, 15] {
        decoded(rrr(0, 0, 4, s, t), 3, Break, 4, s as u8, t as u8, 0);
        decoded(rrr(0, 0, 6, s, t), 3, Rsil, 6, s as u8, t as u8, s as i32);
    } }
    for level in [0, 1, 5, 15] {
        decoded(rrr(0, 0, 7, level, 0), 3, Waiti, 7, level as u8, 0, level as i32);
        decoded(rrr(0, 0, 3, level, 1), 3, Rfi, 3, level as u8, 1, level as i32);
    }
    for (s, op) in [(0, Rfe), (2, Rfde), (4, Rfwo), (5, Rfwu)] {
        decoded(rrr(0, 0, 3, s, 0), 3, op, 3, s as u8, 0, 0);
    }
    decoded(rrr(1, 15, 14, 0, 0), 3, Rfdo, 14, 0, 0, 0);
    for s in [0, 1] { decoded(rrr(1, 15, 14, s, 1), 3, Rfdd, 14, s as u8, 1, 0); }
    for sr in [0, 1, 2, 3, 12, 16, 17, 32, 33, 34, 35, 72, 73, 177, 209, 224, 226, 227, 228, 230, 231, 232, 234, 235, 238, 240, 241, 242] {
        for t in [0, 1, 15] {
            decoded(rsr(3, 0, t, sr), 3, Rsr, 0, 0, t as u8, sr as i32);
            if sr != 235 { decoded(rsr(3, 1, t, sr), 3, Wsr, 0, 0, t as u8, sr as i32); }
            if ![226, 227, 235].contains(&sr) { decoded(rsr(1, 6, t, sr), 3, Xsr, 0, 0, t as u8, sr as i32); }
        }
    }
    for (op1, op2) in [(3, 0), (3, 1), (1, 6)] {
        for sr in 0..256 {
            if ![0, 1, 2, 3, 4, 12, 16, 17, 32, 33, 34, 35, 72, 73, 177, 209, 224, 226, 227, 228, 230, 231, 232, 234, 235, 238, 240, 241, 242].contains(&sr) {
                assert_eq!(decode(rsr(op1, op2, 1, sr)), Err(()), "unimplemented special register {sr}");
            }
        }
    }
    for bits in [rsr(1, 6, 1, 226), rsr(1, 6, 1, 227), rsr(1, 6, 1, 235), rsr(3, 1, 1, 235)] {
        assert_eq!(decode(bits), Err(()), "forbidden special-register access {bits:06x}");
    }
}

#[test]
fn reserved_and_uncovered_encodings_are_rejected() {
    for (bits, property) in [
        (0x00000e, "reserved op0 E"), (0x00000f, "reserved op0 F"),
        (rrr(0, 7, 1, 1, 1), "reserved RST0"),
        (callx(0, 1, 0), "reserved SNM0 m=1"),
        (callx(3, 2, 0), "reserved JR n=3"),
        (callx(0, 2, 1), "RET requires s=0"),
        (rrr(0, 0, 2, 1, 1), "RSYNC requires s=0"),
        (rrr(0, 0, 5, 0, 1), "SYSCALL requires t=0"),
        (rrr(0, 0, 7, 1, 1), "WAITI requires t=0"),
        (rrr(0, 4, 0, 1, 1), "SSR requires t=0"),
        (rrr(0, 4, 4, 1, 2), "SSAI reserves t bits 3:1"),
        (rrr(0, 4, 8, 1, 0), "ROTW requires s=0"),
        (rrr(0, 6, 1, 2, 1), "NEG/ABS only s=0/1"),
        (rrr(1, 9, 1, 1, 1), "SRL requires s=0"),
        (rrr(1, 10, 1, 1, 1), "SLL requires t=0"),
        (rrr(1, 11, 1, 1, 1), "SRA requires s=0"),
        (rrr(1, 0, 1, 1, 0), "undefined SLLI sa=0 rejected"),
        (rrr(5, 15, 1, 15, 1), "EXTUI mask past bit 31 is reserved"),
        (rrrn(13, 1, 0, 0), "reserved ST3 narrow"),
        (rrrn(13, 15, 1, 0), "RET.N requires s=0"),
        (rrrn(13, 15, 1, 3), "NOP.N requires s=0"),
        (rrrn(13, 15, 2, 6), "reserved ILH"),
        (rrr(15, 0, 1, 1, 1), "double-precision ADD.D"),
        (bri8(3, 1, 1, 8, 5), "Loop Option"),
        (rrr(4, 7, 0, 1, 1) | 4, "MAC16 MUL.AA.LL"),
        (rrr(1, 12, 1, 1, 1), "16-bit multiply MUL16U"),
        (rrr(1, 13, 1, 1, 1), "16-bit multiply MUL16S"),
        (rrr(0, 4, 14, 1, 1), "Miscellaneous NSA"),
        (rrr(3, 2, 1, 1, 1), "Miscellaneous SEXT"),
        (rrr(3, 3, 1, 1, 1), "Miscellaneous CLAMPS"),
        (rrr(2, 5, 1, 1, 1), "reserved RST2 op2=5"),
        (rri8(2, 14, 1, 1, 1), "Conditional Store S32C1I"),
        (rsr(3, 0, 1, 5), "Extended L32R LITBASE"),
        (rrr(3, 14, 1, 1, 1), "unconfigured user register RUR"),
        (rrr(3, 15, 1, 1, 1), "unconfigured user register WUR"),
        (rsr(3, 0, 1, 233), "debug state DEBUGCAUSE"),
        (rsr(3, 0, 1, 109), "memory-error state MESR"),
        (rrr(0, 5, 4, 1, 0), "MMU IITLB"),
        (rsr(3, 0, 1, 92), "MMU/MPU state"),
        (rrr(6, 0, 1, 1, 1), "CUST0"),
        (rrr(7, 0, 1, 1, 1), "CUST1 / designer vector space"),
    ] {
        assert_eq!(decode(bits), Err(()), "{property}: {bits:06x}");
    }
    // No S3 vector opcode map exists in the permitted ISA document (p. 680
    // delegates custom space to the designer). Do not invent one for a test.
    for offset in [-3, -2, -1, 0] {
        for opcode in 0..16 { assert_eq!(decode(rri8(7, opcode, 1, 1, offset)), Err(()), "reserved RRI8 branch displacement {offset}"); }
        for m in 0..4 {
            assert_eq!(decode(bri8(2, m, 1, 1, offset)), Err(()), "reserved B4CONST branch displacement");
            assert_eq!(decode(bri12(1, m, 1, offset)), Err(()), "reserved BRI12 branch displacement");
        }
    }
}

#[test]
fn minmax_decode_execute_and_aliases() {
    // MIN/MAX diagrams pp. 489–492, RRR p. 656, RST3 p. 668.
    // Encode independently of emit32; unsigned extrema must select complete
    // operand words, including the sign bit, without a subtraction overflow.
    let (mut cpu, mut mem, _, flash) = setup(&[]);
    for s in [0u32, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX] {
        for t in [0u32, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX] {
            for (opcode, op, expected) in [
                (4, Op::Min, (s as i32).min(t as i32) as u32),
                (5, Op::Max, (s as i32).max(t as i32) as u32),
                (6, Op::Minu, s.min(t)), (7, Op::Maxu, s.max(t)),
            ] {
                for dst in [2, 3, 15] {
                    let bits = rrr(3, opcode, dst, 2, 3);
                    decoded(bits, 3, op, dst as u8, 2, 3, 0);
                    mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
                    cpu.pc = FLASH; cpu.ar[2] = s; cpu.ar[3] = t;
                    assert_eq!(cpu.step(&mut mem), Ok(()), "{op:?}: instruction retires");
                    assert_eq!(cpu.ar[dst as usize], expected, "{op:?}: signedness, tie and destination alias");
                    if dst != 2 { assert_eq!(cpu.ar[2], s, "{op:?}: first source preserved"); }
                    if dst != 3 { assert_eq!(cpu.ar[3], t, "{op:?}: second source preserved"); }
                }
            }
        }
    }
}

#[test]
fn arithmetic_logical_moves_and_multiply_corners() {
    let (mut cpu, mut mem, _, flash) = setup(&[]);
    for (s, t) in [(0u32, 0u32), (1, u32::MAX), (0x8000_0000, u32::MAX), (0x7fff_ffff, 2), (0xaaaa_5555, 0x5555_aaaa)] {
        for (op1, op2, expected) in [
            (0, 8, s.wrapping_add(t)), (0, 12, s.wrapping_sub(t)),
            (0, 9, (s << 1).wrapping_add(t)), (0, 10, (s << 2).wrapping_add(t)), (0, 11, (s << 3).wrapping_add(t)),
            (0, 13, (s << 1).wrapping_sub(t)), (0, 14, (s << 2).wrapping_sub(t)), (0, 15, (s << 3).wrapping_sub(t)),
            (0, 1, s & t), (0, 2, s | t), (0, 3, s ^ t),
            (2, 7, u32::from((s as i32) < (t as i32))), (2, 6, u32::from(s < t)),
            (2, 8, s.wrapping_mul(t)), (2, 10, ((s as u64 * t as u64) >> 32) as u32),
            (2, 11, ((s as i32 as i64 * t as i32 as i64) >> 32) as u32),
        ] {
            let bits = rrr(op1, op2, 15, 0, 1);
            mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
            cpu.pc = FLASH; cpu.ar[0] = s; cpu.ar[1] = t;
            assert_eq!(cpu.step(&mut mem), Ok(()), "arithmetic instruction retires {bits:06x}");
            assert_eq!(cpu.ar[15], expected, "arithmetic result {bits:06x}, s={s:08x}, t={t:08x}");
        }
    }
    for value in [0u32, 1, 0x8000_0000, u32::MAX] {
        for (sf, expected) in [(0, value.wrapping_neg()), (1, if value as i32 >= 0 { value } else { value.wrapping_neg() })] {
            let bits = rrr(0, 6, 15, sf, 1);
            mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
            cpu.pc = FLASH; cpu.ar[1] = value;
            assert_eq!(cpu.step(&mut mem), Ok(()), "NEG/ABS retires");
            assert_eq!(cpu.ar[15], expected, "NEG/ABS wraps at signed minimum");
        }
        for opcode in 8..12 {
            let bits = rrr(3, opcode, 15, 0, 1);
            mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
            cpu.pc = FLASH; cpu.ar[0] = 0x12345678; cpu.ar[1] = value; cpu.ar[15] = 0xcafe;
            let take = match opcode { 8 => value == 0, 9 => value != 0, 10 => (value as i32) < 0, _ => (value as i32) >= 0 };
            assert_eq!(cpu.step(&mut mem), Ok(()), "conditional move retires");
            assert_eq!(cpu.ar[15], if take { 0x12345678 } else { 0xcafe }, "conditional move preserves false destination");
        }
    }
    for opcode in [10, 11] {
        let bits = rrr(2, opcode, 15, 0, 1);
        mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
        cpu.pc = FLASH; cpu.config.mul32_high = false;
        let before = format!("{cpu:?}");
        assert_eq!(cpu.step(&mut mem), Err(Stop::Exception { cause: cause::ILLEGAL, pc: FLASH, vaddr: 0 }), "disabled Mul32High raises illegal");
        assert_eq!(format!("{cpu:?}"), before, "disabled multiply is atomic");
    }
}

#[test]
fn sign_extension_and_narrow_execution() {
    for imm in [-2048i32, -1, 0, 1, 0x555, 2047] {
        let (mut cpu, mut mem, _, _) = setup(&[(rri8(2, 10, (imm as u32 >> 8) & 15, 15, imm), 3)]);
        assert_eq!(cpu.run(&mut mem, 5), Stop::Break { pc: FLASH + 3 }, "MOVI program ends in BREAK");
        assert_eq!(cpu.ar[15], imm as u32, "MOVI sign extends twelve bits");
    }
    for imm in [-32, -1, 0, 1, 0x55, 95] {
        let (mut cpu, mut mem, _, _) = setup(&[(ri7(1, imm), 2), (rrrn(13, 0, 1, 15), 2), (rrrn(10, 0, 1, 15), 2), (rrrn(11, 0, 0, 0), 2)]);
        assert_eq!(cpu.run(&mut mem, 8), Stop::Break { pc: FLASH + 8 }, "narrow program ends in BREAK");
        assert_eq!(cpu.ar[1], imm as u32, "MOVI.N sign extends only 96..127");
        assert_eq!(cpu.ar[15], imm as u32, "MOV.N destination is t");
        assert_eq!(cpu.ar[0], (imm as u32).wrapping_mul(2).wrapping_sub(1), "ADD.N and ADDI.N -1");
    }
    for imm in [-128i32, -1, 0, 1, 0x55, 127] {
        let (mut cpu, mut mem, _, _) = setup(&[(rri8(2, 12, 1, 15, imm), 3), (rri8(2, 13, 15, 0, imm), 3)]);
        cpu.ar[1] = 0xffff_ff80;
        assert_eq!(cpu.run(&mut mem, 8), Stop::Break { pc: FLASH + 6 }, "ADDI/ADDMI program finishes");
        assert_eq!(cpu.ar[15], 0xffff_ff80u32.wrapping_add(imm as u32), "ADDI signed offset");
        assert_eq!(cpu.ar[0], cpu.ar[15].wrapping_add((imm * 256) as u32), "ADDMI sign extend then scale");
    }
    for imm in [1u32, 5, 15] {
        let (mut cpu, mut mem, _, _) = setup(&[(rrrn(11, 15, 1, imm), 2)]);
        cpu.ar[1] = u32::MAX;
        assert_eq!(cpu.run(&mut mem, 3), Stop::Break { pc: FLASH + 2 }, "positive ADDI.N completes");
        assert_eq!(cpu.ar[15], imm - 1, "positive ADDI.N wraps without sign extending the nibble");
    }
}

#[test]
fn shifts_through_sar_and_immediate_fields() {
    let (mut cpu, mut mem, _, flash) = setup(&[(NOP, 3), (NOP, 3)]);
    let hi = 0x8765_4321u32;
    let lo = 0xfedc_ba98u32;
    for value in [0u32, 1, 3, 15, 31, 32, u32::MAX] {
        for (setter, expected_sar) in [
            (rrr(0, 4, 0, 2, 0), value & 31),
            (rrr(0, 4, 1, 2, 0), 32 - (value & 31)),
            (rrr(0, 4, 2, 2, 0), (value & 3) * 8),
            (rrr(0, 4, 3, 2, 0), 32 - (value & 3) * 8),
            (rrr(0, 4, 4, value & 15, (value >> 4) & 1), value & 31),
        ] {
            for (shift, expected) in [
                (rrr(1, 10, 15, 0, 0), (((hi as u64) << 32) >> expected_sar) as u32),
                (rrr(1, 9, 15, 0, 1), ((lo as u64) >> expected_sar) as u32),
                (rrr(1, 11, 15, 0, 1), ((lo as i32 as i64) >> expected_sar) as u32),
                (rrr(1, 8, 15, 0, 1), ((((hi as u64) << 32) | lo as u64) >> expected_sar) as u32),
            ] {
                mem.bytes_mut(flash)[..3].copy_from_slice(&setter.to_le_bytes()[..3]);
                mem.bytes_mut(flash)[3..6].copy_from_slice(&shift.to_le_bytes()[..3]);
                cpu.pc = FLASH; cpu.ar[0] = hi; cpu.ar[1] = lo; cpu.ar[2] = value;
                assert_eq!(cpu.step(&mut mem), Ok(()), "SAR setup retires {setter:06x}");
                assert_eq!(cpu.sar, expected_sar, "SAR setter masks input and represents 32");
                assert_eq!(cpu.step(&mut mem), Ok(()), "SAR shift retires at {expected_sar}");
                assert_eq!(cpu.ar[15], expected, "funnel shift result {shift:06x} at SAR={expected_sar}");
                assert_eq!(cpu.step(&mut mem), Err(Stop::Break { pc: FLASH + 6 }), "shift program terminates");
            }
        }
    }
    for amount in [0u32, 1, 15, 16, 21, 31] {
        let mut cases = vec![(rrr(1, 2 | (amount >> 4), 15, amount & 15, 1), ((lo as i32) >> amount) as u32)];
        if amount <= 15 { cases.push((rrr(1, 4, 15, amount, 1), lo >> amount)); }
        if amount != 0 {
            let encoded = 32 - amount;
            cases.push((rrr(1, encoded >> 4, 15, 0, encoded & 15), hi << amount));
        }
        for (bits, expected) in cases {
            mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
            cpu.pc = FLASH; cpu.ar[0] = hi; cpu.ar[1] = lo;
            assert_eq!(cpu.step(&mut mem), Ok(()), "immediate shift retires");
            assert_eq!(cpu.ar[15], expected, "immediate shift {amount}, opcode {bits:06x}");
        }
        for width in [1u32, 2, 6, 16] {
            if amount + width <= 32 {
                for dest in [1, 15] {
                    let bits = rrr(4 | (amount >> 4), width - 1, dest, amount & 15, 1);
                    mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
                    cpu.pc = FLASH; cpu.ar[1] = lo;
                    assert_eq!(cpu.step(&mut mem), Ok(()), "EXTUI retires");
                    assert_eq!(cpu.ar[dest as usize], (lo >> amount) & ((1 << width) - 1), "EXTUI retains destination independently of mask");
                }
            }
        }
    }
    for sar in [33, 63] {
        for bits in [rrr(1, 10, 15, 0, 0), rrr(1, 9, 15, 0, 1), rrr(1, 11, 15, 0, 1), rrr(1, 8, 15, 0, 1)] {
            mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
            cpu.pc = FLASH; cpu.sar = sar;
            let before = format!("{cpu:?}");
            assert_eq!(cpu.step(&mut mem), Err(Stop::Unmodeled { pc: FLASH, what: "undefined shift: SAR > 32" }), "undefined SAR is explicit");
            assert_eq!(format!("{cpu:?}"), before, "undefined shift changes nothing");
        }
    }
}

#[test]
fn loads_stores_aliases_and_alignment() {
    let program = [
        (rri8(2, 4, 1, 0, 1), 3), // S8I a0,a1,1
        (rri8(2, 5, 1, 0, 2), 3), // S16I a0,a1,4
        (rri8(2, 6, 1, 0, 2), 3), // S32I a0,a1,8
        (rri8(2, 0, 1, 2, 1), 3),
        (rri8(2, 1, 1, 3, 2), 3),
        (rri8(2, 9, 1, 4, 2), 3),
        (rri8(2, 2, 1, 5, 2), 3),
        (rrrn(9, 15, 1, 0), 2), // S32I.N +60
        (rrrn(8, 15, 1, 15), 2),
    ];
    let (mut cpu, mut mem, ram, _) = setup(&program);
    cpu.ar[0] = 0x8765_fedc; cpu.ar[1] = RAM;
    assert_eq!(cpu.run(&mut mem, 20), Stop::Break { pc: FLASH + 25 }, "load/store program reaches BREAK");
    assert_eq!(cpu.ar[2], 0xdc, "L8UI zero extension");
    assert_eq!(cpu.ar[3], 0xfedc, "L16UI zero extension");
    assert_eq!(cpu.ar[4], 0xffff_fedc, "L16SI sign extension");
    assert_eq!(cpu.ar[5], 0x8765_fedc, "L32I word load");
    assert_eq!(cpu.ar[15], 0x8765_fedc, "L32I.N and S32I.N offset scaling");
    assert_eq!(&mem.bytes(ram)[4..6], &[0xdc, 0xfe], "S16I little endian");
    assert_eq!(&mem.bytes(ram)[8..12], &[0xdc, 0xfe, 0x65, 0x87], "S32I little endian");
    assert_eq!(mem.read(IRAM + 8, Width::W, Kind::Fetch, cpu.instret), Ok(0x8765_fedc), "instruction mapping aliases RAM");

    let (mut cpu, mut mem, ram, flash) = setup(&[(NOP, 3)]);
    for (opcode, offset, width_mask, store) in [(1, 1, 1, false), (9, 1, 1, false), (2, 3, 3, false), (5, 1, 1, true), (6, 3, 3, true)] {
        let bits = rri8(2, opcode, 1, 15, 0);
        mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
        mem.bytes_mut(ram)[0..4].copy_from_slice(&[0x78, 0x86, 0x34, 0x12]);
        cpu.pc = FLASH; cpu.ar[1] = RAM + offset; cpu.ar[15] = 0xaabb_ccdd;
        cpu.config.unaligned = Unaligned::Exception;
        let before = format!("{cpu:?}"); let memory_before = mem.bytes(ram).to_vec();
        assert_eq!(cpu.step(&mut mem), Err(Stop::Exception { cause: cause::LOAD_STORE_ALIGNMENT, pc: FLASH, vaddr: RAM + offset }), "unaligned exception precedes memory access");
        assert_eq!(format!("{cpu:?}"), before, "alignment fault preserves registers");
        assert_eq!(mem.bytes(ram), memory_before.as_slice(), "alignment fault preserves memory");
        cpu.config.unaligned = Unaligned::Ignore;
        assert_eq!(cpu.step(&mut mem), Ok(()), "without alignment option access rounds down");
        if store {
            assert_eq!(&mem.bytes(ram)[0..=width_mask], &0xaabb_ccddu32.to_le_bytes()[0..=width_mask], "unaligned store updates aligned containing item");
        } else {
            assert_eq!(cpu.ar[15], match opcode { 1 => 0x8678, 9 => 0xffff_8678, _ => 0x1234_8678 }, "unaligned load selects containing item");
        }
        mem.bytes_mut(ram)[0..8].copy_from_slice(&[0x78, 0x86, 0x34, 0x12, 0x9a, 0xbc, 0xde, 0xf0]);
        cpu.pc = FLASH; cpu.ar[15] = 0xaabb_ccdd;
        cpu.config.unaligned = Unaligned::Hardware;
        assert_eq!(cpu.step(&mut mem), Ok(()), "hardware-handled unaligned access completes");
        let o = offset as usize;
        if store {
            assert_eq!(&mem.bytes(ram)[o..=o + width_mask], &0xaabb_ccddu32.to_le_bytes()[0..=width_mask], "hardware unaligned store writes at the address itself");
        } else {
            let bytes = &mem.bytes(ram)[o..o + 4];
            let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            assert_eq!(cpu.ar[15], match opcode { 1 => word & 0xffff, 9 => word as u16 as i16 as i32 as u32, _ => word }, "hardware unaligned load reads at the address itself");
        }
    }
}

#[test]
fn literal_load_straddles_word_and_fetch_does_not_overread() {
    let (mut cpu, mut mem, ram, _) = setup(&[]);
    mem.bytes_mut(ram)[0x200..0x204].copy_from_slice(&0x89ab_cdefu32.to_le_bytes());
    // PC=...207, base=(PC+3)&~3=...208, offset=-8.
    mem.bytes_mut(ram)[0x207..0x20a].copy_from_slice(&ri16(15, 65534).to_le_bytes()[..3]);
    mem.bytes_mut(ram)[0x20a..0x20d].copy_from_slice(&BREAK.to_le_bytes()[..3]);
    cpu.pc = IRAM + 0x207;
    assert_eq!(cpu.run(&mut mem, 3), Stop::Break { pc: IRAM + 0x20a }, "L32R fetch straddles four-byte boundary");
    assert_eq!(cpu.ar[15], 0x89ab_cdef, "unaligned-PC literal base is correct");

    // A narrow instruction at the last two bytes of a mapping must not fetch
    // a third byte, even though the next step would fault.
    mem.bytes_mut(ram)[65534..].copy_from_slice(&(0xf03du16).to_le_bytes());
    cpu.pc = IRAM + 65534;
    assert_eq!(cpu.step(&mut mem), Ok(()), "narrow instruction at mapping end fetches exactly two bytes");
    assert_eq!(cpu.pc, IRAM + 65536, "narrow instruction advances by two");
    mem.bytes_mut(ram)[65534..].copy_from_slice(&(NOP as u16).to_le_bytes());
    cpu.pc = IRAM + 65534;
    let before = format!("{cpu:?}");
    assert_eq!(cpu.step(&mut mem), Err(Stop::Exception { cause: cause::INSTR_FETCH_ERROR, pc: IRAM + 65534, vaddr: IRAM + 65536 }), "wide fetch reports failing third byte");
    assert_eq!(format!("{cpu:?}"), before, "partial fetch does not retire");
}

#[test]
fn all_branch_conditions_taken_and_not_taken() {
    let (mut cpu, mut mem, _, flash) = setup(&[(NOP, 3)]);
    // Reuse one fixture. No run loop is needed: test both successor PCs.
    for (s, t) in [(0u32, 0u32), (0, 1), (1, 0), (u32::MAX, 1), (0x8000_0000, 0x7fff_ffff), (0x7fff_ffff, 0x8000_0000), (0x5555_aaaa, 0xaaaa_5555), (u32::MAX, 31)] {
        for (opcode, take) in [
            (0, s & t == 0), (1, s == t), (2, (s as i32) < (t as i32)), (3, s < t),
            (4, !s & t == 0), (5, s & (1 << (t & 31)) == 0),
            (8, s & t != 0), (9, s != t), (10, (s as i32) >= (t as i32)), (11, s >= t),
            (12, !s & t != 0), (13, s & (1 << (t & 31)) != 0),
        ] {
            let bits = rri8(7, opcode, 0, 1, 5);
            mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
            cpu.pc = FLASH; cpu.ar[0] = s; cpu.ar[1] = t;
            assert_eq!(cpu.step(&mut mem), Ok(()), "register branch retires");
            assert_eq!(cpu.pc, FLASH + if take { 9 } else { 3 }, "register branch predicate opcode={opcode} s={s:08x} t={t:08x}");
        }
    }
    for bit in [0u32, 1, 15, 16, 21, 31] { for set in [false, true] { for sense in [false, true] {
        let bits = rri8(7, if sense { 14 } else { 6 } | (bit >> 4), 15, bit & 15, -4);
        mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
        cpu.pc = FLASH; cpu.ar[15] = if set { 1 << bit } else { !(1 << bit) };
        assert_eq!(cpu.step(&mut mem), Ok(()), "immediate bit branch retires");
        assert_eq!(cpu.pc, FLASH + if sense == set { 0 } else { 3 }, "BBCI/BBSI includes high bit of bit index");
    } } }
    for s in [0u32, 1, u32::MAX, 0x8000_0000, 0x7fff_ffff] {
        for m in 0..4 {
            let bits = bri12(1, m, 15, 5);
            mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
            cpu.pc = FLASH; cpu.ar[15] = s;
            let take = match m { 0 => s == 0, 1 => s != 0, 2 => (s as i32) < 0, _ => (s as i32) >= 0 };
            assert_eq!(cpu.step(&mut mem), Ok(()), "zero branch retires");
            assert_eq!(cpu.pc, FLASH + if take { 9 } else { 3 }, "zero branch signed corners");
        }
        for z in [0, 1] {
            let bits = ri6(z, 15, 0);
            mem.bytes_mut(flash)[..2].copy_from_slice(&bits.to_le_bytes()[..2]);
            cpu.pc = FLASH; cpu.ar[15] = s;
            assert_eq!(cpu.step(&mut mem), Ok(()), "narrow zero branch offset zero is legal");
            assert_eq!(cpu.pc, FLASH + if (s == 0) == (z == 0) { 4 } else { 2 }, "narrow branch uses PC+4 on taken path");
        }
    }
    let signed: [i32; 16] = [-1, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256];
    let unsigned: [i32; 16] = [32768, 65536, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256];
    for r in 0..16 {
        for (n, m) in [(2, 0), (2, 1), (2, 2), (2, 3), (3, 2), (3, 3)] {
            let c = if n == 2 { signed[r as usize] } else { unsigned[r as usize] };
            for s in [(c as u32).wrapping_sub(1), c as u32, (c as u32).wrapping_add(1), 0x8000_0000, u32::MAX] {
                let bits = bri8(n, m, 15, r, 5);
                mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
                cpu.pc = FLASH; cpu.ar[15] = s;
                let take = match (n, m) {
                    (2, 0) => s == c as u32, (2, 1) => s != c as u32,
                    (2, 2) => (s as i32) < c, (2, 3) => (s as i32) >= c,
                    (3, 2) => s < c as u32, _ => s >= c as u32,
                };
                assert_eq!(cpu.step(&mut mem), Ok(()), "B4CONST branch retires");
                assert_eq!(cpu.pc, FLASH + if take { 9 } else { 3 }, "B4CONST/B4CONSTU table entry {r} n={n} m={m}");
            }
        }
    }
}

#[test]
fn divide_and_remainder_corners() {
    let (mut cpu, mut mem, _, flash) = setup(&[(NOP, 3)]);
    for opcode in 12..16 {
        let bits = rrr(2, opcode, 15, 0, 1);
        mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
        for (s, t) in [(0u32, 1u32), (7, 3), ((-7i32) as u32, 3), (7, (-3i32) as u32), ((-7i32) as u32, (-3i32) as u32), (0x8000_0000, u32::MAX), (u32::MAX, 1), (u32::MAX, 0x8000_0000)] {
            cpu.pc = FLASH; cpu.ar[0] = s; cpu.ar[1] = t;
            let expected = match opcode { 12 => s / t, 13 => ((s as i32 as i64) / (t as i32 as i64)) as u32, 14 => s % t, _ => ((s as i32 as i64) % (t as i32 as i64)) as u32 };
            assert_eq!(cpu.step(&mut mem), Ok(()), "divide/remainder retires");
            assert_eq!(cpu.ar[15], expected, "division truncates toward zero, remainder follows dividend, MIN/-1 wraps");
        }
        for enabled in [true, false] {
            cpu.pc = FLASH; cpu.ar[1] = 0; cpu.config.div32 = enabled;
            let before = format!("{cpu:?}");
            assert_eq!(cpu.step(&mut mem), Err(Stop::Exception { cause: if enabled { cause::DIVIDE_BY_ZERO } else { cause::ILLEGAL }, pc: FLASH, vaddr: 0 }), "disabled division precedes divide-by-zero check");
            assert_eq!(format!("{cpu:?}"), before, "division exception leaves all state unchanged");
        }
        cpu.config.div32 = true;
    }
}

#[test]
fn call0_callx0_and_returns() {
    for ret in [(callx(0, 2, 0), 3), (rrrn(13, 15, 0, 0), 2)] {
        let program = [(call(0, 1), 3), (BREAK, 3), (rrrn(13, 15, 0, 3), 2), (ri7(7, 55), 2), ret];
        let (mut cpu, mut mem, _, _) = setup(&program);
        assert_eq!(cpu.run(&mut mem, 10), Stop::Break { pc: FLASH + 3 }, "CALL0 function returns to caller BREAK");
        assert_eq!(cpu.ar[0], FLASH + 3, "CALL0 saves PC+3");
        assert_eq!(cpu.ar[7], 55, "CALL0 executes callee");
        assert_eq!(cpu.instret, 3, "CALL0, callee move, and return retire");
        for source in [0, 15] {
            let mut program = program;
            program[0] = (callx(0, 3, source), 3);
            let (mut cpu, mut mem, _, _) = setup(&program);
            cpu.ar[source as usize] = FLASH + 8;
            assert_eq!(cpu.run(&mut mem, 10), Stop::Break { pc: FLASH + 3 }, "CALLX0 executes indirect callee and returns");
            assert_eq!(cpu.ar[0], FLASH + 3, "CALLX0 reads target before overwriting a0");
            assert_eq!(cpu.ar[7], 55, "CALLX0 callee side effect");
        }
    }
    let (mut cpu, mut mem, _, _) = setup(&[(callx(2, 2, 15), 3)]);
    cpu.ar[15] = FLASH + 3;
    assert_eq!(cpu.run(&mut mem, 5), Stop::Break { pc: FLASH + 3 }, "JX uses its source register");
}

#[test]
fn unsupported_returns_stop_without_changes() {
    for (bits, len, what) in [
        (rrr(0, 0, 3, 2, 0), 3, "RFDE"),
        (rrr(1, 15, 14, 0, 0), 3, "RFDO"),
        (rrr(1, 15, 14, 0, 1), 3, "RFDD"),
        (rrr(1, 15, 14, 1, 1), 3, "RFDD"),
        (rrr(0, 0, 3, 3, 1), 3, "RFI"),
    ] {
        for traps in [TrapMode::Stop, TrapMode::Take] {
            let (mut cpu, mut mem, ram, flash) = setup(&[(bits, len)]);
            cpu.traps = traps; cpu.ps = 1 << 18;
            cpu.ar[0] = 0xabcdef; cpu.ar[1] = RAM; cpu.ar[15] = 0x1234;
            let before = format!("{cpu:?}"); let memory_before = mem.bytes(ram).to_vec(); let code_before = mem.bytes(flash).to_vec();
            assert_eq!(cpu.step(&mut mem), Err(Stop::Unmodeled { pc: FLASH, what }), "named stop for {what}");
            assert_eq!(format!("{cpu:?}"), before, "{what} preserves every CPU field");
            assert_eq!(mem.bytes(ram), memory_before.as_slice(), "{what} preserves RAM");
            assert_eq!(mem.bytes(flash), code_before.as_slice(), "{what} preserves flash");
        }
    }
}

#[test]
fn level_interrupts_use_both_vectors_in_both_trap_modes() {
    for traps in [TrapMode::Stop, TrapMode::Take] { for um in [0, 32] {
        let (mut cpu, mut mem, _, _) = setup(&[(NOP, 3)]);
        cpu.traps = traps; cpu.ps = um; cpu.intenable = 1;
        cpu.excvaddr = 0x12345678; cpu.excsave1 = 0xabcdef01;
        cpu.set_lines(1);
        assert_eq!(cpu.step(&mut mem), Ok(()), "level-1 interrupt enters in either trap mode");
        assert_eq!(cpu.pc, IRAM + if um == 0 { 0x100 } else { 0x180 }, "interrupt chooses vector using original UM");
        assert_eq!(cpu.epc1, FLASH, "interrupt EPC1 is the instruction not yet executed");
        assert_eq!(cpu.exccause, cause::LEVEL1_INTERRUPT, "interrupt cause is four");
        assert_eq!(cpu.ps, um | 16, "interrupt sets EXCM and preserves INTLEVEL/UM");
        assert_eq!(cpu.excvaddr, 0x12345678, "interrupt leaves EXCVADDR unchanged");
        assert_eq!(cpu.excsave1, 0xabcdef01, "interrupt leaves software scratch unchanged");
        assert_eq!((cpu.instret, cpu.ccount), (0, 0), "interrupt entry is not retirement");
        assert_eq!(cpu.interrupt & 1, 1, "taking an interrupt does not acknowledge it");
        cpu.set_lines(0);
        assert_eq!(cpu.interrupt & 1, 0, "level bit follows falling input");
        assert_eq!(cpu.step(&mut mem), Ok(()), "RFE returns from level-1 handler");
        assert_eq!((cpu.pc, cpu.ps), (FLASH, um), "RFE restores PC and clears only EXCM");
        assert_eq!(cpu.run(&mut mem, 3), Stop::Break { pc: FLASH + 3 }, "interrupted instruction resumes");
    } }
    let (mut cpu, mut mem, _, _) = setup(&[(rrr(0, 0, 6, 15, 15), 3), (NOP, 3)]);
    cpu.ps = 32;
    assert_eq!(cpu.step(&mut mem), Ok(()), "RSIL 15 retires");
    assert_eq!(cpu.ar[15], 32, "RSIL returns complete old PS");
    assert_eq!(cpu.ps, 47, "RSIL replaces just INTLEVEL");
    cpu.intenable = 1; cpu.set_lines(1);
    assert_eq!(cpu.step(&mut mem), Ok(()), "RSIL 15 masks pending level-1 interrupt");
    assert_eq!(cpu.pc, FLASH + 6, "masked interrupt allows next instruction");

    let (mut cpu, mut mem, _, _) = setup(&[(NOP, 3)]);
    cpu.intenable = (1 << 6) | 1; cpu.set_lines((1 << 6) | 1);
    let before = format!("{cpu:?}");
    assert_eq!(cpu.step(&mut mem), Err(Stop::Unmodeled { pc: FLASH, what: "level-3 interrupt" }), "highest enabled pending interrupt wins");
    assert_eq!(format!("{cpu:?}"), before, "high-priority stop preserves state");
    cpu.ps = 3;
    assert_eq!(cpu.step(&mut mem), Ok(()), "INTLEVEL masks higher-priority interrupt too");
    assert_eq!(cpu.pc, FLASH + 3, "masked level-3 allows instruction");

    // PS.EXCM masks up to EXCMLEVEL (p. 134): the S3's 3 masks a level-3 source, 1 does not.
    for (excm_level, masked) in [(3, true), (1, false)] {
        let (mut cpu, mut mem, _, _) = setup(&[(NOP, 3)]);
        cpu.config.excm_level = excm_level;
        cpu.ps = 16;
        cpu.intenable = 1 << 6; cpu.set_lines(1 << 6);
        let outcome = cpu.step(&mut mem);
        if masked {
            assert_eq!(outcome, Ok(()), "EXCMLEVEL {excm_level}: EXCM masks the level-3 interrupt");
        } else {
            assert_eq!(outcome, Err(Stop::Unmodeled { pc: FLASH, what: "level-3 interrupt" }), "EXCMLEVEL {excm_level}: EXCM does not mask level 3");
        }
    }
}

#[test]
fn edge_and_software_interrupt_write_rules() {
    let (mut cpu, mut mem, _, flash) = setup(&[(rsr(3, 1, 15, 227), 3)]);
    cpu.ar[15] = u32::MAX;
    cpu.set_lines(0x80000003);
    cpu.set_lines(0);
    assert_eq!(cpu.interrupt, 0x80000002, "falling edge preserves edge latches but clears level");
    assert_eq!(cpu.step(&mut mem), Ok(()), "INTCLEAR write retires");
    assert_eq!(cpu.interrupt, 0, "INTCLEAR acknowledges edge interrupts including bit 31");
    cpu.set_lines(2); cpu.pc = FLASH;
    assert_eq!(cpu.step(&mut mem), Ok(()), "clear edge while input remains high");
    cpu.set_lines(2);
    assert_eq!(cpu.interrupt, 0, "unchanged high input does not relatch edge");
    cpu.set_lines(0); cpu.set_lines(2);
    assert_eq!(cpu.interrupt, 2, "next rising edge relatches");

    mem.bytes_mut(flash)[..3].copy_from_slice(&rsr(3, 1, 15, 226).to_le_bytes()[..3]);
    cpu.pc = FLASH; cpu.set_lines(1);
    assert_eq!(cpu.step(&mut mem), Ok(()), "INTSET write retires");
    assert_eq!(cpu.interrupt, 7, "INTSET affects only software bits, retains level and edge");
    cpu.intenable = 1 << 2;
    assert_eq!(cpu.step(&mut mem), Ok(()), "software interrupt is taken next boundary");
    assert_eq!(cpu.exccause, cause::LEVEL1_INTERRUPT, "software interrupt uses level-1 cause");
    assert_eq!(cpu.epc1, FLASH + 3, "software interrupt interrupts next instruction");
    cpu.intenable = 0; cpu.ps = 0; cpu.pc = FLASH;
    mem.bytes_mut(flash)[..3].copy_from_slice(&rsr(3, 1, 15, 227).to_le_bytes()[..3]);
    assert_eq!(cpu.step(&mut mem), Ok(()), "software/edge acknowledge write retires");
    assert_eq!(cpu.interrupt, 1, "INTCLEAR clears software and edge but not level");
}

#[test]
fn timers_match_wrap_clear_and_wake_waiti() {
    let (mut cpu, mut mem, _, flash) = setup(&[(NOP, 3)]);
    cpu.ccount = u32::MAX - 1;
    cpu.ccompare = [0, u32::MAX, 5];
    cpu.intenable = (1 << 3) | (1 << 4) | (1 << 5);
    cpu.ps = 15;
    assert_eq!(cpu.next_timer(), Some(1), "nearest enabled timer wins across wrap");
    assert_eq!(cpu.step(&mut mem), Ok(()), "retirement advances timer");
    assert_eq!(cpu.interrupt, 1 << 4, "CCOUNT match latches timer 1");
    assert_eq!(cpu.next_timer(), Some(1), "equal comparator does not hide next positive event");
    cpu.advance(1);
    assert_eq!(cpu.ccount, 0, "CCOUNT wraps");
    assert_eq!(cpu.interrupt, (1 << 3) | (1 << 4), "wrapped match latches timer 0");
    cpu.advance(6);
    assert_eq!(cpu.interrupt, (1 << 3) | (1 << 4) | (1 << 5), "advance catches passed comparator 2");
    assert_eq!(cpu.instret, 1, "advance does not retire instructions");
    cpu.pc = FLASH; cpu.ar[15] = u32::MAX;
    mem.bytes_mut(flash)[..3].copy_from_slice(&rsr(3, 1, 15, 227).to_le_bytes()[..3]);
    assert_eq!(cpu.step(&mut mem), Ok(()), "INTCLEAR against timer bits retires");
    assert_eq!(cpu.interrupt, 0x38, "INTCLEAR cannot clear timer interrupts");
    for timer in 0..3 {
        cpu.pc = FLASH; cpu.ar[15] = 100 + timer;
        mem.bytes_mut(flash)[..3].copy_from_slice(&rsr(3, 1, 15, 240 + timer).to_le_bytes()[..3]);
        assert_eq!(cpu.step(&mut mem), Ok(()), "writing CCOMPARE retires");
        assert_eq!(cpu.interrupt & (1 << (3 + timer)), 0, "CCOMPARE write clears its timer interrupt");
        assert_eq!(cpu.ccompare[timer as usize], 100 + timer, "CCOMPARE stores written value");
    }
    cpu.intenable = 0;
    assert_eq!(cpu.next_timer(), None, "no enabled timer means no scheduled timer");
    cpu.intenable = 1 << 3; cpu.ccompare[0] = cpu.ccount;
    assert_eq!(cpu.next_timer(), Some(0), "zero distance denotes full 2^32 wrap");
    let count = cpu.ccount;
    cpu.advance(0);
    assert_eq!((cpu.ccount, cpu.interrupt), (count, 0), "zero advance does not fabricate a comparator match");
    cpu.advance(u32::MAX);
    assert_eq!(cpu.interrupt & (1 << 3), 0, "full wrap match has not happened one tick early");
    cpu.advance(1);
    assert_ne!(cpu.interrupt & (1 << 3), 0, "full wrap eventually matches equal comparator");

    let (mut cpu, mut mem, _, _) = setup(&[(rrr(0, 0, 7, 0, 0), 3)]);
    cpu.ps = 15; cpu.intenable = 1 << 3; cpu.ccompare[0] = 10;
    assert_eq!(cpu.step(&mut mem), Err(Stop::Waiti { pc: FLASH + 3 }), "WAITI retires then stops at successor");
    assert_eq!((cpu.instret, cpu.ccount, cpu.ps), (1, 1, 0), "WAITI updates INTLEVEL and retires exactly once");
    assert_eq!(cpu.next_timer(), Some(9), "sleep event queue distance");
    let before = format!("{cpu:?}");
    assert_eq!(cpu.step(&mut mem), Err(Stop::Waiti { pc: FLASH + 3 }), "sleep persists without enabled pending interrupt");
    assert_eq!(format!("{cpu:?}"), before, "polling WAITI does not advance time");
    cpu.advance(cpu.next_timer().unwrap());
    assert_eq!(cpu.step(&mut mem), Ok(()), "timer wakes WAITI at next boundary");
    assert_eq!((cpu.pc, cpu.epc1, cpu.exccause), (IRAM + 0x100, FLASH + 3, cause::LEVEL1_INTERRUPT), "WAITI interrupt saves successor PC");
    assert_eq!((cpu.instret, cpu.ccount), (1, 10), "wakeup entry does not retire");
    cpu.intenable = 0;
    assert_eq!(cpu.step(&mut mem), Ok(()), "RFE clears the sleep latch through interrupt return");
    assert_eq!(cpu.step(&mut mem), Err(Stop::Break { pc: FLASH + 3 }), "WAITI resumes after its instruction");
}

#[test]
fn precise_exceptions_in_stop_and_take_modes() {
    for (bits, len, fault_pc, address, fault_cause) in [
        (0, 3, FLASH, None, cause::ILLEGAL),
        (rrrn(13, 15, 0, 6), 2, FLASH, None, cause::ILLEGAL),
        (0x005000, 3, FLASH, None, cause::SYSCALL),
        (rri8(2, 6, 1, 15, 0), 3, FLASH, Some(FLASH), cause::LOAD_STORE_ERROR),
        (rri8(2, 2, 1, 15, 0), 3, FLASH, Some(0x1000), cause::LOAD_STORE_ERROR),
        (NOP, 3, 0x1000, Some(0x1000), cause::INSTR_FETCH_ERROR),
        (rrr(2, 13, 15, 0, 2), 3, FLASH, None, cause::DIVIDE_BY_ZERO),
        (rri8(2, 6, 1, 15, 0), 3, FLASH, Some(RAM + 1), cause::LOAD_STORE_ALIGNMENT),
        (rsr(3, 0, 15, 255), 3, FLASH, None, cause::ILLEGAL),
        (rrr(15, 0, 15, 0, 2), 3, FLASH, None, cause::ILLEGAL),
    ] {
        for traps in [TrapMode::Stop, TrapMode::Take] { for um in [0, 32] {
            let (mut cpu, mut mem, ram, flash) = setup(&[(bits, len)]);
            cpu.pc = fault_pc; cpu.traps = traps; cpu.ps = um | (1 << 18) | 0xc0;
            cpu.ar[0] = 0x8000_0000; cpu.ar[1] = address.unwrap_or(RAM); cpu.ar[2] = 0; cpu.ar[15] = 0xabcdef12;
            cpu.excvaddr = 0x12345678; cpu.epc1 = 0x87654321; cpu.exccause = 42;
            cpu.excsave1 = 0xabcdef; cpu.instret = 9; cpu.ccount = 17;
            let before = format!("{cpu:?}"); let before_ar = cpu.ar;
            let before_ram = mem.bytes(ram).to_vec(); let before_flash = mem.bytes(flash).to_vec();
            let old_ps = cpu.ps;
            if traps == TrapMode::Stop {
                assert_eq!(cpu.step(&mut mem), Err(Stop::Exception { cause: fault_cause, pc: fault_pc, vaddr: address.unwrap_or(0) }), "precise stop reports cause and original PC/address");
                assert_eq!(format!("{cpu:?}"), before, "TrapMode::Stop preserves all CPU state");
            } else {
                assert_eq!(cpu.step(&mut mem), Ok(()), "TrapMode::Take enters handler");
                assert_eq!(cpu.pc, IRAM + if um == 0 { 0x100 } else { 0x180 }, "exception chooses kernel/user vector");
                assert_eq!(cpu.epc1, fault_pc, "exception saves faulting PC");
                assert_eq!(cpu.exccause, fault_cause, "exception saves Table 65 cause");
                assert_eq!(cpu.excvaddr, address.unwrap_or(0x12345678), "EXCVADDR changes only for address causes");
                assert_eq!(cpu.ps, old_ps | 16, "exception only sets EXCM in PS");
                assert_eq!(cpu.excsave1, 0xabcdef, "EXCSAVE1 is software scratch");
                assert_eq!(cpu.ar, before_ar, "fault never writes destination registers");
                assert_eq!((cpu.instret, cpu.ccount), (9, 17), "exception does not retire or tick");
                assert_eq!(cpu.step(&mut mem), Ok(()), "RFE retires in exception handler");
                assert_eq!((cpu.pc, cpu.ps), (fault_pc, old_ps), "RFE restores faulting PC, clears EXCM, preserves UM/WOE/RING");
                assert_eq!((cpu.instret, cpu.ccount), (10, 18), "RFE retires once");
            }
            assert_eq!(mem.bytes(ram), before_ram.as_slice(), "exception preserves RAM");
            assert_eq!(mem.bytes(flash), before_flash.as_slice(), "faulting store cannot mutate flash");
            cpu.pc = fault_pc; cpu.ps |= 16;
            let before_double = format!("{cpu:?}");
            assert_eq!(cpu.step(&mut mem), Err(Stop::Unmodeled { pc: fault_pc, what: "double exception" }), "fault in EXCM stops as double exception in either mode");
            assert_eq!(format!("{cpu:?}"), before_double, "double exception stop changes nothing");
        } }
    }
    for traps in [TrapMode::Stop, TrapMode::Take] { for (bits, len) in [(BREAK, 3), (rrrn(13, 15, 15, 2), 2)] {
        let (mut cpu, mut mem, _, _) = setup(&[(bits, len)]);
        cpu.traps = traps; cpu.ps = 31;
        let before = format!("{cpu:?}");
        assert_eq!(cpu.step(&mut mem), Err(Stop::Break { pc: FLASH }), "BREAK always stops even in EXCM and at INTLEVEL 15");
        assert_eq!(format!("{cpu:?}"), before, "BREAK does not retire or enter a vector");
    } }
}

#[test]
fn special_register_storage_masks_exchange_and_read_only() {
    for (sr, value, expected) in [
        (3, u32::MAX, 63), (72, 15, 15), (73, 65535, 65535),
        (177, 0x12345678, 0x12345678), (209, 0xabcdef01, 0xabcdef01),
        (224, 0x123456ab, 0xab), (228, u32::MAX, 0x8000007f),
        (230, u32::MAX, 0x00070fff), (231, 0x42000101, 0x42000100),
        (232, u32::MAX, 63), (238, 0x12345678, 0x12345678),
        (240, 0x12345678, 0x12345678), (241, 0x12345678, 0x12345678), (242, 0x12345678, 0x12345678),
    ] {
        let (mut cpu, mut mem, _, _) = setup(&[(rsr(3, 1, 1, sr), 3), (rsr(3, 0, 15, sr), 3)]);
        cpu.ar[1] = value;
        assert_eq!(cpu.run(&mut mem, 5), Stop::Break { pc: FLASH + 6 }, "WSR/RSR storage program completes sr={sr}");
        assert_eq!(cpu.ar[15], expected, "special register width/write mask sr={sr}");
    }
    // XSR.WINDOWBASE rotates: its old-window destination is covered by
    // windowed_instruction_table instead of this plain-storage sequence.
    for sr in [3, 73, 177, 209, 224, 228, 230, 231, 232, 238, 240, 241, 242] {
        let (mut cpu, mut mem, _, _) = setup(&[(rsr(3, 1, 0, sr), 3), (rsr(1, 6, 15, sr), 3), (rsr(3, 0, 1, sr), 3)]);
        // Values legal in every listed register, including PS and VECBASE.
        cpu.ar[0] = 2; cpu.ar[15] = 4;
        assert_eq!(cpu.run(&mut mem, 5), Stop::Break { pc: FLASH + 9 }, "XSR sequence completes sr={sr}");
        assert_eq!(cpu.ar[15], 2, "XSR returns old special-register value sr={sr}");
        assert_eq!(cpu.ar[1], 4, "XSR stores old AR operand sr={sr}");
    }
    let (mut cpu, mut mem, _, _) = setup(&[(rsr(3, 0, 15, 235), 3), (rsr(3, 0, 1, 227), 3)]);
    assert_eq!(cpu.run(&mut mem, 5), Stop::Break { pc: FLASH + 6 }, "PRID and INTCLEAR reads complete");
    assert_eq!(Some(cpu.ar[15]), cpu.config.prid, "PRID reads configured identity");
    assert_eq!(cpu.ar[1], 0, "undefined INTCLEAR read deterministically returns zero");
    for bits in [rsr(3, 1, 15, 235), rsr(1, 6, 15, 235), rsr(1, 6, 15, 226), rsr(1, 6, 15, 227), rsr(3, 0, 15, 255)] {
        let (mut cpu, mut mem, _, _) = setup(&[(bits, 3)]);
        let before = format!("{cpu:?}");
        assert_eq!(cpu.step(&mut mem), Err(Stop::Exception { cause: cause::ILLEGAL, pc: FLASH, vaddr: 0 }), "illegal special-register access faults");
        assert_eq!(format!("{cpu:?}"), before, "illegal special access is atomic");
    }
    let (mut cpu, mut mem, _, _) = setup(&[(rsr(3, 1, 0, 234), 3), (rsr(3, 0, 1, 234), 3), (rsr(1, 6, 15, 234), 3)]);
    cpu.ar[0] = 100; cpu.ar[15] = 200;
    assert_eq!(cpu.run(&mut mem, 5), Stop::Break { pc: FLASH + 9 }, "CCOUNT write/read/exchange program completes");
    assert_eq!(cpu.ar[1], 101, "CCOUNT read sees preceding write and retirement tick");
    assert_eq!(cpu.ar[15], 102, "XSR.CCOUNT returns count before its own retirement");
    assert_eq!(cpu.ccount, 201, "CCOUNT replacement is followed by retirement tick");
    assert_eq!(cpu.instret, 3, "CCOUNT writes do not change instret");

    let (mut cpu, mut mem, _, _) = setup(&[(rsr(1, 6, 15, 240), 3)]);
    cpu.ccompare[0] = 50; cpu.interrupt = 1 << 3; cpu.ar[15] = 100;
    assert_eq!(cpu.step(&mut mem), Ok(()), "XSR.CCOMPARE allowed");
    assert_eq!((cpu.ar[15], cpu.ccompare[0], cpu.interrupt), (50, 100, 0), "XSR.CCOMPARE reads old value and clears timer on write");
}

#[test]
fn reset_state_and_exact_run_limits() {
    let (cpu, mut mem, _, _) = setup(&[(NOP, 3)]);
    let reset = Cpu::new(cpu.config, 0x12345678);
    assert_eq!(reset.pc, 0x12345678, "reset PC is caller supplied");
    assert_eq!(reset.ps, 31, "reset PS has INTLEVEL 15 and EXCM set");
    assert_eq!((reset.window_base, reset.window_start), (0, 1), "specified window reset hygiene");
    assert_eq!(reset.vecbase, IRAM, "configured VECBASE reset");
    assert_eq!(reset.intenable, 0, "reset interrupt enables clear");
    assert_eq!(reset.traps, TrapMode::Stop, "default trap mode is Stop");
    assert_eq!((reset.instret, reset.ccount), (0, 0), "reset counters are deterministic");
    let mut cpu = cpu;
    assert_eq!(cpu.run(&mut mem, 0), Stop::Limit, "zero budget stops without fetching");
    assert_eq!((cpu.pc, cpu.instret), (FLASH, 0), "zero limit changes no execution state");
    assert_eq!(cpu.run(&mut mem, 1), Stop::Limit, "limit wins after exactly one retirement");
    assert_eq!((cpu.pc, cpu.instret, cpu.ccount), (FLASH + 3, 1, 1), "one-retirement limit state");
    assert_eq!(cpu.run(&mut mem, 10), Stop::Break { pc: FLASH + 3 }, "run returns first stop");

    // J -4 targets itself. A small deterministic bound exercises repeated run.
    let jump = ((-4i32) as u32 & 0x3ffff) << 6 | 6;
    let (mut cpu, mut mem, _, _) = setup(&[(jump, 3)]);
    cpu.ccount = u32::MAX - 4;
    assert_eq!(cpu.run(&mut mem, 100), Stop::Limit, "loop stops at retirement budget");
    assert_eq!((cpu.pc, cpu.instret, cpu.ccount), (FLASH, 100, 95), "run counts retirements and wraps CCOUNT");
    assert_eq!(cpu.run(&mut mem, 7), Stop::Limit, "second run has a fresh relative budget");
    assert_eq!((cpu.instret, cpu.ccount), (107, 102), "repeated run counts remain exact");

    let syncs = [0x002000, 0x002010, 0x002020, 0x002030, 0x002800, 0x002080, 0x0020c0, 0x0020d0, NOP];
    let code: Vec<_> = syncs.into_iter().map(|bits| (bits, 3)).collect();
    let (mut cpu, mut mem, _, _) = setup(&code);
    assert_eq!(cpu.run(&mut mem, 20), Stop::Break { pc: FLASH + 27 }, "synchronizations are ordered no-ops");
    assert_eq!((cpu.instret, cpu.ccount), (9, 9), "each synchronization retires once");
}

struct AccessLog {
    memory: Memory,
    // address, width in bytes, kind (fetch/load/store), retired time
    accesses: Vec<(u32, u8, u8, u64)>,
}

impl Bus for AccessLog {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, now: u64) -> Result<u32, ()> {
        let size = match &width { Width::B => 1, Width::H => 2, Width::W => 4 };
        let tag = match &kind { Kind::Fetch => 0, Kind::Load => 1, Kind::Store => 2 };
        self.accesses.push((addr, size, tag, now));
        self.memory.read(addr, width, kind, now)
    }
    fn write(&mut self, addr: u32, width: Width, value: u32, now: u64) -> Result<(), ()> {
        let size = match &width { Width::B => 1, Width::H => 2, Width::W => 4 };
        self.accesses.push((addr, size, 2, now));
        self.memory.write(addr, width, value, now)
    }
}

#[test]
fn bus_access_kind_width_and_retired_timestamp() {
    let program = [(rrrn(13, 15, 0, 3), 2), (rri8(2, 2, 1, 15, 0), 3), (rri8(2, 4, 1, 15, 4), 3)];
    let (mut cpu, mut memory, ram, _) = setup(&program);
    memory.bytes_mut(ram)[..4].copy_from_slice(&0x12345678u32.to_le_bytes());
    cpu.ar[1] = RAM;
    let mut bus = AccessLog { memory, accesses: Vec::new() };
    assert_eq!(cpu.run(&mut bus, 3), Stop::Limit, "traced program retires three instructions");
    assert_eq!(bus.accesses, vec![
        (FLASH, 1, 0, 0), (FLASH + 1, 1, 0, 0),
        (FLASH + 2, 1, 0, 1), (FLASH + 3, 1, 0, 1), (FLASH + 4, 1, 0, 1), (RAM, 4, 1, 1),
        (FLASH + 5, 1, 0, 2), (FLASH + 6, 1, 0, 2), (FLASH + 7, 1, 0, 2), (RAM + 4, 1, 2, 2),
    ], "bus receives exact bytes, access kinds, widths, and pre-retirement timestamps");
}

// New RRR diagrams use [op2:23..20, op1:19..16, r:15..12,
// s:11..8, t:7..4, op0=0:3..0] (p. 656). These tuples are transcribed
// from the cited instruction pages, independently of decode().
fn fp_words() -> Vec<(u32, Op, u8, u8, u8, i32)> {
    use Op::*;
    let mut words = Vec::new();
    for (op2, op) in [
        (0, AddS), // p. 328: 0000 1010 r s t 0000
        (1, SubS), // p. 628: 0001 1010 r s t 0000
        (2, MulS), // p. 522: 0010 1010 r s t 0000
        (4, MaddS), // p. 486: 0100 1010 r s t 0000
        (5, MsubS), // p. 517: 0101 1010 r s t 0000
        (6, MaddnS), // p. 488: 0110 1010 r s t 0000
        (7, DivnS), // p. 408: 0111 1010 r s t 0000
        (8, RoundS), // p. 580: 1000 1010 r s scale 0000
        (9, TruncS), // p. 633: 1001 1010 r s scale 0000
        (10, FloorS), // p. 433: 1010 1010 r s scale 0000
        (11, CeilS), // p. 383: 1011 1010 r s scale 0000
        (12, FloatS), // p. 431: 1100 1010 r s scale 0000
        (13, UfloatS), // p. 636: 1101 1010 r s scale 0000
        (14, UtruncS), // p. 643: 1110 1010 r s scale 0000
    ] {
        words.push((rrr(10, op2, 13, 9, 6), op, 13, 9, 6, if op2 >= 8 { 6 } else { 0 }));
    }
    for (t, op) in [
        (0, MovS), // p. 498: 1111 1010 r s 0000 0000
        (1, AbsS), // p. 325: 1111 1010 r s 0001 0000
        (3, ConstS), // p. 386: 1111 1010 r index 0011 0000
        (4, Rfr), // p. 573: 1111 1010 r s 0100 0000
        // WFR p. 647 incorrectly prints t=2 (CVTD.S); Table 228 p. 671 gives 5.
        (5, Wfr), (6, NegS), // NEG.S p. 541: 1111 1010 r s 0110 0000
        (7, Div0S), // p. 406: 1111 1010 r s 0111 0000
        (8, Recip0S), // p. 560: 1111 1010 r s 1000 0000
        (9, Sqrt0S), // p. 612: 1111 1010 r s 1001 0000
        (10, Rsqrt0S), // p. 584: 1111 1010 r s 1010 0000
        (11, Nexp01S), // p. 542: 1111 1010 r s 1011 0000
        (12, MksadjS), // p. 495: 1111 1010 r s 1100 0000
        (13, MkdadjS), // p. 493: 1111 1010 r s 1101 0000
        (14, AddexpS), // p. 330: 1111 1010 r s 1110 0000
        (15, AddexpmS), // p. 331: 1111 1010 r s 1111 0000
    ] {
        words.push((rrr(10, 15, 13, 9, t), op, 13, 9, t as u8, if op == ConstS { 9 } else { 0 }));
    }
    for (op2, op) in [
        (1, UnS), // p. 641: 0001 1011 r s t 0000
        (2, OeqS), // p. 548: 0010 1011 r s t 0000
        (3, UeqS), // p. 634: 0011 1011 r s t 0000
        (4, OltS), // p. 551: 0100 1011 r s t 0000
        (5, UltS), // p. 639: 0101 1011 r s t 0000
        (6, OleS), // p. 549: 0110 1011 r s t 0000
        (7, UleS), // p. 637: 0111 1011 r s t 0000
        (8, MoveqzS), // p. 500: 1000 1011 r s t 0000
        (9, MovnezS), // p. 512: 1001 1011 r s t 0000
        (10, MovltzS), // p. 510: 1010 1011 r s t 0000
        (11, MovgezS), // p. 505: 1011 1011 r s t 0000
        // MOVF.S p. 503 duplicates MOVEQZ.S. Table 229 p. 671 resolves C.
        (12, MovfS), (13, MovtS), // MOVT.S p. 516: 1101 1011 r s t 0000
    ] {
        words.push((rrr(11, op2, 13, 9, 6), op, 13, 9, 6, 0));
    }
    for (r, op) in [(0, Lsi), (8, Lsip), (4, Ssi), (12, Ssip)] {
        // pp. 478, 480, 619, 620: imm8 r s t 0011; imm8 is a word offset.
        words.push((rri8(3, r, 9, 13, 0x91), op, r as u8, 9, 13, 0x91 * 4));
    }
    for (op2, op) in [(0, Lsx), (1, Lsxp), (4, Ssx), (5, Ssxp)] {
        // pp. 482, 483, 624, 625: op2 1000 r s t 0000.
        words.push((rrr(8, op2, 13, 9, 6), op, 13, 9, 6, 0));
    }
    // RUR p. 587: 1110 0011 r s t 0000, ur=16*s+t (232/233 p. 317).
    words.push((rrr(3, 14, 13, 14, 8), RurFcr, 13, 14, 8, 232));
    words.push((rrr(3, 14, 13, 14, 9), RurFsr, 13, 14, 9, 233));
    // WUR p. 651: op2 0011 sr t 0000. Its op2=E duplicates RUR;
    // Table 221 p. 668 gives F. Retain RSR's absent r/s field convention.
    words.push((rsr(3, 15, 13, 232), WurFcr, 0, 0, 13, 232));
    words.push((rsr(3, 15, 13, 233), WurFsr, 0, 0, 13, 233));
    words
}

#[test]
fn decode_boolean_and_single_precision_fields() {
    use Op::*;
    for (bits, op, r, s, t, imm) in fp_words() { decoded(bits, 3, op, r, s, t, imm); }
    for (r, op) in [(8, Any4), (9, All4), (10, Any8), (11, All8)] {
        // pp. 337–341: 0000 0000 reduction s t 0000.
        // Misaligned bs is undefined execution, not an illegal encoding.
        for s in [0, 1, 8, 15] { decoded(rrr(0, 0, r, s, 15), 3, op, r as u8, s as u8, 15, 0); }
    }
    for (op2, op) in [(0, Andb), (1, Andbc), (2, Orb), (3, Orbc), (4, Xorb)] {
        // pp. 339–340, 552–553, 653: op2 0010 r s t 0000.
        decoded(rrr(2, op2, 15, 8, 3), 3, op, 15, 8, 3, 0);
    }
    for (op2, op) in [(12, Movf), (13, Movt)] {
        // MOVT p. 514: 1101 0011 r s t 0000. MOVF p. 501 duplicates
        // MOVEQZ's 1000; use Table 221's 1100, preserving MOVEQZ.
        decoded(rrr(3, op2, 15, 8, 3), 3, op, 15, 8, 3, 0);
    }
    for (r, op) in [(0, Bf), (1, Bt)] { for off in [-128, -4, 1, 127] {
        // pp. 353, 372: imm8 000r s 0111 0110 (RRI8).
        let bits = rri8(6, r, 15, 7, off);
        decoded(bits, 3, op, r as u8, 15, 7, off);
        assert_eq!(target(&decode(bits).unwrap().0, FLASH), Some(FLASH.wrapping_add(4).wrapping_add(off as u32)), "Boolean branch target {op:?}");
    } }
    for (op1, op2, op) in [(3, 0, Rsr), (3, 1, Wsr), (1, 6, Xsr)] {
        decoded(rsr(op1, op2, 15, 4), 3, op, 0, 0, 15, 4);
    }
    for bits in [rrr(10, 15, 1, 2, 2), rrr(15, 0, 1, 2, 3), rrr(14, 1, 1, 2, 3), rri8(3, 1, 2, 3, 4)] {
        assert_eq!(decode(bits), Err(()), "double-precision conversion/arithmetic/compare/load stays excluded");
    }
}

#[test]
fn boolean_logic_reductions_moves_and_register_access() {
    for (opcode, mask, all) in [(8, 15, false), (9, 15, true), (10, 255, false), (11, 255, true)] {
        for value in [0u16, 1, 0x7f, 0xff] {
            let (mut cpu, mut mem, _, _) = setup(&[(rrr(0, 0, opcode, 8, 0), 3)]);
            cpu.br = value << 8;
            assert_eq!(cpu.step(&mut mem), Ok(()), "Boolean reduction works with CP0 disabled");
            let expected = if all { value & mask == mask } else { value & mask != 0 };
            assert_eq!(cpu.br, (value << 8) | u16::from(expected), "reduction computes its complete source group before writing");
        }
    }
    for opcode in 0..5 { for bs in [false, true] { for bt in [false, true] {
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(2, opcode, 8, 8, 15), 3)]);
        cpu.br = (u16::from(bs) << 8) | (u16::from(bt) << 15) | 1;
        let expected = match opcode { 0 => bs & bt, 1 => bs & !bt, 2 => bs | bt, 3 => bs | !bt, _ => bs ^ bt };
        assert_eq!(cpu.step(&mut mem), Ok(()), "Boolean logic retires");
        assert_eq!(cpu.br, (u16::from(expected) << 8) | (u16::from(bt) << 15) | 1, "Boolean logic truth table, aliasing, and preservation");
    } } }
    for (opcode, when) in [(12, false), (13, true)] { for bit in [false, true] {
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(3, opcode, 15, 1, 8), 3)]);
        cpu.br = u16::from(bit) << 8; cpu.ar[1] = 0x87654321; cpu.ar[15] = 7;
        assert_eq!(cpu.step(&mut mem), Ok(()), "MOVF/MOVT retires without CP0");
        assert_eq!(cpu.ar[15], if bit == when { 0x87654321 } else { 7 }, "MOVF/MOVT tests BR, not AR");
    } }
    let (mut cpu, mut mem, _, _) = setup(&[
        (rsr(3, 1, 1, 4), 3), (rsr(3, 0, 2, 4), 3), (rsr(1, 6, 3, 4), 3), (rsr(3, 0, 4, 4), 3),
    ]);
    cpu.ar[1] = 0xa55a; cpu.ar[3] = 0x8001;
    // p. 109: the chosen reset FSR has determined flags, including Underflow.
    assert_eq!((cpu.br, cpu.fr, cpu.fcr, cpu.fsr, cpu.fsr_underflow_unknown), (0, [0; 16], 0, 0, false),
        "undefined Boolean/FP reset state deterministically starts at zero");
    assert_eq!(cpu.run(&mut mem, 4), Stop::Limit, "BR special-register sequence retires");
    assert_eq!((cpu.ar[2], cpu.ar[3], cpu.ar[4], cpu.br), (0xa55a, 0xa55a, 0x8001, 0x8001), "BR reads zero-extend and XSR returns previous bits");
}

#[test]
fn fp_arithmetic_exact_rounding_flags_and_fusion() {
    // Values follow IEEE single precision (pp. 107–110), Operations pp. 328,
    // 487–488, 518, 523, 628. I/U/O/Z are bits 7/8/9/10, PDF p. 109.
    for (opcode, accumulator, s, t, expected, flags, property) in [
        (0, 0, 0x3fc00000, 0x40100000, 0x40700000, 0, "1.5 + 2.25 = 3.75"),
        (1, 0, 0x3fc00000, 0x40100000, 0xbf400000, 0, "1.5 - 2.25 = -0.75"),
        (2, 0, 0x3fc00000, 0x40100000, 0x40580000, 0, "1.5 * 2.25 = 3.375"),
        (0, 0, 0x3f800000, 0x33800000, 0x3f800000, 0x80, "1 + 2^-24 ties to even 1"),
        (0, 0, 0x3f800001, 0x33800000, 0x3f800002, 0x80, "odd significand plus half ULP ties upward"),
        (1, 0, 0x3f800000, 0x33000000, 0x3f800000, 0x80, "1 - 2^-25 ties to even 1"),
        (2, 0, 0x3f800001, 0x3f800001, 0x3f800002, 0x80, "(1+2^-23)^2 discards 2^-46"),
        (0, 0, 0x80000000, 0x80000000, 0x80000000, 0, "-0 + -0 = -0"),
        (0, 0, 0x80000000, 0, 0, 0, "opposite zeros give +0 in nearest mode"),
        (2, 0, 0x80000000, 0x40000000, 0x80000000, 0, "-0 * 2 = -0"),
        (0, 0, 0x7f7fffff, 0x7f7fffff, 0x7f800000, 0x280, "maximum plus maximum overflows with O/I"),
        (2, 0, 0x7f7fffff, 0x40000000, 0x7f800000, 0x280, "maximum times two overflows with O/I"),
        (1, 0, 0x00800000, 0x007fffff, 1, 0, "min normal - max subnormal = 2^-149 exactly"),
        (2, 0, 0x00800000, 0x3f000000, 0x00400000, 0, "2^-126 * 0.5 = exact subnormal 2^-127"),
        (2, 0, 1, 0x3f000000, 0, 0x180, "2^-149 * 0.5 ties to zero with U/I"),
        // (2^-126 + 2^-149)(1 - 2^-20) = 2^-126 - 7*2^-149 - 2^-169: tiny either way, so U/I.
        (2, 0, 0x00800001, 0x3f7ffff0, 0x007ffff9, 0x180, "inexact result below 2^-126 rounds subnormal with U/I"),
        // (1+2^-23)*(1-2^-23)-1 = -2^-46; rounding the product first gives 0.
        (4, 0xbf800000, 0x3f800001, 0x3f7ffffe, 0xa8800000, 0, "MADD.S rounds once through cancellation"),
        (5, 0x3f800000, 0x3f800001, 0x3f7ffffe, 0x28800000, 0, "MSUB.S computes accumulator minus unrounded product"),
        (6, 0xbf800000, 0x3f800001, 0x3f7ffffe, 0xa8800000, 0, "MADDN.S also rounds once"),
        // (1+2^-12)^2 = 1+2^-11+2^-24: a midpoint with an even lower value.
        // Adding 2^-149 puts it ABOVE the midpoint; an f64 sum would lose that bit.
        (4, 1, 0x3f800800, 0x3f800800, 0x3f801001, 0x80, "fused rounding preserves bits below f64 precision"),
        (4, 0x80000001, 0x3f800800, 0x3f800800, 0x3f801000, 0x80, "negative tiny addend selects below midpoint"),
        (4, 0xff7fffff, 0x7f7fffff, 0x40000000, 0x7f7fffff, 0, "unrounded product does not overflow before cancellation"),
        (0, 0, 0x7f800000, 0x3f800000, 0x7f800000, 0, "infinity plus finite stays infinity without overflow flag"),
    ] {
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, opcode, 15, 1, 2), 3)]);
        cpu.cpenable = 1; cpu.fr[15] = accumulator; cpu.fr[1] = s; cpu.fr[2] = t; cpu.fsr = 0x400;
        assert_eq!(cpu.step(&mut mem), Ok(()), "{property}: retires");
        assert_eq!(cpu.fr[15], expected, "{property}: result bits");
        assert_eq!(cpu.fsr, flags | 0x400, "{property}: new flags OR with old flags");
        assert_eq!((cpu.instret, cpu.ccount), (1, 1), "{property}: one retirement");
    }
    for mode in 0..4 { for negative in [false, true] {
        let sign = if negative { 0x80000000 } else { 0 };
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, 0, 15, 1, 2), 3)]);
        cpu.cpenable = 1; cpu.fcr = mode; cpu.fr[1] = sign | 0x3f800000; cpu.fr[2] = sign | 0x33800000;
        assert_eq!(cpu.step(&mut mem), Ok(()), "all four rounding modes execute exactly");
        let away = (mode == 2 && !negative) || (mode == 3 && negative);
        assert_eq!(cpu.fr[15], sign | (0x3f800000 + u32::from(away)), "directed rounding respects the result sign");
        assert_eq!(cpu.fsr, 0x80, "directed inexact arithmetic sets I");
    } }
    let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, 6, 15, 1, 2), 3)]);
    cpu.cpenable = 1; cpu.fcr = 0x7f; cpu.fsr = 0x400;
    cpu.fr[15] = 1; cpu.fr[1] = 0x3f800800; cpu.fr[2] = 0x3f800800;
    assert_eq!(cpu.step(&mut mem), Ok(()), "MADDN.S ignores RM and does not trap for its inexact result");
    assert_eq!((cpu.fr[15], cpu.fsr), (0x3f801001, 0x400), "MADDN.S forces nearest and leaves all flags untouched");
}

#[test]
fn fp_comparisons_and_boolean_branches() {
    // Compare pages 548–551, 634–641 explicitly make +0 == -0.
    for opcode in 1..=7 {
        for (s, t, equal, less) in [
            (0x3f800000, 0x40000000, false, true), // 1 < 2
            (0x40000000, 0x3f800000, false, false), // 2 > 1
            (0xbf800000, 0xc0000000, false, false), // -1 > -2
            (0x80000000, 0, true, false),
            (0x7f800000, 0x7f800000, true, false),
        ] {
            let (mut cpu, mut mem, _, _) = setup(&[(rrr(11, opcode, 15, 1, 2), 3)]);
            cpu.cpenable = 1; cpu.fr[1] = s; cpu.fr[2] = t; cpu.br = 0x5555; cpu.fsr = 0x80;
            let expected = match opcode { 1 => false, 2 | 3 => equal, 4 | 5 => less, _ => less || equal };
            assert_eq!(cpu.step(&mut mem), Ok(()), "ordered compare {opcode} retires");
            assert_eq!(cpu.br, 0x5555 | (u16::from(expected) << 15), "compare {opcode} writes only its Boolean");
            assert_eq!(cpu.fsr, 0x80, "ordered compare preserves sticky flags");
            for (branch, when) in [(0, false), (1, true)] {
                let (mut branch_cpu, mut branch_mem, _, _) = setup(&[(rri8(6, branch, 15, 7, 12), 3)]);
                branch_cpu.br = cpu.br;
                assert_eq!(branch_cpu.step(&mut branch_mem), Ok(()), "BT/BF use compare result with CP0 disabled");
                assert_eq!(branch_cpu.pc, FLASH + if expected == when { 16 } else { 3 }, "BT/BF taken and not-taken target");
            }
        }
        // The Summary defines no signaling-NaN encoding. OLT/OLE can
        // decide V for every NaN; the other five must stop without guessing.
        for nan in [0x7f800001, 0x7fc00001, 0xffc12345] {
            let (mut cpu, mut mem, ram, _) = setup(&[(rrr(11, opcode, 15, 1, 2), 3)]);
            cpu.cpenable = 1; cpu.fr[1] = nan; cpu.fr[2] = 0; cpu.br = 0x8001;
            if opcode == 4 || opcode == 6 {
                assert_eq!(cpu.step(&mut mem), Ok(()), "OLT/OLE define unordered behavior for any NaN");
                assert_eq!((cpu.br, cpu.fsr), (1, 0x800), "unordered ordered-compare is false and sets Invalid");
            } else {
                let what = match opcode {
                    1 => "UN.S: signaling NaN encoding not specified by ISA Summary",
                    2 => "OEQ.S: signaling NaN encoding not specified by ISA Summary",
                    3 => "UEQ.S: signaling NaN encoding not specified by ISA Summary",
                    5 => "ULT.S: signaling NaN encoding not specified by ISA Summary",
                    _ => "ULE.S: signaling NaN encoding not specified by ISA Summary",
                };
                assert_unmodeled_unchanged(&mut cpu, &mut mem, ram, what);
            }
        }
    }
}

#[test]
fn fp_moves_register_bits_and_control_status_access() {
    for opcode in 8..=13 { for condition in [0, 1, 0x80000000] { for bit in [false, true] {
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(11, opcode, 15, 1, 2), 3)]);
        cpu.cpenable = 1; cpu.fr[1] = 0xff800001; cpu.fr[15] = 0x12345678;
        cpu.ar[2] = condition; cpu.br = u16::from(bit) << 2; cpu.fsr = 0xf80;
        let take = match opcode { 8 => condition == 0, 9 => condition != 0, 10 => (condition as i32) < 0,
            11 => (condition as i32) >= 0, 12 => !bit, _ => bit };
        assert_eq!(cpu.step(&mut mem), Ok(()), "conditional FP move retires, even with NaN source");
        assert_eq!(cpu.fr[15], if take { 0xff800001 } else { 0x12345678 }, "conditional move uses AR or BR as specified");
        assert_eq!(cpu.fsr, 0xf80, "non-arithmetic moves leave all flags alone");
    } } }
    let (mut cpu, mut mem, _, _) = setup(&[
        (rrr(10, 15, 15, 1, 5), 3), // WFR f15,a1
        (rrr(10, 15, 14, 15, 0), 3), // MOV.S f14,f15
        (rrr(10, 15, 2, 14, 4), 3), // RFR a2,f14
        (rsr(3, 15, 3, 232), 3), // WUR.FCR a3
        (rsr(3, 15, 3, 233), 3), // WUR.FSR a3
        (rrr(3, 14, 4, 14, 8), 3), // RUR.FCR a4
        (rrr(3, 14, 5, 14, 9), 3), // RUR.FSR a5
        (rsr(3, 15, 0, 233), 3), // WUR.FSR a0 clears flags
    ]);
    cpu.cpenable = 1; cpu.ar[1] = 0xff800001; cpu.ar[3] = 0xfff;
    assert_eq!(cpu.run(&mut mem, 8), Stop::Limit, "FP register transfer program completes");
    assert_eq!((cpu.fr[15], cpu.fr[14], cpu.ar[2]), (0xff800001, 0xff800001, 0xff800001), "WFR/MOV.S/RFR preserve every bit without NaN arithmetic");
    assert_eq!((cpu.ar[4], cpu.ar[5], cpu.fcr, cpu.fsr), (0x7f, 0xf80, 0x7f, 0), "FCR/FSR ignore each other's fields and WUR replaces rather than ORs");
    for (t, input, expected) in [(1, 0xc0200000, 0x40200000), (6, 0x40200000, 0xc0200000), (1, 0x80000000, 0), (6, 0, 0x80000000)] {
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, 15, 15, 1, t), 3)]);
        cpu.cpenable = 1; cpu.fr[1] = input; cpu.fsr = 0xf80;
        assert_eq!(cpu.step(&mut mem), Ok(()), "ABS.S/NEG.S on defined numeric values retires");
        assert_eq!((cpu.fr[15], cpu.fsr), (expected, 0xf80), "sign transform preserves magnitude and flags including signed zero");
    }
}

#[test]
fn fp_scaled_integer_conversions_and_limits() {
    // Fixed-to-single (pp. 432, 636): value / 2^scale, rounded in FCR.RM.
    for (opcode, input, scale, expected, flags, property) in [
        (12, 3, 1, 0x3fc00000, 0, "signed 3/2 = 1.5"),
        (13, 7, 2, 0x3fe00000, 0, "unsigned 7/4 = 1.75"),
        (12, 0x80000000, 0, 0xcf000000, 0, "signed minimum -2^31 is exact"),
        (12, 0x7fffffff, 0, 0x4f000000, 0x80, "signed maximum rounds to 2^31"),
        (13, 0xffffffff, 0, 0x4f800000, 0x80, "unsigned maximum rounds to 2^32"),
        (12, 0x80000000, 15, 0xc7800000, 0, "signed minimum / 2^15 = -2^16"),
        (12, 0x7fffffff, 15, 0x47800000, 0x80, "scaled signed maximum rounds to 2^16"),
        (13, 0xffffffff, 15, 0x48000000, 0x80, "scaled unsigned maximum rounds to 2^17"),
    ] {
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, opcode, 15, 1, scale), 3)]);
        cpu.cpenable = 1; cpu.ar[1] = input; cpu.fsr = 0x400;
        assert_eq!(cpu.step(&mut mem), Ok(()), "{property}: conversion retires");
        assert_eq!((cpu.fr[15], cpu.fsr), (expected, flags | 0x400), "{property}: result and sticky flags");
    }
    for mode in 0..4 { for (opcode, input, sign) in [(12, 0xfeffffff, 0x80000000), (13, 0x01000001, 0)] {
        // ±(2^24+1) is exactly halfway between adjacent f32 integers.
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, opcode, 15, 1, 0), 3)]);
        cpu.cpenable = 1; cpu.fcr = mode; cpu.ar[1] = input;
        assert_eq!(cpu.step(&mut mem), Ok(()), "integer-to-float supports each directed mode");
        let away = (mode == 2 && sign == 0) || (mode == 3 && sign != 0);
        assert_eq!((cpu.fr[15], cpu.fsr), (sign | (0x4b800000 + u32::from(away)), 0x80), "integer conversion tie and direction");
    } }
    for opcode in [8, 9, 10, 11, 14] {
        let unsigned = opcode == 14;
        let upper = if unsigned { u32::MAX } else { 0x7fffffff };
        let positive_fraction = if opcode == 8 || opcode == 11 { 2 } else { 1 };
        let negative_fraction = if opcode == 8 || opcode == 10 { 0xfffffffe } else { 0xffffffff };
        for (input, scale, expected, flags, property) in [
            (0x3fe00000, 3, 14, 0, "1.75 * 8 = 14 exact"),
            (0xbfe00000, 3, 0xfffffff2, 0, "-1.75 * 8 = -14, including UTRUNC's signed negative rule"),
            (0x3fe00000, 0, positive_fraction, 0x80, "positive 1.75 rounds as instruction specifies"),
            (0xbfe00000, 0, negative_fraction, 0x80, "negative -1.75 rounds as instruction specifies"),
            (0xcf000000, 0, 0x80000000, 0, "signed minimum remains valid for negative inputs"),
            (0xcf000001, 0, 0x80000000, 0x800, "-2^31-256 saturates with Invalid only"),
            (0x4effffff, 0, 0x7fffff80, 0, "largest f32 below signed maximum = 2^31-128"),
            (0x7f800000, 0, upper, 0x800, "positive infinity saturates"),
            (0xff800000, 0, 0x80000000, 0x800, "negative infinity uses signed saturation"),
            (0x7f800001, 0, upper, 0x800, "any NaN saturates to the positive limit"),
            (0xffc00001, 15, upper, 0x800, "NaN sign and scale do not change saturation"),
            (0x80000000, 15, 0, 0, "scaled negative zero converts exactly to integer zero"),
            (if unsigned { 0x4f7fffff } else { 0x4f000000 }, 0,
                if unsigned { 0xffffff00 } else { upper }, if unsigned { 0 } else { 0x800 }, "upper range edge"),
            (if unsigned { 0x48000000 } else { 0x47800000 }, 15, upper, 0x800, "scaling reaches out-of-range 2^32 or 2^31"),
        ] {
            let (mut cpu, mut mem, ram, _) = setup(&[(rrr(10, opcode, 15, 1, scale), 3)]);
            cpu.cpenable = 1; cpu.fr[1] = input; cpu.fcr = 3; cpu.fsr = 0x400;
            if unsigned && input & 0x80000000 != 0 && flags != 0x800 && expected != 0 {
                // p. 643 gives signed result bits for these cases, but
                // does not say whether V uses signed or unsigned range.
                assert_unmodeled_unchanged(&mut cpu, &mut mem, ram,
                    "UTRUNC.S: negative signed-range result has unspecified Invalid flag");
                continue;
            }
            assert_eq!(cpu.step(&mut mem), Ok(()), "conversion {opcode}: {property}: retires");
            assert_eq!((cpu.ar[15], cpu.fsr), (expected, flags | 0x400), "conversion {opcode}: {property}: bits and flags independent of FCR.RM");
        }
    }
    for (input, expected) in [(0x40200000, 2), (0x40600000, 4), (0xc0200000, 0xfffffffe)] {
        // ROUND.S: 2.5 -> 2, 3.5 -> 4, -2.5 -> -2 (nearest/even).
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, 8, 15, 1, 0), 3)]);
        cpu.cpenable = 1; cpu.fr[1] = input;
        assert_eq!(cpu.step(&mut mem), Ok(()), "ROUND.S midpoint retires");
        assert_eq!((cpu.ar[15], cpu.fsr), (expected, 0x80), "ROUND.S uses even tie and Inexact");
    }
}

#[test]
fn fp_exact_sequence_bit_operations() {
    for (index, expected) in [(0, 0), (1, 0x3f800000), (2, 0x40000000), (3, 0x3f000000)] {
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, 15, 15, index, 3), 3)]);
        cpu.cpenable = 1; cpu.fcr = 0x7f; cpu.fsr = 0xf80;
        assert_eq!(cpu.step(&mut mem), Ok(()), "CONST.S Table pp. 386–387 retires");
        assert_eq!((cpu.fr[15], cpu.fsr), (expected, 0xf80), "CONST.S exact table bits and no flags");
    }
    for (t, r, s, expected) in [
        // Exponents 127+128-127=128, signs XOR, mantissa of r unchanged (p. 330).
        (14, 0xbfc00000, 0xc0000000, 0x40400000),
        // Exponents 254+130-127=257 wraps to 1 in the eight-bit field.
        (14, 0x7f123456, 0x41000000, 0x00923456),
        // p. 332: source bit22=sign and bits21:14=128, not its FP exponent.
        (15, 0x3fc00000, 0x00600000, 0xc0400000),
        (14, 0x3fc00000, 0x3fc00000, 0x3fc00000),
    ] {
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, 15, 15, 1, t), 3)]);
        cpu.cpenable = 1; cpu.fr[15] = r; cpu.fr[1] = s; cpu.fsr = 0xf80;
        assert_eq!(cpu.step(&mut mem), Ok(()), "exponent adjustment retires");
        assert_eq!((cpu.fr[15], cpu.fsr), (expected, 0xf80), "adjustment is field arithmetic without floating-point flags");
    }
    for (input, expected) in [
        (0, 0xc0000000), (0x80000000, 0x40000000), // p. 543: !sign || 1 || 30 zeros
        (0x7f800000, 0xbf800000), (0xffc12345, 0x3fc12345), // !sign || 0 || seven ones || fraction
        (0x40c00000, 0xbfc00000), // -6 / 4 = -1.5
        (0xc1400000, 0x40400000), // -(-12) / 4 = +3
        (0x3e800000, 0xbf800000), // normal 1/4: N=-1 exactly, -(1/4)/(1/4)=-1
        (0x00800000, 0xbf800000), // normal 4^-63: N=-63 exactly
    ] {
        let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, 15, 15, 1, 11), 3)]);
        cpu.cpenable = 1; cpu.fr[1] = input; cpu.fsr = 0xf80;
        assert_eq!(cpu.step(&mut mem), Ok(()), "NEXP01.S defined formula/special-value case retires");
        assert_eq!((cpu.fr[15], cpu.fsr), (expected, 0xf80), "NEXP01.S exact field construction and preserved flags");
    }
}

#[test]
fn fp_bus_addresses_post_increments_alignment_and_faults() {
    for (bits, indexed, post, store) in [
        (rri8(3, 0, 1, 15, 3), false, false, false), // LSI f15,a1,12
        (rri8(3, 8, 1, 15, 3), false, true, false), // LSIP
        (rrr(8, 0, 15, 1, 2), true, false, false), // LSX f15,a1,a2
        (rrr(8, 1, 15, 1, 2), true, true, false), // LSXP
        (rri8(3, 4, 1, 15, 3), false, false, true), // SSI
        (rri8(3, 12, 1, 15, 3), false, true, true), // SSIP
        (rrr(8, 4, 15, 1, 2), true, false, true), // SSX
        (rrr(8, 5, 15, 1, 2), true, true, true), // SSXP
    ] {
        for unaligned in [Unaligned::Ignore, Unaligned::Hardware, Unaligned::Exception] {
            let (mut cpu, mut memory, ram, _) = setup(&[(bits, 3)]);
            cpu.config.unaligned = unaligned; cpu.cpenable = 1;
            cpu.ar[1] = RAM + 17; cpu.ar[2] = 12; cpu.fr[15] = 0xffc12345;
            let address = RAM + if post { 17 } else { 29 };
            let accessed = if unaligned == Unaligned::Hardware { address } else { address & !3 };
            let offset = (accessed - RAM) as usize;
            memory.bytes_mut(ram)[offset..offset + 4].copy_from_slice(&0x3fc00000u32.to_le_bytes());
            let before_cpu = format!("{cpu:?}"); let before_memory = memory.bytes(ram).to_vec();
            let mut bus = AccessLog { memory, accesses: Vec::new() };
            if unaligned == Unaligned::Exception {
                assert_eq!(cpu.step(&mut bus), Err(Stop::Exception { cause: cause::LOAD_STORE_ALIGNMENT, pc: FLASH, vaddr: address }), "FP alignment fault uses effective address");
                assert_eq!(format!("{cpu:?}"), before_cpu, "FP alignment fault preserves base, destination, flags, and counters");
                assert_eq!(bus.memory.bytes(ram), before_memory.as_slice(), "FP alignment fault leaves memory unchanged");
                assert_eq!(bus.accesses.len(), 3, "alignment fault happens before data-bus access");
            } else {
                assert_eq!(cpu.step(&mut bus), Ok(()), "FP load/store retires under selected alignment policy");
                assert_eq!(bus.accesses.last(), Some(&(accessed, 4, if store { 2 } else { 1 }, 0)), "FP access uses word width and pre-retirement timestamp");
                assert_eq!(cpu.ar[1], RAM + if post { 29 } else { 17 }, "post-increment uses original base plus immediate/index, not aligned address");
                assert_eq!(cpu.fr[15], if store { 0xffc12345 } else { 0x3fc00000 }, "FP loads/stores preserve raw bits");
                let expected = if store { 0xffc12345u32 } else { 0x3fc00000u32 };
                assert_eq!(&bus.memory.bytes(ram)[offset..offset + 4], &expected.to_le_bytes(), "FP store writes selected address only");
                assert_eq!((cpu.fsr, cpu.instret), (0, 1), "FP memory access is non-arithmetic and retires once");
            }
        }
        for (base, fault_address) in [(0x1000, if post { 0x1000 } else { 0x100c }), (FLASH, if post { FLASH } else { FLASH + 12 })] {
            if !store && base == FLASH { continue; } // Flash reads are permitted.
            let (mut cpu, mut mem, ram, _) = setup(&[(bits, 3)]);
            cpu.cpenable = 1; cpu.ar[1] = base; cpu.ar[2] = 12; cpu.fr[15] = 0xaabbccdd;
            let before = format!("{cpu:?}"); let before_ram = mem.bytes(ram).to_vec();
            assert_eq!(cpu.step(&mut mem), Err(Stop::Exception { cause: cause::LOAD_STORE_ERROR, pc: FLASH, vaddr: fault_address }), "FP bus failure uses effective address, indexed={indexed}");
            assert_eq!(format!("{cpu:?}"), before, "FP bus fault cannot write FR or post-increment base");
            assert_eq!(mem.bytes(ram), before_ram.as_slice(), "failed FP store preserves memory");
        }
    }
    // An aliased index/base must be read before post-increment (p. 484).
    let (mut cpu, mut mem, ram, _) = setup(&[(rrr(8, 1, 15, 1, 1), 3)]);
    cpu.cpenable = 1; cpu.ar[1] = RAM;
    mem.bytes_mut(ram)[..4].copy_from_slice(&0x12345678u32.to_le_bytes());
    assert_eq!(cpu.step(&mut mem), Ok(()), "LSXP permits aliased base and index");
    assert_eq!((cpu.fr[15], cpu.ar[1]), (0x12345678, RAM.wrapping_add(RAM)), "aliased LSXP uses old base for load and both addition inputs");
}

#[test]
fn coprocessor_zero_gate_precedes_every_fp_effect() {
    // Table 71 p. 150 and EXCCAUSE Table 65 p. 138 give cause 32.
    // §4.4.4 pp. 150–151 gates all references to CP state except shared BR;
    // RUR/WUR explicitly list the disabled exception (pp. 587, 652).
    for (bits, op, _, _, _, _) in fp_words() {
        for traps in [TrapMode::Stop, TrapMode::Take] {
            let (mut cpu, memory, ram, _) = setup(&[(bits, 3)]);
            cpu.cpenable = 0xfe; cpu.traps = traps; cpu.fr = [0x3f800000; 16];
            cpu.br = 0xa55a; cpu.fcr = 3; cpu.fsr = 0xf80; cpu.excvaddr = 0x12345678;
            let before = format!("{cpu:?}"); let before_fr = cpu.fr; let before_ar = cpu.ar;
            let before_memory = memory.bytes(ram).to_vec();
            let mut bus = AccessLog { memory, accesses: Vec::new() };
            if traps == TrapMode::Stop {
                assert_eq!(cpu.step(&mut bus), Err(Stop::Exception { cause: 32, pc: FLASH, vaddr: 0 }), "{op:?}: CP0 disabled takes precedence over undefined data/approximations");
                assert_eq!(format!("{cpu:?}"), before, "{op:?}: disabled stop preserves all CPU state");
            } else {
                assert_eq!(cpu.step(&mut bus), Ok(()), "{op:?}: disabled CP0 enters exception handler");
                assert_eq!((cpu.exccause, cpu.epc1, cpu.pc, cpu.ps), (32, FLASH, IRAM + 0x100, 16), "{op:?}: precise disabled cause, EPC1 and kernel vector");
                assert_eq!((cpu.fr, cpu.ar, cpu.br, cpu.fcr, cpu.fsr), (before_fr, before_ar, 0xa55a, 3, 0xf80), "{op:?}: disabled exception writes no operand/FP state");
                assert_eq!((cpu.excvaddr, cpu.instret, cpu.ccount), (0x12345678, 0, 0), "{op:?}: CP cause does not set EXCVADDR or retire");
            }
            assert_eq!(bus.accesses.len(), 3, "{op:?}: disabled FP accesses only instruction bytes");
            assert_eq!(bus.memory.bytes(ram), before_memory.as_slice(), "{op:?}: CP gate preserves memory");
        }
    }
    let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, 0, 15, 1, 2), 3)]);
    cpu.cpenable = 1; cpu.fr[1] = 0x3f800000; cpu.fr[2] = 0x40000000;
    assert_eq!(cpu.step(&mut mem), Ok(()), "setting CPENABLE bit zero permits FP execution");
    assert_eq!((cpu.fr[15], cpu.instret), (0x40400000, 1), "enabled ADD.S computes 1+2=3");
}

fn assert_unmodeled_unchanged(cpu: &mut Cpu, mem: &mut Memory, ram: usize, what: &'static str) {
    let before = format!("{cpu:?}"); let before_memory = mem.bytes(ram).to_vec();
    // p. 109: a deferred FSR observation can stop after earlier instructions retired.
    let pc = cpu.pc;
    assert_eq!(cpu.step(mem), Err(Stop::Unmodeled { pc, what }), "named document-boundary stop: {what}");
    assert_eq!(format!("{cpu:?}"), before, "stop preserves every CPU field: {what}");
    assert_eq!(mem.bytes(ram), before_memory.as_slice(), "stop preserves memory: {what}");
}

#[test]
fn all_unspecified_fp_sequence_instructions_stop_atomically() {
    for (bits, what) in [
        (rrr(10, 15, 15, 1, 7), "DIV0.S: approximation not specified bit-exactly"),
        (rrr(10, 15, 15, 1, 8), "RECIP0.S: approximation not specified bit-exactly"),
        (rrr(10, 15, 15, 1, 9), "SQRT0.S: approximation not specified bit-exactly"),
        (rrr(10, 15, 15, 1, 10), "RSQRT0.S: approximation not specified bit-exactly"),
        (rrr(10, 7, 15, 1, 2), "DIVN.S: special exponent interpretation not specified bit-exactly"),
        (rrr(10, 15, 15, 1, 12), "MKSADJ.S: square_root_adjust not specified bit-exactly"),
        (rrr(10, 15, 15, 1, 13), "MKDADJ.S: divide_adjust not specified bit-exactly"),
        (rrr(10, 15, 15, 4, 3), "CONST.S: reserved constant index"),
        (rrr(10, 15, 15, 1, 11), "NEXP01.S: below-one normalization/TRUNC rule is ambiguous"),
        (rsr(3, 15, 1, 232), "WUR.FCR: nonzero MBZ bits have unspecified behavior"),
        (rsr(3, 15, 1, 233), "WUR.FSR: nonzero MBZ bits have unspecified behavior"),
        (rrr(0, 0, 9, 1, 15), "ALL4: bs not a multiple of 4 is undefined"),
        (rrr(0, 0, 8, 1, 15), "ANY4: bs not a multiple of 4 is undefined"),
        (rrr(0, 0, 11, 4, 15), "ALL8: bs not a multiple of 8 is undefined"),
        (rrr(0, 0, 10, 4, 15), "ANY8: bs not a multiple of 8 is undefined"),
        (rsr(3, 1, 1, 4), "WSR.BR: nonzero high 16 bits are undefined"),
        (rsr(1, 6, 1, 4), "XSR.BR: nonzero high 16 bits are undefined"),
    ] {
        let (mut cpu, mut mem, ram, _) = setup(&[(bits, 3)]);
        cpu.cpenable = 1; cpu.fr[1] = 0x3f000000; cpu.fr[2] = 0x3fc00000;
        cpu.fr[15] = 0x12345678; cpu.ar[1] = 0xffff1234; cpu.br = 0xa55a; cpu.fsr = 0xf80;
        assert_unmodeled_unchanged(&mut cpu, &mut mem, ram, what);
    }
}

#[test]
fn unspecified_nan_results_and_enabled_traps_stop_atomically() {
    for (bits, what) in [
        (rrr(10, 15, 15, 1, 1), "ABS.S: NaN result bits not specified by ISA Summary"),
        (rrr(10, 15, 15, 1, 6), "NEG.S: NaN result bits not specified by ISA Summary"),
        (rrr(10, 0, 15, 1, 2), "ADD.S: NaN result bits not specified by ISA Summary"),
        (rrr(10, 1, 15, 1, 2), "SUB.S: NaN result bits not specified by ISA Summary"),
        (rrr(10, 2, 15, 1, 2), "MUL.S: NaN result bits not specified by ISA Summary"),
        (rrr(10, 4, 15, 1, 2), "MADD.S: NaN result bits not specified by ISA Summary"),
        (rrr(10, 5, 15, 1, 2), "MSUB.S: NaN result bits not specified by ISA Summary"),
        (rrr(10, 6, 15, 1, 2), "MADDN.S: NaN result bits not specified by ISA Summary"),
    ] {
        for nan in [0x7f800001, 0x7fc12345, 0xffc54321] {
            let (mut cpu, mut mem, ram, _) = setup(&[(bits, 3)]);
            cpu.cpenable = 1; cpu.fr[1] = nan; cpu.fr[2] = 0x3f800000; cpu.fr[15] = 0x40000000;
            assert_unmodeled_unchanged(&mut cpu, &mut mem, ram, what);
        }
    }
    for (opcode, r, s, t, what) in [
        (0, 0, 0x7f800000, 0xff800000, "ADD.S: NaN result bits not specified by ISA Summary"),
        (1, 0, 0x7f800000, 0x7f800000, "SUB.S: NaN result bits not specified by ISA Summary"),
        (2, 0, 0x7f800000, 0, "MUL.S: NaN result bits not specified by ISA Summary"),
        (4, 0xff800000, 0x7f800000, 0x3f800000, "MADD.S: NaN result bits not specified by ISA Summary"),
        (5, 0x7f800000, 0x7f800000, 0x3f800000, "MSUB.S: NaN result bits not specified by ISA Summary"),
        (6, 0, 0, 0x7f800000, "MADDN.S: NaN result bits not specified by ISA Summary"),
        (4, 0x7fc12345, 0x3f800000, 0x3f800000, "MADD.S: NaN result bits not specified by ISA Summary"),
    ] {
        let (mut cpu, mut mem, ram, _) = setup(&[(rrr(10, opcode, 15, 1, 2), 3)]);
        cpu.cpenable = 1; cpu.fr[15] = r; cpu.fr[1] = s; cpu.fr[2] = t;
        assert_unmodeled_unchanged(&mut cpu, &mut mem, ram, what);
    }
    for (bits, r, s, t, ar, enables, what) in [
        (rrr(10, 0, 15, 1, 2), 0, 0x3f800000, 0x33800000, 0, 4, "ADD.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 1, 15, 1, 2), 0, 0x3f800000, 0x33000000, 0, 4, "SUB.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 2, 15, 1, 2), 0, 1, 0x3f000000, 0, 8, "MUL.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 4, 15, 1, 2), 1, 0x3f800800, 0x3f800800, 0, 4, "MADD.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 5, 15, 1, 2), 0x3f800000, 1, 1, 0, 4, "MSUB.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 12, 15, 1, 0), 0, 0, 0, 0x01000001, 4, "FLOAT.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 13, 15, 1, 0), 0, 0, 0, 0x01000001, 4, "UFLOAT.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 8, 15, 1, 0), 0, 0x3fc00000, 0, 0, 4, "ROUND.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 9, 15, 1, 0), 0, 0x3fc00000, 0, 0, 4, "TRUNC.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 10, 15, 1, 0), 0, 0x3fc00000, 0, 0, 4, "FLOOR.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 11, 15, 1, 0), 0, 0x3fc00000, 0, 0, 4, "CEIL.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(10, 14, 15, 1, 0), 0, 0x7fc00000, 0, 0, 64, "UTRUNC.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(11, 4, 15, 1, 2), 0, 0x7fc00000, 0, 0, 64, "OLT.S: enabled FP trap behavior is implementation-dependent"),
        (rrr(11, 6, 15, 1, 2), 0, 0x7fc00000, 0, 0, 64, "OLE.S: enabled FP trap behavior is implementation-dependent"),
    ] {
        let (mut cpu, mut mem, ram, _) = setup(&[(bits, 3)]);
        cpu.cpenable = 1; cpu.fcr = enables; cpu.fr[15] = r; cpu.fr[1] = s; cpu.fr[2] = t; cpu.ar[1] = ar;
        assert_unmodeled_unchanged(&mut cpu, &mut mem, ram, what);
    }
}

#[test]
fn tininess_delivers_result_and_defers_unknown_underflow_until_observed() {
    // ISA Summary p. 109: 2^-126 - 2^-150 rounds to 2^-126; only U is
    // undetermined. An already sticky U determines the status nevertheless.
    let mul = rrr(10, 2, 15, 1, 2);
    let read = rrr(3, 14, 4, 14, 9);
    let write = rsr(3, 15, 3, 233);
    for sticky in [0, 0x100] {
        let (mut cpu, mut mem, ram, _) = setup(&[(mul, 3), (read, 3), (write, 3), (read, 3)]);
        cpu.cpenable = 1; cpu.fr[1] = 0x00800000; cpu.fr[2] = 0x3f7fffff; cpu.fsr = sticky;
        assert_eq!(cpu.step(&mut mem), Ok(()));
        assert_eq!((cpu.fr[15], cpu.fsr, cpu.fsr_underflow_unknown), (0x00800000, sticky | 0x80, sticky == 0));
        let saved = cpu.clone();
        cpu = Cpu::new(cpu.config, 0);
        cpu.clone_from(&saved);
        if sticky == 0 {
            assert_unmodeled_unchanged(&mut cpu, &mut mem, ram,
                "RUR.FSR: Underflow for a result rounding up to 2^-126 needs IEEE754's tininess after rounding, which the ISA Summary names but does not define");
            cpu.pc += 3;
        } else {
            assert_eq!(cpu.step(&mut mem), Ok(()));
            assert_eq!(cpu.ar[4], 0x180, "p. 109: sticky Underflow is determined");
        }
        cpu.ar[3] = 0x400;
        assert_eq!(cpu.step(&mut mem), Ok(()));
        assert!(!cpu.fsr_underflow_unknown, "p. 109: WUR.FSR determines the flags");
        assert_eq!(cpu.step(&mut mem), Ok(()));
        assert_eq!(cpu.ar[4], 0x400);
    }
    for sticky in [0, 0x100] {
        let (mut cpu, mut mem, ram, _) = setup(&[(mul, 3)]);
        cpu.cpenable = 1; cpu.fcr = 8; cpu.fsr = sticky;
        cpu.fr[1] = 0x00800000; cpu.fr[2] = 0x3f7fffff;
        assert_unmodeled_unchanged(&mut cpu, &mut mem, ram,
            "MUL.S: Underflow for a result rounding up to 2^-126 needs IEEE754's tininess after rounding, which the ISA Summary names but does not define");
    }
    // p. 109: known Underflow later resolves the sticky bit; p. 488:
    // MADDN.S must leave both the flags and their uncertainty unchanged.
    let (mut cpu, mut mem, _, _) = setup(&[(mul, 3), (rrr(10, 6, 14, 1, 2), 3), (mul, 3), (read, 3)]);
    cpu.cpenable = 1; cpu.fr[1] = 0x00800000; cpu.fr[2] = 0x3f7fffff;
    assert_eq!(cpu.step(&mut mem), Ok(()));
    cpu.fcr = 0x7f;
    assert_eq!(cpu.step(&mut mem), Ok(()));
    assert_eq!((cpu.fr[14], cpu.fsr, cpu.fsr_underflow_unknown), (0x00800000, 0x80, true));
    cpu.fcr = 0; cpu.fr[1] = 1; cpu.fr[2] = 0x3f000000;
    assert_eq!(cpu.step(&mut mem), Ok(()));
    assert!(!cpu.fsr_underflow_unknown);
    assert_eq!(cpu.step(&mut mem), Ok(()));
    assert_eq!(cpu.ar[4], 0x180);
}

#[test]
fn fp_directed_overflow_underflow_and_zero_sign() {
    for mode in 0..4 {
        for (opcode, s, t, expected, flags, property) in [
            // Maximum finite * 2 overflows; RM chooses +infinity or max finite.
            (2, 0x7f7fffff, 0x40000000, if mode == 0 || mode == 2 { 0x7f800000 } else { 0x7f7fffff }, 0x280, "positive directed overflow"),
            // -2^-149 * 0.5 is a negative halfway subnormal, or -0.
            (2, 0x80000001, 0x3f000000, if mode == 3 { 0x80000001 } else { 0x80000000 }, 0x180, "negative directed underflow"),
            // 1-1 is exact; only round-down produces negative zero.
            (1, 0x3f800000, 0x3f800000, if mode == 3 { 0x80000000 } else { 0 }, 0, "exact cancellation zero sign"),
        ] {
            let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, opcode, 15, 1, 2), 3)]);
            cpu.cpenable = 1; cpu.fcr = mode; cpu.fr[1] = s; cpu.fr[2] = t;
            assert_eq!(cpu.step(&mut mem), Ok(()), "RM={mode}: {property}: retires");
            assert_eq!((cpu.fr[15], cpu.fsr), (expected, flags), "RM={mode}: {property}: bits and flags");
        }
    }
}

// Window fixtures are data, independently transcribed from pp. 244–247,
// 289–290 and the instruction pages. Physical index = (4*WB + AR) mod 64.
#[derive(Clone)]
struct WindowState {
    physical: [u32; 64],
    base: u32,
    start: u32,
    ps: u32,
    pc: u32,
    memory: [u32; 16],
}

impl WindowState {
    fn new() -> Self {
        let mut state = Self {
            physical: std::array::from_fn(|n| 0x1234_0000 + n as u32),
            base: 14, start: 1 << 14, ps: (1 << 18) | (2 << 16) | (9 << 8) | 0x23,
            pc: FLASH, memory: std::array::from_fn(|n| 0x5678_0000 + n as u32),
        };
        state.set_ar(1, RAM + 0x440);
        state
    }

    fn ar(&self, n: u32) -> u32 { self.physical[((4 * self.base + n) % 64) as usize] }
    fn set_ar(&mut self, n: u32, value: u32) {
        self.physical[((4 * self.base + n) % 64) as usize] = value;
    }
}

struct WindowCase {
    name: &'static str,
    bits: u32,
    length: u32,
    before: WindowState,
    after: WindowState,
    stop: Option<&'static str>,
}

impl WindowCase {
    fn new(name: &'static str, bits: u32, length: u32, before: WindowState) -> Self {
        let mut after = before.clone();
        after.pc += length;
        Self { name, bits, length, before, after, stop: None }
    }

    fn stop(mut self, what: &'static str) -> Self {
        self.after = self.before.clone();
        self.stop = Some(what);
        self
    }
}

fn seed_physical(cpu: &mut Cpu, mem: &mut Memory, flash: usize, physical: &[u32; 64], base: u32) {
    // Populate all four disjoint windows via architectural ROTW, without a
    // production test setter. The instruction under test remains at FLASH.
    cpu.ps = 0;
    cpu.window_base = 0;
    for chunk in physical.chunks_exact(16) {
        cpu.ar.copy_from_slice(chunk);
        cpu.pc = FLASH + 0x800;
        mem.bytes_mut(flash)[0x800..0x803].copy_from_slice(&rotw(4).to_le_bytes()[..3]);
        assert_eq!(cpu.step(mem), Ok(()), "fixture commits a physical register quarter");
    }
    cpu.pc = FLASH + 0x800;
    mem.bytes_mut(flash)[0x800..0x803].copy_from_slice(&rotw(base as i32).to_le_bytes()[..3]);
    assert_eq!(cpu.step(mem), Ok(()), "fixture selects initial visible window");
    cpu.pc = FLASH;
    cpu.instret = 0;
    cpu.ccount = 0;
    assert_eq!(cpu.physical_ar(), *physical, "fixture initializes every physical AR");
}

#[test]
fn windowed_instruction_table() {
    let mut rows = Vec::new();
    // CALL and CALLX, pp. 374–382: no rotation, including with WOE off.
    for n in 1..=3 { for indirect in [false, true] { for woe in [false, true] {
        let mut before = WindowState::new();
        if !woe { before.ps &= !(1 << 18); before.start = 0xffff; }
        before.set_ar(3, FLASH + 0x40);
        let bits = if indirect { callx(n, 3, 3) } else { call(n, 15) };
        let mut row = WindowCase::new("CALL/CALLX return address and increment", bits, 3, before);
        row.after.set_ar(4 * n, (n << 30) | ((FLASH + 3) & 0x3fff_ffff));
        row.after.ps = (row.before.ps & !(3 << 16)) | (n << 16);
        row.after.pc = FLASH + 0x40;
        rows.push(row);
    } } }
    for n in [1, 2, 3] {
        let mut before = WindowState::new();
        before.set_ar(n * 4, FLASH + 0x40);
        let mut row = WindowCase::new("CALLXn reads an aliased target first", callx(n, 3, n * 4), 3, before);
        row.after.set_ar(n * 4, (n << 30) | ((FLASH + 3) & 0x3fff_ffff));
        row.after.ps = (row.before.ps & !(3 << 16)) | (n << 16);
        row.after.pc = FLASH + 0x40;
        rows.push(row);
    }
    // ENTRY pp. 426–427: source before rotation, destination after it.
    for n in 0..=3 { for s in 0..=3 { for bytes in [0, 64, 32760] {
        let mut before = WindowState::new();
        before.ps = (before.ps & !(3 << 16)) | (n << 16);
        before.set_ar(s, RAM + 0x440);
        let mut row = WindowCase::new("ENTRY stack adjustment and wrap", entry(s, bytes), 3, before);
        row.after.base = (14 + n) % 16;
        row.after.start |= 1 << row.after.base;
        row.after.set_ar(s, RAM + 0x440 - bytes);
        rows.push(row);
    } } }
    for (s, ps) in [(4, 1 << 18), (15, 1 << 18), (1, 0), (1, (1 << 18) | 16)] {
        let mut before = WindowState::new(); before.ps = ps;
        rows.push(WindowCase::new("undefined ENTRY", entry(s, 64), 3, before)
            .stop("ENTRY: operand > a3, WOE clear, or EXCM set is undefined"));
    }
    // RETW pp. 566–568: return PC high bits come from PC, not a0.
    for narrow in [false, true] { for n in 1..=3 { for base in [0, 1, 15] {
        let mut before = WindowState::new(); before.base = base;
        before.start = (1 << base) | (1 << ((base + 16 - n) % 16));
        before.set_ar(0, (n << 30) | ((FLASH + 0x24) & 0x3fff_ffff));
        let (bits, length) = retw(narrow);
        let mut row = WindowCase::new("RETW restores caller and clears callee WS", bits, length, before);
        row.after.base = (base + 16 - n) % 16;
        row.after.start &= !(1 << base);
        row.after.pc = FLASH + 0x24;
        rows.push(row);
    } } }
    for narrow in [false, true] { for invalid in 0..4 {
        let mut before = WindowState::new();
        before.set_ar(0, (2 << 30) | 0x200_0024);
        before.start |= 1 << 12;
        match invalid {
            0 => before.set_ar(0, 0x200_0024),
            1 => before.start |= 1 << 13,
            2 => before.ps &= !(1 << 18),
            _ => before.ps |= 16,
        }
        let (bits, length) = retw(narrow);
        rows.push(WindowCase::new("undefined RETW state", bits, length, before).stop(if narrow {
            "RETW.N: zero/mismatched increment, WOE clear, or EXCM set is undefined"
        } else { "RETW: zero/mismatched increment, WOE clear, or EXCM set is undefined" }));
    } }
    for amount in -8..=7 {
        let mut before = WindowState::new(); before.ps &= !(1 << 18);
        before.start = 0xffff;
        let mut row = WindowCase::new("ROTW signed modulo sixteen", rotw(amount), 3, before);
        row.after.base = ((14 + amount) & 15) as u32;
        rows.push(row);
    }
    // MOVSP p. 514: any of the three preceding windows suffices.
    for distance in 1..=3 { for woe in [false, true] {
        let mut before = WindowState::new(); before.start |= 1 << (14 - distance);
        if !woe { before.ps &= !(1 << 18); }
        let mut row = WindowCase::new("MOVSP copies with caller resident", rrr(0, 0, 1, 1, 15), 3, before);
        row.after.set_ar(15, row.before.ar(1));
        rows.push(row);
    } }
    // L32E/S32E pp. 456, 592: all sixteen one-extended offsets.
    for store in [false, true] { for offset in (-64..=-4).step_by(4) {
        let before = WindowState::new();
        let mut row = WindowCase::new("L32E/S32E negative word offsets", window_mem(store, 15, 1, offset), 3, before);
        let word = ((64 + offset) / 4) as usize;
        if store { row.after.memory[word] = row.before.ar(15); }
        else { row.after.set_ar(15, row.before.memory[word]); }
        rows.push(row);
    } }
    for underflow in [false, true] {
        let mut before = WindowState::new(); before.ps |= 16;
        before.start = if underflow { 1 << 9 } else { (1 << 9) | (1 << 14) };
        let mut row = WindowCase::new("RFW restores OWB, EPC1, EXCM and handler WS bit", rfw(underflow), 3, before);
        row.after.base = 9;
        row.after.ps &= !16;
        if underflow { row.after.start |= 1 << 14; } else { row.after.start &= !(1 << 14); }
        row.after.pc = FLASH + 0x60;
        rows.push(row);
    }
    // Tables 147–148 pp. 289–290, XSR completion p. 278.
    for sr in [72, 73] { for access in 0..3 { for value in [0, 1, if sr == 72 { 15 } else { 65535 }] {
        let mut before = WindowState::new(); before.ps &= !(1 << 18);
        before.start = 0x8421; before.set_ar(15, value);
        let bits = match access { 0 => rsr(3, 0, 15, sr), 1 => rsr(3, 1, 15, sr), _ => rsr(1, 6, 15, sr) };
        let mut row = WindowCase::new("RSR/WSR/XSR window registers and old-window XSR destination", bits, 3, before);
        if access != 1 { row.after.set_ar(15, if sr == 72 { 14 } else { 0x8421 }); }
        if access != 0 {
            if sr == 72 { row.after.base = value; } else { row.after.start = value; }
        }
        rows.push(row);
    } } }
    for (sr, value, wsr_stop, xsr_stop) in [
        (72, 16, "WSR.WINDOWBASE: nonzero high 28 bits are undefined", "XSR.WINDOWBASE: nonzero high 28 bits are undefined"),
        (73, 65536, "WSR.WINDOWSTART: nonzero high 16 bits are undefined", "XSR.WINDOWSTART: nonzero high 16 bits are undefined"),
    ] { for exchange in [false, true] {
        let mut before = WindowState::new(); before.set_ar(1, value);
        let bits = if exchange { rsr(1, 6, 1, sr) } else { rsr(3, 1, 1, sr) };
        rows.push(WindowCase::new("undefined high window-register bits", bits, 3, before)
            .stop(if exchange { xsr_stop } else { wsr_stop }));
    } }
    for row in rows {
        let (mut cpu, mut mem, ram, flash) = setup(&[(row.bits, row.length)]);
        seed_physical(&mut cpu, &mut mem, flash, &row.before.physical, row.before.base);
        cpu.window_start = row.before.start; cpu.ps = row.before.ps;
        cpu.epc1 = FLASH + 0x60; cpu.exccause = 0x25; cpu.excvaddr = 0xabcdef00;
        for (word, value) in row.before.memory.iter().enumerate() {
            mem.bytes_mut(ram)[0x400 + word * 4..0x404 + word * 4].copy_from_slice(&value.to_le_bytes());
        }
        let cpu_before = format!("{cpu:?}");
        let expected = row.stop.map_or(Ok(()), |what| Err(Stop::Unmodeled { pc: FLASH, what }));
        assert_eq!(cpu.step(&mut mem), expected, "{}: execution outcome", row.name);
        if row.stop.is_some() {
            assert_eq!(format!("{cpu:?}"), cpu_before, "{}: undefined operation is atomic", row.name);
        }
        assert_eq!(cpu.physical_ar(), row.after.physical, "{}: all 64 physical registers", row.name);
        let visible: [u32; 16] = std::array::from_fn(|n| row.after.ar(n as u32));
        assert_eq!(cpu.ar, visible, "{}: all 16 visible registers", row.name);
        assert_eq!((cpu.window_base, cpu.window_start, cpu.ps, cpu.pc),
            (row.after.base, row.after.start, row.after.ps, row.after.pc), "{}: WB/WS/PS/PC", row.name);
        assert_eq!((cpu.epc1, cpu.exccause, cpu.excvaddr), (FLASH + 0x60, 0x25, 0xabcdef00),
            "{}: unrelated exception state preserved", row.name);
        let retired = u64::from(row.stop.is_none());
        assert_eq!((cpu.instret, cpu.ccount), (retired, retired as u32), "{}: exact retirement", row.name);
        for (word, value) in row.after.memory.iter().enumerate() {
            assert_eq!(&mem.bytes(ram)[0x400 + word * 4..0x404 + word * 4], &value.to_le_bytes(),
                "{}: memory word {word}", row.name);
        }
        let saved = cpu.clone();
        cpu = Cpu::new(cpu.config, 0);
        cpu.clone_from(&saved);
        assert_eq!(cpu.physical_ar(), row.after.physical, "{}: Clone preserves hidden ARs", row.name);
    }
    let (mut cpu, mut mem, _, flash) = setup(&[(rotw(4), 3)]);
    let state = WindowState::new();
    seed_physical(&mut cpu, &mut mem, flash, &state.physical, 0);
    let old_visible = cpu.ar;
    cpu.window_base = 4;
    assert_eq!(cpu.ar, old_visible, "direct WindowBase write relabels without rotating");
    assert_eq!(&cpu.physical_ar()[16..32], &old_visible, "physical view overlays relabeled public ARs");
    cpu.ar[0] = 0xfeedbeef;
    assert_eq!(cpu.step(&mut mem), Ok(()), "rotation commits direct visible writes");
    assert_eq!(cpu.physical_ar()[16], 0xfeedbeef, "old relabeled window is written back on rotation");
    assert_eq!(cpu.ar[0], state.physical[32], "new window loads previously hidden data");
}

#[test]
fn window_exceptions_and_alloca() {
    // p. 246: the first occupied window determines WB; its successor,
    // independently of the faulting CALL size, determines the vector size.
    for traps in [TrapMode::Stop, TrapMode::Take] { for base in [0, 14, 15] {
        for distance in 1..=3 { for size in 1..=3 {
            for bits in [call(distance, 10), callx(distance, 3, 3), entry(1, 64), rri8(2, 10, 0, distance * 4, 7)] {
                let (mut cpu, mut mem, _, flash) = setup(&[(bits, 3)]);
                let physical = WindowState::new().physical;
                seed_physical(&mut cpu, &mut mem, flash, &physical, base);
                cpu.traps = traps;
                cpu.ps = (1 << 18) | (distance << 16) | (9 << 8) | 0xe3;
                let victim = (base + distance) % 16;
                cpu.window_start = (1 << base) | (1 << victim) | (1 << ((victim + size) % 16));
                cpu.exccause = 0x25; cpu.excvaddr = 0xaabbccdd;
                let start = cpu.window_start; let ps = cpu.ps;
                assert_eq!(cpu.step(&mut mem), Ok(()), "overflow enters dedicated vector in either TrapMode");
                assert_eq!((cpu.pc, cpu.window_base), (IRAM + (size - 1) * 0x80, victim),
                    "overflow vector uses victim size, rotation uses first conflict");
                assert_eq!((cpu.ps, cpu.epc1, cpu.window_start), ((ps & !0xf00) | (base << 8) | 16, FLASH, start),
                    "overflow sets EXCM/OWB/EPC1 and preserves UM/CALLINC/WS");
                assert_eq!(cpu.physical_ar(), physical, "overflow aborts before any operand write");
                let visible: [u32; 16] = std::array::from_fn(|n| physical[(victim as usize * 4 + n) % 64]);
                assert_eq!(cpu.ar, visible, "overflow exposes victim registers to handler");
                assert_eq!((cpu.exccause, cpu.excvaddr, cpu.instret, cpu.ccount), (0x25, 0xaabbccdd, 0, 0),
                    "dedicated overflow has no general cause or retirement");
            }
        } }
        for size in 1..=3 { for narrow in [false, true] {
            let (bits, len) = retw(narrow);
            let (mut cpu, mut mem, ram, flash) = setup(&[(bits, len)]);
            let mut state = WindowState::new(); state.base = base;
            state.set_ar(0, (size << 30) | ((FLASH + 0x24) & 0x3fff_ffff));
            seed_physical(&mut cpu, &mut mem, flash, &state.physical, base);
            cpu.traps = traps; cpu.ps = (1 << 18) | (2 << 16) | (9 << 8) | 0xe3;
            cpu.window_start = 1 << base; cpu.exccause = 0x25; cpu.excvaddr = 0xaabbccdd;
            let ps = cpu.ps;
            let caller = (base + 16 - size) % 16;
            let offset = (size - 1) * 0x80 + 0x40;
            mem.bytes_mut(ram)[offset as usize..offset as usize + 3].copy_from_slice(&rfw(true).to_le_bytes()[..3]);
            assert_eq!(cpu.step(&mut mem), Ok(()), "RETW underflow enters vector");
            assert_eq!((cpu.pc, cpu.window_base, cpu.window_start), (IRAM + offset, caller, 1 << base),
                "underflow rotates to missing caller without clearing callee WS");
            assert_eq!((cpu.ps, cpu.epc1), ((ps & !0xf00) | (base << 8) | 16, FLASH),
                "underflow records pre-rotation WB and faulting RETW address");
            assert_eq!(cpu.physical_ar(), state.physical, "underflow changes no register contents");
            assert_eq!((cpu.exccause, cpu.excvaddr, cpu.instret, cpu.ccount), (0x25, 0xaabbccdd, 0, 0),
                "underflow preserves general exception state and does not retire");
            // No register filling needed to test retry: the preserved image
            // stands in for a completed fill, and RFWU marks it resident.
            assert_eq!(cpu.step(&mut mem), Ok(()), "RFWU resumes faulting RETW");
            assert_eq!((cpu.pc, cpu.window_base, cpu.window_start, cpu.ps),
                (FLASH, base, (1 << base) | (1 << caller), (ps & !0xf00) | (base << 8)),
                "RFWU restores callee and marks the filled caller");
            assert_eq!(cpu.step(&mut mem), Ok(()), "retried RETW completes");
            assert_eq!((cpu.pc, cpu.window_base, cpu.window_start), (FLASH + 0x24, caller, 1 << caller),
                "retried RETW returns and clears callee WS");
        } }
    } }
    for size in 1..=3 { for underflow in [false, true] {
        let instruction = if underflow { retw(false).0 } else { rri8(2, 10, 0, 4, 7) };
        let (mut cpu, mut mem, _, _) = setup(&[(instruction, 3)]);
        cpu.ps = 1 << 18; cpu.vecbase = IRAM + 0x1000;
        cpu.config.vectors.window = [0x200, 0x240, 0x280, 0x2c0, 0x300, 0x340];
        cpu.window_start = if underflow { 1 } else { 3 | (1 << (1 + size)) };
        cpu.ar[0] = (size << 30) | 0x200_0024;
        assert_eq!(cpu.step(&mut mem), Ok(()), "relocated window exception enters handler");
        let index = (size as usize - 1) * 2 + usize::from(underflow);
        assert_eq!(cpu.pc, IRAM + 0x1000 + cpu.config.vectors.window[index],
            "all six window vectors use VECBASE plus their configured offset");
    } }
    // p. 246: one instruction may require more than one overflow/retry.
    let (mut cpu, mut mem, ram, _) = setup(&[(rri8(2, 10, 0, 12, 7), 3)]);
    cpu.ps = 1 << 18; cpu.window_start = 1 | 2 | 4 | 16;
    for offset in [0, 0x80, 0x100] {
        mem.bytes_mut(ram)[offset..offset + 3].copy_from_slice(&rfw(false).to_le_bytes()[..3]);
    }
    for (vector, base, start) in [(IRAM, 1, 23), (IRAM + 0x80, 2, 21)] {
        assert_eq!(cpu.step(&mut mem), Ok(()), "repeated overflow enters next victim");
        assert_eq!((cpu.pc, cpu.window_base, cpu.window_start), (vector, base, start), "first unspilled victim wins");
        assert_eq!(cpu.step(&mut mem), Ok(()), "RFWO retries original instruction");
        assert_eq!((cpu.pc, cpu.window_base), (FLASH, 0), "RFWO restores original window and PC");
    }
    assert_eq!(cpu.step(&mut mem), Ok(()), "instruction completes once its referenced groups are free");
    assert_eq!((cpu.ar[12], cpu.window_start), (7, 17), "overflow does not spill beyond highest referenced group");
    for ps in [0, 16, (1 << 18) | 16] {
        let (mut cpu, mut mem, _, _) = setup(&[(rri8(2, 10, 0, 15, 7), 3)]);
        cpu.ps = ps; cpu.window_start = 0xffff;
        assert_eq!(cpu.step(&mut mem), Ok(()), "WOE clear or EXCM set suppresses overflow checking");
        assert_eq!((cpu.ar[15], cpu.window_base, cpu.window_start, cpu.ps), (7, 0, 0xffff, ps),
            "disabled overflow check leaves window control untouched");
    }
    for bits in [rri8(2, 10, 15, 2, -1), rrr(5, 0, 2, 15, 1), rrr(10, 0, 15, 15, 15), rrr(2, 0, 15, 15, 15)] {
        let (mut cpu, mut mem, _, _) = setup(&[(bits, 3)]);
        cpu.ps = 1 << 18; cpu.window_start = 0xffff; cpu.cpenable = 1;
        assert_eq!(cpu.step(&mut mem), Ok(()), "immediate, Boolean and FP fields are not AR operands");
        assert_eq!(cpu.pc, FLASH + 3, "non-AR high register fields cannot raise overflow");
    }
    // p. 146: even a disabled FP destination in an occupied AR window
    // overflows before reporting the coprocessor exception.
    let (mut cpu, mut mem, _, _) = setup(&[(rrr(10, 8, 15, 0, 0), 3)]);
    cpu.ps = 1 << 18; cpu.window_start = 7;
    assert_eq!(cpu.step(&mut mem), Ok(()), "window overflow precedes coprocessor-disabled exception");
    assert_eq!((cpu.pc, cpu.window_base, cpu.exccause), (IRAM, 1, 0), "FP AR destination invokes ordinary WindowCheck");
    for n in 1..=2 {
        let (mut cpu, mut mem, ram, _) = setup(&[(callx(n, 3, 15), 3)]);
        cpu.ps = 1 << 18; cpu.window_start = 1 | (1 << (n + 1));
        assert_unmodeled_unchanged(&mut cpu, &mut mem, ram,
            "CALLX: source-register overflow check conflicts with generic WindowCheck");
    }
    // MOVSP's AllocaCause (5) uses normal user/kernel and double exceptions.
    for traps in [TrapMode::Stop, TrapMode::Take] { for um in [0, 32] { for woe in [0, 1 << 18] {
        let (mut cpu, mut mem, ram, _) = setup(&[(rrr(0, 0, 1, 2, 1), 3)]);
        cpu.traps = traps; cpu.ps = um | woe; cpu.window_start = 1;
        cpu.ar[1] = RAM + 0x800; cpu.ar[2] = RAM + 0x700;
        cpu.excvaddr = 0x11223344; cpu.exccause = 0x25;
        let before = format!("{cpu:?}"); let physical = cpu.physical_ar();
        let memory = mem.bytes(ram).to_vec();
        if traps == TrapMode::Stop {
            assert_eq!(cpu.step(&mut mem), Err(Stop::Exception { cause: cause::ALLOCA, pc: FLASH, vaddr: 0 }),
                "MOVSP reports AllocaCause when no caller window is present");
            assert_eq!(format!("{cpu:?}"), before, "stopped alloca is atomic");
        } else {
            assert_eq!(cpu.step(&mut mem), Ok(()), "MOVSP alloca enters general vector");
            assert_eq!((cpu.pc, cpu.epc1, cpu.exccause, cpu.ps),
                (IRAM + if um == 0 { 0x100 } else { 0x180 }, FLASH, 5, um | woe | 16),
                "alloca uses UM-selected vector, cause 5 and EXCM");
        }
        assert_eq!((cpu.physical_ar(), cpu.excvaddr, cpu.instret, cpu.ccount), (physical, 0x11223344, 0, 0),
            "alloca preserves ARs/EXCVADDR and never retires MOVSP");
        assert_eq!(mem.bytes(ram), memory.as_slice(), "alloca performs no memory writes");
        cpu.ps = um | woe | 16; cpu.pc = FLASH;
        assert_unmodeled_unchanged(&mut cpu, &mut mem, ram, "double exception");
    } } }
    for store in [false, true] { for unaligned in [Unaligned::Ignore, Unaligned::Exception, Unaligned::Hardware] {
        let (mut cpu, mut memory, ram, _) = setup(&[(window_mem(store, 2, 1, -4), 3)]);
        cpu.config.unaligned = unaligned; cpu.ar[1] = RAM + 0x45; cpu.ar[2] = 0x76543210;
        memory.bytes_mut(ram)[0x40..0x45].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55]);
        let before = format!("{cpu:?}"); let before_memory = memory.bytes(ram).to_vec();
        let mut bus = AccessLog { memory, accesses: Vec::new() };
        if unaligned == Unaligned::Exception {
            assert_eq!(cpu.step(&mut bus), Err(Stop::Exception {
                cause: cause::LOAD_STORE_ALIGNMENT, pc: FLASH, vaddr: RAM + 0x41,
            }), "L32E/S32E alignment fault names effective negative-offset address");
            assert_eq!(format!("{cpu:?}"), before, "L32E/S32E alignment fault preserves CPU");
            assert_eq!(bus.memory.bytes(ram), before_memory.as_slice(), "L32E/S32E alignment fault preserves memory");
            assert_eq!(bus.accesses.len(), 3, "L32E/S32E alignment fault precedes data bus access");
        } else {
            let offset = if unaligned == Unaligned::Hardware { 0x41 } else { 0x40 };
            assert_eq!(cpu.step(&mut bus), Ok(()), "L32E/S32E uses configured unaligned-access policy");
            assert_eq!(bus.accesses.last(), Some(&(RAM + offset, 4, if store { 2 } else { 1 }, 0)),
                "L32E/S32E access width, address and timestamp");
            if store {
                assert_eq!(&bus.memory.bytes(ram)[offset as usize..offset as usize + 4], &0x76543210u32.to_le_bytes(),
                    "S32E stores raw word at selected address");
            } else {
                assert_eq!(cpu.ar[2], if unaligned == Unaligned::Hardware { 0x55443322 } else { 0x44332211 },
                    "L32E loads raw word at selected address");
            }
        }
    } }
    for store in [false, true] {
        let (mut cpu, mut mem, ram, _) = setup(&[(window_mem(store, 2, 1, -4), 3)]);
        cpu.ps = (1 << 18) | 16; cpu.ar[1] = RAM;
        assert_unmodeled_unchanged(&mut cpu, &mut mem, ram, "double exception");
    }
}

#[test]
fn optional_context_register_storage() {
    // pp. 280–283, 316: full-width storage except sign-extended ACCHI.
    // THREADPTR uses UR 231, independently of SR 231 (VECBASE).
    for number in [12, 16, 17, 32, 33, 34, 35, 231] {
        let thread = number == 231;
        let write = |reg| if thread { rsr(3, 15, reg, number) } else { rsr(3, 1, reg, number) };
        let read = |reg| if thread { rrr(3, 14, reg, number >> 4, number & 15) }
            else { rsr(3, 0, reg, number) };
        let first = if number == 17 { 0xffff_ff80 } else { 0xfedc_ba98 };
        let second = if number == 17 { 0x7f } else { 0x8765_4321 };
        let change = if thread { write(3) } else { rsr(1, 6, 3, number) };
        let (mut cpu, mut mem, ram, flash) = setup(&[
            (write(1), 3), (read(2), 3), (change, 3), (read(4), 3),
        ]);
        cpu.ar[1] = first; cpu.ar[3] = second;
        // THREADPTR is independent of CPENABLE (which is zero here).
        assert_eq!(cpu.run(&mut mem, 4), Stop::Limit, "register {number}: storage executes");
        assert_eq!((cpu.ar[2], cpu.ar[4]), (first, second), "register {number}: read/write round trip");
        assert_eq!(cpu.ar[3], if thread { second } else { first }, "register {number}: exchange returns old value");
        let saved = cpu.clone();
        cpu = Cpu::new(cpu.config, 0);
        cpu.clone_from(&saved);
        assert_eq!((cpu.acclo, cpu.acchi, cpu.m, cpu.scompare1, cpu.threadptr),
            (saved.acclo, saved.acchi, saved.m, saved.scompare1, saved.threadptr),
            "register {number}: Clone preserves option storage");
        assert_eq!(cpu.vecbase, IRAM, "register {number}: UR access does not alias VECBASE");
        for bits in [write(1), read(2), change] {
            cpu.pc = FLASH;
            mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
            match number {
                12 => cpu.config.s32c1i = false,
                231 => cpu.config.threadptr = false,
                _ => cpu.config.mac16 = false,
            }
            let before = format!("{cpu:?}");
            assert_eq!(cpu.step(&mut mem), Err(Stop::Exception { cause: cause::ILLEGAL, pc: FLASH, vaddr: 0 }),
                "register {number}: absent option faults on every access");
            assert_eq!(format!("{cpu:?}"), before, "register {number}: absent option leaves state unchanged");
        }
        if number == 17 {
            cpu.config.mac16 = true;
            cpu.ar[1] = 0x80; // not sign-extended
            for (bits, what) in [
                (write(1), "WSR.ACCHI: input is not a sign-extended byte"),
                (rsr(1, 6, 1, 17), "XSR.ACCHI: input is not a sign-extended byte"),
            ] {
                mem.bytes_mut(flash)[..3].copy_from_slice(&bits.to_le_bytes()[..3]);
                assert_unmodeled_unchanged(&mut cpu, &mut mem, ram, what);
            }
        }
    }
}

#[test]
fn loop_register_storage_and_lend_stop() {
    // Tables 129–131 pp. 278–279; no LOOP instruction is introduced.
    for sr in 0..=2 {
        let (mut cpu, mut mem, _, _) = setup(&[
            (rsr(3, 1, 1, sr), 3), (rsr(3, 0, 2, sr), 3),
            (rsr(1, 6, 3, sr), 3), (rsr(3, 0, 4, sr), 3),
        ]);
        cpu.ar[1] = 0xfedcba98; cpu.ar[3] = 0x87654321;
        assert_eq!(cpu.run(&mut mem, 4), Stop::Limit, "loop SR write/read/exchange executes");
        assert_eq!((cpu.ar[2], cpu.ar[3], cpu.ar[4]), (0xfedcba98, 0xfedcba98, 0x87654321),
            "loop SRs store all 32 bits and XSR returns old value");
        let saved = cpu.clone(); cpu = Cpu::new(cpu.config, 0); cpu.clone_from(&saved);
        assert_eq!((cpu.lbeg, cpu.lend, cpu.lcount), (saved.lbeg, saved.lend, saved.lcount),
            "Clone preserves all loop state");
    }
    for length in [2, 3] { for excm in [false, true] { for count in [0, 1, u32::MAX] {
        let bits = if length == 2 { rrrn(13, 0, 1, 2) } else { rrr(0, 8, 2, 1, 1) };
        let (mut cpu, mut mem, ram, _) = setup(&[(bits, length)]);
        cpu.lbeg = FLASH; cpu.lend = FLASH + length; cpu.lcount = count;
        cpu.ps = if excm { 16 } else { 0 }; cpu.ar[1] = 7;
        if !excm && count != 0 {
            assert_unmodeled_unchanged(&mut cpu, &mut mem, ram,
                "zero-overhead loop: LCOUNT non-zero at LEND");
        } else {
            assert_eq!(cpu.step(&mut mem), Ok(()), "zero LCOUNT or EXCM disables loop-back");
            assert_eq!((cpu.pc, cpu.lcount), (FLASH + length, count), "disabled loop preserves count and advances sequentially");
        }
    } } }
    let (mut cpu, mut mem, ram, _) = setup(&[(bri12(1, 0, 1, 4), 3)]);
    cpu.lbeg = FLASH; cpu.lend = FLASH + 8; cpu.lcount = 3;
    assert_eq!(cpu.step(&mut mem), Ok(()), "taken branch to LEND exits without loop-back");
    assert_eq!((cpu.pc, cpu.lcount), (FLASH + 8, 3), "branch target LEND does not decrement LCOUNT");
    cpu.pc = FLASH; cpu.lend = FLASH + 3;
    assert_unmodeled_unchanged(&mut cpu, &mut mem, ram,
        "zero-overhead loop: LCOUNT non-zero at LEND");
}

struct WindowRng(u32);

impl WindowRng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

struct WindowNode {
    size: u32,
    locals: Vec<u32>,
    tag: u32,
    narrow: bool,
    // Child index, second-argument salt, register-indirect call.
    children: Vec<(usize, u32, bool)>,
}

fn window_tree(rng: &mut WindowRng, nodes: &mut Vec<WindowNode>, depth: usize, spine: bool, cycle: [u32; 3]) -> usize {
    let index = nodes.len();
    let size = if spine { cycle[(depth - 1) % 3] } else { 1 + rng.next() % 3 };
    let locals = (3..size * 4).map(|_| 1 + rng.next() % 2047).collect();
    let tag = 1 + rng.next() % 2047;
    let narrow = rng.next() & 1 != 0;
    nodes.push(WindowNode { size, locals, tag, narrow, children: Vec::new() });
    if spine && depth < 20 {
        let child = window_tree(rng, nodes, depth + 1, true, cycle);
        let salt = rng.next() % 2048;
        // Alternating groups of three guarantee every call encoding while
        // a shuffled cycle guarantees deep spills of every window size.
        nodes[index].children.push((child, salt, (depth - 1) / 3 % 2 != 0));
        if rng.next() & 1 != 0 {
            let side = window_tree(rng, nodes, depth + 1, false, cycle);
            let salt = rng.next() % 2048;
            nodes[index].children.push((side, salt, rng.next() & 1 != 0));
        }
    }
    index
}

const WINDOW_STACK: u32 = RAM + 0xf000;
const WINDOW_CODE: u32 = IRAM + 0x1000;

fn window_expected(nodes: &[WindowNode], index: usize, first: u32, second: u32, words: &mut Vec<(u32, u32)>) -> u32 {
    let node = &nodes[index];
    let initial = first ^ second;
    let locals: Vec<_> = node.locals.iter().map(|constant| initial ^ *constant).collect();
    let sp = WINDOW_STACK - (index as u32 + 1) * 128;
    words.extend([(sp, initial), (sp + 4, locals[0]), (sp + 8, node.tag)]);
    let mut result = initial;
    for &(child, salt, _) in &node.children {
        let child_result = window_expected(nodes, child, result.wrapping_add(locals[0]), locals[0] ^ salt, words);
        result = result.wrapping_add(child_result) ^ locals[0];
    }
    for local in locals { result = result.wrapping_mul(2).wrapping_add(local); }
    result
}

fn window_emit(code: &mut Vec<u8>, bits: u32, length: u32) -> usize {
    let offset = code.len();
    code.extend_from_slice(&bits.to_le_bytes()[..length as usize]);
    offset
}

fn window_movi(reg: u32, value: u32) -> u32 {
    rri8(2, 10, (value >> 8) & 15, reg, value as i32)
}

fn window_handlers(mem: &mut Memory, ram: usize) {
    // ISA Summary §6.1.6 pp. 252–254, including its split base/extra
    // save areas. Each handler fits in its S3 64-byte vector slot.
    for n in 1..=3 { for underflow in [false, true] {
        let mut code = Vec::new();
        let sp = 4 * n + 1;
        if !underflow {
            window_emit(&mut code, window_mem(true, 0, sp, -16), 3);
            if n > 1 { window_emit(&mut code, window_mem(false, 0, 1, -12), 3); }
            for r in 1..4 { window_emit(&mut code, window_mem(true, r, sp, -16 + r as i32 * 4), 3); }
            for r in 4..4 * n {
                window_emit(&mut code, window_mem(true, r, 0, -16 * n as i32 + (r as i32 - 4) * 4), 3);
            }
        } else {
            for r in 0..3 { window_emit(&mut code, window_mem(false, r, sp, -16 + r as i32 * 4), 3); }
            let scratch = 4 * n - 1;
            if n > 1 { window_emit(&mut code, window_mem(false, scratch, 1, -12), 3); }
            window_emit(&mut code, window_mem(false, 3, sp, -4), 3);
            for r in 4..4 * n {
                window_emit(&mut code, window_mem(false, r, scratch, -16 * n as i32 + (r as i32 - 4) * 4), 3);
            }
        }
        window_emit(&mut code, rfw(underflow), 3);
        assert!(code.len() <= 0x40, "window handler fits configured vector spacing");
        let offset = (n as usize - 1) * 0x80 + if underflow { 0x40 } else { 0 };
        mem.bytes_mut(ram)[offset..offset + code.len()].copy_from_slice(&code);
    } }
}

struct WindowTrace {
    memory: Memory,
    vectors: [u32; 6],
    entries: [usize; 6],
}

impl Bus for WindowTrace {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, now: u64) -> Result<u32, ()> {
        if kind == Kind::Fetch {
            for (index, vector) in self.vectors.iter().enumerate() {
                if addr == *vector { self.entries[index] += 1; }
            }
        }
        self.memory.read(addr, width, kind, now)
    }

    fn write(&mut self, addr: u32, width: Width, value: u32, now: u64) -> Result<(), ()> {
        self.memory.write(addr, width, value, now)
    }
}

#[test]
fn random_windowed_call_trees() {
    for seed in [1, 0x12345678, 0x31415926, 0xfedcba98] {
        let mut rng = WindowRng(seed);
        let mut cycle = [1, 2, 3];
        for i in (1..3).rev() { cycle.swap(i, rng.next() as usize % (i + 1)); }
        let mut nodes = Vec::new();
        assert_eq!(window_tree(&mut rng, &mut nodes, 1, true, cycle), 0, "generated root is node zero");
        let mut code = Vec::new();
        // Driver uses CALL4 so it too may be spilled and filled. Its BREAK
        // proves that the root's saved return address survived the whole tree.
        let root_call = window_emit(&mut code, call(1, 0), 3);
        window_emit(&mut code, BREAK, 3);
        let mut addresses = vec![0; nodes.len()];
        let mut patches = vec![(root_call, 0, 1)];
        let mut call_kinds = [0; 6];
        for (index, node) in nodes.iter().enumerate() {
            match code.len() % 4 {
                1 => { window_emit(&mut code, NOP, 3); }
                2 => { window_emit(&mut code, rrrn(13, 15, 0, 3), 2); }
                3 => { window_emit(&mut code, NOP, 3); window_emit(&mut code, rrrn(13, 15, 0, 3), 2); }
                _ => {}
            }
            addresses[index] = WINDOW_CODE + code.len() as u32;
            let parent = nodes.iter().position(|parent| parent.children.iter().any(|&(child, _, _)| child == index));
            let framesize = if let Some(parent) = parent { (index - parent) as u32 * 128 } else { 128 };
            assert!(framesize <= 32760, "generated frame fits ENTRY immediate");
            window_emit(&mut code, entry(1, framesize), 3);
            window_emit(&mut code, rrr(0, 3, 2, 2, 3), 3); // incoming arguments mixed into a2
            for (local, constant) in node.locals.iter().enumerate() {
                window_emit(&mut code, window_movi(15, *constant), 3);
                window_emit(&mut code, rrr(0, 3, local as u32 + 3, 2, 15), 3);
            }
            window_emit(&mut code, rri8(2, 6, 1, 2, 0), 3);
            window_emit(&mut code, rri8(2, 6, 1, 3, 1), 3);
            window_emit(&mut code, window_movi(15, node.tag), 3);
            window_emit(&mut code, rri8(2, 6, 1, 15, 2), 3);
            for &(child, salt, indirect) in &node.children {
                let n = node.size;
                window_emit(&mut code, rrr(0, 8, n * 4 + 2, 2, 3), 3);
                window_emit(&mut code, window_movi(n * 4, salt), 3);
                window_emit(&mut code, rrr(0, 3, n * 4 + 3, 3, n * 4), 3);
                if indirect {
                    // The target goes in a(4n), the register CALLXn then
                    // overwrites, as compiled long calls do (L32R a8;
                    // CALLX8 a8). L32R's backward literal lives before the
                    // generated text.
                    let pc = WINDOW_CODE + code.len() as u32;
                    let literal = IRAM + 0x800 + child as u32 * 4;
                    let displacement = (literal as i64 - ((pc + 3) & !3) as i64) / 4;
                    window_emit(&mut code, ri16(n * 4, displacement as u32 & 65535), 3);
                    window_emit(&mut code, callx(n, 3, n * 4), 3);
                } else {
                    let offset = window_emit(&mut code, call(n, 0), 3);
                    patches.push((offset, child, n));
                }
                call_kinds[(n as usize - 1) * 2 + usize::from(indirect)] += 1;
                window_emit(&mut code, rrr(0, 8, 2, 2, n * 4 + 2), 3);
                window_emit(&mut code, rrr(0, 3, 2, 2, 3), 3);
            }
            for local in 3..node.size * 4 { window_emit(&mut code, rrr(0, 9, 2, 2, local), 3); }
            let (bits, len) = retw(node.narrow);
            window_emit(&mut code, bits, len);
        }
        for (offset, child, size) in patches {
            let pc = WINDOW_CODE + offset as u32;
            let displacement = (addresses[child] as i64 - ((pc & !3) + 4) as i64) / 4;
            code[offset..offset + 3].copy_from_slice(&call(size, displacement as i32).to_le_bytes()[..3]);
        }
        for (kind, count) in call_kinds.iter().enumerate() {
            assert!(*count > 0, "seed {seed:#x}: generated CALL/CALLX kind {kind} occurs");
        }
        let first = rng.next(); let second = rng.next();
        let mut words = Vec::new();
        let expected = window_expected(&nodes, 0, first, second, &mut words);
        let (mut cpu, mut memory, ram, _) = setup(&[]);
        window_handlers(&mut memory, ram);
        assert!(0x800 + addresses.len() * 4 <= 0x1000, "literal pool precedes generated text");
        assert!(0x1000 + code.len() < 0x8000, "generated text remains below all stack frames");
        for (index, address) in addresses.iter().enumerate() {
            memory.bytes_mut(ram)[0x800 + index * 4..0x804 + index * 4].copy_from_slice(&address.to_le_bytes());
        }
        memory.bytes_mut(ram)[0x1000..0x1000 + code.len()].copy_from_slice(&code);
        // Distinct monotonically lower SPs keep every function's three
        // local words inspectable, even after sibling calls have returned.
        cpu.pc = WINDOW_CODE; cpu.ps = 1 << 18; cpu.traps = TrapMode::Take;
        let base = seed & 15;
        cpu.window_base = base; cpu.window_start = 1 << base;
        cpu.ar[0] = 0x40001234; cpu.ar[1] = WINDOW_STACK;
        cpu.ar[2] = 0xa5a5c3c3; cpu.ar[3] = 0x5a5a3c3c;
        cpu.ar[6] = first; cpu.ar[7] = second;
        let vectors = std::array::from_fn(|i| IRAM + cpu.config.vectors.window[i]);
        let mut bus = WindowTrace { memory, vectors, entries: [0; 6] };
        assert_eq!(cpu.run(&mut bus, 20000), Stop::Break { pc: WINDOW_CODE + 3 },
            "seed {seed:#x}: generated call tree returns to driver within bounded work");
        assert_eq!(cpu.ar[6], expected, "seed {seed:#x}: returned value matches independent Rust recursion");
        assert_eq!((cpu.ar[0], cpu.ar[1], cpu.ar[2], cpu.ar[3]),
            (0x40001234, WINDOW_STACK, 0xa5a5c3c3, 0x5a5a3c3c),
            "seed {seed:#x}: driver registers survive overflow and underflow");
        for (address, value) in words {
            let offset = (address - RAM) as usize;
            assert_eq!(&bus.memory.bytes(ram)[offset..offset + 4], &value.to_le_bytes(),
                "seed {seed:#x}: function local at {address:#x} survives all calls");
        }
        for (index, count) in bus.entries.iter().enumerate() {
            assert!(*count > 0, "seed {seed:#x}: vector {index} was actually fetched");
        }
        assert_eq!((cpu.window_base, cpu.window_start, cpu.ps & 16), (base, 1 << base, 0),
            "seed {seed:#x}: final window and active-window set equal initial state");
    }
}
