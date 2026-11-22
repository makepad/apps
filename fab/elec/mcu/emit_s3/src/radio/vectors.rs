//! Window exceptions for the ESP32-S3's 64-register windowed ABI.
//!
//! ISA citations refer to Cadence's *Xtensa ISA Summary*, printed pages.
//! Vector offsets: ESP-IDF v6.1.1,
//! `components/xtensa/esp32s3/include/xtensa/config/core-isa.h`.
//! The stack must follow §6.1.5 (pp. 248–252), including the 16 reserved
//! bytes below SP and the extra save area at the top of each frame.

use fab_elec_mcu_emit32::xtensa::{assemble, ccz, enc, reg::*, Asm};

/// XCHAL_WINDOW_OF4_VECOFS, core-isa.h:581.
pub const OF4_OFFSET: usize = 0x000;
/// XCHAL_WINDOW_UF4_VECOFS, core-isa.h:582.
pub const UF4_OFFSET: usize = 0x040;
/// XCHAL_WINDOW_OF8_VECOFS, core-isa.h:583.
pub const OF8_OFFSET: usize = 0x080;
/// XCHAL_WINDOW_UF8_VECOFS, core-isa.h:584.
pub const UF8_OFFSET: usize = 0x0c0;
/// XCHAL_WINDOW_OF12_VECOFS, core-isa.h:585.
pub const OF12_OFFSET: usize = 0x100;
/// XCHAL_WINDOW_UF12_VECOFS, core-isa.h:586.
pub const UF12_OFFSET: usize = 0x140;
/// Software placement, after the six window vector slots.
pub const ALLOCA_OFFSET: usize = 0x180;
/// XCHAL_USER_VECOFS, core-isa.h:572.
pub const USER_VECTOR_OFFSET: usize = 0x340;

/// Reserved bytes per hardware window vector.
pub const WINDOW_VECTOR_SIZE: usize = 0x40;
/// Entire reserved window block, including the alloca body and zero padding.
pub const WINDOW_BLOCK_SIZE: usize = 0x300;
/// Space available to the user exception vector.
pub const USER_VECTOR_MAX_SIZE: usize = 0x80;
/// Hardware vector order: OF4, UF4, OF8, UF8, OF12, UF12.
pub const WINDOW_VECTOR_OFFSETS: [usize; 6] = [
    OF4_OFFSET, UF4_OFFSET, OF8_OFFSET, UF8_OFFSET, OF12_OFFSET, UF12_OFFSET,
];
/// Actual instruction bytes in the same order as WINDOW_VECTOR_OFFSETS.
pub const WINDOW_HANDLER_SIZES: [usize; 6] = [15, 15, 30, 30, 42, 42];
/// Actual instruction bytes in the software alloca body.
pub const ALLOCA_SIZE: usize = 94;
/// Actual instruction bytes in the user exception dispatcher.
pub const USER_VECTOR_SIZE: usize = 33;
/// VECBASE-relative address of the jump to fault_entry.
pub const USER_UNHANDLED_OFFSET: usize = USER_VECTOR_OFFSET + USER_VECTOR_SIZE - 3;
pub(crate) const USER_IRQ_JUMP_OFFSET: usize = USER_VECTOR_OFFSET + 27;

pub(crate) fn irq_displacement(vecbase: u32, irq_entry: u32) -> i32 {
    let displacement = i64::from(irq_entry) - i64::from(vecbase) - USER_IRQ_JUMP_OFFSET as i64 - 4;
    assert!((-131072..131072).contains(&displacement), "irq_entry is outside user-vector J range");
    displacement as i32
}

// ISA pp. 242, 284, 295, 301: special-register numbers.
const WINDOWBASE: u32 = 72;
const PS: u32 = 230;
const EXCCAUSE: u32 = 232;
const EXCSAVE1: u32 = 209;

fn rsr(a: &mut Asm, reg: u8, special: u32) {
    a.op24(enc::rsr(0, 3, 0, special, reg));
}

// All cross-vector jumps use VECBASE-relative arithmetic, with J's PC+4
// origin. No absolute address, relocation, literal pool, or runtime state.
fn jump_to(a: &mut Asm, origin: usize, target: usize) {
    a.j(target as i32 - origin as i32 - a.base.offset() as i32 - 4);
}

/// Spill a0..a3 below the next frame's SP (a5); ISA §6.1.6 p. 252.
/// RFWO clears this window's WS bit and retries at OWB/EPC1 (p. 575).
fn overflow4(a: &mut Asm) {
    a.s32e(A0, A5, -16);
    a.s32e(A1, A5, -12);
    a.s32e(A2, A5, -8);
    a.s32e(A3, A5, -4);
    a.rfwo();
}

