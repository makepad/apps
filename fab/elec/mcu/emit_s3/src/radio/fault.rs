//! One-shot radio fault capture. Offsets below are WORDS, including the length.
//! ISA citations refer to printed pages of the Xtensa ISA Summary.

use fab_elec_mcu_emit32::xtensa::{cc, ccz, enc, reg::*, Asm};
use fab_elec_mcu_rt::{sched::Fault, watch};
use std::{collections::BTreeMap, fmt};
use super::task::*;
use super::kernel::{INTR_MATRIX, MODEM_RESET_FIELD, SYSCON_WIFI_RST_EN, WIFI_SOURCES};
use super::sched::{DEAD, READY, YIELD_LINE};

pub const RADIO_RUNNING: u32 = 0;
pub const RADIO_DEAD: u32 = 1;
pub const RADIO_STOPPED: u32 = 2;

/// The ordinary runtime SRAM clear establishes the initial island status.
pub const ISLAND_NOT_LOADED: u32 = 0;
pub const ISLAND_READY: u32 = 1;
pub const ISLAND_ABSENT: u32 = 2;
pub const ISLAND_MISMATCH: u32 = 3;
pub const FAULT_GUARD: u32 = 0x100;
pub const BACKTRACE_DEPTH: u32 = 12;
pub const TRACE_ENTRY_WORDS: u32 = 8;
pub const FAULT_TRACE_ENTRIES: u32 = 8;
/// Written last: zero means absent/incomplete, FAULT_WORDS means complete.
pub const FAULT_LENGTH: u32 = 0;
pub const FAULT_CAUSE: u32 = 1;
pub const FAULT_PC: u32 = 2;
pub const FAULT_ADDRESS: u32 = 3;
pub const FAULT_PS: u32 = 4;
pub const FAULT_TASK: u32 = 5;
pub const FAULT_TASK_INDEX: u32 = 6;
pub const FAULT_CONTEXT: u32 = 7;
pub const FAULT_WINDOWBASE: u32 = 8;
pub const FAULT_WINDOWSTART: u32 = 9;
pub const FAULT_TRACE_HEAD: u32 = 10;
/// Sixty-four physical ARs, starting at 4*WINDOWBASE and wrapping modulo 64.
pub const FAULT_AR: u32 = 11;
pub const FAULT_DEPTH: u32 = FAULT_AR + 64;
pub const FAULT_BACKTRACE: u32 = FAULT_DEPTH + 1;
pub const FAULT_TRACE: u32 = FAULT_BACKTRACE + BACKTRACE_DEPTH * 2;
pub const FAULT_ISLAND_GUARD: u32 = FAULT_TRACE + FAULT_TRACE_ENTRIES * TRACE_ENTRY_WORDS;
pub const FAULT_ISLAND_STATUS: u32 = FAULT_ISLAND_GUARD + 1;
/// Published length in words, excluding transport padding.
pub const FAULT_WORDS: u32 = FAULT_ISLAND_STATUS + 1;
pub const FAULT_FRAMES: u32 = FAULT_WORDS.div_ceil(4);
/// Byte offset from the record base to the published stop cause.
pub const STOP_CAUSE_OFFSET: u32 = FAULT_FRAMES * 16;
/// A masked island port stores its nonzero cause here before taking the shared ILL.
pub const STOP_PENDING_OFFSET: u32 = STOP_CAUSE_OFFSET + 4;

/// Collect one record per instance. Frames may arrive in any order; equal
/// repeats are accepted even after completion, differing repeats are errors.
/// Non-record events are ignored. Completion is yielded exactly once.
#[derive(Default, Debug)]
pub struct RecordCollector {
    frames: BTreeMap<u16, [u32; 4]>,
    length: Option<usize>,
    yielded: bool,
}

impl RecordCollector {
    pub fn new() -> Self { Self::default() }

