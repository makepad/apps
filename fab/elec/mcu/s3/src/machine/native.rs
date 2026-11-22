//! Native page units (`Tier::Native`, the `jit` feature): the warm decoded instructions of one
//! 4 KiB page of the flash instruction window compiled into one function by the shared back end.
//! Warm: T1 ran it at least `WARM` times. Code that ran once (start-up, set-up) stays on T1, and
//! the unit leaves for T1 where it reaches such code; once that code warms up, the page compiles
//! again with it.
//!
//! The forms `fab_elec_mcu_xtensa::jit` lowers run inline. A straight-line run of them is one
//! stretch of code that tests the budget for the whole run once, at its head, and counts the
//! run in at its end as the lane counts each instruction: TICK by the period, the budget,
//! INSTRET and the PC. Every other instruction, and every marked one, calls `step`: the lane's
//! own body for one instruction, so it stops and returns where the lane would. Loads and
//! stores in the SRAM data window run inline as `LaneBus` makes them; any other access, a store
//! while a GDMA transfer runs or into a page the journal has not saved, leaves the run before
//! the instruction for its `step` block. A unit is entered at its block leaders, and only while
//! the window gate is off (the runtime runs with PS.WOE clear; only an instruction that ends the
//! lane can set it); elsewhere the lane runs.

use std::any::Any;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::offset_of;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, Weak};

use fab_elec_mcu_emu::board::Board;
use fab_elec_mcu_emu::jit::rir::{Cls, Cond, Conv, IOp, Inst, Mem, VReg, U32, U64};
use fab_elec_mcu_emu::jit::{self, Unit, UnitBuilder, UnitStats};
use fab_elec_mcu_emu::{PcSet, Until};
use fab_elec_mcu_xtensa::insn::{target, Op};
use fab_elec_mcu_xtensa::jit::{self as lower, Flow, Host, Regs};
use fab_elec_mcu_xtensa::sim::{lane, Cpu, Decoded};

use super::{LaneRun, LaneStep, S3Machine, UNDECODED};
use crate::bus::{Mmu, S3Bus, SAVED, SRAM_DATA, SRAM_LEN};
use crate::foreign::ForeignCode;

/// A unit's exit codes (zero is never returned: the helper's "on to the next instruction").
pub(super) const EXIT_BUDGET: u32 = 1;
pub(super) const EXIT_STOP: u32 = 2;
pub(super) const EXIT_RETURN: u32 = 3;
pub(super) const EXIT_MISS: u32 = 4;
pub(super) const EXIT_PANIC: u32 = 5;
/// The helper's "on at `LaneCtx::next`", not the next instruction: the unit dispatches there.
/// Never a unit's exit.
const JUMPED: u32 = 6;

/// T1 instructions run in a page before its unit is compiled.
const HOT: u32 = 4096;

/// T1 runs of an instruction that make it part of its page's unit.
const WARM: u8 = 2;

/// What a unit and its helper share for one call. `next` is read by the unit's code at offset 0.
#[repr(C)]
pub(super) struct LaneCtx<B: Board> {
    /// The PC after the instruction the helper ran.
    pub next: u32,
    /// Whether the unit ran an instruction (the lane's `ran`).
    pub ran: u32,
    /// The SRAM, the journal's page flags, and whether a GDMA transfer is under way (`Memory`).
    pub sram: *mut u8,
    pub flags: *const u8,
    pub dma: u32,
    pub machine: *mut S3Machine<B>,
    pub until: *const Until<'static>,
    pub run: LaneRun,
    pub panic: Option<Box<dyn Any + Send>>,
}

/// The helper every non-inline instruction of a unit calls: the lane's body for the instruction
/// at `pc`, decoded as `*d`. Zero when the lane goes on to the next instruction, `JUMPED` when
/// it goes on elsewhere (at `next`), else the unit's exit code. Never unwinds: a panic is kept
/// in the context for the dispatcher.
#[cfg(target_arch = "x86_64")]
unsafe extern "sysv64" fn step<B: Board>(ctx: *mut u8, pc: u32, d: u64) -> u32 {
    unsafe { step_body::<B>(ctx, pc, d) }
}

#[cfg(not(target_arch = "x86_64"))]
unsafe extern "C" fn step<B: Board>(ctx: *mut u8, pc: u32, d: u64) -> u32 {
    unsafe { step_body::<B>(ctx, pc, d) }
}

