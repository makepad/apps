//! Operand encodings from the Xtensa ISA Summary instruction pages. This module
//! only encodes and decodes bytes; it never invokes the execution model.

use crate::{xt::{self, Instruction}, Error, Location};
use fab_elec_mcu_xtensa::insn::{self, Op};

pub(crate) fn slot(original: &Instruction, pc: u32, target: u32, symbol: &str, at: &Location) -> Result<u32, Error> {
    use Op::*;
    let fail = |why: &str| at.error(format!("SLOT0_OP {symbol} target {target:#010x} at {pc:#010x}: {why}"));
    // (mask, shift, width, scale, base, minimum encoded displacement).
    // RI16 pp. 461–462,657: one-extension, *not* sign-extension.
    // CALL pp. 372–377,657: signed imm18 words from aligned PC + 4.
    // J p. 449: signed imm18 bytes from PC + 4.
    // BRI12 pp. 351–352,658; BRI8 pp. 350–351,365–366,658;
    // RRI8 pp. 337–350,354–371,656; BF/BT pp. 353–354,371–372.
    // RI6 pp. 352–353,367–368,659: imm6[3:0] at 15:12,
    // imm6[5:4] at 5:4. LOOP family pp. 473–478: unsigned imm8 LEND.
    let (mask, shift, width, scale, base, minimum) = if original.loop_end.is_some() {
        (0xff0000, 16, 8, 1, pc.wrapping_add(4), 0)
    } else {
        match original.op() {
            Some(L32r) => (0xffff00, 8, 16, 4, pc.wrapping_add(3) & !3, -65536),
            Some(Call0 | Call4 | Call8 | Call12) => (0xffffc0, 6, 18, 4, (pc & !3).wrapping_add(4), -131072),
            Some(J) => (0xffffc0, 6, 18, 1, pc.wrapping_add(4), -131072),
            Some(Beqz | Bnez | Bltz | Bgez) => (0xfff000, 12, 12, 1, pc.wrapping_add(4), -2048),
            Some(BeqzN | BnezN) => (0xf030, 0, 6, 1, pc.wrapping_add(4), 0),
            Some(Beqi | Bnei | Blti | Bgei | Bltui | Bgeui | Beq | Bne | Blt | Bge
                | Bltu | Bgeu | Ball | Bany | Bnall | Bnone | Bbc | Bbs | Bbci | Bbsi | Bf | Bt) =>
                (0xff0000, 16, 8, 1, pc.wrapping_add(4), -128),
            _ => return Err(fail("unsupported operand format")),
        }
    };
    // ELF32 addresses wrap. All representable displacements are far smaller
    // than 2 GiB, so the signed wrapping difference has a unique encoding.
    let delta = i64::from(target.wrapping_sub(base) as i32);
    if delta % scale != 0 { return Err(fail("misaligned target")); }
    let value = delta / scale;
    if value < minimum || value >= minimum + (1i64 << width) {
        return Err(fail("operand out of range"));
    }
    let conditional = original.loop_end.is_none() && matches!(original.op(), Some(
        Beqz | Bnez | Bltz | Bgez | Beqi | Bnei | Blti | Bgei | Bltui | Bgeui
        | Beq | Bne | Blt | Bge | Bltu | Bgeu | Ball | Bany | Bnall | Bnone
        | Bbc | Bbs | Bbci | Bbsi | Bf | Bt
    ));
    // Xtensa ISA Summary §3.8.5 p. 62 reserves these wide branch offsets.
    if conditional && (-3..=0).contains(&delta) { return Err(fail("reserved conditional branch offset -3..=0")); }
    let encoded = value as u32 & ((1 << width) - 1);
    let field = if mask == 0xf030 { ((encoded & 15) << 12) | (encoded & 0x30) } else { encoded << shift };
    let bits = (original.bits & !mask) | field;
    if original.loop_end.is_some() {
        let decoded = xt::instruction(bits, pc).map_err(|_| fail("patched LOOP does not decode"))?;
        if decoded.kind != original.kind || decoded.length != original.length
            || decoded.bits & !mask != original.bits & !mask || decoded.target() != Some(target)
        {
            return Err(fail("patched LOOP op, registers or target mismatch"));
        }
    } else {
        let before = original.decoded.ok_or_else(|| fail("original instruction has no decoded op"))?;
        let (after, length) = insn::decode(bits).map_err(|_| fail("patched instruction does not decode"))?;
        if length != original.length || (after.op, after.r, after.s, after.t) != (before.op, before.r, before.s, before.t)
            || insn::target(&after, pc) != Some(target)
        {
            return Err(fail("patched instruction op, registers or target mismatch"));
        }
    }
    Ok(bits)
}

pub(crate) fn gate(pc: u32, literal: u32, name: &str, at: &Location) -> Result<[u8; 12], Error> {
    // Xtensa ISA Summary: L32R a9 pp. 461–462 (RI16 p. 657);
    // L32I a9,a9,0 pp. 458–459 (RRI8 p. 656); JX a9 p. 450
    // (CALLX p. 657). Only a9 is clobbered, before the callee's ENTRY.
    let load = xt::instruction(0xffff91, pc).map_err(|message| at.error(message))?;
    let bits = slot(&load, pc, literal, name, at)?;
    let mut stub = [0; 12];
    for (offset, word, op, s, t) in [
        (0, bits, Op::L32r, 0, 9), (3, 0x002992, Op::L32i, 9, 9), (6, 0x0009a0, Op::Jx, 9, 0),
    ] {
        let (decoded, length) = insn::decode(word).map_err(|_| at.at(offset as u32).error(format!("gate {name}: instruction does not decode")))?;
        if length != 3 || decoded.op != op || decoded.s != s || decoded.t != t
            || (op == Op::L32i && decoded.imm != 0)
        {
            return Err(at.at(offset as u32).error(format!("gate {name}: opcode or register mismatch")));
        }
        stub[offset..offset + 3].copy_from_slice(&word.to_le_bytes()[..3]);
    }
    Ok(stub)
}
