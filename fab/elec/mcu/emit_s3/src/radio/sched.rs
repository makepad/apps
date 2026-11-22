//! Tickless level-1 scheduling policy for the radio kernel.
//!
//! ISA citations are printed pages of the Xtensa ISA Summary.

use fab_elec_mcu_emit32::xtensa::{assemble, cc, ccz, enc, reg::*, Asm};
use fab_elec_mcu_rt::watch::{FAULT_KERNEL_CRIT_SPAN, FAULT_KERNEL_ISR_DEPTH, SITE_RADIO_SCHEDULER};

use super::task::{TCB_PRIORITY, TCB_STATE};

pub const READY: u32 = 0;
pub const BLOCKED: u32 = 1;
pub const DEAD: u32 = 2;
/// S3 interrupt 7: software, level 1 (core-isa.h:428).
pub const YIELD_LINE: u32 = 7;
/// Bytes per ISR slot: line mask, windowed island entry, argument.
pub const ISR_SLOT: u32 = 12;
/// Cursor word immediately after current; shared with irq_entry's idle wake.
pub const CURSOR_OFFSET: u32 = 4;

/// Marked slot allowance: the original 20 plus 27 marking instructions and
/// two report appends of at most 15 instructions each (kernel/ports.rs,
/// KERNEL_REPORT). See sched_code for the path count.
pub const MARKED_ISR_SLOT_INSNS: u64 = 20 + 27 + 2 * 15;

/// The caller allocates and init clears the three ISR mark words in radio
/// SRAM, separate from the ISR slots. The report address is the kernel's port-78 S3 routine:
/// a2=code, a3=arg, a10=site; clobbers a4..a7, preserves a12..a15.
/// pick appends at INTLEVEL 3; exports run at 15, so no other appender can
/// interleave with this masked append. The dispatcher drains while masked.
#[derive(Clone, Copy)]
pub struct SchedMarks {
    /// CCOUNT sampled immediately before the shim call.
    pub isr_start: u32,
    /// Largest wrapping CCOUNT difference seen on return from the shim.
    pub isr_max_span: u32,
    /// Slot entry associated with that maximum.
    pub isr_max_entry: u32,
    /// KernelCode::crit_depth, observed without changing the nesting count.
    pub crit_depth: u32,
    pub report: u32,
    /// Same cycle threshold as the kernel's crit_span_report word (KernelCode::crit_span_report).
    pub threshold: u32,
}

/// Word-aligned RAM owned by this core. Counts are RAM words, not capacities.
/// Publish table changes with interrupts masked; tables remain stable in pick.
/// TCB addresses are nonzero, cursor < task_count, and idle is always READY.
/// Dispatcher is tasks[0]. Idle alone has priority zero: every island task
/// must have priority >= 1. R4 task creation enforces this.
/// OSI maps the assumed IDF priorities 0..=24 to 1..=25. The priority word
/// and the unsigned comparison in pick retain all 32 bits, including 25.
/// R4 ISR registration must reject slots naming any wake line or the yield line.
/// Tasks become READY only inside pick or inside a kernel export. An export
/// runs from a task through a gate, never while idle is current; consequently
/// idle current means every other task is blocked or dead.
/// Wake lines, ISR masks and the yield line have distinct owners. All owned
/// lines are level 1; other INTENABLE writers use INTLEVEL 15 and cannot
/// preempt pick's level-3 read-modify-write sequences.
pub struct SchedLayout {
    /// RadioLayout.current: word holding the running TCB's address.
    pub current: u32,
    /// Word at current + CURSOR_OFFSET: index in tasks of the last task picked.
    pub cursor: u32,
    /// Word: valid entries in tasks; tasks can be added at run time.
    pub task_count: u32,
    /// Array of TCB addresses, one word each.
    pub tasks: u32,
    /// Word: valid slots in isrs.
    pub isr_count: u32,
    /// Array of ISR_SLOT-byte slots {mask, entry, arg}.
    pub isrs: u32,
    /// Word: picks finding an enabled pending line nobody owns.
    pub stray: u32,
    /// TCB that wake_lines wake.
    pub dispatcher: u32,
    /// INTENABLE bits that wake dispatcher; all are masked on any wake.
    /// The dispatcher re-enables the ones it waits for when it next sleeps,
    /// like the runtime's kernel vector before radio.
    pub wake_lines: u32,
    /// Lines serviced directly by the level-1 vector, including arrivals during pick.
    pub vector_lines: u32,
    /// ShimCode.call; each ISR entry must share its 1-GB region (p. 252).
    pub shim: u32,
    /// None retains the original scheduler bytes.
    pub marks: Option<SchedMarks>,
}