    pub fn push(&mut self, event: watch::Event) -> Result<Option<Vec<u32>>, String> {
        let watch::Event::RadioRecord { index, words } = event else { return Ok(None); };
        if self.frames.get(&index).is_some_and(|previous| *previous != words) {
            return Err(format!("radio record frame {index} differs from its repeat"));
        }
        let length = if index == 0 {
            let length = words[0] as usize;
            if length == 0 || length > (usize::from(u16::MAX) + 1) * 4 {
                return Err(format!("radio record length {length} cannot be framed"));
            }
            Some(length)
        } else { self.length };
        if let Some(length) = length {
            let check = |index: u16, words: &[u32; 4]| -> Result<(), String> {
                let start = usize::from(index) * 4;
                if start >= length { return Err(format!("radio record frame {index} exceeds length {length}")); }
                if words.iter().enumerate().any(|(i, &word)| start + i >= length && word != 0) {
                    return Err(format!("radio record frame {index} has nonzero padding"));
                }
                Ok(())
            };
            check(index, &words)?;
            if self.length.is_none() {
                for (&index, words) in &self.frames { check(index, words)?; }
            }
        }
        self.frames.insert(index, words);
        self.length = length;
        if let Some(length) = length {
            if !self.yielded && self.frames.len() == length.div_ceil(4) {
                // All indices are in range and unique, so this covers every word.
                let mut record = Vec::with_capacity(length);
                for words in self.frames.values() { record.extend_from_slice(words); }
                record.truncate(length);
                self.yielded = true;
                return Ok(Some(record));
            }
        }
        Ok(None)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceEntry {
    pub gate: u32,
    pub args: [u32; 6],
    pub ccount: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioFault {
    pub cause: u32,
    pub pc: u32,
    pub address: u32,
    pub ps: u32,
    pub task: u32,
    pub task_index: u32,
    pub context: u32,
    pub window_base: u32,
    pub window_start: u32,
    pub trace_head: u32,
    /// Physical ARs starting at 4*WINDOWBASE, exactly as captured.
    pub ars: [u32; 64],
    /// (PC, SP), beginning with the faulting frame; later PCs are return addresses.
    pub backtrace: Vec<(u32, u32)>,
    /// Captured entries are already newest first, capped at eight.
    pub trace: Vec<TraceEntry>,
    /// Value read from the end of the linked island's SRAM window at the fault.
    pub island_guard: u32,
    pub island_status: u32,
    map: String,
}

impl RadioFault {
    pub fn decode(words: &[u32]) -> Result<Self, String> {
        if words.len() != FAULT_WORDS as usize || words[FAULT_LENGTH as usize] != FAULT_WORDS {
            return Err(format!("radio fault requires {FAULT_WORDS} words and a matching length word"));
        }
        let island_status = words[FAULT_ISLAND_STATUS as usize];
        if island_status > ISLAND_MISMATCH {
            return Err(format!("radio fault island status {island_status} exceeds {ISLAND_MISMATCH}"));
        }
        let depth = words[FAULT_DEPTH as usize];
        if depth > BACKTRACE_DEPTH { return Err(format!("radio fault depth {depth} exceeds {BACKTRACE_DEPTH}")); }
        let tail = (FAULT_BACKTRACE + depth * 2) as usize;
        if words[tail..FAULT_TRACE as usize].iter().any(|&word| word != 0) {
            return Err("radio fault has a nonzero backtrace tail beyond its depth".into());
        }
        let trace_head = words[FAULT_TRACE_HEAD as usize];
        let trace = words[FAULT_TRACE as usize..FAULT_ISLAND_GUARD as usize].chunks_exact(TRACE_ENTRY_WORDS as usize)
            .take(trace_head.min(FAULT_TRACE_ENTRIES) as usize)
            .map(|entry| TraceEntry { gate: entry[0], args: entry[1..7].try_into().unwrap(), ccount: entry[7] }).collect();
        Ok(Self {
            cause: words[FAULT_CAUSE as usize], pc: words[FAULT_PC as usize], address: words[FAULT_ADDRESS as usize],
            ps: words[FAULT_PS as usize], task: words[FAULT_TASK as usize], task_index: words[FAULT_TASK_INDEX as usize],
            context: words[FAULT_CONTEXT as usize], window_base: words[FAULT_WINDOWBASE as usize],
            window_start: words[FAULT_WINDOWSTART as usize], trace_head,
            ars: words[FAULT_AR as usize..FAULT_DEPTH as usize].try_into().unwrap(),
            backtrace: words[FAULT_BACKTRACE as usize..tail].chunks_exact(2).map(|pair| (pair[0], pair[1])).collect(),
            trace, island_guard: words[FAULT_ISLAND_GUARD as usize], island_status, map: String::new(),
        })
    }

    /// Attach the island's text map for Display. Without a matching row, Display
    /// retains the numeric PC. Decoding the wire record itself needs no map.
    pub fn with_map(mut self, map: &str) -> Self { self.map = map.to_owned(); self }

    /// The recorder already rotates physical ARs into WINDOWBASE-relative
    /// order. CALL0's a2/a3/a4 remain indices 2/3/4 at every WINDOWBASE.
    /// BlockInCritical carries the export index in a4; other fatal faults
    /// use only the code and source site in a2/a3.
    /// Only fatal kernel faults and loop caps originate at the ILL stub.
    pub fn kernel_fault(&self) -> Option<Fault> {
        if self.cause != 0 || fab_elec_mcu_island::symbolize(&self.map, self.pc)?.0 != "kernel_fault" {
            return None;
        }
        let code = self.ars[2].try_into().ok()?;
        let arg = if code == watch::FAULT_KERNEL_BLOCK_IN_CRITICAL { self.ars[4] as i32 } else { 0 };
        watch::Event::Fault { code, site: self.ars[3].try_into().ok()?, arg }.kernel_fault()
            .filter(|fault| matches!(fault, Fault::Kernel { .. } | Fault::LoopCap { .. }))
    }
}

pub fn cause_name(cause: u32) -> Option<&'static str> {
    Some(match cause {
        0 => "illegal", 1 => "syscall", 2 => "instruction fetch", 3 => "load/store error",
        6 => "divide by zero", 9 => "alignment", 32 => "coprocessor 0 disabled",
        FAULT_GUARD => "guard", _ => return None,
    })
}

impl fmt::Display for RadioFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(fault) = self.kernel_fault() {
            let (name, site) = match fault {
                Fault::Kernel { kind, site } => (kind.name(), site),
                Fault::LoopCap { site } => ("loop cap", site),
                _ => unreachable!("kernel fault decoder only returns kernel faults and loop cap"),
            };
            writeln!(f, "kernel fault: {name} at site {site}")?;
        }
        match cause_name(self.cause) {
            Some(name) => write!(f, "radio fault: {name}"),
            None => write!(f, "radio fault: cause {}", self.cause),
        }?;
        writeln!(f, " pc={:#010x} address={:#010x} ps={:#010x}", self.pc, self.address, self.ps)?;
        writeln!(f, "task={:#010x} task_index={} context={:#010x} window_base={} window_start={:#x} trace_head={}",
            self.task, self.task_index, self.context, self.window_base, self.window_start, self.trace_head)?;
        let guard_note = match self.island_status {
            ISLAND_NOT_LOADED => " (not armed: island not loaded)",
            ISLAND_ABSENT => " (not armed: island absent)",
            ISLAND_MISMATCH => " (not armed: island mismatch)",
            ISLAND_READY if self.island_guard != GUARD => " damaged",
            _ => "",
        };
        writeln!(f, "island_guard={:#010x}{guard_note}", self.island_guard)?;
        writeln!(f, "backtrace (a return address points after its call; PCs are shown as recorded):")?;
        for (index, &(pc, sp)) in self.backtrace.iter().enumerate() {
            write!(f, "  {index}: ")?;
            match fab_elec_mcu_island::symbolize(&self.map, pc) {
                Some((name, offset)) => write!(f, "{name}+0x{offset:x} ({pc:#010x})")?,
                None => write!(f, "{pc:#010x}")?,
            }
            writeln!(f, " sp={sp:#010x}")?;
        }
        writeln!(f, "ARs (WB-relative): {:08x?}", self.ars)?;
        for entry in &self.trace {
            writeln!(f, "trace gate={} args={:08x?} ccount={}", entry.gate, entry.args, entry.ccount)?;
        }
        Ok(())
    }
}

