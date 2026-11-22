//! Install zero-cleared radio state, then arm the first level-1 pick.
//! ISA citations are printed pages of the Xtensa ISA Summary.

use fab_elec_mcu_emit32::xtensa::{assemble, cc, enc, reg::*, Asm};

use crate::{image::SramLayout, Radio};
use super::{initial_frame, TaskAbi, CONTEXT_SIZE, GUARD, TCB_FRAME, TCB_PRIORITY,
    TCB_STACK_BASE, TCB_STACK_TOP, YIELD_LINE};

/// initial_frame needs 64 root bytes, CONTEXT_SIZE and a guard word, rounded to 16.
pub const MIN_STACK_BYTES: u32 = (64 + CONTEXT_SIZE as u32 + 4 + 15) & !15;
/// 336 initial bytes plus 176 bytes of margin; idle itself uses no stack.
pub const IDLE_STACK_BYTES: u32 = 512;
/// Interrupt scratch below the deepest dispatcher SP, plus its guard, rounded to 16.
pub const DISPATCHER_HEADROOM: u32 = (CONTEXT_SIZE as u32 + 4 + 15) & !15;

pub struct BootCode {
    pub bytes: Vec<u8>,
    pub start: u32,
    /// Number of periodic feeds on the READY path through the installer.
    pub(crate) feeds: u64,
}

const STORES_PER_FEED: usize = 128;

/// Only nonzero words: the caller already cleared the radio allocation. The running
/// dispatcher has no synthetic frame; its first exception fills FRAME at SP-256.
fn stores(l: &SramLayout, profile: &Radio, idle: u32, exit: u32) -> (Vec<(u32, u32)>, Vec<(u32, u32)>) {
    let r = l.radio.as_ref().expect("radio boot requires radio SRAM");
    let mut common = vec![(l.stack_top - l.stack_bytes, GUARD), (r.irq_stack.base, GUARD),
        (r.tasks, r.dispatcher), (r.task_count, 2), (r.current, r.dispatcher)];
    let mut optional = Vec::new();
    for (offset, value) in [(TCB_FRAME, l.stack_top - CONTEXT_SIZE as u32),
        (TCB_STACK_BASE, l.stack_top - l.stack_bytes), (TCB_STACK_TOP, l.stack_top),
        (TCB_PRIORITY, profile.dispatcher_priority)]
    { common.push((r.dispatcher + offset, value)); }
    let start = profile.start.map(|s| crate::BootTask { entry: profile.island.glue["radio_start"],
        abi: TaskAbi::Windowed, priority: s.priority, stack_bytes: s.stack_bytes,
        args: [r.wifi_config.expect("start task config"), 0, 0, 0, 0, 0] });
    let tasks = std::iter::once((r.idle, &r.idle_stack, idle, TaskAbi::Call0, 0, [0; 6]))
        .chain(r.boot_tasks.iter().zip(&r.boot_stacks).zip(profile.boot_tasks.iter().chain(start.as_ref()))
            .map(|((&tcb, stack), task)| (tcb, stack, task.entry, task.abi, task.priority, task.args)));
    for (index, (tcb, stack, entry, abi, priority, args)) in tasks.enumerate() {
        let out = if index == 0 { &mut common } else { &mut optional };
        let mut put = |at, value| { if value != 0 { out.push((at, value)); } };
        put(r.tasks + (index as u32 + 1) * 4, tcb);
        put(stack.base, GUARD);
        let frame = initial_frame(stack.base, stack.top, entry, exit, abi, args);
        for (offset, value) in [(TCB_FRAME, frame.address), (TCB_STACK_BASE, stack.base),
            (TCB_STACK_TOP, stack.top), (TCB_PRIORITY, priority)]
        { put(tcb + offset, value); }
        for (word, bytes) in frame.bytes.chunks_exact(4).enumerate() {
            put(frame.address + word as u32 * 4, u32::from_le_bytes(bytes.try_into().unwrap()));
        }
    }
    if !optional.is_empty() {
        optional.push((r.task_count, 2 + r.boot_tasks.len() as u32));
    }
    (common, optional)
}

/// CALL0 with INTLEVEL=15, WOE=EXCM=0. Before each start the caller must
/// clear the rebuild spans (the entire allocation at boot), then run ISLAND_START.
/// The absolute boot stores omit zero initializers. INTENABLE and PS are ORed with the required
/// bits and INTSET sets the yield bit. Init enters with UM=0. Its panel
/// delay restores level 15 after each WAITI, and setup and every feed preserve it.
/// Leaves UM=1, WOE=0, INTLEVEL=15. TURN's RSIL 0 admits the pending yield.
pub fn boot_code(base: u32, l: &SramLayout, profile: &Radio, idle: u32, exit: u32) -> BootCode {
    assert_eq!(base & 3, 0, "boot literal table requires word alignment");
    assert_eq!(base >> 30, exit >> 30, "task_exit must share the runtime's 1-GB region");
    let (common, optional) = stores(l, profile, idle, exit);
    let mut code = BootCode { bytes: Vec::new(), start: 0,
        feeds: ((common.len() + optional.len()) / STORES_PER_FEED) as u64 };
    code.bytes = assemble(&mut Asm::new(1, A3), |a| {
        a.literal_table();
        code.start = base + a.base.offset();
        // Profile-sized straight-line sequences feed every 128 stores, counting both lists.
        let mut stores_since_feed = 0;
        let mut store = |a: &mut Asm, &(address, value): &(u32, u32)| {
            // Fixed literal loads keep the size independent of absolute base
            // and of the dispatcher's stack size during the two link passes.
            a.mov_reg_i32(A2, address);
            a.mov_reg_i32(A3, value);
            a.s32i(A3, A2, 0);
            stores_since_feed += 1;
            if stores_since_feed == STORES_PER_FEED {
                // Same instruction sequence as Code::feed for radio images. Only a2/a3
                // serve the installer; a9..a11 are free across these stores.
                a.op24(enc::rrr(0, 0, 0, 6, 15, A11));
                a.mov_reg_i32(A9, crate::RWDT_FEED); a.mov_reg_i32(A10, crate::WDT_KEY);
                a.s32i(A10, A9, crate::RWDT_PROTECT - crate::RWDT_FEED);
                a.mov_reg_i32_optimised(A10, crate::RWDT_FEED_BIT); a.s32i(A10, A9, 0);
                a.mov_reg_i32_optimised(A10, 0);
                a.s32i(A10, A9, crate::RWDT_PROTECT - crate::RWDT_FEED);
                a.wsr(A11, 230); a.rsync();
                stores_since_feed = 0;
            }
        };
        for word in &common { store(a, word); }
        if !optional.is_empty() {
            a.mov_reg_i32(A2, l.radio.as_ref().unwrap().island_status);
            a.l32i(A2, A2, 0); a.bcci_reg_label(cc::NE, A2, super::ISLAND_READY as i32, 0);
            for word in &optional { store(a, word); }
            a.base.label_assign(0);
        }
        a.op24(enc::rsr(0, 3, 0, 228, A2));
        a.movi(A3, (1 << YIELD_LINE) as i32);
        a.or(A2, A2, A3);
        a.wsr(A2, 228);
        a.rsync(); // INTENABLE, p. 305.
        a.wsr(A3, 226); // INTSET, p. 304; still masked until TURN.
        a.op24(enc::rsr(0, 3, 0, 230, A2));
        a.movi(A3, 1 << 5);
        a.or(A2, A2, A3);
        a.wsr(A2, 230);
        a.rsync(); // PS.UM and pending INTSET, pp. 286–287,304.
        a.ret_n();
    });
    code
}
