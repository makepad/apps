//! CALL0 entry to the windowed radio island.
//!
//! ISA citations are printed pages of the Xtensa ISA Summary.

use fab_elec_mcu_emit32::xtensa::{assemble, enc, reg::*, Asm};

/// Bytes the shim allocates below its caller's SP.
pub const SHIM_FRAME: u32 = 64;

/// Executable bytes, including the literal table, and the absolute CALL0 entry.
pub struct ShimCode { pub bytes: Vec<u8>, pub call: u32 }

/// Assemble at a word-aligned absolute base (literal table allowed).
///
/// Called with CALL0/CALLX0 from CALL0 code in a CALL0 task's state:
/// PS.WOE=0, PS.EXCM=0, WINDOWSTART == 1<<WINDOWBASE. The task restore
/// and boot paths supply this state. The shim never writes either window
/// register; RETW leaves only the current window live again.
///
/// Inputs: a2 is the island function's address, which must share the shim's
/// 1-GB region because RETW keeps PC[31:30] (p. 252). a3..a7 are its first
/// five arguments and become the callee's a2..a6. Returns the callee's a2
/// in a2. Preserves a0, a1, a12..a15 and PS (the entry PS is restored, so
/// the CALLINC and OWB that CALLX8 and window exceptions change come back
/// too). Clobbers a2..a11 and whatever the windowed ABI lets the callee
/// clobber: SAR, BR, the loop registers, MAC16 and FP caller-saved state.
///
/// Stack: SHIM_FRAME bytes below the caller's SP, plus the island's own
/// depth. With S = caller SP - SHIM_FRAME, the frame map is:
/// - S+0..16: a12..a15;
/// - S+16: entry PS;
/// - S+32..48: OF8 extra save area, located by [S-12] = S+64;
/// - the rest spare.
///
/// S-16..S is the island's top base save area. Its backlink at S-12 is
/// outside the interrupt payload at SP-256..SP-48. OF8 spills a0..a3
/// below the island SP and a4..a7 into S+32..48; no spill reaches the
/// caller's frame (§6.1.5 pp. 250–253).
pub fn shim_code(base: u32) -> ShimCode {
    assert_eq!(base & 3, 0, "shim literal table requires word alignment");
    const PS: u32 = 230;
    const WOE: u32 = 1 << 18;
    let mut code = ShimCode { bytes: Vec::new(), call: 0 };
    code.bytes = assemble(&mut Asm::new(0, A9), |a| {
        a.literal_table();
        code.call = base.checked_add(a.base.offset()).expect("shim address overflow");
        a.addi(A1, A1, -(SHIM_FRAME as i32));
        for reg in 12..16 { a.s32i(reg, A1, u32::from(reg - 12) * 4); }
        // S32I has no negative offset. This synthetic predecessor keeps
        // even the oldest CALL8 frame's extra save area inside the shim.
        a.addi(A8, A1, -16);
        a.addi(A9, A1, SHIM_FRAME as i32);
        a.s32i(A9, A8, 4);
        for reg in 3..8 { a.mov_n(reg + 7, reg); }

        a.op24(enc::rsr(0, 3, 0, PS, A8));
        a.s32i(A8, A1, 16);
        a.mov_reg_i32(A9, WOE);
        a.or(A8, A8, A9);
        a.wsr(A8, PS);
        a.rsync(); // WOE must be visible to WindowCheck, p. 289.
        a.callx8(A2); // a0 survives; result returns in a10 (pp. 247–252).

        // CALLX8 changes CALLINC; window exceptions change OWB. Restore
        // the whole entry PS, not just WOE (pp. 246–247, 284–289).
        a.l32i(A8, A1, 16);
        a.wsr(A8, PS);
        a.rsync();
        a.mov_n(A2, A10);
        for reg in 12..16 { a.l32i(reg, A1, u32::from(reg - 12) * 4); }
        a.addi(A1, A1, SHIM_FRAME as i32);
        a.ret_n();
    });
    base.checked_add(code.bytes.len() as u32).expect("shim address overflow");
    code
}
