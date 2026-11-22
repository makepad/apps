use crate::{emit::ENTRY_FIXED_INSNS, runtime::MAX_OP_INSNS};
use std::{collections::BTreeMap, mem};

/// Offsets and structure recorded anew on each assembly pass. Only the final
/// pass is evaluated; spans contain instructions, never literals or padding.
pub(crate) enum Count {
    Span { start: u32, end: u32, times: u64 },
    Port,
    /// Complete CALLX0-through-RET/ILL cost. The op span already includes
    /// CALLX0, so add only the remaining instructions. No loop exclusions.
    PortCost { port: u16, cost: u64 },
    Loop(u64),
    EndLoop,
    If,
    Else,
    EndIf,
}

#[derive(Default)]
pub(crate) struct PortCount {
    pub calls: u64,
    pub insns: u64,
}

#[derive(Default)]
struct Path {
    total: u64,
    ports: BTreeMap<u16, PortCount>,
}

impl Path {
    fn add_charge(&mut self, port: u16, charge: PortCount, times: u64) -> Option<()> {
        let sum = self.ports.entry(port).or_default();
        sum.calls = sum.calls.checked_add(charge.calls.checked_mul(times)?)?;
        sum.insns = sum.insns.checked_add(charge.insns.checked_mul(times)?)?;
        Some(())
    }

    fn followed_by(mut self, body: Self, times: u64) -> Option<Self> {
        self.total = self.total.checked_add(body.total.checked_mul(times)?)?;
        if times != 0 {
            for (port, charge) in body.ports { self.add_charge(port, charge, times)?; }
        }
        Some(self)
    }
}

enum Scope {
    Loop { before: Path, trips: u64 },
    If { before: Path },
    Else { before: Path, yes: Path },
}

pub(crate) fn instructions(bytes: &[u8], start: u32, end: u32) -> u64 {
    let mut pc = start;
    let mut count = 0u32;
    while pc < end {
        // Emitted code uses Xtensa's 16/24-bit formats: op0 >= 8 is density.
        // Same length rule as xtensa::insn::decode (ISA pp. 656–659).
        pc += if bytes[pc as usize] & 8 != 0 { 2 } else { 3 };
        count += 1;
    }
    assert_eq!(pc, end, "instruction bound span ends at an instruction boundary");
    u64::from(count)
}

/// Bound final relaxed instructions: sum straight-line spans, multiply loop
/// bodies (including control/backedges) by counted trips capped by AIR's cap,
/// or by the cap for other loops, and take the larger IF arm. Preheaders and
/// following exit code stay outside the repeated body; IF tests precede the
/// choice and ELSE's skip belongs to the then arm. Inline helpers count their
/// whole span once plus extra visits annotated in float.rs; acyclic internal
/// branches may over-count. Every app port adds MAX_OP_INSNS (no tighter complete
/// all-input port bound is documented in runtime.rs), excluding self-feeding
/// loops. Kernel ports instead supply their complete per-call bound, including
/// raw byte loops. Wrapper/clear spans are absent: add their existing bounds once here.
/// Return None on u64 overflow, preserving the image builder's refusal.
pub(crate) fn bound(bytes: &[u8], counts: &[Count], stack_bytes: u32) -> Option<u64> {
    attributed_bound(bytes, counts, stack_bytes).map(|(total, _)| total)
}

/// Return the bound and the kernel port calls charged along its selected path.
pub(crate) fn attributed_bound(bytes: &[u8], counts: &[Count], stack_bytes: u32) -> Option<(u64, BTreeMap<u16, PortCount>)> {
    let mut path = Path::default();
    let mut scopes = Vec::new();
    for count in counts {
        match *count {
            Count::Span { start, end, times } => {
                path.total = path.total.checked_add(instructions(bytes, start, end).checked_mul(times)?)?;
            }
            Count::Port => path.total = path.total.checked_add(MAX_OP_INSNS)?,
            Count::PortCost { port, cost } => {
                path.total = path.total.checked_add(cost.checked_sub(1)?)?;
                // CALLX0 is already in the op span and belongs to this port's share.
                path.add_charge(port, PortCount { calls: 1, insns: cost }, 1)?;
            }
            Count::Loop(trips) => {
                scopes.push(Scope::Loop { before: mem::take(&mut path), trips });
            }
            Count::EndLoop => {
                let Some(Scope::Loop { before, trips }) = scopes.pop() else { unreachable!("loop scope"); };
                path = before.followed_by(path, trips)?;
            }
            Count::If => {
                scopes.push(Scope::If { before: mem::take(&mut path) });
            }
            Count::Else => {
                let Some(Scope::If { before }) = scopes.pop() else { unreachable!("then scope"); };
                scopes.push(Scope::Else { before, yes: mem::take(&mut path) });
            }
            Count::EndIf => {
                let Some(Scope::Else { before, yes }) = scopes.pop() else { unreachable!("else scope"); };
                // The original else_total.max(then_total) keeps the then arm on equality.
                let arm = if yes.total >= path.total { yes } else { path };
                path = before.followed_by(arm, 1)?;
            }
        }
    }
    assert!(scopes.is_empty(), "balanced instruction bound scopes");
    let total = path.total.checked_add(ENTRY_FIXED_INSNS)?.checked_add(u64::from(stack_bytes).checked_sub(16)?)?;
    Some((total, path.ports))
}
