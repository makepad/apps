//! Windowed imports into bounded CALL0 kernel primitives.
//!
//! ISA citations are printed pages of the Xtensa ISA Summary.

use fab_elec_mcu_emit32::xtensa::{assemble, cc, ccz, enc, reg::*, Asm};
use fab_elec_mcu_rt::watch::FAULT_KERNEL_BLOCK_IN_CRITICAL;
use super::{sched::BLOCKED, task::{TCB_RESULT, TCB_STATE}};

/// Ordinary masked path: RSIL, save PS, lower SP, load target, CALLX0,
/// restore SP, load PS, WSR, RSYNC = 9 instructions around the gate.
pub const MASKED_INSNS: u64 = 9;
/// Six instructions through restoring SP, then 16 through block's RSIL 0
/// with the critical-depth check. READY costs 9 + 5; fatal depth costs
/// 6 + 7 + 8 + ILL, also 22. Count only the stretch before unmasking.
pub const MASKED_BLOCKING_INSNS: u64 = 6 + 16;
/// Six hidden-argument instructions and four replacing the exit INTLEVEL.
pub const MASKED_EXIT_LEVEL_INSNS: u64 = MASKED_INSNS + 6 + 4;
/// Three call-site instructions plus trace_record's nineteen.
pub const MASKED_TRACE_INSNS: u64 = 3 + 19;
pub fn masked_insns(blocks: bool, exit_level: bool, trace: bool) -> u64 {
    (if blocks { MASKED_BLOCKING_INSNS } else if exit_level { MASKED_EXIT_LEVEL_INSNS } else { MASKED_INSNS })
        + if trace { MASKED_TRACE_INSNS } else { 0 }
}

pub struct Gate<'a> {
    pub name: &'a str,
    pub call: u32,
    pub blocks: bool,
    /// Some(n): n pack arguments, followed by hidden entry PS, caller RA and CCOUNT.
    /// The CALL0 routine returns exit INTLEVEL in a3. Never a blocking gate.
    pub exit_level: Option<u8>,
    /// Kernel export index, carried by fatal BlockInCritical records.
    pub export: u16,
}

#[derive(Clone, Copy)]
pub struct Blocking {
    /// Address of the word holding the running TCB.
    pub current: u32,
    /// R3's CALL0 block entry.
    pub block: u32,
    /// Optional module nesting word. None retains the original block path.
    pub crit_depth: Option<u32>,
    /// Absolute address of the kernel_fault ILL stub.
    pub kernel_fault: u32,
}

pub struct Exports {
    /// Thunk code with its literal table, assembled at `base`.
    pub bytes: Vec<u8>,
    /// Absolute thunk addresses in gate order: the gate table's contents.
    pub thunks: Vec<u32>,
    /// island::abi_hash(roots, gate names); radio start compares it with the island header's.
    pub abi_hash: u64,
}