#[inline(always)]
unsafe fn step_body<B: Board>(ctx: *mut u8, pc: u32, d: u64) -> u32 {
    // SAFETY: the dispatcher passes its own LaneCtx<B>, alive and not otherwise borrowed for the
    // call; `d` points into the unit's own snapshot of the page, which the dispatcher's Arc keeps.
    let ctx = unsafe { &mut *(ctx as *mut LaneCtx<B>) };
    // The unit stores the PC only where it leaves, so every way out of here has it stored.
    unsafe { (*ctx.machine).cpu.pc = pc; }
    // The lane's loop condition.
    if ctx.run.budget == 0 { return EXIT_BUDGET; }
    let (machine, until, run) = (ctx.machine, ctx.until, &mut ctx.run);
    let d = unsafe { &*(d as *const Decoded) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let m = unsafe { &mut *machine };
        let step = m.lane_step::<super::spin::Plain>(run, unsafe { &*until }, pc, d);
        (step, m.cpu.pc, m.memory())
    }));
    // The instruction may have started a transfer (a register write the lane went on after).
    if let Ok((_, _, memory)) = &result { (ctx.sram, ctx.flags, ctx.dma) = (memory.sram, memory.flags, memory.dma); }
    match result {
        Ok((LaneStep::Next, next, _)) => {
            ctx.ran = 1;
            if next == pc.wrapping_add(u32::from(d.len)) { return 0; }
            ctx.next = next;
            JUMPED
        }
        // A spin probe started: it runs in the decoded lane, which sees each instruction.
        Ok((LaneStep::Probe, ..)) => { ctx.ran = 1; EXIT_MISS }
        Ok((LaneStep::Stop, ..)) => EXIT_STOP,
        Ok((LaneStep::Return, ..)) => { ctx.ran = 1; EXIT_RETURN }
        Err(payload) => { ctx.panic = Some(payload); EXIT_PANIC }
    }
}

/// What a unit's inline loads and stores read before they touch the SRAM.
pub(super) struct Memory {
    pub sram: *mut u8,
    pub flags: *const u8,
    pub dma: u32,
}

impl<B: Board> S3Machine<B> {
    /// The SRAM and its journal flags where they are now, and whether a GDMA transfer runs.
    pub(super) fn memory(&mut self) -> Memory {
        Memory { sram: self.bus.sram.as_mut_ptr(), flags: self.bus.journal.flags(), dma: u32::from(self.bus.periph.dma_active()) }
    }
}

/// A compiled page: the unit, the page snapshot its code points into, the marks it was built
/// for (offsets in the page) and how many decoded instructions it covers.
pub(super) struct Native {
    pub unit: Unit,
    _page: Arc<[Decoded]>,
    marks: Box<[u16]>,
    covered: u32,
}

type Cell = Arc<OnceLock<Option<Arc<Native>>>>;

/// The units of every machine in the process, by what they were built from: the board type
/// (its helper), the flash image (held weakly, so an image and its entries can go), the MMU
/// table, the page and its marks. Machines cloned from one another, or built for one image,
/// share their compiles.
#[derive(PartialEq, Eq, Hash)]
struct Key {
    helper: usize,
    flash: usize,
    mmu: u64,
    base: u32,
    marks: Box<[u16]>,
}

struct Shared {
    flash: Weak<Vec<u8>>,
    mmu: Arc<Mmu>,
    cell: Cell,
}

static SHARED: LazyLock<Mutex<HashMap<Key, Shared>>> = LazyLock::new(Default::default);

fn key<B: Board>(bus: &S3Bus, base: u32, marks: Box<[u16]>) -> Key {
    let mut h = DefaultHasher::new();
    bus.regs.mmu().hash(&mut h);
    Key { helper: step::<B> as *const () as usize, flash: Arc::as_ptr(bus.flash_image()) as usize, mmu: h.finish(), base, marks }
}

fn page_marks(marks: &PcSet, base: u32) -> Box<[u16]> {
    marks.marks_in(base, base + 4096).map(|pc| (pc - base) as u16).collect()
}

