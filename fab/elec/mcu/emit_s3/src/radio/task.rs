//! Level-1 task contexts for the 64-register ESP32-S3.
//!
//! ISA citations are printed pages of the Xtensa ISA Summary. Each task
//! must keep 256 bytes of mapped stack headroom below its lowest possible
//! SP, above the guard word. The interrupt frame starts at SP-256; its
//! payload ends at SP-48. SP-16..SP is the caller's ABI base save area
//! (§6.1.5 pp. 250–252), never interrupt scratch space.

use fab_elec_mcu_emit32::xtensa::{assemble, cc, ccz, enc, reg::*, Asm};

/// Fourteen words, a multiple of eight bytes so heap TCBs stay eight-aligned.
pub const TCB_SIZE: usize = 56;
/// Saved context-frame address (word 0).
pub const TCB_FRAME: u32 = 0;
/// Lowest stack address, containing GUARD (word 1).
pub const TCB_STACK_BASE: u32 = 4;
/// Exclusive top of the stack allocation (word 2).
pub const TCB_STACK_TOP: u32 = 8;
/// Scheduler-defined state (word 3).
pub const TCB_STATE: u32 = 12;
/// Scheduler-defined priority (word 4).
pub const TCB_PRIORITY: u32 = 16;
/// Little-endian u64 deadline, low word at 20, high word at 24.
/// The TCB requires only word alignment: access these as two words.
pub const TCB_DEADLINE: u32 = 20;
/// Name address (word 7).
pub const TCB_NAME: u32 = 28;
/// Result of the blocking kernel operation (word 8).
pub const TCB_RESULT: u32 = 32;
/// Next TCB in the object's priority/FIFO wait list (word 9).
pub const TCB_WAIT_NEXT: u32 = 36;
/// Wait-list identity, zero when no object owns this wait (word 10).
pub const TCB_WAIT_OBJ: u32 = 40;
/// Argument retained for completion by the waker (word 11).
pub const TCB_WAIT_ARG: u32 = 44;
/// Lazily allocated Wi-Fi thread semaphore; zero until first use (word 12).
pub const TCB_THREAD_SEM: u32 = 48;
/// Address of the list head word or predecessor's TCB_WAIT_NEXT that holds this TCB, zero when unlinked (word 13).
pub const TCB_WAIT_PREV: u32 = 52;
/// Stack-base sentinel, checked before publishing a switched-out context.
pub const GUARD: u32 = 0x51ac_c0de;

/// Reserved bytes below the interrupted SP, including untouched ABI space.
pub const CONTEXT_SIZE: usize = 256;
/// Actual saved payload; bytes 208..240 are spare and 240..256 are ABI space.
pub const CONTEXT_PAYLOAD_SIZE: usize = 208;
/// EPC1, the interrupted instruction address.
pub const CONTEXT_PC: u32 = 0;
/// PS as observed on exception entry, including EXCM.
pub const CONTEXT_PS: u32 = 4;
/// Sixteen words a0..a15, with a1 retaining the interrupted task's real SP.
pub const CONTEXT_AR: u32 = 8;
/// SAR, special register 3.
pub const CONTEXT_SAR: u32 = 72;
/// Loop begin, special register 0.
pub const CONTEXT_LBEG: u32 = 76;
/// Loop end, special register 1.
pub const CONTEXT_LEND: u32 = 80;
/// Loop count, special register 2.
pub const CONTEXT_LCOUNT: u32 = 84;
/// Boolean register, special register 4.
pub const CONTEXT_BR: u32 = 88;
/// Conditional Store comparison register, special register 12.
pub const CONTEXT_SCOMPARE1: u32 = 92;
/// Thread pointer, user register 231.
pub const CONTEXT_THREADPTR: u32 = 96;
/// MAC16 accumulator low word, special register 16.
pub const CONTEXT_ACCLO: u32 = 100;
/// Sign-extended MAC16 accumulator high byte, special register 17.
pub const CONTEXT_ACCHI: u32 = 104;
/// Four words M0..M3, special registers 32..35.
pub const CONTEXT_M: u32 = 108;
/// Original coprocessor enables, special register 224.
pub const CONTEXT_CPENABLE: u32 = 124;
/// Floating-point control, user register 232.
pub const CONTEXT_FCR: u32 = 128;
/// Floating-point status, user register 233.
pub const CONTEXT_FSR: u32 = 132;
/// Sixteen raw floating-point words f0..f15.
pub const CONTEXT_FR: u32 = 136;
/// WINDOWBASE for fault reports only; never restored.
pub const CONTEXT_WINDOWBASE: u32 = 200;
/// WINDOWSTART before spilling, for fault reports only; never restored.
pub const CONTEXT_WINDOWSTART: u32 = 204;