/// Reload a0..a3, preserving a4..a15; ISA §6.1.6 pp. 252–253.
/// RFWU marks this window live, then retries at OWB/EPC1 (p. 576).
fn underflow4(a: &mut Asm) {
    a.l32e(A0, A5, -16);
    a.l32e(A1, A5, -12);
    a.l32e(A2, A5, -8);
    a.l32e(A3, A5, -4);
    a.rfwu();
}

/// Spill a0..a7, preserving a8..a15; ISA §6.1.6 p. 253.
/// The backlink at SP-12 locates the extra save area in this frame.
fn overflow8(a: &mut Asm) {
    a.s32e(A0, A9, -16);
    a.l32e(A0, A1, -12);
    a.s32e(A1, A9, -12);
    a.s32e(A2, A9, -8);
    a.s32e(A3, A9, -4);
    a.s32e(A4, A0, -32);
    a.s32e(A5, A0, -28);
    a.s32e(A6, A0, -24);
    a.s32e(A7, A0, -20);
    a.rfwo();
}

/// Reload a0..a7, preserving a8..a15; ISA §6.1.6 p. 253.
fn underflow8(a: &mut Asm) {
    a.l32e(A0, A9, -16);
    a.l32e(A1, A9, -12);
    a.l32e(A2, A9, -8);
    a.l32e(A7, A1, -12);
    a.l32e(A3, A9, -4);
    a.l32e(A4, A7, -32);
    a.l32e(A5, A7, -28);
    a.l32e(A6, A7, -24);
    a.l32e(A7, A7, -20);
    a.rfwu();
}

/// Spill a0..a11, preserving a12..a15; ISA §6.1.6 p. 253.
fn overflow12(a: &mut Asm) {
    a.s32e(A0, A13, -16);
    a.l32e(A0, A1, -12);
    a.s32e(A1, A13, -12);
    a.s32e(A2, A13, -8);
    a.s32e(A3, A13, -4);
    a.s32e(A4, A0, -48);
    a.s32e(A5, A0, -44);
    a.s32e(A6, A0, -40);
    a.s32e(A7, A0, -36);
    a.s32e(A8, A0, -32);
    a.s32e(A9, A0, -28);
    a.s32e(A10, A0, -24);
    a.s32e(A11, A0, -20);
    a.rfwo();
}

/// Reload a0..a11, preserving a12..a15; ISA §6.1.6 pp. 253–254.
fn underflow12(a: &mut Asm) {
    a.l32e(A0, A13, -16);
    a.l32e(A1, A13, -12);
    a.l32e(A2, A13, -8);
    a.l32e(A11, A1, -12);
    a.l32e(A3, A13, -4);
    a.l32e(A4, A11, -48);
    a.l32e(A5, A11, -44);
    a.l32e(A6, A11, -40);
    a.l32e(A7, A11, -36);
    a.l32e(A8, A11, -32);
    a.l32e(A9, A11, -28);
    a.l32e(A10, A11, -24);
    a.l32e(A11, A11, -20);
    a.rfwu();
}

/// MOVSP recovery, derived from pp. 513–514, 576, 578–579, not an ISA listing.
/// Entry: EXCM=1, EPC1=MOVSP, EXCSAVE1=original a0, window still at MOVSP.
fn alloca(a: &mut Asm) {
    const CALL4: usize = 0;
    const CALL8: usize = 1;

    // MOVSP guarantees the preceding three WS bits are clear (p. 514).
    // After ROTW -1, a0..a3 are dead; original a0 is now a4. EXCM
    // suppresses WindowCheck (p. 246), so these scratch accesses cannot spill.
    a.rotw(-1);
    rsr(a, A0, PS);
    rsr(a, A1, WINDOWBASE);
    a.addi(A1, A1, 1);
    a.extui(A1, A1, 0, 4); // original WindowBase, including wrap 15 -> 0
    a.extui(A2, A0, 8, 4);
    a.xor(A2, A2, A1); // old OWB XOR desired OWB
    // Shift the changed bits into PS.OWB without modifying SAR. All other
    // PS fields (UM, EXCM, WOE, CALLINC, INTLEVEL, RING) survive (pp. 284–289).
    for _ in 0..8 { a.add_n(A2, A2, A2); }
    a.xor(A0, A0, A2);
    a.wsr(A0, PS);
    a.rsync();

    rsr(a, A0, EXCSAVE1);
    a.extui(A0, A0, 30, 2); // CALLn's increment in the saved a0, p. 247
    a.addi(A0, A0, -1);
    a.bccz_reg_label(ccz::EQ, A0, CALL4);
    a.rotw(-1);
    a.addi(A4, A4, -1); // previous scratch a0 is now a4, still below MOVSP
    a.bccz_reg_label(ccz::EQ, A4, CALL8);
    a.rotw(-1);

    // Original a0 moves to a(4n). Restore it before UF uses the caller's
    // dead registers. RFWU sets that caller's WS bit and returns to the
    // original window/EPC1; re-executed MOVSP now succeeds (p. 576).
    rsr(a, A12, EXCSAVE1);
    jump_to(a, ALLOCA_OFFSET, UF12_OFFSET);
    a.base.label_assign(CALL8);
    rsr(a, A8, EXCSAVE1);
    jump_to(a, ALLOCA_OFFSET, UF8_OFFSET);
    a.base.label_assign(CALL4);
    rsr(a, A4, EXCSAVE1);
    jump_to(a, ALLOCA_OFFSET, UF4_OFFSET);
}

