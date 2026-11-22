//! Property-guided instruction reading, including options absent from the CPU model.

use crate::{elf::{Object, Rela, SymbolSection}, u32le, Error};
use fab_elec_mcu_xtensa::insn::{self, Insn, Op};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range { pub offset: u32, pub size: u32, pub flags: u32 }

impl Range {
    pub fn instructions(&self) -> bool { self.flags & 2 != 0 && self.flags & 9 == 0 }
}

#[derive(Clone, Debug)]
pub struct Instruction {
    pub offset: u32,
    pub length: u32,
    pub bits: u32,
    pub kind: String,
    pub decoded: Option<Insn>,
    pub loop_end: Option<u32>,
}

impl Instruction {
    pub fn target(&self) -> Option<u32> {
        self.loop_end.or_else(|| self.decoded.as_ref().and_then(|i| insn::target(i, self.offset)))
    }

    pub fn op(&self) -> Option<Op> { self.decoded.map(|i| i.op) }

    pub fn slot_format(&self) -> bool {
        use Op::*;
        self.loop_end.is_some() || matches!(self.op(), Some(
            L32r | J | Call0 | Call4 | Call8 | Call12 | Beqz | Bnez | Bltz | Bgez
            | Beqi | Bnei | Blti | Bgei | Bltui | Bgeui | Beq | Bne | Blt | Bge
            | Bltu | Bgeu | Ball | Bany | Bnall | Bnone | Bbc | Bbs | Bbci | Bbsi
            | Bf | Bt | BeqzN | BnezN
        ))
    }