const EXCM: u32 = 1 << 4;
const UM: u32 = 1 << 5;
const WOE: u32 = 1 << 18;
const PS: u32 = 230;

/// Initial PS for a root entered through the windowed CALL8 ABI.
pub const WINDOWED_PS: u32 = UM | WOE | (2 << 16);
/// Synthetic CALL8 return link; the top bits encode the window increment.
pub const fn windowed_exit_link(exit: u32) -> u32 { (2 << 30) | (exit & 0x3fff_ffff) }

/// Absolute SRAM/code addresses used by the byte producer.
pub struct RadioLayout {
    /// Word containing the running task's TCB address; the scheduler cursor
    /// is the next word, at sched::CURSOR_OFFSET (also enforced by sched_code).
    pub current: u32,
    /// 16-byte-aligned exclusive top of the dedicated interrupt stack.
    pub irq_stack_top: u32,
    /// Guard address; backtrace loads start at base+4.
    pub irq_stack_base: u32,
    /// TCB address array and the word holding its current length.
    pub tasks: u32,
    pub task_count: u32,
    pub dispatcher: u32,
    pub idle: u32,
    pub isr_count: u32,
    pub wake_lines: u32,
    /// Enables owned by the image's level-1 vector, preserved across radio recovery.
    pub vector_lines: u32,
    /// Half-open shim range: the walk stops before traversing its CALL0 caller.
    pub shim_start: u32,
    pub shim_end: u32,
    pub radio_state: u32,
    /// Island window sentinel sampled by the fault recorder.
    pub island_guard: u32,
    /// Island load status sampled alongside the sentinel.
    pub island_status: u32,
    /// Start of the padded fault record. The dispatcher stop cause follows at STOP_CAUSE_OFFSET.
    pub fault: u32,
    pub trace_head: u32,
    pub trace: u32,
    pub trace_entries: u32,
    /// CALL0 routine: a2=current TCB in, a2=next TCB out. May clobber every
    /// AR except a1; must preserve the kernel's PS/window/zero-LCOUNT state.
    pub pick: u32,
}

/// Executable bytes, including the literal table, and absolute entry points.
pub struct KernelCode {
    pub bytes: Vec<u8>,
    /// Entered from cause 4 with original a0 in EXCSAVE1 and EXCM=1.
    pub irq_entry: u32,
    /// a2=next TCB; level-1 interrupts masked, WOE=0, LCOUNT=0.
    pub restore: u32,
    /// Guard fault after the full save: a0=frame, a2=current, a3=found guard.
    pub stack_fault: u32,
    /// Unhandled user exception: original a0 in EXCSAVE1, EXCM=1.
    pub fault_entry: u32,
    /// Dispatcher CALL0 at INTLEVEL 15, a2=cause. Returns masked without a pick.
    pub radio_stop: u32,
    /// Runtime faults stop here until the RWDT resets the core.
    pub fatal: u32,
    /// ILL reached when a task's root function returns.
    pub task_exit: u32,
    /// CALL0 adapter trace; a8=gate, a2..a7=arguments. Clobbers a8..a11.
    pub trace_record: u32,
}