/// Executable bytes, including the literal table, and absolute entries.
pub struct SchedCode {
    pub bytes: Vec<u8>,
    /// CALL0: a2=current TCB in, a2=next TCB out; see sched_code's contract.
    pub pick: u32,
    /// CALL0: block a2's TCB, then resume and return at INTLEVEL 0.
    /// Clobbers only a2..a3; PS changes only in INTLEVEL.
    /// Caller is a CALL0 task (WOE=0) or a blocking gate (WOE=1) with a1 back
    /// at T, so every live caller frame's save area is where a spill expects.
    pub block: u32,
    /// WAITI 0 followed by a jump back; entry for a TaskAbi::Call0 task.
    /// Preserves all ARs and PS except INTLEVEL, which becomes zero (p. 644).
    pub idle: u32,
    /// Marked J .: no READY task, including idle. Never a fallback selection.
    pub no_task: u32,
}

/// Assemble at a word-aligned absolute base (literal table allowed).
///
/// pick implements RadioLayout.pick: CALL0 on the interrupt stack, a2 equal
/// to [current], INTLEVEL=3, WOE=0, EXCM=0, LCOUNT=0, and normally WINDOWSTART
/// equal to 1<<WINDOWBASE. Fault recovery may retain abandoned windows with
/// isr_count=0: no shim is called, and restore discards those windows after
/// selection. It preserves a1, PS and both window registers, returns
/// LCOUNT=0, and may clobber all other ARs and the shim's caller-saved state.
/// The kernel publishes the returned TCB to current. Stack use is 16 bytes
/// plus SHIM_FRAME and the ISR's depth. ISRs must return through the windowed
/// ABI with PS.INTLEVEL restored to 3; they may ready tasks, but may
/// neither block nor mutate the scheduler's tables during this pass.
///
/// Pending enabled bits are acknowledged before ISRs. INTCLEAR affects only
/// software/edge requests, never level requests (pp. 153–156, 304); RSYNC
/// orders its effect before handlers. Wake and stray masks use INTENABLE
/// read-modify-write at INTLEVEL 3 (other writers are serialized at level 15),
/// with RSYNC before further use (p. 305). Every valid slot contributes its
/// ownership mask, including slots not called. ISRs run before selection.
/// The scan visits cursor+1 modulo n first, cursor last; unsigned priorities
/// and strict improvement give round robin among the highest READY peers.
///
/// For n tasks and m slots, a returning pick retires at most
/// 64 + 20*n + S*m + sum(C_i) instructions: S=20 without marks, S=77 with
/// marks (MARKED_ISR_SLOT_INSNS). The longest marked path has 3 instructions
/// before the shim, then 17 through the span report call and 7 through the
/// depth report call. Each call loads the scheduler site with one L32R.
/// Add two 15-instruction port-78 appends: 27+30=57.
/// Count each expanded branch's executed arm; a skipped report is cheaper.
/// The append's successful path is 6 through its space check, then 9 through
/// RET.N; full-ring paths are shorter. Tests count the emitted paths.
/// C_i counts each matching slot's
/// entire shim/ISR execution, including window exceptions (the shim itself
/// is 30 instructions). The bound counts expanded conditional branches;
/// it excludes kernel save/restore, higher-priority interrupts and no_task.
/// No bound on arbitrary ISR code or its stack depth is implied.
///
/// block is called at INTLEVEL=15, UM=1, EXCM=0, with a2 its own TCB and
/// the yield line enabled. Its caller is a CALL0 task (WOE=0) or a blocking
/// gate thunk (WOE=1) whose a1 is back at T, so every live caller frame's
/// save area is where a spill expects it. It stores BLOCKED, sets
/// INTSET, lowers INTLEVEL to zero, then RSYNCs before RET.N. INTSET needs
/// RSYNC before an instruction that must follow its interrupt (p. 304);
/// RSIL itself needs no SYNC (pp. 156, 582–583). The task resumes at that
/// synchronization boundary when selected READY, then returns at level 0.
/// All registers except a2..a3 are preserved, including a0/a1, and all PS
/// fields except INTLEVEL. An already-pending enabled wake is processed in
/// the same pass as yield, after BLOCKED is stored, so it cannot be lost.
pub fn sched_code(base: u32, layout: &SchedLayout) -> SchedCode {
    assert_eq!(base & 3, 0, "scheduler literal table requires word alignment");
    assert_eq!(Some(layout.cursor), layout.current.checked_add(CURSOR_OFFSET),
        "idle wake requires cursor immediately after current");
    for address in [layout.current, layout.cursor, layout.task_count, layout.tasks,
        layout.isr_count, layout.isrs, layout.stray, layout.dispatcher]
    { assert_eq!(address & 3, 0, "scheduler RAM requires word alignment"); }
    if let Some(m) = layout.marks {
        for address in [m.isr_start, m.isr_max_span, m.isr_max_entry, m.crit_depth] {
            assert_eq!(address & 3, 0, "scheduler mark words require word alignment");
        }
    }
    const SLOTS: usize = 0;
    const SLOT: usize = 1;
    const NEXT_SLOT: usize = 2;
    const STRAY: usize = 3;
    const SCAN: usize = 4;
    const TASK: usize = 5;
    const INDEX: usize = 6;
    const CHOOSE: usize = 7;
    const NEXT_TASK: usize = 8;
    const NO_TASK: usize = 9;
    const IDLE: usize = 10;
    const CHECK_DEPTH: usize = 11;
    const MARKED: usize = 12;
    let mut code = SchedCode { bytes: Vec::new(), pick: 0, block: 0, idle: 0, no_task: 0 };
    code.bytes = assemble(&mut Asm::new(13, A3), |a| {
        a.literal_table();
        code.pick = base.checked_add(a.base.offset()).expect("scheduler address overflow");
        a.addi(A1, A1, -16);
        a.s32i(A0, A1, 0);
        a.op24(enc::rsr(0, 3, 0, 226, A12));
        a.op24(enc::rsr(0, 3, 0, 228, A3));
        a.and(A12, A12, A3); // p, preserved across every shim call
        a.wsr(A12, 227);
        a.rsync(); // INTCLEAR -> handler, p. 304.

        a.mov_reg_i32(A3, layout.wake_lines);
        a.and(A3, A12, A3);
        a.bccz_reg_label(ccz::EQ, A3, SLOTS);
        a.op24(enc::rsr(0, 3, 0, 228, A4));
        a.mov_reg_i32(A3, !layout.wake_lines);
        a.and(A4, A4, A3);
        a.wsr(A4, 228);
        a.rsync();
        a.mov_reg_i32(A3, layout.dispatcher);
        a.movi(A4, READY as i32);
        a.s32i(A4, A3, TCB_STATE);

        a.base.label_assign(SLOTS);
        a.movi(A13, 0); // slot index
        a.movi(A14, 0); // handled: union of all valid slot masks
        a.mov_reg_i32(A15, layout.isrs);
        a.base.label_assign(SLOT);
        a.mov_reg_i32(A4, layout.isr_count);
        a.l32i(A4, A4, 0);
        a.bcc_reg_reg_label(cc::GEU, A13, A4, STRAY);
        a.l32i(A4, A15, 0);
        a.or(A14, A14, A4);
        a.and(A4, A4, A12);
        a.bccz_reg_label(ccz::EQ, A4, NEXT_SLOT);
        a.l32i(A2, A15, 4);
        a.l32i(A3, A15, 8);
        a.mov_reg_i32(A4, layout.shim);
        if let Some(m) = layout.marks {
            a.mov_reg_i32(A5, m.isr_start);
            a.op24(enc::rsr(0, 3, 0, 234, A6));
            a.s32i(A6, A5, 0);
        }
        a.callx0(A4);
        if let Some(m) = layout.marks {
            a.op24(enc::rsr(0, 3, 0, 234, A3));
            a.mov_reg_i32(A4, m.isr_start);
            a.l32i(A4, A4, 0);
            a.sub(A3, A3, A4);
            a.mov_reg_i32(A4, m.isr_max_span);
            a.l32i(A5, A4, 0);
            a.bcc_reg_reg_label(cc::GEU, A5, A3, CHECK_DEPTH);
            a.s32i(A3, A4, 0);
            a.l32i(A5, A15, 4);
            a.mov_reg_i32(A4, m.isr_max_entry);
            a.s32i(A5, A4, 0);
            a.mov_reg_i32(A4, m.threshold);
            a.bcc_reg_reg_label(cc::GEU, A4, A3, CHECK_DEPTH);
            a.movi(A2, FAULT_KERNEL_CRIT_SPAN as i32);
            a.mov_reg_i32(A10, u32::from(SITE_RADIO_SCHEDULER));
            a.mov_reg_i32(A4, m.report);
            a.callx0(A4);
            a.base.label_assign(CHECK_DEPTH);
            a.mov_reg_i32(A4, m.crit_depth);
            a.l32i(A3, A4, 0);
            a.bccz_reg_label(ccz::EQ, A3, MARKED);
            a.movi(A2, FAULT_KERNEL_ISR_DEPTH as i32);
            a.mov_reg_i32(A10, u32::from(SITE_RADIO_SCHEDULER));
            a.mov_reg_i32(A4, m.report);
            a.callx0(A4);
            a.base.label_assign(MARKED);
        }
        // The shim permits loop-register clobbers. Keep the kernel's
        // zero-LCOUNT contract even after an ISR exits a loop early.
        a.movi(A4, 0);
        a.wsr(A4, 2);
        a.isync(); // LCOUNT before potential loop use, pp. 279–280.
        a.base.label_assign(NEXT_SLOT);
        a.addi(A13, A13, 1);
        a.addi(A15, A15, ISR_SLOT as i32);
        a.j_label(SLOT);

        a.base.label_assign(STRAY);
        a.mov_reg_i32(A3, (1 << YIELD_LINE) | layout.wake_lines | layout.vector_lines);
        a.or(A3, A3, A14);
        a.movi(A4, -1);
        a.xor(A3, A3, A4);
        a.and(A3, A3, A12);
        a.bccz_reg_label(ccz::EQ, A3, SCAN);
        a.op24(enc::rsr(0, 3, 0, 228, A5));
        a.xor(A3, A3, A4);
        a.and(A5, A5, A3);
        a.wsr(A5, 228);
        a.rsync();
        a.mov_reg_i32(A3, layout.stray);
        a.l32i(A4, A3, 0);
        a.addi(A4, A4, 1);
        a.s32i(A4, A3, 0);

        a.base.label_assign(SCAN);
        a.mov_reg_i32(A4, layout.task_count);
        a.l32i(A4, A4, 0);
        a.bccz_reg_label(ccz::EQ, A4, NO_TASK);
        a.mov_n(A5, A4); // entries remaining
        a.mov_reg_i32(A6, layout.cursor);
        a.l32i(A6, A6, 0);
        a.mov_reg_i32(A9, layout.tasks);
        a.movi(A2, 0); // no candidate yet; priority zero is still eligible
        a.movi(A7, 0); // best unsigned priority
        a.movi(A10, 0);
        a.base.label_assign(TASK);
        a.addi(A6, A6, 1);
        a.bcc_reg_reg_label(cc::LTU, A6, A4, INDEX);
        a.movi(A6, 0);
        a.base.label_assign(INDEX);
        a.addx4(A11, A6, A9);
        a.l32i(A11, A11, 0);
        a.l32i(A12, A11, TCB_STATE);
        // Only READY (zero) is eligible; DEAD tasks can never be picked.
        a.bcc_reg_reg_label(cc::NE, A12, A10, NEXT_TASK);
        a.l32i(A12, A11, TCB_PRIORITY);
        a.bcc_reg_reg_label(cc::EQ, A2, A10, CHOOSE);
        a.bcc_reg_reg_label(cc::GEU, A7, A12, NEXT_TASK);
        a.base.label_assign(CHOOSE);
        a.mov_n(A2, A11);
        a.mov_n(A7, A12);
        a.mov_n(A8, A6);
        a.base.label_assign(NEXT_TASK);
        a.addi(A5, A5, -1);
        a.bcc_reg_reg_label(cc::NE, A5, A10, TASK);
        a.bccz_reg_label(ccz::EQ, A2, NO_TASK);
        a.mov_reg_i32(A3, layout.cursor);
        a.s32i(A8, A3, 0);
        a.l32i(A0, A1, 0);
        a.addi(A1, A1, 16);
        a.ret_n();

        a.base.label_assign(NO_TASK);
        code.no_task = base.checked_add(a.base.offset()).expect("scheduler address overflow");
        a.j(-4);

        code.block = base.checked_add(a.base.offset()).expect("scheduler address overflow");
        a.movi(A3, BLOCKED as i32);
        a.s32i(A3, A2, TCB_STATE);
        a.movi(A3, (1 << YIELD_LINE) as i32);
        a.wsr(A3, 226);
        a.op24(enc::rrr(0, 0, 0, 6, 0, A3)); // RSIL a3,0, pp. 582–583.
        a.rsync(); // INTSET -> interrupt -> RET.N, p. 304.
        a.ret_n();

        a.base.label_assign(IDLE);
        code.idle = base.checked_add(a.base.offset()).expect("scheduler address overflow");
        a.waiti(0);
        a.j_label(IDLE);
    });
    base.checked_add(code.bytes.len() as u32).expect("scheduler address overflow");
    code
}