/// One page's native state: T1's count towards the compile, the compile it waits on, the unit.
#[derive(Clone)]
pub(super) struct PageJit {
    count: u32,
    cell: Option<Cell>,
    unit: Option<Arc<Native>>,
    /// The marks stamp `unit`'s marks were last found equal to.
    stamp: u64,
    /// The shared table was asked for this page since its unit went.
    looked: bool,
    /// A compile gave no code: this page stays on the decoded tier.
    dead: bool,
    /// On machines with a table: whether the page holds no table PC, classified before its
    /// first count. A page that holds one never counts, so it stays on the decoded tier.
    pub(super) owned: Option<bool>,
    /// T1 runs of each byte offset of the page (saturating), from its first counted instruction.
    runs: Option<Box<[u8]>>,
}

impl Default for PageJit {
    fn default() -> Self {
        PageJit { count: 0, cell: None, unit: None, stamp: u64::MAX, looked: false, dead: false, owned: None, runs: None }
    }
}

impl PageJit {
    /// Whether this page, at `base`, holds no PC of `foreign`: classified on the first ask.
    #[inline]
    pub(super) fn owned(&mut self, base: u32, foreign: &dyn ForeignCode) -> bool {
        match self.owned { Some(owned) => owned, None => self.classify(base, foreign) }
    }

    // Out of line, so the scan stays out of the decoded loop that inlines `count`.
    #[inline(never)]
    fn classify(&mut self, base: u32, foreign: &dyn ForeignCode) -> bool {
        // Include the two possible trailing instruction bytes on the next page.
        let owned = !(base..base + 4098).any(|at| foreign.contains(at));
        self.owned = Some(owned);
        owned
    }

    /// The unit to enter at `pc` under `marks`, collecting a finished compile first. A unit
    /// built for other marks in the page goes, and the page counts towards a new one.
    #[inline]
    pub(super) fn unit(&mut self, pc: u32, marks: &PcSet, base: u32) -> Option<Arc<Native>> {
        if let Some(cell) = &self.cell {
            if let Some(done) = cell.get() {
                match done {
                    Some(native) => { self.unit = Some(native.clone()); self.stamp = u64::MAX; }
                    None => self.dead = true,
                }
                self.cell = None;
            }
        }
        let native = self.unit.as_ref()?;
        if marks.stamp() != self.stamp {
            if native.marks != page_marks(marks, base) {
                (self.unit, self.looked, self.count) = (None, false, 0);
                return None;
            }
            self.stamp = marks.stamp();
        }
        native.unit.entries().binary_search(&pc).is_ok().then(|| native.clone())
    }

    /// Counts a T1 instruction of this page, at byte `offset`. The first asks the shared table for
    /// the page's unit; at the threshold, the page's warm decoded instructions are compiled on the
    /// shared worker (or here, when `jit::inline_compile` is set) and published there.
    #[inline]
    pub(super) fn count<B: Board>(&mut self, base: u32, offset: usize, page: &Arc<[Decoded]>, bus: &S3Bus, marks: &PcSet) {
        self.count += 1;
        if self.dead { return; }
        let runs = self.runs.get_or_insert_with(|| vec![0; 4096].into_boxed_slice());
        runs[offset] = runs[offset].saturating_add(1);
        if !self.looked {
            self.looked = true;
            if self.unit.is_none() && self.cell.is_none() {
                let key = key::<B>(bus, base, page_marks(marks, base));
                let table = SHARED.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(shared) = table.get(&key).filter(|s| s.flash.strong_count() > 0 && *s.mmu == **bus.regs.mmu()) {
                    self.cell = Some(shared.cell.clone());
                    return;
                }
            }
        }
        if self.count < HOT || self.cell.is_some() { return; }
        self.count = 0;
        // Runs only grow, so an equal count is the same set.
        let warm: Vec<bool> = self.runs.as_deref().map_or(vec![false; page.len()], |runs| {
            (0..page.len()).map(|o| runs[o] >= WARM && page[o].class & (UNDECODED | lane::SLOW) == 0).collect()
        });
        let covered = warm.iter().filter(|&&w| w).count() as u32;
        if covered == 0 || self.unit.as_ref().is_some_and(|u| u.covered == covered) { return; }
        let marks = page_marks(marks, base);
        // L32R's literals, read now: the unit lives only while the image and the MMU table it
        // was built for stay (a table change resets the decoded code, units with it).
        let literals: Vec<(u16, u32)> = (0..page.len()).filter(|&o| page[o].class == 0 && page[o].insn.op == Op::L32r)
            .filter_map(|o| {
                let at = target(&page[o].insn, base + o as u32)?;
                Some((o as u16, bus.peek_literal(at)?))
            }).collect();
        let key = key::<B>(bus, base, marks.clone());
        let page = page.clone();
        let job = move || build::<B>(base, &page, &warm, &marks, &literals)
            .map(|(unit, _)| Arc::new(Native { unit, _page: page, marks, covered }));
        let cell: Cell = if jit::inline_compile() {
            let cell = Arc::new(OnceLock::new());
            let _ = cell.set(job());
            cell
        } else {
            jit::compiler().submit(job)
        };
        let mut table = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        table.retain(|_, s| s.flash.strong_count() > 0);
        table.insert(key, Shared { flash: Arc::downgrade(bus.flash_image()), mmu: bus.regs.mmu().clone(), cell: cell.clone() });
        self.cell = Some(cell);
    }
}