/// Assemble at a word-aligned absolute base. `gates` contains import names,
/// CALL0 implementation addresses and blocking flags in gate-index order.
/// Imports arrive through CALLX8 with PS.CALLINC=2; ENTRY rotates arguments
/// into a2..a7 and RETW.N returns a2 in the island caller's a10 (pp. 247–252).
/// The thunk and island
/// must share a 1-GB region because RETW keeps PC[31:30] (p. 252).
///
/// Implementations run in ring 0 with PS.INTLEVEL=15, WOE=1 and older island
/// frames live. CALL0 references to high registers may overflow older frames;
/// that is normal. They must keep interrupts masked, be bounded, never block
/// or wait, and never call the shim, which requires WINDOWSTART == 1<<WINDOWBASE.
/// They must keep the CALL0 ABI, including a12..a15, a1 and its stack discipline.
/// They own nothing above their entry SP, which is T-16, where T is the thunk's
/// SP after ENTRY. Everything above that SP belongs to the thunk and its callers.
///
/// T+0 and T+4 hold the windowed link and entry PS. This extra save area is
/// unused by window spills: the thunk makes no windowed call and remains the
/// current frame. T-16..T is the caller's base save area (§6.1.5 pp. 250–253).
/// An interrupt before RSIL may already have spilled there. Masking prevents
/// spills of this caller while CALL0 moves SP; WindowCheck reaches only the
/// next three groups, never this CALL8 caller at WB-2 (p. 246). Restore SP to
/// T before unmasking so subsequent spills and RETW's reload use the same area.
/// A blocking gate checks current only after restoring a1 to T, then calls
/// R3 block if the implementation left it BLOCKED and reloads TCB_RESULT on
/// resumption. The check adds five masked instructions when READY, or seven
/// before block plus block's five through RSIL 0 (twelve before unmasking).
/// With a critical-depth word, READY adds nothing further; BLOCKED at depth
/// zero adds four instructions (sixteen through unmasking). Nonzero depth
/// executes eight instructions in the new check, including the jump to
/// kernel_fault but excluding its ILL. The check adds nine static instructions.
/// Exit-level gates add six instructions to pass entry PS, caller RA and
/// CCOUNT (one RSR immediately after RSIL, before saving PS),
/// and four to replace only the exit INTLEVEL. Their CALL0 routine adds two
/// instructions to load the requested level. Compared with two hidden arguments,
/// the cycle stamp adds one thunk instruction and two argument-store instructions
/// in that routine. Ordinary gates keep their bytes.
/// Optional tracing adds three call-site instructions and trace_record's
/// nineteen: the R3-5 fixture's masked maximum grows from 69 to 91 retired
/// instructions. Gates with neither extension retain their original bytes
/// and ABI hash.
pub fn exports(base: u32, roots: &[&str], gates: &[Gate<'_>], trace: Option<u32>, blocking: Option<Blocking>) -> Exports {
    assert_eq!(base & 3, 0, "thunk literal table requires word alignment");
    assert!(blocking.is_some() || gates.iter().all(|gate| !gate.blocks),
        "blocking gates require current and R3 block");
    if let Some(b) = blocking { assert_eq!(b.current & 3, 0, "current requires word alignment"); }
    for gate in gates {
        if let Some(n) = gate.exit_level {
            assert!(!gate.blocks, "exit-level gates cannot block");
            assert!(n <= 3, "exit-level gate requires three hidden argument registers");
        }
    }
    const PS: u32 = 230;
    let roots: Vec<_> = roots.iter().map(|root| (*root).into()).collect();
    let names: Vec<_> = gates.iter().map(|gate| gate.name).collect();
    let mut code = Exports {
        bytes: Vec::new(), thunks: Vec::new(), abi_hash: fab_elec_mcu_island::abi_hash(&roots, &names),
    };
    code.bytes = assemble(&mut Asm::new(gates.len() * 2, A8), |a| {
        code.thunks.clear(); // assemble traverses the closure three times
        a.literal_table();
        for (index, gate) in gates.iter().enumerate() {
            code.thunks.push(base.checked_add(a.base.offset()).expect("thunk address overflow"));
            a.entry(A1, 32);
            a.s32i(A0, A1, 0);
            // RSIL a8,15: next instruction sees INTLEVEL without a SYNC
            // (pp. 582–583; Table 201 p. 662). UM does not select the ring.
            a.op24(enc::rrr(0, 0, 0, 6, 15, A8));
            if let Some(n) = gate.exit_level {
                a.op24(enc::rsr(0, 3, 0, 234, A4 + n)); // CCOUNT
            }
            a.s32i(A8, A1, 4);
            if let Some(n) = gate.exit_level {
                a.mov_n(A2 + n, A8);
                // CALL8 tags a0[31:30]; RETW takes the region from its PC.
                a.mov_reg_i32(A9, 0x3fff_ffff);
                a.and(A3 + n, A0, A9);
                a.mov_reg_i32(A9, base & 0xc000_0000);
                a.or(A3 + n, A3 + n, A9);
            }
            a.addi(A1, A1, -16);
            if let Some(trace) = trace {
                a.movi(A8, i32::try_from(index).expect("gate index exceeds MOVI range"));
                a.mov_reg_i32(A9, trace);
                a.callx0(A9);
            }
            a.mov_reg_i32(A8, gate.call);
            a.callx0(A8);
            a.addi(A1, A1, 16);
            if gate.blocks {
                let b = blocking.expect("checked blocking gate contract");
                a.mov_reg_i32(A3, b.current);
                a.l32i(A3, A3, 0);
                a.l32i(A8, A3, TCB_STATE);
                a.bcci_reg_label(cc::NE, A8, BLOCKED as i32, index);
                if let Some(depth) = b.crit_depth {
                    a.mov_reg_i32(A8, depth);
                    a.l32i(A8, A8, 0);
                    a.bccz_reg_label(ccz::EQ, A8, gates.len() + index);
                    a.movi(A2, FAULT_KERNEL_BLOCK_IN_CRITICAL as i32);
                    a.movi(A3, 0); // no source site; the backtrace names the island caller
                    a.mov_reg_i32_optimised(A4, u32::from(gate.export));
                    a.mov_reg_i32(A8, b.kernel_fault);
                    a.jx(A8);
                    a.base.label_assign(gates.len() + index);
                }
                a.mov_n(A2, A3);
                a.mov_reg_i32(A8, b.block);
                a.callx0(A8);
                a.mov_reg_i32(A3, b.current);
                a.l32i(A3, A3, 0);
                a.l32i(A2, A3, TCB_RESULT);
                a.base.label_assign(index);
            }
            a.l32i(A8, A1, 4);
            if gate.exit_level.is_some() {
                a.movi(A9, -16);
                a.and(A8, A8, A9);
                a.extui(A3, A3, 0, 4);
                a.or(A8, A8, A3);
            }
            a.wsr(A8, PS); // all entry PS fields except an explicit exit level, pp. 284–289
            a.rsync();
            a.l32i(A0, A1, 0);
            a.retw_n();
        }
    });
    base.checked_add(code.bytes.len() as u32).expect("thunk address overflow");
    code
}