// Labels 0 and 1 belong to task.rs (guard entry and shared pick tail).
const FATAL: usize = 2;
const CHECK_TASK: usize = 3;
const DUMP: usize = 4;
const HEADER: usize = 5;
const FIND: usize = 6;
const FOUND: usize = 7;
const INDEX_DONE: usize = 8;
const TASK_STACK: usize = 9;
const WALK: usize = 10;
const CALLER: usize = 11;
const SPILLED: usize = 12;
const INVALID: usize = 13;
const APPEND: usize = 14;
const WALK_DONE: usize = 15;
const COPY: usize = 16;
const COPY_COUNT: usize = 17;
const COPY_ENTRY: usize = 18;
const COMPLETE: usize = 19;
const KILL: usize = 20;
const NEXT: usize = 21;
const KILLED: usize = 22;
const EXIT: usize = 23;
const QUIESCE: usize = 24;
const ISLAND_STOP: usize = 25;
const QUIESCE_AND_PICK: usize = 26;
pub(crate) const LABELS: usize = QUIESCE_AND_PICK + 1;

fn rsr(a: &mut Asm, r: u8, sr: u32) { a.op24(enc::rsr(0, 3, 0, sr, r)); }
fn put(a: &mut Asm, r: u8, word: u32) { a.s32i(r, A0, word * 4); }
fn get(a: &mut Asm, r: u8, word: u32) { a.l32i(r, A0, word * 4); }