/// Byte offsets of the state a unit's code reads and writes.
#[derive(Clone, Copy)]
struct Layout {
    machine: i32,
    ran: i32,
    sram: i32,
    flags: i32,
    dma: i32,
    budget: i32,
    period: i32,
    cpu: i32,
    pc: i32,
    instret: i32,
    tick: i32,
}

impl Layout {
    fn of<B: Board>() -> Layout {
        let run = offset_of!(LaneCtx<B>, run);
        let cpu = offset_of!(S3Machine<B>, cpu);
        Layout {
            machine: offset_of!(LaneCtx<B>, machine) as i32,
            ran: offset_of!(LaneCtx<B>, ran) as i32,
            sram: offset_of!(LaneCtx<B>, sram) as i32,
            flags: offset_of!(LaneCtx<B>, flags) as i32,
            dma: offset_of!(LaneCtx<B>, dma) as i32,
            budget: (run + offset_of!(LaneRun, budget)) as i32,
            period: (run + offset_of!(LaneRun, period)) as i32,
            cpu: cpu as i32,
            pc: (cpu + offset_of!(Cpu, pc)) as i32,
            instret: (cpu + offset_of!(Cpu, instret)) as i32,
            tick: offset_of!(S3Machine<B>, tick) as i32,
        }
    }
}

/// One run's host for the instruction being lowered: where its side exits go (its `step` block,
/// or the miss exit), the PC, and how many of the run's instructions came before it.
struct Exits {
    at: Layout,
    ctx: VReg,
    machine: VReg,
    budget: VReg,
    slow: u32,
    pc: u32,
    done: u64,
}

impl Exits {
    /// The SRAM offset of an access of `bytes` at `addr` in the SRAM data window, as LaneBus's
    /// `sram_load` and `sram_store` take it (unaligned too: the S3 handles unaligned accesses in
    /// hardware); any other address leaves by `side`.
    fn sram_offset(&mut self, b: &mut UnitBuilder, addr: VReg, bytes: u8, side: u32) -> VReg {
        let n = b.vreg(Cls::I);
        b.emit(Inst::IBinI(IOp::Sub, U32, n, addr, i64::from(SRAM_DATA)));
        let fits = b.vreg(Cls::I);
        b.emit(Inst::ICmpI(Cond::Le, false, fits, n, (SRAM_LEN - usize::from(bytes)) as i64));
        let on = b.block();
        b.branch(fits, on, side);
        b.switch_to(on);
        n
    }

    /// The host address of SRAM offset `n`.
    fn sram_at(&mut self, b: &mut UnitBuilder, n: VReg) -> VReg {
        let sram = b.vreg(Cls::I);
        b.emit(Inst::Load(Mem::Int(8, false), sram, self.ctx, self.at.sram));
        let at = b.vreg(Cls::I);
        b.emit(Inst::IBin(IOp::Add, U64, at, sram, n));
        at
    }

    /// The journal flag byte of the SRAM page holding offset `n`.
    fn flag(&mut self, b: &mut UnitBuilder, flags: VReg, n: VReg) -> VReg {
        let page = b.vreg(Cls::I);
        b.emit(Inst::IBinI(IOp::Shr, U32, page, n, 12));
        let at = b.vreg(Cls::I);
        b.emit(Inst::IBin(IOp::Add, U64, at, flags, page));
        let flag = b.vreg(Cls::I);
        b.emit(Inst::Load(Mem::Int(1, false), flag, at, 0));
        flag
    }
}