    /// Window increment in address registers, including CALL0's zero.
    pub fn call_size(&self) -> Option<u8> {
        match self.op()? {
            Op::Call0 | Op::Callx0 => Some(0),
            Op::Call4 | Op::Callx4 => Some(4),
            Op::Call8 | Op::Callx8 => Some(8),
            Op::Call12 | Op::Callx12 => Some(12),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Decoded {
    pub ranges: BTreeMap<usize, Vec<Range>>,
    pub instructions: BTreeMap<usize, BTreeMap<u32, Instruction>>,
    pub failures: Vec<Error>,
}

fn zero_branch(bits: u32) -> Option<Insn> {
    let n = (bits >> 4) & 3;
    let m = (bits >> 6) & 3;
    let r = (bits >> 12) & 15;
    let shift = match bits & 15 {
        6 if n == 1 => 12,
        6 if n == 2 || (n == 3 && (m >= 2 || (m == 1 && r <= 1))) => 16,
        7 => 16,
        _ => return None,
    };
    if bits >> shift != 0 { return None; }
    // The CPU decoder rejects zero offsets for executable wide branches. A
    // relocation may leave that field zero: decode the opcode with offset one,
    // then restore the original field for insn::target. This reader never executes it.
    let (mut decoded, _) = insn::decode(bits | (1 << shift)).ok()?;
    decoded.bits = bits;
    if !matches!(decoded.op, Op::Beqi | Op::Bnei | Op::Blti | Op::Bgei | Op::Bltui | Op::Bgeui) {
        decoded.imm = 0;
    }
    Some(decoded)
}

pub fn instruction(bits: u32, pc: u32) -> Result<Instruction, &'static str> {
    let op0 = bits & 15;
    let length = match op0 { 0..=7 => 3, 8..=13 => 2, _ => return Err("reserved instruction length op0=14/15") };
    let bits = bits & if length == 2 { 0xffff } else { 0xffffff };
    let r = (bits >> 12) & 15;
    let t = (bits >> 4) & 15;
    // Xtensa Instruction Set Architecture (ISA) Reference Manual, LOOP pp. 473–475,
    // LOOPGTZ pp. 475–476, LOOPNEZ pp. 476–478; opcode table 254 pp. 678–679.
    // The BRI8 encoding shares RRI8's imm8/r/s fields: op0=6, n=3, m=1.
    let loop_kind = if op0 == 6 && t == 7 {
        match r { 8 => Some("LOOP"), 9 => Some("LOOPNEZ"), 10 => Some("LOOPGTZ"), _ => None }
    } else { None };
    let decoded = insn::decode(bits).ok().map(|(i, _)| i).or_else(|| zero_branch(bits));
    let op1 = (bits >> 16) & 15;
    let op2 = (bits >> 20) & 15;
    let kind = if let Some(kind) = loop_kind {
        kind.into()
    } else if op0 == 0 && matches!((op1, op2), (3, 0) | (3, 1) | (1, 6)) {
        // Also name option-specific SRs that the execution model refuses.
        let access = match (op1, op2) { (3, 0) => "RSR", (3, 1) => "WSR", _ => "XSR" };
        format!("{access}.SR{}", (bits >> 8) & 255)
    } else if let Some(i) = decoded {
        match i.op {
            Op::Rsil => format!("RSIL level={}", i.imm),
            Op::Waiti => format!("WAITI level={}", i.imm),
            Op::RurFcr | Op::RurFsr => format!("RUR.UR{}", i.imm),
            Op::WurFcr | Op::WurFsr => format!("WUR.UR{}", i.imm),
            _ => format!("{:?}", i.op).to_ascii_uppercase(),
        }
    } else {
        format!("OPTION op0={op0:x}/op1={op1:x}/op2={op2:x} length={length}")
    };
    Ok(Instruction {
        offset: pc, length, bits, kind, decoded,
        loop_end: loop_kind.map(|_| pc.wrapping_add(4).wrapping_add(bits >> 16)),
    })
}

fn property(object: &Object, section: usize, offset: u32, entry: &[u8], relocations: &[Rela]) -> Result<(usize, Range), Error> {
    let at = object.location(section).at(offset);
    let mut matches = relocations.iter().filter(|r| r.offset == offset);
    let relocation = matches.next().ok_or_else(|| at.error(".xt.prop offset lacks R_XTENSA_32"))?;
    if relocation.kind != 1 || matches.next().is_some() { return Err(at.error(".xt.prop offset needs exactly one R_XTENSA_32")); }
    let symbol = object.symbol(relocation)?;
    let SymbolSection::Section(target) = symbol.shndx else { return Err(at.error(".xt.prop offset must describe a section")); };
    if symbol.kind != 3 { return Err(at.error(".xt.prop offset must use a section symbol")); }
    let start = symbol.value.wrapping_add(relocation.addend as u32).wrapping_add(u32le(entry, 0, &at)?);
    let size = u32le(entry, 4, &at)?;
    let target_section = object.sections.get(target).ok_or_else(|| at.error(".xt.prop invalid target section"))?;
    if start.checked_add(size).is_none_or(|end| end > target_section.size) {
        return Err(at.error(format!(".xt.prop range {start:#x}+{size:#x} outside {}", target_section.name)));
    }
    Ok((target, Range { offset: start, size, flags: u32le(entry, 8, &at)? }))
}

pub fn inspect(object: &Object) -> Decoded {
    let mut result = Decoded::default();
    for (index, section) in object.sections.iter().enumerate().filter(|(_, s)| s.name.starts_with(".xt.prop")) {
        let at = object.location(index);
        if section.size % 12 != 0 { result.failures.push(at.error(".xt.prop size is not a multiple of 12")); continue; }
        let relocs = object.relocations.get(&index).map(Vec::as_slice).unwrap_or(&[]);
        for relocation in relocs {
            if relocation.offset % 12 != 0 {
                result.failures.push(at.at(relocation.offset).error(".xt.prop relocation is not on an offset word"));
            }
        }
        for (n, entry) in section.data.chunks_exact(12).enumerate() {
            match property(object, index, (n * 12) as u32, entry, relocs) {
                Ok((target, range)) => result.ranges.entry(target).or_default().push(range),
                Err(error) => result.failures.push(error),
            }
        }
    }
    for (index, section) in object.sections.iter().enumerate() {
        let at = object.location(index);
        let ranges = result.ranges.entry(index).or_default();
        ranges.sort_by_key(|r| (r.offset, r.size, r.flags));
        if section.flags & 4 != 0 {
            let mut covered = 0;
            for range in ranges.iter().filter(|r| r.size != 0) {
                if range.offset > covered {
                    let alignment_fill = range.offset - covered < 4 && range.offset % 4 == 0
                        && section.data.get(covered as usize..range.offset as usize)
                            .is_some_and(|gap| gap.iter().all(|byte| *byte == 0));
                    if !alignment_fill {
                        result.failures.push(at.at(covered).error("exec bytes not covered by .xt.prop"));
                    }
                }
                covered = covered.max(range.offset + range.size); // checked by property
            }
            if covered < section.size { result.failures.push(at.at(covered).error("exec bytes not covered by .xt.prop")); }
        }
        let mut instructions = BTreeMap::new();
        let mut previous_end = 0;
        for range in ranges.iter().filter(|r| r.instructions() && r.size != 0) {
            if section.flags & 4 == 0 {
                result.failures.push(at.at(range.offset).error("instruction range in non-exec section"));
                continue;
            }
            if range.offset < previous_end {
                result.failures.push(at.at(range.offset).error("overlapping .xt.prop instruction ranges"));
                continue;
            }
            let end = range.offset + range.size;
            previous_end = end;
            let mut pc = range.offset;
            while pc < end {
                let Some(first) = section.data.get(pc as usize) else {
                    result.failures.push(at.at(pc).error("truncated instruction range")); break;
                };
                let length = match first & 15 {
                    0..=7 => 3, 8..=13 => 2,
                    _ => { result.failures.push(at.at(pc).error("reserved instruction length op0=14/15")); break; }
                };
                let Some(next) = pc.checked_add(length).filter(|next| *next <= end) else {
                    result.failures.push(at.at(pc).error("instruction crosses .xt.prop range end")); break;
                };
                let Some(raw) = section.data.get(pc as usize..next as usize) else {
                    result.failures.push(at.at(pc).error("truncated instruction bytes")); break;
                };
                let bits = raw.iter().enumerate().fold(0, |bits, (n, b)| bits | ((*b as u32) << (n * 8)));
                match instruction(bits, pc) {
                    Ok(insn) => { instructions.insert(pc, insn); }
                    Err(message) => result.failures.push(at.at(pc).error(message)),
                }
                pc = next;
            }
        }
        if !instructions.is_empty() { result.instructions.insert(index, instructions); }
    }
    result
}