/// Appended immediately after restore's RFE. Accepted faults only write the
/// record until it is complete; no instruction uses the faulting SP to store.
/// The cleared allocation and one-shot DEAD check leave every unused word zero.
///
/// Retired-instruction upper bound, user vector through resumed RFE with
/// sched_code: 1115 + 34*n, for n table entries. This includes twelve frames,
/// eight trace entries, expanded branches, the DEAD scan and the picker with
/// zero ISR slots. The walk costs at most 47 per additional frame, the ring
/// 23 per entry, guard and status capture plus padding 9, the two fault scans 14*n, pick 64+20*n, and restore 100.
/// Quiescing adds 15 fixed instructions: 5 for modem reset, 6 for the four
/// routes, 1 to ready the dispatcher, and 3 for the literal load, call and return.
/// Testing a zero pending word adds one address load, one word load and one branch.
/// A requested stop retires at most 151 + 28*n from the ILL (which does not retire)
/// to the first restore: vector 9, classification through BR's RSYNC 20, pending
/// check 4, consume cause and branch 6, call 2, quiesce 29+8*n, publish 3,
/// pick tail 10, picker linkage 4, and the zero-ISR picker 64+20*n.
/// Guard capture is cheaper, even including irq_entry's spill. This is a
/// derived bound, not a chip-cycle measurement or the normal switch bound.
pub(crate) fn emit(a: &mut Asm, base: u32, l: &RadioLayout, code: &mut KernelCode, pick: usize) {
    // Guard entry: the full save has already abandoned the live windows.
    a.mov_reg_i32(A4, l.radio_state);
    a.l32i(A4, A4, 0);
    a.bccz_reg_label(ccz::NE, A4, FATAL);
    for tcb in [l.dispatcher, l.idle] {
        a.mov_reg_i32(A4, tcb);
        a.bcc_reg_reg_label(cc::EQ, A2, A4, FATAL);
    }
    a.mov_n(A5, A0);
    a.mov_reg_i32(A0, l.fault);
    a.movi(A4, FAULT_GUARD as i32);
    put(a, A4, FAULT_CAUSE);
    put(a, A3, FAULT_ADDRESS);
    for (offset, word) in [(CONTEXT_PC, FAULT_PC), (CONTEXT_PS, FAULT_PS),
        (CONTEXT_WINDOWBASE, FAULT_WINDOWBASE), (CONTEXT_WINDOWSTART, FAULT_WINDOWSTART)]
    { a.l32i(A4, A5, offset); put(a, A4, word); }
    for reg in 0..16 {
        a.l32i(A4, A5, CONTEXT_AR + reg * 4);
        put(a, A4, FAULT_AR + reg);
    }
    a.movi(A15, 0); // guard: context zero, no walk
    put(a, A15, FAULT_CONTEXT);
    a.j_label(HEADER);

    code.fault_entry = base + a.base.offset();
    // EXCM suppresses WindowCheck and hardware loops (pp. 246, 279, 286).
    // Classify before ANY record write, using only a0 until task comparison.
    a.mov_reg_i32(A0, l.radio_state);
    a.l32i(A0, A0, 0);
    a.bccz_reg_label(ccz::NE, A0, FATAL);
    a.mov_reg_i32(A0, l.irq_stack_base);
    a.bcc_reg_reg_label(cc::LTU, A1, A0, CHECK_TASK);
    a.mov_reg_i32(A0, l.irq_stack_top);
    a.bcc_reg_reg_label(cc::LTU, A0, A1, CHECK_TASK);
    a.movi(A0, 1);
    a.wsr(A0, 4); // BR is scratch: context flag, outside the AR file.
    a.j_label(DUMP);
    a.base.label_assign(CHECK_TASK);
    a.wsr(A2, 12); // SCOMPARE1 holds original a2 during classification.
    a.rsync(); // special-register write before its later RSR, pp. 283, 586.
    a.mov_reg_i32(A0, l.current);
    a.l32i(A0, A0, 0);
    for tcb in [l.dispatcher, l.idle] {
        a.mov_reg_i32(A2, tcb);
        a.bcc_reg_reg_label(cc::EQ, A0, A2, FATAL);
    }
    rsr(a, A2, 12);
    a.movi(A0, 0);
    a.wsr(A0, 4);

    a.base.label_assign(DUMP);
    a.rsync(); // BR's context flag before its later RSR, pp. 282, 586.
    // Only a0 is scratch until a stop is confirmed. EXCSAVE1 holds its original value.
    a.mov_reg_i32(A0, l.fault + STOP_PENDING_OFFSET);
    a.l32i(A0, A0, 0);
    a.bccz_reg_label(ccz::NE, A0, ISLAND_STOP);
    a.mov_reg_i32(A0, l.fault);
    for reg in 1..16 { put(a, reg, FAULT_AR + u32::from(reg)); }
    rsr(a, A2, 209);
    put(a, A2, FAULT_AR);
    // ROTW changes only the visible mapping (pp. 242, 578–579). Each new a0
    // was the already-dumped a4; the newly exposed a12..a15 remain untouched.
    // EXCM stays set throughout: no spill handler can touch a corrupt stack.
    for rotation in 1..=12 {
        a.mov_n(A4, A0);
        a.rotw(1);
        for reg in 12..16 { put(a, reg, FAULT_AR + rotation * 4 + u32::from(reg)); }
    }
    a.rotw(4); // original WB and original a0's record pointer
    for (sr, word) in [(232, FAULT_CAUSE), (177, FAULT_PC), (238, FAULT_ADDRESS),
        (230, FAULT_PS), (4, FAULT_CONTEXT), (72, FAULT_WINDOWBASE), (73, FAULT_WINDOWSTART)]
    { rsr(a, A2, sr); put(a, A2, word); }
    a.movi(A15, 1); // exception: walk after the common header

    a.base.label_assign(HEADER);
    a.mov_reg_i32(A2, l.current);
    a.l32i(A2, A2, 0);
    put(a, A2, FAULT_TASK);
    a.movi(A6, -1);
    put(a, A6, FAULT_TASK_INDEX);
    a.mov_reg_i32(A3, l.tasks);
    a.mov_reg_i32(A4, l.task_count);
    a.l32i(A4, A4, 0);
    a.movi(A5, 0);
    a.base.label_assign(FIND);
    a.bcc_reg_reg_label(cc::GEU, A5, A4, INDEX_DONE);
    a.l32i(A6, A3, 0);
    a.bcc_reg_reg_label(cc::EQ, A6, A2, FOUND);
    a.addi(A3, A3, 4);
    a.addi(A5, A5, 1);
    a.j_label(FIND);
    a.base.label_assign(FOUND);
    put(a, A5, FAULT_TASK_INDEX);
    a.base.label_assign(INDEX_DONE);
    a.mov_reg_i32(A3, l.trace_head);
    a.l32i(A3, A3, 0);
    put(a, A3, FAULT_TRACE_HEAD);
    a.bccz_reg_label(ccz::EQ, A15, COPY);

    get(a, A3, FAULT_CONTEXT);
    a.bccz_reg_label(ccz::EQ, A3, TASK_STACK);
    a.mov_reg_i32(A8, l.irq_stack_base + 4);
    a.mov_reg_i32(A9, l.irq_stack_top);
    a.j_label(WALK);
    a.base.label_assign(TASK_STACK);
    a.l32i(A8, A2, TCB_STACK_BASE);
    a.addi(A8, A8, 4);
    a.l32i(A9, A2, TCB_STACK_TOP);
    a.base.label_assign(WALK);
    get(a, A2, FAULT_PC);
    get(a, A3, FAULT_AR);
    get(a, A4, FAULT_AR + 1);
    put(a, A2, FAULT_BACKTRACE);
    put(a, A4, FAULT_BACKTRACE + 1);
    get(a, A10, FAULT_WINDOWBASE);
    get(a, A11, FAULT_WINDOWSTART);
    a.movi(A5, 1); // recorded depth, including frame zero
    a.movi(A6, 0); // cumulative window distance
    a.movi(A7, 1); // live suffix
    a.base.label_assign(CALLER);
    a.movi(A12, BACKTRACE_DEPTH as i32);
    a.bcc_reg_reg_label(cc::GEU, A5, A12, WALK_DONE);
    a.op24(enc::extui(A12, A3, 30, 2));
    a.bccz_reg_label(ccz::EQ, A12, WALK_DONE);
    a.add_n(A6, A6, A12);
    a.mov_reg_i32(A15, 0xc000_0000);
    a.and(A2, A2, A15);
    a.mov_reg_i32(A15, 0x3fff_ffff);
    a.and(A15, A3, A15);
    a.or(A2, A2, A15);
    a.bccz_reg_label(ccz::EQ, A7, SPILLED);
    // ISA §6.1.3, pp. 245–246: WindowCheck reserves registers lazily;
    // a caller at d=13..15 can still alias the current a4..a15. Live
    // frames form a contiguous chain ending at the current frame. Once
    // a caller is spilled, no older live frame starts at WB-d for d<16;
    // d=16 would wrap onto the current frame, so never consult WS there.
    a.movi(A12, 16);
    a.bcc_reg_reg_label(cc::GEU, A6, A12, SPILLED);
    a.sub(A12, A10, A6);
    a.op24(enc::extui(A12, A12, 0, 4));
    a.movi(A15, 1);
    a.ssl(A12);
    a.sll(A15, A15);
    a.and(A15, A15, A11);
    a.bccz_reg_label(ccz::EQ, A15, SPILLED);
    // (64 - 4*d) modulo 64 indexes the WB-relative dump.
    a.op24(enc::slli(A12, A6, 2));
    a.movi(A15, 64);
    a.sub(A12, A15, A12);
    a.op24(enc::extui(A12, A12, 0, 6));
    a.addx4(A12, A12, A0);
    a.l32i(A13, A12, FAULT_AR * 4);
    a.l32i(A14, A12, (FAULT_AR + 1) * 4);
    a.j_label(APPEND);
    a.base.label_assign(SPILLED);
    a.movi(A7, 0);
    // ISA §6.1.5–6.1.6, pp. 250–253: caller a0/a1 are at callee
    // SP-16/SP-12. Compare before subtracting to avoid unsigned wrap.
    a.addi(A12, A8, 16);
    a.bcc_reg_reg_label(cc::LTU, A4, A12, INVALID);
    a.bcc_reg_reg_label(cc::LTU, A9, A4, INVALID);
    a.addi(A12, A4, -16);
    a.l32i(A13, A12, 0);
    a.l32i(A14, A12, 4);
    a.j_label(APPEND);
    a.base.label_assign(INVALID);
    a.movi(A13, 0);
    a.movi(A14, 0);
    a.base.label_assign(APPEND);
    a.op24(enc::slli(A15, A5, 3));
    a.add_n(A15, A15, A0);
    a.s32i(A2, A15, FAULT_BACKTRACE * 4);
    a.s32i(A14, A15, (FAULT_BACKTRACE + 1) * 4);
    a.addi(A5, A5, 1);
    a.mov_reg_i32(A12, base + a.base.label_offset(EXIT).unwrap_or(0));
    a.bcc_reg_reg_label(cc::EQ, A2, A12, WALK_DONE);
    a.mov_reg_i32(A12, l.shim_start);
    a.bcc_reg_reg_label(cc::LTU, A2, A12, NEXT); // outside shim, check SP
    a.mov_reg_i32(A12, l.shim_end);
    a.bcc_reg_reg_label(cc::LTU, A2, A12, WALK_DONE);
    a.base.label_assign(NEXT);
    a.bcc_reg_reg_label(cc::GEU, A4, A14, WALK_DONE);
    a.bcc_reg_reg_label(cc::LTU, A9, A14, WALK_DONE);
    a.op24(enc::extui(A12, A14, 0, 4));
    a.bccz_reg_label(ccz::NE, A12, WALK_DONE);
    a.mov_n(A3, A13);
    a.mov_n(A4, A14);
    a.j_label(CALLER);
    a.base.label_assign(WALK_DONE);
    put(a, A5, FAULT_DEPTH);

    a.base.label_assign(COPY);
    if l.trace_entries != 0 {
        get(a, A3, FAULT_TRACE_HEAD);
        a.movi(A2, l.trace_entries.min(FAULT_TRACE_ENTRIES) as i32);
        a.bcc_reg_reg_label(cc::GEU, A3, A2, COPY_COUNT);
        a.mov_n(A2, A3);
        a.base.label_assign(COPY_COUNT);
        a.bccz_reg_label(ccz::EQ, A2, COMPLETE);
        a.mov_reg_i32(A4, l.fault + FAULT_TRACE * 4);
        a.mov_reg_i32(A5, l.trace);
        a.movi(A6, (l.trace_entries - 1) as i32);
        a.base.label_assign(COPY_ENTRY);
        a.addi(A3, A3, -1);
        a.and(A8, A3, A6);
        a.op24(enc::slli(A8, A8, 5));
        a.add_n(A8, A8, A5);
        for word in 0..TRACE_ENTRY_WORDS {
            a.l32i(A9, A8, word * 4);
            a.s32i(A9, A4, word * 4);
        }
        a.addi(A4, A4, (TRACE_ENTRY_WORDS * 4) as i32);
        a.addi(A2, A2, -1);
        a.bccz_reg_label(ccz::NE, A2, COPY_ENTRY);
    }
    a.base.label_assign(COMPLETE);
    for (address, word) in [(l.island_guard, FAULT_ISLAND_GUARD), (l.island_status, FAULT_ISLAND_STATUS)] {
        a.mov_reg_i32(A2, address);
        a.l32i(A2, A2, 0);
        put(a, A2, word);
    }
    a.movi(A2, 0);
    for word in FAULT_WORDS..FAULT_FRAMES * 4 { put(a, A2, word); }
    a.memw(); // All record stores before the completion word, pp. 490–491.
    a.movi(A2, FAULT_WORDS as i32);
    put(a, A2, FAULT_LENGTH); // publication: always the final record store
    let call_quiesce = |a: &mut Asm| {
        a.mov_reg_i32(A3, base + a.base.label_offset(QUIESCE).unwrap_or(0));
        a.callx0(A3);
    };
    a.movi(A2, RADIO_DEAD as i32);
    a.base.label_assign(QUIESCE_AND_PICK);
    call_quiesce(a); // preserves a2, the state to publish
    a.mov_reg_i32(A3, l.radio_state);
    a.memw(); // Publish the state after quiescing and recording its cause.
    a.s32i(A2, A3, 0);
    a.movi(A3, 0);
    a.wsr(A3, 2);
    a.isync(); // LCOUNT zero before clearing EXCM, pp. 279–280, 286.
    a.movi(A3, (1 << 5) | 3);
    a.wsr(A3, 230);
    a.rsync(); // WOE=0 and INTLEVEL=3 before abandoning the fault SP, pp. 285–289.
    a.mov_reg_i32(A1, l.irq_stack_top);
    a.mov_reg_i32(A2, l.current);
    a.l32i(A2, A2, 0);
    a.j_label(pick);

    a.base.label_assign(FATAL);
    code.fatal = base + a.base.offset();
    a.j(-4);
    a.base.label_assign(EXIT);
    code.task_exit = base + a.base.offset();
    a.op24(0); // ILL: IllegalInstructionCause=0, p. 151.

    a.base.label_assign(ISLAND_STOP);
    // a0 holds the observed cause until it is stored. a2 then carries STOPPED
    // across the stackless quiesce call, which uses a3..a8 and returns via a0.
    a.mov_reg_i32(A2, l.fault + STOP_CAUSE_OFFSET);
    a.movi(A3, 0);
    a.s32i(A3, A2, STOP_PENDING_OFFSET - STOP_CAUSE_OFFSET);
    a.s32i(A0, A2, 0);
    a.movi(A2, RADIO_STOPPED as i32);
    a.j_label(QUIESCE_AND_PICK);

    // Dispatcher CALL0 at INTLEVEL 15. a2 is the cause; a9 saves the link.
    // Eleven instructions outside quiesce, at most 40 + 8*n through RET.N.
    code.radio_stop = base + a.base.offset();
    a.mov_n(A9, A0);
    call_quiesce(a);
    a.mov_reg_i32(A3, l.fault + STOP_CAUSE_OFFSET);
    a.s32i(A2, A3, 0);
    a.mov_reg_i32(A3, l.radio_state);
    a.movi(A4, RADIO_STOPPED as i32);
    a.memw();
    a.s32i(A4, A3, 0);
    a.mov_n(A0, A9);
    a.ret_n();

    quiesce(a, l);

    code.trace_record = base + a.base.offset();
    if l.trace_entries != 0 {
        a.mov_reg_i32(A10, l.trace_head);
        a.l32i(A9, A10, 0);
        a.addi(A11, A9, 1);
        a.s32i(A11, A10, 0);
        a.movi(A11, (l.trace_entries - 1) as i32);
        a.and(A9, A9, A11);
        a.op24(enc::slli(A9, A9, 5));
        a.mov_reg_i32(A10, l.trace);
        a.add_n(A10, A10, A9);
        a.s32i(A8, A10, 0);
        for reg in 2..8 { a.s32i(reg, A10, u32::from(reg - 1) * 4); }
        rsr(a, A8, 234); // CCOUNT is read only here (p. 306).
        a.s32i(A8, A10, 28);
    }
    a.ret_n();
}