impl Host for Exits {
    fn side_exit(&mut self, b: &mut UnitBuilder, regs: &Regs) -> u32 {
        let (back, side) = (b.current(), b.block());
        b.switch_to(side);
        regs.store(b);
        if self.done > 0 {
            count_in(b, self.at, self.ctx, self.machine, self.budget, self.done);
            let pc = b.iconst(i64::from(self.pc));
            b.emit(Inst::Store(Mem::Int(4, false), self.machine, self.at.pc, pc));
        }
        b.jump(self.slow);
        b.switch_to(back);
        side
    }

    fn load(&mut self, b: &mut UnitBuilder, regs: &Regs, addr: VReg, bytes: u8, signed: bool) -> VReg {
        let side = self.side_exit(b, regs);
        let n = self.sram_offset(b, addr, bytes, side);
        let at = self.sram_at(b, n);
        let v = b.vreg(Cls::I);
        b.emit(Inst::Load(Mem::Int(bytes, signed), v, at, 0));
        if !signed { return v; }
        let word = b.vreg(Cls::I);
        b.emit(Inst::Conv(Conv::IntToInt(U32), word, v));
        word
    }

    /// As `store_at` makes it: no GDMA transfer under way (one may be reading the bytes), and
    /// the pages written saved since the mark (else `step` saves them). One test for all three:
    /// the flags are read at offset 0 when the address is outside the SRAM.
    fn store(&mut self, b: &mut UnitBuilder, regs: &Regs, addr: VReg, bytes: u8, value: VReg) {
        let side = self.side_exit(b, regs);
        let n = b.vreg(Cls::I);
        b.emit(Inst::IBinI(IOp::Sub, U32, n, addr, i64::from(SRAM_DATA)));
        let fits = b.vreg(Cls::I);
        b.emit(Inst::ICmpI(Cond::Le, false, fits, n, (SRAM_LEN - usize::from(bytes)) as i64));
        let zero = b.iconst(0);
        let first = b.vreg(Cls::I);
        b.emit(Inst::Sel(first, fits, n, zero));
        let flags = b.vreg(Cls::I);
        b.emit(Inst::Load(Mem::Int(8, false), flags, self.ctx, self.at.flags));
        let mut saved = self.flag(b, flags, first);
        if bytes > 1 {
            let last = b.vreg(Cls::I);
            b.emit(Inst::IBinI(IOp::Add, U32, last, first, i64::from(bytes) - 1));
            let other = self.flag(b, flags, last);
            let both = b.vreg(Cls::I);
            b.emit(Inst::IBin(IOp::And, U32, both, saved, other));
            saved = both;
        }
        let dma = b.vreg(Cls::I);
        b.emit(Inst::Load(Mem::Int(4, false), dma, self.ctx, self.at.dma));
        let idle = b.vreg(Cls::I);
        b.emit(Inst::ICmpI(Cond::Eq, false, idle, dma, 0));
        let open = b.vreg(Cls::I);
        b.emit(Inst::IBin(IOp::And, U32, open, fits, idle));
        let ok = b.vreg(Cls::I);
        b.emit(Inst::IBin(IOp::And, U32, ok, open, saved));
        let bit = b.vreg(Cls::I);
        b.emit(Inst::IBinI(IOp::And, U32, bit, ok, i64::from(SAVED)));
        let write = b.block();
        b.branch(bit, write, side);
        b.switch_to(write);
        let at = self.sram_at(b, n);
        b.emit(Inst::Store(Mem::Int(bytes, false), at, 0, value));
    }
}