/// Exactly 0x300 bytes for VECBASE+0x000..0x2ff; unused bytes are zero.
/// L32E/S32E provide the negative save-area offsets and original ring access
/// semantics (ISA pp. 455–457, 591–592). No runtime installation is performed.
pub fn window_block() -> Vec<u8> {
    let mut block = vec![0; WINDOW_BLOCK_SIZE];
    let handlers: [fn(&mut Asm); 6] = [
        overflow4, underflow4, overflow8, underflow8, overflow12, underflow12,
    ];
    for (i, handler) in handlers.into_iter().enumerate() {
        let bytes = assemble(&mut Asm::new(0, A0), handler);
        assert_eq!(bytes.len(), WINDOW_HANDLER_SIZES[i], "window handler {i} size");
        assert!(bytes.len() <= WINDOW_VECTOR_SIZE, "window handler {i} exceeds slot");
        let start = WINDOW_VECTOR_OFFSETS[i];
        block[start..start + bytes.len()].copy_from_slice(&bytes);
    }
    let bytes = assemble(&mut Asm::new(2, A0), alloca);
    assert_eq!(bytes.len(), ALLOCA_SIZE, "alloca handler size");
    assert!(ALLOCA_OFFSET + bytes.len() <= WINDOW_BLOCK_SIZE, "alloca exceeds block");
    block[ALLOCA_OFFSET..ALLOCA_OFFSET + bytes.len()].copy_from_slice(&bytes);
    block
}

/// Dispatcher for PS.UM=1 exceptions (ISA pp. 143–147, 286–287).
/// Saves only a0, dispatching Level1InterruptCause=4 (p. 151) and
/// AllocaCause=5 (pp. 241, 513–514). Must be
/// installed at USER_VECTOR_OFFSET with window_block() at the same VECBASE.
pub fn user_vector(vecbase: u32, irq_entry: u32, fault_entry: u32) -> Vec<u8> {
    const UNHANDLED: usize = 0;
    const INTERRUPT: usize = 1;
    let irq_jump = irq_displacement(vecbase, irq_entry);
    let fault_jump = i64::from(fault_entry) - i64::from(vecbase) - USER_UNHANDLED_OFFSET as i64 - 4;
    assert!((-131072..131072).contains(&fault_jump), "fault_entry is outside user-vector J range");
    let bytes = assemble(&mut Asm::new(2, A0), |a| {
        a.wsr(A0, EXCSAVE1);
        rsr(a, A0, EXCCAUSE);
        a.addi(A0, A0, -5);
        a.bccz_reg_label(ccz::NE, A0, INTERRUPT);
        jump_to(a, USER_VECTOR_OFFSET, ALLOCA_OFFSET);

        a.base.label_assign(INTERRUPT);
        a.addi(A0, A0, 1); // EXCCAUSE - 4
        a.bccz_reg_label(ccz::NE, A0, UNHANDLED);
        assert_eq!(a.base.offset() as usize, USER_IRQ_JUMP_OFFSET - USER_VECTOR_OFFSET);
        a.j(irq_jump);

        // EXCCAUSE is still intact and EXCSAVE1 holds original a0.
        a.base.label_assign(UNHANDLED);
        assert_eq!(a.base.offset() as usize, USER_UNHANDLED_OFFSET - USER_VECTOR_OFFSET);
        a.j(fault_jump as i32);
    });
    assert_eq!(bytes.len(), USER_VECTOR_SIZE, "user vector size");
    assert!(bytes.len() <= USER_VECTOR_MAX_SIZE, "user vector exceeds slot");
    bytes
}