fn rsr(a: &mut Asm, reg: u8, sr: u32) { a.op24(enc::rsr(0, 3, 0, sr, reg)); }
fn rur(a: &mut Asm, reg: u8, ur: u8) {
    a.op24(enc::rrr(0, 3, 14, reg, ur >> 4, ur & 15));
}
fn wur(a: &mut Asm, reg: u8, ur: u32) { a.op24(enc::rsr(0, 3, 15, ur, reg)); }
fn save_sr(a: &mut Asm, sr: u32, offset: u32) {
    rsr(a, A2, sr);
    a.s32i(A2, A0, offset);
}
fn restore_sr(a: &mut Asm, sr: u32, offset: u32) {
    a.l32i(A3, A0, offset);
    a.wsr(A3, sr);
}

/// Assemble at a word-aligned absolute base. Install window_block() at
/// vecbase and user_vector(vecbase, irq_entry, fault_entry) at vecbase+0x340.
/// No runtime installation or scheduler policy is supplied here.
pub fn kernel_code(base: u32, vecbase: u32, layout: &RadioLayout) -> KernelCode {
    assert_eq!(base & 3, 0, "kernel literal table requires word alignment");
    assert_eq!(layout.current & 3, 0, "current TCB word requires word alignment");
    assert_eq!(layout.irq_stack_top & 15, 0, "interrupt stack requires ABI alignment");
    assert!(layout.trace_entries == 0 || ((2..=1024).contains(&layout.trace_entries)
        && layout.trace_entries.is_power_of_two()), "invalid adapter trace capacity");
    for address in [layout.irq_stack_base, layout.tasks, layout.task_count, layout.dispatcher,
        layout.idle, layout.isr_count, layout.radio_state, layout.island_guard, layout.island_status,
        layout.fault, layout.trace_head, layout.trace]
    { assert_eq!(address & 3, 0, "radio fault SRAM requires word alignment"); }
    const FAULT: usize = 0;
    const PICK: usize = 1;
    const IDLE_FAST: usize = super::fault::LABELS;
    const IDLE_GENERAL: usize = IDLE_FAST + 1;
    const RESTORE: usize = IDLE_GENERAL + 1;
    let mut code = KernelCode { bytes: Vec::new(), irq_entry: 0, restore: 0, stack_fault: 0,
        fault_entry: 0, radio_stop: 0, fatal: 0, task_exit: 0, trace_record: 0 };
    code.bytes = assemble(&mut Asm::new(RESTORE + 1, A3), |a| {
        a.literal_table();
        code.irq_entry = base.checked_add(a.base.offset()).expect("kernel address overflow");
        // ADDMI a0,a1,-256. Keep a1 unchanged until every caller is spilled:
        // the most recent caller's S32E/L32E use this SP (pp. 252–254).
        a.op24(enc::rri8(2, 13, A1, A0, 255));
        a.s32i(A2, A0, CONTEXT_AR + 8);
        a.s32i(A3, A0, CONTEXT_AR + 12);
        a.mov_reg_i32(A2, layout.current);
        a.l32i(A2, A2, 0);
        a.mov_reg_i32(A3, layout.idle);
        a.bcc_reg_reg_label(cc::EQ, A2, A3, IDLE_FAST);
        a.s32i(A1, A0, CONTEXT_AR + 4);
        for reg in 4..16 { a.s32i(reg, A0, CONTEXT_AR + u32::from(reg) * 4); }
        // Only a0/a2/a3 are scratch until spilling finishes. Higher physical
        // ARs can still belong to an older live frame (pp. 245–246).
        for (sr, offset) in [(177, CONTEXT_PC), (PS, CONTEXT_PS),
            (209, CONTEXT_AR), (3, CONTEXT_SAR), (4, CONTEXT_BR),
            (0, CONTEXT_LBEG), (1, CONTEXT_LEND), (2, CONTEXT_LCOUNT)]
        { save_sr(a, sr, offset); }
        // pp. 280/286: WSR LCOUNT=0 -> ISYNC -> clear PS.EXCM.
        // Keep loops disabled throughout the spill and the CALL0 picker.
        a.movi(A2, 0);
        a.wsr(A2, 2);
        a.isync();
        for (sr, offset) in [(12, CONTEXT_SCOMPARE1), (16, CONTEXT_ACCLO),
            (17, CONTEXT_ACCHI), (32, CONTEXT_M), (33, CONTEXT_M + 4),
            (34, CONTEXT_M + 8), (35, CONTEXT_M + 12)]
        { save_sr(a, sr, offset); }
        rur(a, A2, 231);
        a.s32i(A2, A0, CONTEXT_THREADPTR);
        save_sr(a, 224, CONTEXT_CPENABLE);
        a.movi(A3, 1);
        a.or(A2, A2, A3);
        a.wsr(A2, 224);
        a.rsync(); // CPENABLE -> RSYNC -> coprocessor access, p. 312.
        for (ur, offset) in [(232, CONTEXT_FCR), (233, CONTEXT_FSR)] {
            rur(a, A2, ur);
            a.s32i(A2, A0, offset);
        }
        for reg in 0..16 {
            a.rfr(A2, reg);
            a.s32i(A2, A0, CONTEXT_FR + u32::from(reg) * 4);
        }
        save_sr(a, 72, CONTEXT_WINDOWBASE);
        save_sr(a, 73, CONTEXT_WINDOWSTART);

        a.l32i(A2, A0, CONTEXT_PS);
        a.movi(A3, UM as i32);
        a.and(A2, A2, A3);
        a.mov_reg_i32(A3, WOE | 3);
        a.or(A2, A2, A3);
        a.wsr(A2, PS);
        // pp. 285–286: raising INTLEVEL while clearing EXCM opens no gap.
        // p. 289: RSYNC makes WOE visible to WindowCheck.
        a.rsync();
        // p. 246: each AND spills the oldest live frame in its next three
        // groups, RFWO retries it, and ROTW advances after all three clear.
        // Visit WB+1..WB+15 exactly; never spill the current frame at WB+16.
        for rotation in [3, 3, 3, 3, 4] {
            a.and(A12, A12, A12);
            a.rotw(rotation);
        }
        a.l32i(A2, A0, CONTEXT_PS);
        a.movi(A3, UM as i32);
        a.and(A2, A2, A3);
        a.movi(A3, 3);
        a.or(A2, A2, A3);
        a.wsr(A2, PS); // WOE=0 for CALL0 scratch registers, EXCM=0.
        a.rsync(); // pp. 285–289.

        a.mov_reg_i32(A2, layout.current);
        a.l32i(A2, A2, 0);
        a.l32i(A3, A2, TCB_STACK_BASE);
        a.l32i(A3, A3, 0);
        a.mov_reg_i32(A4, GUARD);
        a.bcc_reg_reg_label(cc::NE, A3, A4, FAULT);
        a.s32i(A0, A2, TCB_FRAME);
        a.mov_reg_i32(A1, layout.irq_stack_top);
        a.base.label_assign(PICK);
        a.mov_reg_i32(A3, layout.pick);
        a.callx0(A3);
        a.mov_reg_i32(A3, layout.current);
        a.s32i(A2, A3, 0);

        a.base.label_assign(RESTORE);
        code.restore = base.checked_add(a.base.offset()).expect("kernel address overflow");
        a.l32i(A0, A2, TCB_FRAME);
        a.l32i(A3, A0, CONTEXT_PS);
        a.movi(A4, EXCM as i32);
        a.or(A3, A3, A4);
        a.wsr(A3, PS);
        a.rsync(); // PS interrupt/window controls, pp. 285–289.
        // p. 286: EXCM=1 BEFORE restoring nonzero LCOUNT. pp. 278–280:
        // restore the loop bounds, then count, then ISYNC before loop use.
        // EXCM remains set until RFE (p. 571); no restored loop can fire here.
        restore_sr(a, 0, CONTEXT_LBEG);
        restore_sr(a, 1, CONTEXT_LEND);
        restore_sr(a, 2, CONTEXT_LCOUNT);
        a.isync();

        rsr(a, A3, 72);
        a.movi(A4, 1);
        a.ssl(A3);
        a.sll(A4, A4);
        a.wsr(A4, 73);
        a.rsync(); // WINDOWSTART before CWOE becomes active, p. 290.
        // Never write WINDOWBASE. Callers are in memory; RETW reloads them
        // relative to the new location through real underflow vectors.
        a.movi(A3, 1);
        a.wsr(A3, 224);
        a.rsync(); // p. 312: CP0 must be enabled before FP restoration.
        for (ur, offset) in [(232, CONTEXT_FCR), (233, CONTEXT_FSR)] {
            a.l32i(A3, A0, offset);
            wur(a, A3, ur);
        }
        for reg in 0..16 {
            a.l32i(A3, A0, CONTEXT_FR + u32::from(reg) * 4);
            a.wfr(reg, A3);
        }
        restore_sr(a, 224, CONTEXT_CPENABLE);
        a.rsync(); // p. 312; no further coprocessor access before RFE.
        for (sr, offset) in [(16, CONTEXT_ACCLO), (17, CONTEXT_ACCHI),
            (32, CONTEXT_M), (33, CONTEXT_M + 4), (34, CONTEXT_M + 8),
            (35, CONTEXT_M + 12), (12, CONTEXT_SCOMPARE1)]
        { restore_sr(a, sr, offset); }
        a.l32i(A3, A0, CONTEXT_THREADPTR);
        wur(a, A3, 231);
        restore_sr(a, 4, CONTEXT_BR);
        restore_sr(a, 3, CONTEXT_SAR);
        restore_sr(a, 177, CONTEXT_PC);
        for reg in 2..16 { a.l32i(reg, A0, CONTEXT_AR + u32::from(reg) * 4); }
        a.l32i(A1, A0, CONTEXT_AR + 4);
        a.l32i(A0, A0, CONTEXT_AR);
        a.rfe();

        a.base.label_assign(FAULT);
        code.stack_fault = base.checked_add(a.base.offset()).expect("kernel address overflow");
        super::fault::emit(a, base, layout, &mut code, PICK);

        a.base.label_assign(IDLE_FAST);
        // Idle only executes WAITI 0; J. Its one live window has no callers,
        // WOE=0 and LCOUNT=0; it never blocks, and a fault with idle current
        // is fatal. TCB_FRAME therefore stays on its boot frame. The two
        // entry stores overwrite only that frame's a2/a3, which idle never
        // reads; PC, PS, a1 and CPENABLE (and every other word) stay intact.
        // Only idle has priority zero. While it is current, no other task
        // is READY; a pure dispatcher wake therefore needs no table scan.
        rsr(a, A4, 226);
        rsr(a, A5, 228);
        a.and(A4, A4, A5);
        a.bccz_reg_label(ccz::EQ, A4, IDLE_GENERAL);
        a.mov_reg_i32(A3, !layout.wake_lines);
        a.and(A6, A4, A3);
        a.bccz_reg_label(ccz::NE, A6, IDLE_GENERAL);
        a.wsr(A4, 227);
        a.rsync(); // INTCLEAR before dispatch, p. 304, just as pick.
        a.and(A5, A5, A3);
        a.wsr(A5, 228);
        a.rsync(); // INTENABLE before further use, p. 305.
        a.mov_reg_i32(A2, layout.dispatcher);
        a.movi(A4, super::sched::READY as i32); // READY=0, also cursor index 0
        a.s32i(A4, A2, TCB_STATE);
        a.mov_reg_i32(A3, layout.current);
        a.s32i(A4, A3, super::sched::CURSOR_OFFSET); // boot.rs puts dispatcher first
        a.s32i(A2, A3, 0);
        // restore's three preconditions: EXCM=1 still masks level 1,
        // idle's WOE=0 is unchanged, and its LCOUNT=0 is unchanged.
        a.j_label(RESTORE);

        a.base.label_assign(IDLE_GENERAL);
        // Same transition and instruction order as the normal irq_entry:
        // pp. 280/286: WSR LCOUNT=0 -> ISYNC -> clear PS.EXCM.
        a.movi(A2, 0);
        a.wsr(A2, 2);
        a.isync();
        a.l32i(A2, A0, CONTEXT_PS); // saved UM in idle's unchanged boot frame
        a.movi(A3, UM as i32);
        a.and(A2, A2, A3);
        a.movi(A3, 3);
        a.or(A2, A2, A3);
        a.wsr(A2, PS); // EXCM=0, WOE=0, INTLEVEL=3 for CALL0 pick.
        // pp. 285–286: raising INTLEVEL while clearing EXCM opens no gap.
        a.rsync(); // PS interrupt/window controls, pp. 285–289.
        a.mov_reg_i32(A1, layout.irq_stack_top);
        a.mov_reg_i32(A2, layout.idle);
        a.j_label(PICK);
    });
    base.checked_add(code.bytes.len() as u32).expect("kernel address overflow");
    super::vectors::irq_displacement(vecbase, code.irq_entry);
    code
}