/// Counts `n` retired lane instructions in, as `lane_step` counts each: TICK by the period,
/// the budget, INSTRET; and notes that the unit ran something.
fn count_in(b: &mut UnitBuilder, at: Layout, ctx: VReg, machine: VReg, budget: VReg, n: u64) {
    let n = n as i64;
    let left = b.vreg(Cls::I);
    b.emit(Inst::IBinI(IOp::Sub, U64, left, budget, n));
    b.emit(Inst::Store(Mem::Int(8, false), ctx, at.budget, left));
    let period = b.vreg(Cls::I);
    b.emit(Inst::Load(Mem::Int(8, false), period, ctx, at.period));
    let ticks = b.vreg(Cls::I);
    b.emit(Inst::IBinI(IOp::Mul, U64, ticks, period, n));
    let tick = b.vreg(Cls::I);
    b.emit(Inst::Load(Mem::Int(8, false), tick, machine, at.tick));
    let later = b.vreg(Cls::I);
    b.emit(Inst::IBin(IOp::Add, U64, later, tick, ticks));
    b.emit(Inst::Store(Mem::Int(8, false), machine, at.tick, later));
    let retired = b.vreg(Cls::I);
    b.emit(Inst::Load(Mem::Int(8, false), retired, machine, at.instret));
    let more = b.vreg(Cls::I);
    b.emit(Inst::IBinI(IOp::Add, U64, more, retired, n));
    b.emit(Inst::Store(Mem::Int(8, false), machine, at.instret, more));
    let one = b.iconst(1);
    b.emit(Inst::Store(Mem::Int(4, false), ctx, at.ran, one));
}

/// What `build` makes of each page offset.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Not in the unit: undecoded, cold (run fewer than `WARM` times), or the reference step's.
    Out,
    Inline,
    /// A `step` call.
    Helper,
}

