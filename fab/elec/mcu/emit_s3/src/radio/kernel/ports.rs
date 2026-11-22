use fab_elec_mcu_emit32::{asm::Pass, xtensa::{assemble_relaxed, cc, ccz, enc, Asm, Reg}};
use fab_elec_mcu_rt::{kernel::{access_class, AccessClass}, module::Module, ports::*, watch, KernelLines};
use super::{refusal, KernelOptions, LoopUnit, PortCost};
use crate::{emit::uses_port, insns::instructions, radio::REPORT_RING_ENTRIES, Refusal};

const FAULT: usize = 0;
const SYSTIMER: u32 = 0x6002_3000;
const INTENABLE: u32 = 228;
const INTSET: u32 = 226;

#[derive(Clone, Copy)]
pub(crate) struct ScalarAccess {
    pub class: AccessClass,
    pub width: u32,
    pub write: bool,
    pub instruction: fn(&mut Asm, Reg, Reg, u32),
}

impl ScalarAccess {
    pub(crate) fn check(self, address: u32, regions: &[super::KernelRegion]) -> Result<(), &'static str> {
        if address % self.width != 0 { return Err("misaligned"); }
        let end = u64::from(address) + u64::from(self.width);
        if regions.iter().any(|r| self.class.reaches(r.kind) && (!self.write || r.writable)
            && r.base <= address && end <= u64::from(r.base) + u64::from(r.len)) {
            Ok(())
        } else {
            Err("outside permitted regions")
        }
    }
}

pub(crate) fn scalar_access(port: u16) -> Option<ScalarAccess> {
    let (width, write, instruction): (_, _, fn(&mut Asm, Reg, Reg, u32)) = match port {
        LOAD8U => (1, false, Asm::l8ui),
        LOAD16U => (2, false, Asm::l16ui),
        LOAD32 | PACK_LOAD32 | DEV_LOAD32 => (4, false, Asm::l32i),
        STORE8 | PACK_STORE8 => (1, true, Asm::s8i),
        STORE16 => (2, true, Asm::s16i),
        STORE32 | PACK_STORE32 | DEV_STORE32 => (4, true, Asm::s32i),
        _ => return None,
    };
    Some(ScalarAccess { class: access_class(port).expect("scalar port class"), width, write, instruction })
}