/// ABI expected at the first instruction of a fresh task.
#[derive(Clone, Copy, Debug)]
pub enum TaskAbi { Call0, Windowed }

/// Initial image at `address`, ready to copy into the task stack.
pub struct InitialFrame {
    pub address: u32,
    pub bytes: [u8; CONTEXT_SIZE],
}

/// Construct a root context. Caller installs GUARD at stack_base, copies this
/// image, and sets TCB.frame to address. stack_top is exclusive and 16-aligned.
///
/// The synthetic caller SP is stack_top-64. Its base save area at SP-16..SP
/// contains zeros except SP-12 = stack_top: a synthetic predecessor SP.
/// OF8/OF12 use that backlink to find this root's extra save area at
/// stack_top-32..-16 or stack_top-48..-16 (§6.1.5 pp. 250–253). Thus even
/// the oldest root spills entirely inside this task's allocation. Subsequent
/// ENTRY instructions allocate real frames and real spills supply backlinks.
/// The exit trampoline shares entry's 1-GB region for RETW (p. 252) and
/// runs in the synthetic caller; it must terminate/yield rather than return.
pub fn initial_frame(stack_base: u32, stack_top: u32, entry: u32, exit: u32,
    abi: TaskAbi, arguments: [u32; 6]) -> InitialFrame
{
    assert_eq!(stack_base & 3, 0, "stack guard requires word alignment");
    assert_eq!(stack_top & 15, 0, "initial stack requires ABI alignment");
    let sp = stack_top.checked_sub(64).expect("root stack underflow");
    let address = sp.checked_sub(CONTEXT_SIZE as u32).expect("context stack underflow");
    assert!(address >= stack_base.checked_add(4).expect("guard address overflow"),
        "initial context must fit above the guard; reserve more headroom for task calls");
    let mut frame = InitialFrame { address, bytes: [0; CONTEXT_SIZE] };
    let mut put = |offset: u32, value: u32| {
        let offset = offset as usize;
        frame.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    put(CONTEXT_PC, entry);
    put(CONTEXT_AR + 4, sp);
    put(CONTEXT_CPENABLE, 1); // FP only; other coprocessors must fault visibly.
    put(CONTEXT_SIZE as u32 - 12, stack_top);
    let first_arg = match abi {
        TaskAbi::Windowed => {
            assert_eq!(entry >> 30, exit >> 30, "RETW exit must share entry's 1-GB region");
            put(CONTEXT_PS, WINDOWED_PS);
            put(CONTEXT_AR + 8 * 4, windowed_exit_link(exit));
            10
        }
        TaskAbi::Call0 => {
            put(CONTEXT_PS, UM);
            put(CONTEXT_AR, exit);
            2
        }
    };
    for (index, value) in arguments.into_iter().enumerate() {
        put(CONTEXT_AR + (first_arg + index as u32) * 4, value);
    }
    frame
}