/// Stackless CALL0 with level 1 masked (dispatcher at INTLEVEL 15). Clobbers
/// a3..a8 and INTENABLE, preserving PS, a2 and the return link in a0.
/// At most 29 + 8*n retired instructions for n entries in the task table:
/// 16 for hardware, 6 for setup, 8 per task, 2 to exit, 5 to finish and return.
fn quiesce(a: &mut Asm, l: &RadioLayout) {
    a.base.label_assign(QUIESCE);
    a.mov_reg_i32(A3, SYSCON_WIFI_RST_EN);
    a.l32i(A4, A3, 0);
    a.mov_reg_i32(A5, MODEM_RESET_FIELD);
    a.or(A4, A4, A5);
    a.s32i(A4, A3, 0);
    a.mov_reg_i32(A3, INTR_MATRIX);
    a.movi(A4, 16); // interrupt_core0_reg.h:18, unroute the source.
    for source in 0..WIFI_SOURCES { a.s32i(A4, A3, source * 4); }
    rsr(a, A3, 228);
    a.mov_reg_i32(A4, l.wake_lines | l.vector_lines | (1 << YIELD_LINE));
    a.and(A3, A3, A4);
    a.wsr(A3, 228);
    a.rsync();

    a.mov_reg_i32(A3, l.tasks);
    a.mov_reg_i32(A4, l.task_count);
    a.l32i(A4, A4, 0);
    a.mov_reg_i32(A5, l.dispatcher);
    a.mov_reg_i32(A6, l.idle);
    a.movi(A7, DEAD as i32);
    a.base.label_assign(KILL);
    a.bccz_reg_label(ccz::EQ, A4, KILLED);
    a.l32i(A8, A3, 0);
    a.bcc(cc::EQ, A8, A5, 9 - 4);
    a.bcc(cc::EQ, A8, A6, 6 - 4);
    a.s32i(A7, A8, TCB_STATE);
    a.addi(A3, A3, 4);
    a.addi(A4, A4, -1);
    a.j_label(KILL);
    a.base.label_assign(KILLED);
    a.mov_reg_i32(A3, l.isr_count);
    a.movi(A4, READY as i32); // READY is zero, also the empty ISR count.
    a.s32i(A4, A3, 0);
    a.s32i(A4, A5, TCB_STATE);
    a.ret_n();
}