pub(super) struct Routines {
    pub bytes: Vec<u8>,
    pub targets: Vec<(u16, u32)>,
    pub symbols: Vec<(String, u32, u32)>,
    pub costs: Vec<(u16, PortCost)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span { start: u32, end: u32 }
impl Span {
    fn count(self, bytes: &[u8]) -> u64 { instructions(bytes, self.start, self.end) }
}

struct LoopSpan { span: Span, unit: LoopUnit, alternative: bool }
struct RoutineSpan { port: u16, span: Span, loops: Vec<LoopSpan>, walks: Vec<Span>, faults: bool }
impl RoutineSpan {
    fn cost(&self, bytes: &[u8], fault_insns: u64) -> PortCost {
        let unit = self.loops.first().map(|l| l.unit);
        let mut loop_insns = 0u64;
        let mut per_unit = 0u64;
        let mut group = 0u64;
        let mut end = self.span.start;
        for l in &self.loops {
            assert!(end <= l.span.start && l.span.start < l.span.end && l.span.end <= self.span.end,
                "kernel loop spans are disjoint and inside their routine");
            assert_eq!(Some(l.unit), unit, "a routine's loops use the same repetition unit");
            let count = l.span.count(bytes);
            loop_insns += count;
            if l.alternative { group = group.max(count); }
            else { per_unit += group; group = count; }
            end = l.span.end;
        }
        per_unit += group;
        let non_loop = self.span.count(bytes) - loop_insns;
        PortCost { fixed: non_loop + 1 + if self.faults { fault_insns } else { 0 },
            walk: self.walks.iter().map(|span| span.count(bytes)).sum(), per_unit, non_loop, unit }
    }
}

// Rebuilt on every assembly pass; only the final pass's spans are counted.
struct Emission {
    targets: Vec<(u16, u32)>,
    symbols: Vec<(String, u32, u32)>,
    routines: Vec<RoutineSpan>,
    fault: Span,
    literals: u32,
}

struct Writer<'a> {
    a: &'a mut Asm, next: usize, literals: u32,
    loops: Vec<LoopSpan>, walks: Vec<Span>, faults: bool,
}
impl Writer<'_> {
    fn label(&mut self) -> usize { let n = self.next; self.next += 1; n }
    fn bind(&mut self, label: usize) { self.a.base.label_assign(label); }
    fn loop_start(&mut self, label: usize) -> u32 { self.bind(label); self.a.base.offset() }
    fn loop_end(&mut self, start: u32, unit: LoopUnit) {
        self.loops.push(LoopSpan { span: Span { start, end: self.a.base.offset() }, unit, alternative: false });
    }
    fn loop_end_alternative(&mut self, start: u32, unit: LoopUnit) {
        assert!(!self.loops.is_empty(), "an alternative needs a preceding loop group");
        self.loops.push(LoopSpan { span: Span { start, end: self.a.base.offset() }, unit, alternative: true });
    }
    fn imm(&mut self, reg: Reg, value: u32) {
        self.literals += 1; // Also counts short immediates: conservative reach preflight.
        self.a.mov_reg_i32_optimised(reg, value);
    }
    fn rsr(&mut self, reg: Reg, sr: u32) { self.a.op24(enc::rsr(0, 3, 0, sr, reg)); }
    fn fault(&mut self, code: u8) {
        self.faults = true;
        self.a.mov_n(3, 10); self.imm(2, u32::from(code)); self.a.j_label(FAULT);
    }
    fn stop(&mut self, word: u32) {
        self.imm(4, word); self.a.s32i(2, 4, 0); self.a.memw();
        self.a.mov_n(3, 10); self.a.j_label(FAULT); self.faults = true;
    }
    fn aligned(&mut self, bits: u8) {
        let ok = self.label();
        self.a.extui(8, 2, 0, bits); self.a.bccz_reg_label(ccz::EQ, 8, ok);
        self.fault(watch::FAULT_KERNEL_MISALIGNED); self.bind(ok);
    }
    /// Scratch a8/a9 only. Subtraction avoids an overflowing addr+length.
    /// Each local conditional branch stays short even with many regions;
    /// the unconditional success jump has the blob's checked 18-bit reach.
    fn region(&mut self, o: &KernelOptions, class: AccessClass, addr: Reg, width: Option<u32>, write: bool) {
        let found = self.label();
        let start = self.a.base.offset();
        for r in o.regions.iter().filter(|r| class.reaches(r.kind) && (!write || r.writable) && width.map_or(true, |w| r.len >= w)) {
            let next = self.label();
            self.imm(8, r.base); self.a.sub(8, addr, 8);
            self.imm(9, r.len - width.unwrap_or(0));
            if width.is_none() {
                self.a.bcc_reg_reg_label(cc::LTU, 9, 4, next);
                self.a.sub(9, 9, 4);
            }
            self.a.bcc_reg_reg_label(cc::LTU, 9, 8, next);
            self.a.j_label(found); self.bind(next);
        }
        self.walks.push(Span { start, end: self.a.base.offset() });
        self.fault(watch::FAULT_KERNEL_OUT_OF_REGION); self.bind(found);
    }
    fn raw_prefix(&mut self, code: u8) {
        let bounded = self.label(); let nonzero = self.label();
        self.a.bcc_reg_reg_label(cc::GEU, 5, 4, bounded);
        self.fault(code); self.bind(bounded);
        self.a.bccz_reg_label(ccz::NE, 4, nonzero); self.a.ret_n(); self.bind(nonzero);
    }
    fn routine(&mut self, port: u16, o: &KernelOptions, lines: KernelLines) {
        if let Some(access) = scalar_access(port) {
            if access.width > 1 { self.aligned(access.width.trailing_zeros() as u8); }
            self.region(o, access.class, 2, Some(access.width), access.write);
            (access.instruction)(self.a, if access.write { 3 } else { 2 }, 2, 0);
            self.a.ret_n();
            return;
        }
        let class = || access_class(port).expect("memory port has an access class");
        match port {
            COPY_RAW | PACK_COPY => {
                self.raw_prefix(watch::FAULT_KERNEL_COPY_BOUND);
                let class = class();
                self.region(o, class, 3, None, false); self.region(o, class, 2, None, true);
                let overlap = self.label(); let forward = self.label(); let back = self.label();
                self.a.bcc_reg_reg_label(cc::LTU, 3, 2, overlap); self.a.j_label(forward);
                self.bind(overlap); self.a.sub(8, 2, 3);
                self.a.bcc_reg_reg_label(cc::GEU, 8, 4, forward);
                // dst > src and dst-src < n: both ranges were checked, so the
                // backwards copy cannot corrupt a byte still needed as input.
                self.a.add_n(2, 2, 4); self.a.add_n(3, 3, 4);
                let start = self.loop_start(back); self.a.addi(2, 2, -1); self.a.addi(3, 3, -1);
                self.a.l8ui(8, 3, 0); self.a.s8i(8, 2, 0); self.a.addi(4, 4, -1);
                self.a.bccz_reg_label(ccz::NE, 4, back); self.loop_end(start, LoopUnit::Byte); self.a.ret_n();
                self.bind(forward);
                let next = self.label(); let start = self.loop_start(next);
                self.a.l8ui(8, 3, 0); self.a.s8i(8, 2, 0);
                self.a.addi(2, 2, 1); self.a.addi(3, 3, 1); self.a.addi(4, 4, -1);
                self.a.bccz_reg_label(ccz::NE, 4, next); self.loop_end_alternative(start, LoopUnit::Byte); self.a.ret_n();
            }
            FILL_RAW | PACK_FILL => {
                self.raw_prefix(watch::FAULT_KERNEL_FILL_BOUND); self.region(o, class(), 2, None, true);
                let next = self.label(); let start = self.loop_start(next);
                self.a.s8i(3, 2, 0); self.a.addi(2, 2, 1); self.a.addi(4, 4, -1);
                self.a.bccz_reg_label(ccz::NE, 4, next); self.loop_end(start, LoopUnit::Byte); self.a.ret_n();
            }
            CLZ => {
                let done = self.label(); self.imm(3, 32); self.a.bccz_reg_label(ccz::EQ, 2, done); self.imm(3, 0);
                for bits in [16, 8, 4, 2, 1] {
                    let next = self.label();
                    self.a.extui(4, 2, 32 - bits, bits); self.a.bccz_reg_label(ccz::NE, 4, next);
                    self.a.op24(enc::slli(2, 2, bits)); self.a.addi(3, 3, i32::from(bits)); self.bind(next);
                }
                self.bind(done); self.a.mov_n(2, 3); self.a.ret_n();
            }
            IRQ_ENABLE | IRQ_DISABLE => {
                self.imm(3, !lines.protected); self.a.and(2, 2, 3);
                if port == IRQ_DISABLE { self.imm(3, u32::MAX); self.a.xor(2, 2, 3); }
                self.rsr(3, INTENABLE);
                if port == IRQ_ENABLE { self.a.or(2, 2, 3); } else { self.a.and(2, 2, 3); }
                self.a.wsr(2, INTENABLE); self.a.rsync(); self.a.ret_n();
            }
            IRQ_RAISE => {
                // AIR admits only constant lines in KernelLines::raise. Keeping
                // the shift here shares one routine across those listed lines.
                self.imm(3, 1); self.a.ssl(2); self.a.sll(2, 3);
                self.a.wsr(2, INTSET); self.a.rsync(); self.a.ret_n();
            }
            NOW64 => {
                self.aligned(2); self.region(o, class(), 2, Some(8), true);
                // runtime::stamp's unit-0 UPDATE/VALID protocol. Each further
                // VALID poll adds the recorded span's count (currently +3);
                // PS and stamp are untouched.
                self.imm(4, SYSTIMER); self.imm(5, 1 << 30); self.a.s32i(5, 4, 4);
                let poll = self.label(); let start = self.loop_start(poll);
                self.a.l32i(5, 4, 4); self.a.extui(5, 5, 29, 1); self.a.bccz_reg_label(ccz::EQ, 5, poll);
                self.loop_end(start, LoopUnit::Poll);
                self.a.l32i(6, 4, 0x44); self.a.l32i(7, 4, 0x40);
                self.imm(5, 0x000f_ffff); self.a.and(7, 7, 5);
                self.imm(4, o.t0); self.a.l32i(3, 4, 0); self.a.l32i(5, 4, 4);
                let no_borrow = self.label(); self.imm(8, 0);
                self.a.bcc_reg_reg_label(cc::GEU, 6, 3, no_borrow); self.imm(8, 1); self.bind(no_borrow);
                self.a.sub(6, 6, 3); self.a.sub(7, 7, 5); self.a.sub(7, 7, 8);
                self.a.s32i(6, 2, 0); self.a.s32i(7, 2, 4); self.a.ret_n();
            }
            KERNEL_LOOP_CAP => self.fault(watch::FAULT_LOOP_CAP),
            RADIO_STOP => self.stop(o.stop_pending.expect("stop word preflight")),
            KERNEL_REPORT => {
                // Masked exports (including ISR calls) serialize publication
                // with the dispatcher's drain. Never enter EVENT/WIRE here.
                let full = self.label(); let done = self.label();
                self.imm(4, o.reports.expect("report ring preflight"));
                self.a.l32i(5, 4, 0); self.a.l32i(6, 4, 4); self.a.sub(5, 6, 5);
                self.imm(7, REPORT_RING_ENTRIES); self.a.bcc_reg_reg_label(cc::GEU, 5, 7, full);
                self.a.extui(5, 6, 0, REPORT_RING_ENTRIES.ilog2() as u8);
                self.a.addx2(5, 5, 5); self.a.addx4(5, 5, 4); // ring + slot * 12
                self.a.s32i(2, 5, 12); self.a.s32i(10, 5, 16); self.a.s32i(3, 5, 20);
                self.a.addi(6, 6, 1); self.a.s32i(6, 4, 4);
                if o.stop_pending.is_some() { self.a.j_label(done); } else { self.a.ret_n(); }
                self.bind(full); self.a.l32i(5, 4, 8); self.imm(7, u32::MAX);
                self.a.bcc_reg_reg_label(cc::EQ, 5, 7, done);
                self.a.addi(5, 5, 1); self.a.s32i(5, 4, 8);
                self.bind(done);
                if let Some(word) = o.stop_pending {
                    // Decision 1: publish the report (or its loss) before stopping the task or ISR.
                    // Through publication: 15 instructions with room, 11 on loss, 9 on
                    // saturated loss. The station list takes five from done to a normal RET.
                    // Stopping at 100/107/110 costs 2/3/4 through the decision and six to ILL.
                    let stop = self.label();
                    let first = i32::from(watch::FAULT_RADIO_PHY_INIT_FAILED);
                    self.a.addi(5, 2, -first); self.a.bccz_reg_label(ccz::EQ, 5, stop);
                    for code in watch::FAULT_KERNEL_STOPPING {
                        let offset = i32::from(code) - first;
                        if enc::b4const(offset).is_some() {
                            self.a.bcci_reg_label(cc::EQ, 5, offset, stop);
                        } else {
                            // Publication is complete, so a7's ring limit (or saturated loss count)
                            // is dead. stop still needs the original code in a2 and site in a10.
                            self.a.movi(7, offset);
                            self.a.bcc_reg_reg_label(cc::EQ, 5, 7, stop);
                        }
                    }
                    self.a.ret_n(); self.bind(stop); self.stop(word);
                } else { self.a.ret_n(); }
            }
            _ => unreachable!("verified kernel port"),
        }
    }
}