/// The unit of one page's `warm` offsets. `marks` and `literals` (L32R's words by offset) are what
/// it is built for; the instructions `lower` takes and that no mark names run inline, every other
/// decoded lane instruction calls `step`.
fn build<B: Board>(base: u32, page: &Arc<[Decoded]>, warm: &[bool], marks: &[u16], literals: &[(u16, u32)]) -> Option<(Unit, UnitStats)> {
    let literal = |o: usize| literals.binary_search_by_key(&(o as u16), |&(k, _)| k).ok().map(|i| literals[i].1);
    let kind: Vec<Kind> = (0..page.len()).map(|o| {
        let d = &page[o];
        if !warm[o] || d.class & (UNDECODED | lane::SLOW) != 0 { Kind::Out }
        else if d.class == 0 && marks.binary_search(&(o as u16)).is_err() && lower::inline(&d.insn, literal(o)) { Kind::Inline }
        else { Kind::Helper }
    }).collect();
    let offsets: Vec<usize> = (0..page.len()).filter(|&o| kind[o] != Kind::Out).collect();
    if offsets.is_empty() { return None; }
    // Leaders: static targets in the page, the instructions after a helper or a control
    // transfer, and any without exactly one instruction ending at it.
    let mut leader = vec![false; page.len()];
    let mut preds = vec![0u8; page.len()];
    for &o in &offsets {
        let d = &page[o];
        let end = o + usize::from(d.len);
        if end < page.len() {
            preds[end] = preds[end].saturating_add(1);
            if kind[o] == Kind::Helper || lower::ends_run(&d.insn) { leader[end] = true; }
        }
        if d.insn.op != Op::L32r {
            if let Some(to) = target(&d.insn, base + o as u32) {
                let to = to.wrapping_sub(base) as usize;
                if to < page.len() { leader[to] = true; }
            }
        }
    }
    for &o in &offsets {
        if preds[o] != 1 { leader[o] = true; }
    }
    let mut b = UnitBuilder::new("s3_page");
    b.count_guest(offsets.len() as u32);
    let ctx = b.ctx();
    let at = Layout::of::<B>();
    let helper = b.helper(step::<B> as *const () as usize as u64);
    let code = b.vreg(Cls::I);
    let returned = b.block();
    b.switch_to(returned);
    b.ret(code);
    let miss = b.exit(EXIT_MISS);
    let low = b.exit(EXIT_BUDGET);
    // Every helper's way out but the next instruction, shared: on elsewhere, or the exit.
    let out = b.block();
    b.switch_to(out);
    let jumped = b.vreg(Cls::I);
    b.emit(Inst::ICmpI(Cond::Eq, false, jumped, code, i64::from(JUMPED)));
    let again = b.block();
    b.branch(jumped, again, returned);
    b.switch_to(again);
    let next = b.vreg(Cls::I);
    b.emit(Inst::Load(Mem::Int(4, false), next, ctx, 0));
    b.redispatch(next);
    // A block for each run head and helper instruction; runs follow straight-line inline code.
    // An inline instruction that may leave its run has a `step` block too (`slow`).
    let mut block = vec![u32::MAX; page.len()];
    let mut slow = vec![u32::MAX; page.len()];
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for &o in &offsets {
        match kind[o] {
            Kind::Inline if leader[o] => {
                let mut run = vec![o];
                let mut last = o;
                while !lower::ends_run(&page[last].insn) {
                    let next = last + usize::from(page[last].len);
                    if next >= page.len() || kind[next] != Kind::Inline || leader[next] { break; }
                    run.push(next);
                    last = next;
                }
                block[o] = b.block();
                runs.push(run);
            }
            Kind::Helper => block[o] = b.block(),
            _ => {}
        }
        if kind[o] == Kind::Inline && lower::leaves(&page[o].insn) { slow[o] = b.block(); }
        if leader[o] && block[o] != u32::MAX { b.entry(base + o as u32, block[o]); }
    }
    let block_at = |pc: u32| {
        let o = pc.wrapping_sub(base) as usize;
        if o < page.len() && block[o] != u32::MAX { block[o] } else { miss }
    };
    // The PC is stored where code leaves: at each run's head (for its budget and first side
    // exits), at a side exit after the head, and on the way to an instruction outside the unit.
    for run in &runs {
        b.switch_to(block[run[0]]);
        let machine = b.vreg(Cls::I);
        b.emit(Inst::Load(Mem::Int(8, false), machine, ctx, at.machine));
        let head = b.iconst(i64::from(base + run[0] as u32));
        b.emit(Inst::Store(Mem::Int(4, false), machine, at.pc, head));
        let budget = b.vreg(Cls::I);
        b.emit(Inst::Load(Mem::Int(8, false), budget, ctx, at.budget));
        let short = b.vreg(Cls::I);
        b.emit(Inst::ICmpI(Cond::Lt, false, short, budget, run.len() as i64));
        let body = b.block();
        b.branch(short, low, body);
        b.switch_to(body);
        let mut regs = Regs::new(machine, at.cpu);
        let mut exits = Exits { at, ctx, machine, budget, slow: miss, pc: 0, done: 0 };
        for (j, &o) in run.iter().enumerate() {
            let (d, pc) = (&page[o], base + o as u32);
            (exits.pc, exits.done) = (pc, j as u64);
            exits.slow = if slow[o] != u32::MAX { slow[o] } else { miss };
            let flow = lower::lower(&mut b, &mut regs, &mut exits, &d.insn, pc, d.len, literal(o));
            if j + 1 < run.len() { continue; }
            regs.flush(&mut b);
            count_in(&mut b, at, ctx, machine, budget, run.len() as u64);
            let fall = pc.wrapping_add(u32::from(d.len));
            // The block of `to`, or one that stores it and leaves for the lane.
            let edge = |b: &mut UnitBuilder, to: u32| {
                let block = block_at(to);
                if block != miss { return block; }
                let (back, away) = (b.current(), b.block());
                b.switch_to(away);
                let v = b.iconst(i64::from(to));
                b.emit(Inst::Store(Mem::Int(4, false), machine, at.pc, v));
                b.jump(miss);
                b.switch_to(back);
                away
            };
            match flow {
                Flow::Next => { let to = edge(&mut b, fall); b.jump(to) }
                Flow::Jump(target) => { let to = edge(&mut b, target); b.jump(to) }
                Flow::Indirect(to) => {
                    b.emit(Inst::Store(Mem::Int(4, false), machine, at.pc, to));
                    b.redispatch(to);
                }
                Flow::Branch { taken, target } => {
                    let (yes, no) = (edge(&mut b, target), edge(&mut b, fall));
                    b.branch(taken, yes, no);
                }
            }
        }
    }
    for &o in offsets.iter().filter(|&&o| kind[o] == Kind::Helper || slow[o] != u32::MAX) {
        let pc = base + o as u32;
        b.switch_to(if kind[o] == Kind::Helper { block[o] } else { slow[o] });
        let pc_v = b.iconst(i64::from(pc));
        let d_v = b.iconst(&page[o] as *const Decoded as usize as i64);
        let r = b.call(helper, &[ctx, pc_v, d_v]);
        b.emit(Inst::Mov(code, r));
        // The helper has stored the PC: an instruction outside the unit is a plain miss.
        let fall = pc.wrapping_add(u32::from(page[o].len));
        b.branch(r, out, block_at(fall));
    }
    b.finish(EXIT_MISS)
}