pub(super) fn build(m: &Module<'_>, o: &KernelOptions, lines: KernelLines) -> Result<Routines, Refusal> {
    let used: Vec<_> = (KERNEL_BASE..KERNEL_BASE + KERNEL_PORTS.len() as u16).filter(|&p| uses_port(m, p)).collect();
    if used.contains(&KERNEL_REPORT) && o.reports.is_none() {
        return Err(refusal("kernel port 78 `report` needs a report ring (KernelOptions::reports)"));
    }
    if used.contains(&RADIO_STOP) {
        let word = o.stop_pending.ok_or_else(|| refusal("kernel port 86 `radio_stop` needs a stop word (KernelOptions::stop_pending)"))?;
        if word & 3 != 0 { return Err(refusal("kernel port 86 `radio_stop` needs a word-aligned stop word")); }
    }
    if used.contains(&KERNEL_REPORT) && o.stop_pending.is_some_and(|word| word & 3 != 0) {
        return Err(refusal("kernel port 78 `report` needs a word-aligned stop word when supplied"));
    }
    let labels = used.len().checked_mul(o.regions.len().checked_mul(2).and_then(|n| n.checked_add(32))
        .ok_or_else(|| refusal("kernel routine size overflows"))?).and_then(|n| n.checked_add(1))
        .ok_or_else(|| refusal("kernel routine size overflows"))?;
    let mut asm = Asm::new(labels, 11);
    let emit = |a: &mut Asm| {
        let mut w = Writer { a, next: 1, literals: 0, loops: Vec::new(), walks: Vec::new(), faults: false };
        w.a.literal_table(); w.a.base.align(4); w.bind(FAULT);
        let start = w.a.base.offset(); w.a.op24(0); // ILL
        let fault = Span { start, end: w.a.base.offset() };
        let mut symbols = vec![("kernel_fault".into(), fault.start, fault.end - fault.start)];
        let mut targets = Vec::new();
        let mut routines = Vec::new();
        for &port in &used {
            w.a.base.align(4); let start = w.a.base.offset();
            w.faults = false;
            w.routine(port, o, lines);
            let span = Span { start, end: w.a.base.offset() };
            targets.push((port, start));
            symbols.push((KERNEL_PORTS[(port - KERNEL_BASE) as usize].name.into(), start, span.end - span.start));
            routines.push(RoutineSpan { port, span, loops: std::mem::take(&mut w.loops),
                walks: std::mem::take(&mut w.walks), faults: w.faults });
        }
        w.a.base.align(4);
        Emission { targets, symbols, routines, fault, literals: w.literals }
    };
    asm.base.start_pass(Pass::Compute);
    let mut emission = emit(&mut asm);
    let limit = u64::from(asm.base.offset()) + u64::from(emission.literals) * 4 + 4;
    if limit > 120 * 1024 || u64::from(o.base) + limit > u64::from(u32::MAX) {
        return Err(refusal("kernel routines exceed branch/literal reach or the code address space"));
    }
    let bytes = assemble_relaxed(&mut asm, |a| {
        let next = emit(a);
        if a.base.pass() == Pass::Emit {
            // The preceding pass used the settled branch widths.
            for (computed, emitted) in emission.routines.iter().zip(&next.routines) {
                assert_eq!(computed.walks, emitted.walks, "port {} walk spans differ between final Compute and Emit", emitted.port);
            }
        }
        emission = next;
    });
    let fault_insns = emission.fault.count(&bytes);
    let costs = emission.routines.iter().map(|r| (r.port, r.cost(&bytes, fault_insns))).collect();
    for (_, address) in &mut emission.targets { *address += o.base; }
    for (_, address, _) in &mut emission.symbols { *address += o.base; }
    Ok(Routines { bytes, targets: emission.targets, symbols: emission.symbols, costs })
}
