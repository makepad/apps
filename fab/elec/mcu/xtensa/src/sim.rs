//! CALL0 and windowed execution, precise faults, and external interrupt inputs.

use fab_elec_mcu_emu::bus::{Bus, Kind, Width};

use crate::fp;
use crate::insn::{decode, target, Insn, Op};

#[derive(Clone, Copy, Debug)]
pub struct Vectors {
    /// Byte offset from VECBASE (pp. 147–148).
    pub kernel: u32,
    /// Byte offset from VECBASE (pp. 147–148).
    pub user: u32,
    /// OF4, UF4, OF8, UF8, OF12, UF12 offsets from VECBASE (pp. 147, 241).
    /// ESP32-S3 core-isa.h XCHAL_WINDOW_*_VECOFS: 0, 0x40, 0x80,
    /// 0xc0, 0x100, 0x140; XCHAL_NUM_AREGS is 64.
    pub window: [u32; 6],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntType {
    None,
    Level,
    Edge,
    Software,
    /// Comparator number, 0 through 2 (pp. 161–162).
    Timer(u8),
}

/// Board choices; deliberately no guessed ESP32-S3 interrupt table or defaults.
///
/// Configured interrupts must have levels 1..=15 and timer indices 0..=2.
/// EXCMLEVEL is the configuration's (p. 134: PS.EXCM masks interrupts up to it; the S3's
/// core-isa.h gives 3). Only level 1 is taken; a higher unmasked interrupt produces a named stop.
/// The three behaviours a configuration can give an unaligned load or store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unaligned {
    /// No option: the access uses the aligned item that contains the address (pp. 56–60).
    Ignore,
    /// The Unaligned Exception Option: LoadStoreAlignmentCause (pp. 148–149).
    Exception,
    /// Handled in hardware: the access reads or writes the bytes at the address itself. The S3
    /// (core-isa.h: XCHAL_UNALIGNED_LOAD_HW and _STORE_HW are 1, the exceptions 0).
    Hardware,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Mul32High sub-option, Table 31, p. 89; unspecified by the S3 brief.
    pub mul32_high: bool,
    /// Integer Divide Option, p. 90; the S3 brief specifies a divider.
    pub div32: bool,
    /// MAC16 register storage only, pp. 91–93, 280–281.
    pub mac16: bool,
    /// SCOMPARE1 storage only (not S32C1I), pp. 118–120, 283.
    pub s32c1i: bool,
    /// RUR/WUR.THREADPTR, pp. 255, 316.
    pub threadptr: bool,
    /// What an unaligned load or store does (pp. 56–60, 148–149).
    pub unaligned: Unaligned,
    /// The level PS.EXCM masks interrupts up to (EXCMLEVEL, p. 134). The S3: 3 (core-isa.h
    /// XCHAL_EXCM_LEVEL).
    pub excm_level: u8,
    /// Reset base, p. 296. This model chooses no lock and only bit 0 hardwired
    /// to zero (p. 148); no further implementation-dependent alignment mask.
    pub vecbase_reset: u32,
    pub vectors: Vectors,
    pub int_type: [IntType; 32],
    pub int_level: [u8; 32],
    /// Read-only processor ID, pp. 310–311. None stops an RSR before any
    /// state changes when the board's sources do not document the value.
    pub prid: Option<u32>,
}

/// Table 65, pp. 135–138. These are all causes raised by the modeled options;
/// no MMU privilege or external-register checks run. Window overflow and
/// underflow have dedicated vectors, without EXCCAUSE values (pp. 135, 246–247).
pub mod cause {
    pub const ILLEGAL: u32 = 0;
    pub const SYSCALL: u32 = 1;
    pub const INSTR_FETCH_ERROR: u32 = 2;
    pub const LOAD_STORE_ERROR: u32 = 3;
    pub const LEVEL1_INTERRUPT: u32 = 4;
    pub const ALLOCA: u32 = 5;
    pub const DIVIDE_BY_ZERO: u32 = 6;
    pub const LOAD_STORE_ALIGNMENT: u32 = 9;
    /// Table 65 p. 138; Table 71 p. 150 (Coprocessor Context Option).
    pub const COPROCESSOR0_DISABLED: u32 = 32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapMode {
    /// Stop on general exceptions. Dedicated window vectors, like interrupt
    /// vectors, are entered in both modes; they have no EXCCAUSE number.
    Stop,
    Take,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Break { pc: u32 },
    /// PC of the instruction following the retired WAITI.
    Waiti { pc: u32 },
    /// `vaddr` is zero for causes that do not set EXCVADDR (pp. 135–136).
    Exception { cause: u32, pc: u32, vaddr: u32 },
    Unmodeled { pc: u32, what: &'static str },
    Limit,
}

// PS layout: Table 64 pp. 133–134 and Table 139 p. 284.
const EXCM: u32 = 1 << 4;
const UM: u32 = 1 << 5;
const WOE: u32 = 1 << 18;
const OWB: u32 = 15 << 8;
const CALLINC: u32 = 3 << 16;
const PS_MASK: u32 = 0x0007_0fff; // Table 139, p. 284.

/// Visible architectural state. Use `new` to initialize the private edge and
/// WAITI latches. Direct state writes are useful to debuggers; instruction
/// writes apply each special register's architectural mask.
#[derive(Clone, Debug)]
pub struct Cpu {
    pub ar: [u32; 16],
    /// b0..b15, SR 4 (pp. 97, 282). Undefined after reset (p. 99); choose zero.
    pub br: u16,
    /// Single-precision raw bits, Table 44 pp. 99–100; choose zero at reset.
    pub fr: [u32; 16],
    /// User 232: RM 1:0, trap enables 6:2 (pp. 108, 317). Undefined reset: zero.
    pub fcr: u32,
    /// User 233: V/Z/O/U/I in 11:7 (pp. 109, 317). Undefined reset: zero.
    pub fsr: u32,
    /// ISA Summary p. 109: unknown Underflow, unless sticky FSR.U is already set.
    /// Kept with FSR by Clone, including whole-machine snapshots and restores.
    pub fsr_underflow_unknown: bool,
    pub pc: u32,
    pub sar: u32,
    /// SRs 0..2, pp. 84–86, 278–279. Storage only; a pending loop-back stops.
    pub lbeg: u32,
    pub lend: u32,
    pub lcount: u32,
    /// MAC16 SRs 16–17: low 32 bits and high signed 8 bits (p. 280).
    /// Storage only; arithmetic remains unmodeled. Clone keeps both halves.
    pub acclo: u32,
    /// Signed accumulator high byte; RSR sign-extends it to 32 bits (p. 280).
    pub acchi: i8,
    /// MAC16 SRs 32–35, four full-width data registers (p. 281).
    pub m: [u32; 4],
    /// Conditional Store SR 12, storage only (p. 283).
    pub scompare1: u32,
    /// Thread Pointer UR 231, fully interlocked RUR/WUR storage (p. 316).
    pub threadptr: u32,
    /// Low four bits locate `ar` in the 64-register file (pp. 240, 289).
    /// A direct field write relabels the existing visible registers; it does
    /// not rotate them. WSR/XSR.WINDOWBASE rotate and synchronize the file.
    pub window_base: u32,
    /// Sixteen valid-window bits (p. 290); instruction writes enforce width.
    pub window_start: u32,
    pub ps: u32,
    pub vecbase: u32,
    pub epc1: u32,
    pub excsave1: u32,
    pub exccause: u32,
    pub excvaddr: u32,
    pub cpenable: u32,
    pub interrupt: u32,
    pub intenable: u32,
    pub ccount: u32,
    pub ccompare: [u32; 3],
    pub instret: u64,
    pub traps: TrapMode,
    /// Core configuration. Interrupt types are fixed at construction because
    /// timer routes are cached; runtime requests use set_lines/interrupt.
    pub config: Config,
    // `ar` is authoritative for the current window. These slots are refreshed
    // only on rotation; ordinary CALL0 instructions keep their direct hot path.
    physical: [u32; 64],
    lines: u32,
    waiting: bool,
    // Derived once from Config at construction. Interrupt routing in Config
    // is a core configuration, not runtime state; do not mutate it afterward.
    // Allow all 32 lines to be timers, including duplicate comparator routes.
    timer_lines: [(u8, u8); 32],
    timer_len: usize,
    // The level and the edge lines of `config.int_type`, derived with the timer routes.
    level_lines: u32,
    edge_lines: u32,
}

impl Cpu {
    /// PS resets to 0x1f (pp. 133, 284). The brief chooses WindowBase=0,
    /// WindowStart=1, INTENABLE=0; these are undefined in the ISA (pp. 156,
    /// 244, 289–290, 305). Other undefined reset state is deterministically
    /// zero (AR p. 272, SAR p. 281, exception registers pp. 295–301,
    /// interrupt/timer state pp. 156, 162, CPENABLE p. 312).
    /// LCOUNT resets to zero; undefined LBEG/LEND also choose zero (p. 85).
    pub fn new(config: Config, pc: u32) -> Cpu {
        let mut timer_lines = [(0, 0); 32];
        let mut timer_len = 0;
        let (mut level_lines, mut edge_lines) = (0, 0);
        for n in 0..32 {
            match config.int_type[n] {
                IntType::Level => level_lines |= 1 << n,
                IntType::Edge => edge_lines |= 1 << n,
                _ => {}
            }
            if config.int_type[n] != IntType::None {
                assert!((1..=15).contains(&config.int_level[n]), "interrupt level must be 1..=15");
            }
            if let IntType::Timer(timer) = config.int_type[n] {
                assert!(timer < 3, "timer index must be 0..=2");
                timer_lines[timer_len] = (n as u8, timer);
                timer_len += 1;
            }
        }
        Cpu {
            ar: [0; 16], br: 0, fr: [0; 16], fcr: 0, fsr: 0, fsr_underflow_unknown: false,
            pc, sar: 0, lbeg: 0, lend: 0, lcount: 0, window_base: 0, window_start: 1,
            acclo: 0, acchi: 0, m: [0; 4], scompare1: 0, threadptr: 0,
            ps: 0x1f, vecbase: config.vecbase_reset & !1,
            epc1: 0, excsave1: 0, exccause: 0, excvaddr: 0, cpenable: 0,
            interrupt: 0, intenable: 0, ccount: 0, ccompare: [0; 3],
            instret: 0, traps: TrapMode::Stop, config, lines: 0, waiting: false,
            physical: [0; 64], timer_lines, timer_len, level_lines, edge_lines,
        }
    }

    /// Synchronized physical ARs, including direct writes to `ar`. No state
    /// changes or allocation; direct WindowBase writes relabel the window.
    pub fn physical_ar(&self) -> [u32; 64] {
        let mut physical = self.physical;
        for (n, value) in self.ar.iter().enumerate() {
            physical[((self.window_base as usize & 15) * 4 + n) & 63] = *value;
        }
        physical
    }

    fn rotate_window(&mut self, base: u32) {
        // AddressRegister[(WindowBase + i[3:2]) || i[1:0]], p. 244.
        self.physical = self.physical_ar();
        self.window_base = base & 15;
        for (n, value) in self.ar.iter_mut().enumerate() {
            *value = self.physical[(self.window_base as usize * 4 + n) & 63];
        }
    }

    fn window_live(&self, base: u32) -> bool {
        self.window_start & (1 << (base & 15)) != 0
    }

    fn enter_window(&mut self, base: u32, vector: usize) {
        // pp. 246–247: EPC1/OWB/EXCM change, but EXCCAUSE, EXCVADDR,
        // UM, CALLINC and WindowStart do not. No instruction retires.
        self.epc1 = self.pc;
        self.ps = (self.ps & !OWB) | ((self.window_base & 15) << 8) | EXCM;
        self.rotate_window(base);
        self.pc = self.vecbase.wrapping_add(self.config.vectors.window[vector]);
    }

    fn window_check(&mut self, max: u8) -> bool {
        // WindowCheck, p. 246. The first occupied group determines the
        // rotation; the NEXT occupied group determines how many ARs spill.
        for distance in 1..=max {
            let base = self.window_base.wrapping_add(u32::from(distance));
            if self.window_live(base) {
                let vector = if self.window_live(base.wrapping_add(1)) { 0 }
                    else if self.window_live(base.wrapping_add(2)) { 2 } else { 4 };
                self.enter_window(base, vector);
                return true;
            }
        }
        false
    }

    fn configured_mask(&self) -> u32 {
        let mut mask = 0;
        for n in 0..32 {
            if self.config.int_type[n] != IntType::None {
                mask |= 1 << n;
            }
        }
        mask
    }

    /// Level inputs follow the pin; edge inputs latch only a rising edge
    /// (Table 77, p. 154). Software and timer bits ignore external lines.
    pub fn set_lines(&mut self, lines: u32) {
        let rising = lines & !self.lines;
        self.interrupt = (self.interrupt & !self.level_lines) | (lines & self.level_lines) | (rising & self.edge_lines);
        self.lines = lines;
    }

    /// Advance the counter modulo 2^32. Match on the newly reached counts,
    /// never on the starting count. A comparator equal to the starting count
    /// next matches after a full 2^32 ticks (pp. 162, 307).
    /// Timer requests remain set across interrupt entry: the explicit
    /// CCOMPARE-clear rules (pp. 154, 162, 307) take priority over p. 162's
    /// informal phrase "remembered until the interrupt is taken".
    pub fn advance(&mut self, cycles: u32) {
        for &(line, timer) in &self.timer_lines[..self.timer_len] {
            let distance = self.ccompare[timer as usize].wrapping_sub(self.ccount);
            if distance != 0 && distance <= cycles {
                self.interrupt |= 1 << line;
            }
        }
        self.ccount = self.ccount.wrapping_add(cycles);
    }

    /// Distance to the next comparator match with its INTENABLE bit set,
    /// independent of PS masking. Because 2^32 does not fit in this API's u32,
    /// `Some(0)` denotes a full wrap, not an immediate event. Callers can split
    /// that wait into `advance(u32::MAX)` and `advance(1)`. Already pending
    /// interrupt requests are separately visible in `interrupt` (pp. 162, 307).
    pub fn next_timer(&self) -> Option<u32> {
        let mut next: Option<u64> = None;
        for &(line, timer) in &self.timer_lines[..self.timer_len] {
            if self.intenable & (1 << line) != 0 {
                let d = self.ccompare[timer as usize].wrapping_sub(self.ccount);
                let distance = if d == 0 { 1u64 << 32 } else { d as u64 };
                next = Some(next.map_or(distance, |old| old.min(distance)));
            }
        }
        next.map(|d| d as u32)
    }

    /// WAITI latch, including an interrupt request awaiting its entry step.
    pub fn sleeping(&self) -> bool { self.waiting }

    /// Whether the next step attempts an instruction rather than interrupt
    /// entry or a sleep wait. Machine PC stops/traces apply only in this case.
    pub fn will_execute(&self) -> bool { !self.waiting && self.pending_level() == 0 }

    fn enter_exception(&mut self, cause: u32, vaddr: Option<u32>) {
        self.epc1 = self.pc;
        self.exccause = cause;
        if let Some(addr) = vaddr {
            self.excvaddr = addr;
        }
        let offset = if self.ps & UM != 0 { self.config.vectors.user } else { self.config.vectors.kernel };
        self.ps |= EXCM;
        self.pc = self.vecbase.wrapping_add(offset);
        self.waiting = false;
    }

    fn fault(&mut self, cause: u32, vaddr: Option<u32>) -> Result<(), Stop> {
        // Double exceptions stop in both modes, before any architectural write
        // (pp. 144–145 and the explicit simulator contract).
        if self.ps & EXCM != 0 {
            return Err(Stop::Unmodeled { pc: self.pc, what: "double exception" });
        }
        if self.traps == TrapMode::Stop {
            return Err(Stop::Exception { cause, pc: self.pc, vaddr: vaddr.unwrap_or(0) });
        }
        self.enter_exception(cause, vaddr);
        Ok(())
    }

    /// CINTLEVEL = max(PS.INTLEVEL, EXCM * EXCMLEVEL), p. 134: only lines
    /// above this level are taken, and only they end a WAITI.
    pub fn interrupt_level(&self) -> u8 {
        let excm = if self.ps & EXCM != 0 { u32::from(self.config.excm_level) } else { 0 };
        (self.ps & 15).max(excm) as u8
    }

    fn pending_level(&self) -> u8 {
        let current = self.interrupt_level();
        let mut highest = 0;
        for n in 0..32 {
            if self.config.int_type[n] != IntType::None
                && self.interrupt & self.intenable & (1 << n) != 0
                && self.config.int_level[n] > current
            {
                highest = highest.max(self.config.int_level[n]);
            }
        }
        highest
    }

    fn read_sr(&self, number: i32) -> u32 {
        match number {
            0 => self.lbeg,
            1 => self.lend,
            2 => self.lcount,
            3 => self.sar & 63,
            4 => self.br as u32, // Table 136 p. 282: zero-extend the 16 Booleans.
            12 => self.scompare1,
            16 => self.acclo,
            17 => self.acchi as i32 as u32, // p. 280: sign-extend ACC[39:32].
            32..=35 => self.m[(number - 32) as usize],
            72 => self.window_base & 15,
            73 => self.window_start & 65535,
            177 => self.epc1,
            209 => self.excsave1,
            224 => self.cpenable & 255,
            226 => self.interrupt & self.configured_mask(),
            // RSR.INTCLEAR is defined to return an undefined value, not to be
            // illegal (Table 174, p. 304). Choose zero.
            227 => 0,
            228 => self.intenable & self.configured_mask(),
            230 => self.ps & PS_MASK,
            231 => self.vecbase & !1,
            232 => self.exccause & 63,
            234 => self.ccount,
            235 => self.config.prid.expect("PRID checked before execution"),
            // Undefined outside exception mode (p. 295): choose stored value.
            238 => self.excvaddr,
            240..=242 => self.ccompare[(number - 240) as usize],
            _ => unreachable!("decode validates special-register access"),
        }
    }

    fn write_sr(&mut self, number: i32, value: u32) {
        // Undefined high bits are discarded (SAR p. 282, EXCCAUSE p. 295,
        // CPENABLE p. 312, INTENABLE p. 305). All eight CPENABLE bits are
        // retained; unused coprocessor bits are undefined on read (p. 312).
        match number {
            0 => self.lbeg = value,
            1 => self.lend = value,
            2 => self.lcount = value,
            3 => self.sar = value & 63,
            4 => self.br = value as u16, // Table 136 p. 282: use AR[t]15..0.
            12 => self.scompare1 = value,
            16 => self.acclo = value,
            17 => self.acchi = value as i8,
            32..=35 => self.m[(number - 32) as usize] = value,
            // pp. 289–290: nonzero high bits stop before reaching here.
            72 => self.rotate_window(value),
            73 => self.window_start = value & 65535,
            177 => self.epc1 = value,
            209 => self.excsave1 = value,
            224 => self.cpenable = value & 255,
            226 | 227 => {
                for n in 0..32 {
                    if value & (1 << n) != 0 {
                        match (number, self.config.int_type[n]) {
                            (226, IntType::Software) => self.interrupt |= 1 << n,
                            (227, IntType::Software | IntType::Edge) => self.interrupt &= !(1 << n),
                            _ => {}
                        }
                    }
                }
            }
            228 => self.intenable = value & self.configured_mask(),
            230 => self.ps = value & PS_MASK,
            231 => self.vecbase = value & !1,
            232 => self.exccause = value & 63,
            // p. 307 leaves the cycle of a write open. Choose replacement
            // followed by the normal retirement tick; no elapsed time is
            // implied by jumping the count to a new value.
            234 => self.ccount = value,
            238 => self.excvaddr = value,
            240..=242 => {
                let timer = (number - 240) as u8;
                self.ccompare[timer as usize] = value;
                for n in 0..32 {
                    if self.config.int_type[n] == IntType::Timer(timer) {
                        self.interrupt &= !(1 << n);
                    }
                }
            }
            _ => unreachable!("decode validates special-register access"),
        }
    }

    fn boolean(&self, index: u8) -> bool { self.br & (1 << index) != 0 }

    fn set_boolean(&mut self, index: u8, value: bool) {
        self.br = (self.br & !(1 << index)) | (u16::from(value) << index);
    }

    fn fp_result(&self, op: Op, result: Result<(u32, u32), fp::Issue>) -> Result<(u32, u32), Stop> {
        let (value, flags) = result.map_err(|issue| Stop::Unmodeled {
            pc: self.pc, what: fp::reason(op, issue).unwrap(),
        })?;
        // pp. 108–110 allow implementation-dependent, possibly imprecise
        // traps. Store the seven defined FCR bits, but stop before any write
        // when a newly raised condition has its enable set. Sticky old flags
        // do not retrigger a trap. MADDN.S supplies no new flags (p. 488).
        if flags & ((self.fcr & 0x7c) << 5) != 0 {
            return Err(Stop::Unmodeled { pc: self.pc, what: fp::reason(op, fp::Issue::Trap).unwrap() });
        }
        // p. 109: an undetermined new Underflow may trap even when sticky U
        // was already set. As for other enabled traps, stop before any write.
        if flags & fp::TININESS_UNDEFINED != 0 && self.fcr & (fp::U >> 5) != 0 {
            return Err(Stop::Unmodeled { pc: self.pc, what: fp::reason(op, fp::Issue::Tininess).unwrap() });
        }
        Ok((value, flags))
    }

    fn fp_flags(&mut self, flags: u32) {
        // p. 109; MADDN.S (p. 488) leaves status and its unknown mark alone.
        if flags == 0 { return; }
        // p. 109: sticky U makes the status determined, including when a
        // later operation raises known Underflow after an undetermined one.
        self.fsr |= flags & fp::FSR_MASK;
        self.fsr_underflow_unknown = (self.fsr_underflow_unknown
            || flags & fp::TININESS_UNDEFINED != 0) && self.fsr & fp::U == 0;
    }

    /// At most one instruction retires. Interrupt/exception entry is a
    /// separate successful step with zero retirements; the following step
    /// fetches the first handler instruction. BREAK and unmodeled stops leave
    /// the CPU unchanged. WAITI retires and remains asleep until an unmasked,
    /// enabled interrupt arrives (p. 644; explicit simulator stop contract).
    pub fn step(&mut self, bus: &mut impl Bus) -> Result<(), Stop> {
        if !self.begin_step()? { return Ok(()); }

        // Fetch exactly the instruction's bytes. The first nibble determines
        // length (Table 198, p. 661). No word-alignment or speculative read.
        let first = match bus.read(self.pc, Width::B, Kind::Fetch, self.instret) {
            Ok(v) => v & 255,
            Err(()) => return self.fault(cause::INSTR_FETCH_ERROR, Some(self.pc)),
        };
        let length = if first & 8 == 0 { 3 } else { 2 };
        let mut bits = first;
        for byte in 1..length {
            let addr = self.pc.wrapping_add(byte);
            match bus.read(addr, Width::B, Kind::Fetch, self.instret) {
                Ok(v) => bits |= (v & 255) << (byte * 8),
                // The bus exposes the failing byte, not a fetch-block address.
                // Choose that byte for EXCVADDR (Table 65 p. 135 leaves fetch
                // granularity to the implementation); EPC1 remains the start.
                Err(()) => return self.fault(cause::INSTR_FETCH_ERROR, Some(addr)),
            }
        }
        // A pending loop-back takes precedence even over an illegal encoding.
        self.finish_step(bus, length, || decode(bits))
    }

    /// Execute a part-supplied instruction at the same boundary as an ordinary fetch.
    /// Interrupt entry and an asleep CPU both bypass the supplier.
    /// If the supplier enters a register-window vector, only the vector entry runs.
    pub fn step_supplied<B: Bus>(&mut self, bus: &mut B,
        supply: impl FnOnce(&mut Cpu, &mut B) -> Result<(Insn, u32), Stop>,
    ) -> Result<(), Stop> {
        if !self.begin_step()? { return Ok(()); }
        let pc = self.pc;
        let could_overflow = self.ps & (WOE | EXCM) == WOE;
        let (i, length) = supply(self, bus)?;
        if could_overflow && self.ps & EXCM != 0 && self.epc1 == pc { return Ok(()); }
        self.finish_step(bus, length, || Ok((i, length)))
    }

    #[inline(always)]
    fn begin_step(&mut self) -> Result<bool, Stop> {
        let level = self.pending_level();
        if level > 1 {
            const NAMES: [&str; 16] = [
                "level-0 interrupt", "level-1 interrupt", "level-2 interrupt", "level-3 interrupt",
                "level-4 interrupt", "level-5 interrupt", "level-6 interrupt", "level-7 interrupt",
                "level-8 interrupt", "level-9 interrupt", "level-10 interrupt", "level-11 interrupt",
                "level-12 interrupt", "level-13 interrupt", "level-14 interrupt", "level-15 interrupt",
            ];
            return Err(Stop::Unmodeled { pc: self.pc, what: NAMES[(level.min(15)) as usize] });
        }
        if level == 1 {
            self.enter_exception(cause::LEVEL1_INTERRUPT, None);
            return Ok(false);
        }
        if self.waiting {
            return Err(Stop::Waiti { pc: self.pc });
        }
        Ok(true)
    }

    #[inline(always)]
    fn finish_step(&mut self, bus: &mut impl Bus, length: u32,
        instruction: impl FnOnce() -> Result<(Insn, u32), ()>,
    ) -> Result<(), Stop> {
        use Op::*;
        // Fetch semantics p. 53 and Loop Option p. 86: this uses the
        // sequential successor BEFORE Inst(), not a taken branch's target.
        // CLOOPENABLE = !PS.EXCM (p. 135). Stop before any effects.
        if self.lcount != 0 && self.ps & EXCM == 0
            && self.pc.wrapping_add(length) == self.lend
        {
            return Err(Stop::Unmodeled {
                pc: self.pc, what: "zero-overhead loop: LCOUNT non-zero at LEND",
            });
        }
        let (i, length) = match instruction() {
            Ok(i) => i,
            Err(()) => return self.fault(cause::ILLEGAL, None),
        };
        if self.gated(&i) {
            return self.fault(cause::ILLEGAL, None);
        }
        // p. 135: CWOE = WOE && !EXCM. Window faults cannot nest; a
        // general fault inside a window handler still uses fault() above.
        if self.ps & (WOE | EXCM) == WOE && self.window_gate(&i, window_group(&i, self.ps))? {
            return Ok(());
        }
        // p. 146 puts window overflow before CoprocessorDisabled. §4.4.4
        // pp. 149–151 includes every CP-state access, including RUR/WUR,
        // but excludes shared BR. Gate before FP undefined/approximation
        // stops and data accesses.
        if fp::is_fp(i.op) && self.cpenable & 1 == 0 {
            return self.fault(cause::COPROCESSOR0_DISABLED, None);
        }
        if let Some(what) = unmodeled(i.op) {
            return Err(Stop::Unmodeled { pc: self.pc, what });
        }
        if i.op == Rsr && i.imm == 235 && self.config.prid.is_none() {
            return Err(Stop::Unmodeled {
                pc: self.pc, what: "PRID value not documented for this core",
            });
        }
        if !self.execute(&i, length, bus)? { return Ok(()); }
        self.advance(1);
        if self.waiting { Err(Stop::Waiti { pc: self.pc }) } else { Ok(()) }
    }

    /// The configuration gate (Tables 31 p. 89, options pp. 90–93, 118–120, 255): an
    /// instruction of an option this core lacks is illegal.
    fn gated(&self, i: &Insn) -> bool {
        use Op::*;
        matches!(i.op, Muluh | Mulsh) && !self.config.mul32_high
            || matches!(i.op, Quos | Quou | Rems | Remu) && !self.config.div32
            || matches!(i.op, Rsr | Wsr | Xsr) && match i.imm {
                12 => !self.config.s32c1i,
                16 | 17 | 32..=35 => !self.config.mac16,
                _ => false,
            }
            || matches!(i.op, RurThreadptr | WurThreadptr) && !self.config.threadptr
    }

    /// WindowCheck for a supplied body that accesses a0 through `reg`. On overflow
    /// EPC1 names the body PC, and its caller must return before reading or writing operands.
    pub fn window_check_reg(&mut self, reg: u8) -> bool {
        assert!(reg < 16);
        self.ps & (WOE | EXCM) == WOE && self.window_check(reg / 4)
    }

    /// WindowCheck under CWOE (p. 246) for an instruction of window `group`: true when it entered
    /// a window overflow vector, so the instruction does not execute now.
    fn window_gate(&mut self, i: &Insn, group: u8) -> Result<bool, Stop> {
        use Op::*;
        if self.window_check(group) { return Ok(true); }
        // §6.1.3 checks every referenced AR, but CALLX's explicit
        // WindowCheck ignores s (pp. 379–382). If that difference
        // matters, neither interpretation is silently selected.
        if matches!(i.op, Callx4 | Callx8 | Callx12)
            && (group + 1..=i.s / 4).any(|d|
                self.window_live(self.window_base.wrapping_add(u32::from(d))))
        {
            return Err(Stop::Unmodeled {
                pc: self.pc, what: "CALLX: source-register overflow check conflicts with generic WindowCheck",
            });
        }
        Ok(false)
    }

    /// The instruction's own effects once every gate before it has passed: its data-dependent
    /// stops, the operation, then PC and the retired count. Ok(false): it entered an exception or
    /// a window vector instead and did not retire. CCOUNT is not advanced here: `step` advances it
    /// by one retirement, a fast lane by its whole run before anything can read it.
    pub fn execute(&mut self, i: &Insn, length: u32, bus: &mut impl Bus) -> Result<bool, Stop> {
        use Op::*;
        let r = i.r as usize;
        let s = self.ar[i.s as usize];
        let t = self.ar[i.t as usize];
        // Table 133 p. 280: the upper 24 bits must sign-extend bit 7.
        // Stop before either XSR write when the specified input is undefined.
        if matches!(i.op, Wsr | Xsr) && i.imm == 17 && t != t as i8 as i32 as u32 {
            return Err(Stop::Unmodeled { pc: self.pc, what: if i.op == Wsr {
                "WSR.ACCHI: input is not a sign-extended byte"
            } else { "XSR.ACCHI: input is not a sign-extended byte" } });
        }
        // Table 136 p. 282 leaves a BR write with nonzero high bits
        // undefined. In particular, XSR must stop before either write.
        if matches!(i.op, Wsr | Xsr) && i.imm == 4 && t >> 16 != 0 {
            return Err(Stop::Unmodeled { pc: self.pc, what: if i.op == Wsr {
                "WSR.BR: nonzero high 16 bits are undefined"
            } else { "XSR.BR: nonzero high 16 bits are undefined" } });
        }
        if matches!(i.op, Wsr | Xsr)
            && ((i.imm == 72 && t >> 4 != 0) || (i.imm == 73 && t >> 16 != 0))
        {
            // Tables 147–148, pp. 289–290 explicitly make these undefined.
            let what = match (i.op, i.imm) {
                (Wsr, 72) => "WSR.WINDOWBASE: nonzero high 28 bits are undefined",
                (Xsr, 72) => "XSR.WINDOWBASE: nonzero high 28 bits are undefined",
                (Wsr, _) => "WSR.WINDOWSTART: nonzero high 16 bits are undefined",
                _ => "XSR.WINDOWSTART: nonzero high 16 bits are undefined",
            };
            return Err(Stop::Unmodeled { pc: self.pc, what });
        }
        let mut next = self.pc.wrapping_add(length);
        // Only commit a register after its faulting operations have succeeded.
        // Each covered store is a single atomic bus write, after alignment.
        match i.op {
            All4 | All8 | Any4 | Any8 => {
                let count = if matches!(i.op, All8 | Any8) { 8 } else { 4 };
                if i.s % count != 0 {
                    let what = match i.op {
                        All4 => "ALL4: bs not a multiple of 4 is undefined",
                        Any4 => "ANY4: bs not a multiple of 4 is undefined",
                        All8 => "ALL8: bs not a multiple of 8 is undefined",
                        _ => "ANY8: bs not a multiple of 8 is undefined",
                    };
                    return Err(Stop::Unmodeled { pc: self.pc, what });
                }
                let mask = (1u16 << count) - 1;
                let group = (self.br >> i.s) & mask;
                self.set_boolean(i.t, if matches!(i.op, All4 | All8) { group == mask } else { group != 0 });
            }
            Andb | Andbc | Orb | Orbc | Xorb => {
                let bs = self.boolean(i.s);
                let bt = self.boolean(i.t);
                let value = match i.op {
                    Andb => bs & bt, Andbc => bs & !bt, Orb => bs | bt,
                    Orbc => bs | !bt, _ => bs ^ bt,
                };
                self.set_boolean(i.r, value);
            }
            Movf | Movt => {
                if self.boolean(i.t) == (i.op == Movt) { self.ar[r] = s; }
            }
            Bf | Bt => {
                if self.boolean(i.s) == (i.op == Bt) { next = target(&i, self.pc).unwrap(); }
            }
            RurFcr => self.ar[r] = self.fcr & fp::FCR_MASK,
            RurFsr => {
                // p. 109: defer the tininess stop until the unknown bit is read.
                if self.fsr_underflow_unknown {
                    return Err(Stop::Unmodeled { pc: self.pc, what: fp::reason(i.op, fp::Issue::Tininess).unwrap() });
                }
                self.ar[r] = self.fsr & fp::FSR_MASK;
            }
            WurFcr | WurFsr => {
                // Figures and Table 48 pp. 108–109: MBZ is a write
                // requirement, not an ignored field. No violation result is
                // specified; stop instead of silently discarding those bits.
                if t & 0xffff_f000 != 0 {
                    return Err(Stop::Unmodeled { pc: self.pc, what: if i.op == WurFcr {
                        "WUR.FCR: nonzero MBZ bits have unspecified behavior"
                    } else { "WUR.FSR: nonzero MBZ bits have unspecified behavior" } });
                }
                if i.op == WurFcr { self.fcr = t & fp::FCR_MASK; }
                else {
                    self.fsr = t & fp::FSR_MASK;
                    // p. 109: an explicit FSR write determines all its flags.
                    self.fsr_underflow_unknown = false;
                }
            }
            Rfr => self.ar[r] = self.fr[i.s as usize],
            Wfr => self.fr[r] = s,
            MovS => self.fr[r] = self.fr[i.s as usize],
            MoveqzS | MovnezS | MovltzS | MovgezS | MovfS | MovtS => {
                let take = match i.op {
                    MoveqzS => t == 0, MovnezS => t != 0,
                    MovltzS => (t as i32) < 0, MovgezS => (t as i32) >= 0,
                    MovfS => !self.boolean(i.t), _ => self.boolean(i.t),
                };
                if take { self.fr[r] = self.fr[i.s as usize]; }
            }
            AbsS | NegS => {
                let value = self.fr[i.s as usize];
                // pp. 325, 541 give abs/negate but no NaN result-bit rule.
                if fp::is_nan(value) {
                    return Err(Stop::Unmodeled { pc: self.pc, what: fp::reason(i.op, fp::Issue::Nan).unwrap() });
                }
                self.fr[r] = if i.op == AbsS { value & 0x7fff_ffff } else { value ^ 0x8000_0000 };
            }
            AddS | SubS | MulS | MaddS | MsubS | MaddnS => {
                let (value, flags) = self.fp_result(i.op, fp::arithmetic(
                    i.op, self.fr[r], self.fr[i.s as usize], self.fr[i.t as usize], self.fcr & 3,
                ))?;
                self.fr[r] = value;
                self.fp_flags(flags);
            }
            FloatS | UfloatS => {
                let (value, flags) = self.fp_result(i.op, Ok(fp::from_integer(s, i.op == UfloatS, i.t, self.fcr & 3)))?;
                self.fr[r] = value;
                self.fp_flags(flags);
            }
            CeilS | FloorS | RoundS | TruncS | UtruncS => {
                let (value, flags) = self.fp_result(i.op, fp::to_integer(i.op, self.fr[i.s as usize], i.t))?;
                self.ar[r] = value;
                self.fp_flags(flags);
            }
            OeqS | OleS | OltS | UeqS | UleS | UltS | UnS => {
                let result = fp::compare(i.op, self.fr[i.s as usize], self.fr[i.t as usize])
                    .map(|(value, flags)| (u32::from(value), flags));
                let (value, flags) = self.fp_result(i.op, result)?;
                self.set_boolean(i.r, value != 0);
                self.fp_flags(flags);
            }
            ConstS => {
                // p. 386 says "double" but the mnemonic, Table 45, and
                // 32-bit values pp. 386–387 all specify single precision.
                const VALUES: [u32; 4] = [0, 0x3f80_0000, 0x4000_0000, 0x3f00_0000];
                if i.s >= 4 {
                    return Err(Stop::Unmodeled { pc: self.pc, what: "CONST.S: reserved constant index" });
                }
                self.fr[r] = VALUES[i.s as usize];
            }
            AddexpS | AddexpmS => {
                // Exact bit-field assignments pp. 330, 332: exponent
                // arithmetic truncates to its eight-bit destination, even
                // for exceptional bit patterns; no IEEE flags are raised.
                let source = self.fr[i.s as usize] << if i.op == AddexpmS { 9 } else { 0 };
                let exponent = ((self.fr[r] >> 23) & 255).wrapping_add((source >> 23) & 255).wrapping_sub(127) & 255;
                self.fr[r] = ((self.fr[r] ^ source) & 0x8000_0000)
                    | (exponent << 23) | (self.fr[r] & 0x007f_ffff);
            }
            Nexp01S => {
                let source = self.fr[i.s as usize];
                let magnitude = source & 0x7fff_ffff;
                let sign = !source & 0x8000_0000;
                self.fr[r] = if magnitude >= 0x7f80_0000 {
                    sign | 0x3f80_0000 | (source & 0x007f_ffff)
                } else if magnitude == 0 { sign | 0x4000_0000 }
                else if magnitude < 0x3f80_0000 {
                    // Normal powers 4^-n are unambiguous: LOG2/2 is an
                    // integer and both formula and range rule yield one.
                    if magnitude >= 0x0080_0000 && magnitude & 0x007f_ffff == 0
                        && (magnitude >> 23) & 1 != 0
                    {
                        sign | 0x3f80_0000
                    } else {
                        // p. 543's TRUNC(LOG2(...)/2) conflicts with the prose
                        // 1 <= magnitude < 4 for negative non-even logarithms.
                        // NORMALIZE is not defined either. Preserve the exact
                        // cases above; do not invent this case.
                        return Err(Stop::Unmodeled { pc: self.pc, what: "NEXP01.S: below-one normalization/TRUNC rule is ambiguous" });
                    }
                } else {
                    let parity = (((source >> 23) & 255) - 127) & 1;
                    sign | ((127 + parity) << 23) | (source & 0x007f_ffff)
                };
            }
            Lsi | Lsip | Lsx | Lsxp | Ssi | Ssip | Ssx | Ssxp => {
                let indexed = matches!(i.op, Lsx | Lsxp | Ssx | Ssxp);
                let post = matches!(i.op, Lsip | Lsxp | Ssip | Ssxp);
                let offset = if indexed { t } else { i.imm as u32 };
                let addr = if post { s } else { s.wrapping_add(offset) };
                let fr = if indexed { r } else { i.t as usize };
                if self.config.unaligned == Unaligned::Exception && addr & 3 != 0 {
                    return self.fault(cause::LOAD_STORE_ALIGNMENT, Some(addr)).map(|()| false);
                }
                let aligned = if self.config.unaligned == Unaligned::Hardware { addr } else { addr & !3 };
                if matches!(i.op, Ssi | Ssip | Ssx | Ssxp) {
                    if bus.write(aligned, Width::W, self.fr[fr], self.instret).is_err() {
                        return self.fault(cause::LOAD_STORE_ERROR, Some(addr)).map(|()| false);
                    }
                } else {
                    let value = match bus.read(aligned, Width::W, Kind::Load, self.instret) {
                        Ok(value) => value,
                        Err(()) => return self.fault(cause::LOAD_STORE_ERROR, Some(addr)).map(|()| false),
                    };
                    self.fr[fr] = value;
                }
                // LSIP's AS[s] p. 481 means AR[s], as the description and
                // LSXP p. 484 agree. Increment only after successful access.
                if post { self.ar[i.s as usize] = s.wrapping_add(offset); }
            }
            Ill | IllN => return self.fault(cause::ILLEGAL, None).map(|()| false),
            Syscall => return self.fault(cause::SYSCALL, None).map(|()| false),
            Break | BreakN => return Err(Stop::Break { pc: self.pc }),
            Add | AddN => self.ar[r] = s.wrapping_add(t),
            Addi | Addmi => self.ar[i.t as usize] = s.wrapping_add(i.imm as u32),
            AddiN => self.ar[r] = s.wrapping_add(i.imm as u32),
            Addx2 => self.ar[r] = (s << 1).wrapping_add(t),
            Addx4 => self.ar[r] = (s << 2).wrapping_add(t),
            Addx8 => self.ar[r] = (s << 3).wrapping_add(t),
            Sub => self.ar[r] = s.wrapping_sub(t),
            Subx2 => self.ar[r] = (s << 1).wrapping_sub(t),
            Subx4 => self.ar[r] = (s << 2).wrapping_sub(t),
            Subx8 => self.ar[r] = (s << 3).wrapping_sub(t),
            Neg => self.ar[r] = t.wrapping_neg(),
            Abs => self.ar[r] = if (t as i32) < 0 { t.wrapping_neg() } else { t },
            Salt => self.ar[r] = u32::from((s as i32) < (t as i32)),
            Saltu => self.ar[r] = u32::from(s < t),
            // ISA Summary pp. 489–492: compare signed or zero-extended
            // operands, then copy the selected operand's complete bit pattern.
            Min => self.ar[r] = (s as i32).min(t as i32) as u32,
            Max => self.ar[r] = (s as i32).max(t as i32) as u32,
            Minu => self.ar[r] = s.min(t),
            Maxu => self.ar[r] = s.max(t),
            And => self.ar[r] = s & t,
            Or => self.ar[r] = s | t,
            Xor => self.ar[r] = s ^ t,
            Movi => self.ar[i.t as usize] = i.imm as u32,
            MoviN => self.ar[i.s as usize] = i.imm as u32,
            MovN => self.ar[i.t as usize] = s,
            Moveqz | Movnez | Movltz | Movgez => {
                let take = match i.op {
                    Moveqz => t == 0,
                    Movnez => t != 0,
                    Movltz => (t as i32) < 0,
                    _ => (t as i32) >= 0,
                };
                if take { self.ar[r] = s; }
            }
            Extui => {
                let destination = ((i.bits >> 12) & 15) as usize;
                self.ar[destination] = (t >> i.imm) & ((1u32 << (i.r + 1)) - 1);
            }
            Ssl => self.sar = 32 - (s & 31),
            Ssr => self.sar = s & 31,
            Ssa8l => self.sar = (s & 3) * 8,
            Ssa8b => self.sar = 32 - (s & 3) * 8,
            Ssai => self.sar = i.imm as u32,
            Sll | Srl | Sra | Src => {
                let sa = self.sar & 63;
                // SAR>32 is undefined for these instructions (pp. 610,
                // 613, 615). Stop explicitly rather than invent a result.
                if sa > 32 {
                    return Err(Stop::Unmodeled { pc: self.pc, what: "undefined shift: SAR > 32" });
                }
                self.ar[r] = match i.op {
                    Sll => (((s as u64) << 32) >> sa) as u32,
                    Srl => ((t as u64) >> sa) as u32,
                    Sra => ((t as i32 as i64) >> sa) as u32,
                    _ => ((((s as u64) << 32) | t as u64) >> sa) as u32,
                };
            }
            Slli => self.ar[r] = (((s as u64) << 32) >> i.imm) as u32,
            Srli => self.ar[r] = t >> i.imm,
            Srai => self.ar[r] = ((t as i32) >> i.imm) as u32,
            Mull => self.ar[r] = s.wrapping_mul(t),
            Muluh => self.ar[r] = (((s as u64) * (t as u64)) >> 32) as u32,
            Mulsh => self.ar[r] = (((s as i32 as i64) * (t as i32 as i64)) >> 32) as u32,
            Quos | Quou | Rems | Remu => {
                if t == 0 { return self.fault(cause::DIVIDE_BY_ZERO, None).map(|()| false); }
                // Wider signed division preserves truncation toward zero and
                // gives the specified low 32 bits for MIN/-1 (pp. 557, 562).
                self.ar[r] = match i.op {
                    Quos => ((s as i32 as i64) / (t as i32 as i64)) as u32,
                    Quou => s / t,
                    Rems => ((s as i32 as i64) % (t as i32 as i64)) as u32,
                    _ => s % t,
                };
            }
            L8ui | L16ui | L16si | L32i | L32iN | L32r | L32e
            | S8i | S16i | S32i | S32iN | S32e => {
                // L32E/S32E, pp. 456, 591–592: without an MMU these
                // use the ordinary load/store path with negative offsets.
                let addr = if i.op == L32r {
                    target(&i, self.pc).unwrap()
                } else {
                    s.wrapping_add(i.imm as u32)
                };
                let (width, mask) = match i.op {
                    L8ui | S8i => (Width::B, 0),
                    L16ui | L16si | S16i => (Width::H, 1),
                    _ => (Width::W, 3),
                };
                if self.config.unaligned == Unaligned::Exception && addr & mask != 0 {
                    return self.fault(cause::LOAD_STORE_ALIGNMENT, Some(addr)).map(|()| false);
                }
                // Without the option, use the aligned item which contains the address (loads
                // pp. 56–57, stores pp. 59–60); hardware handling uses the address as it is.
                let aligned = if self.config.unaligned == Unaligned::Hardware { addr } else { addr & !mask };
                if matches!(i.op, S8i | S16i | S32i | S32iN | S32e) {
                    let value = match i.op { S8i => t & 255, S16i => t & 65535, _ => t };
                    if bus.write(aligned, width, value, self.instret).is_err() {
                        return self.fault(cause::LOAD_STORE_ERROR, Some(addr)).map(|()| false);
                    }
                } else {
                    let value = match bus.read(aligned, width, Kind::Load, self.instret) {
                        Ok(value) => value,
                        Err(()) => return self.fault(cause::LOAD_STORE_ERROR, Some(addr)).map(|()| false),
                    };
                    self.ar[i.t as usize] = match i.op {
                        L8ui => value & 255,
                        L16ui => value & 65535,
                        L16si => value as u16 as i16 as i32 as u32,
                        _ => value,
                    };
                }
            }
            J => next = target(&i, self.pc).unwrap(),
            Jx => next = s,
            Ret | RetN => next = self.ar[0],
            Call0 => { self.ar[0] = next; next = target(&i, self.pc).unwrap(); }
            Callx0 => { self.ar[0] = next; next = s; }
            Call4 | Call8 | Call12 | Callx4 | Callx8 | Callx12 => {
                let n = match i.op { Call4 | Callx4 => 1, Call8 | Callx8 => 2, _ => 3 };
                // §6.1.4 p. 247 and pp. 374–382. The target is read before
                // a(4n) is written, as §6.1.4 and CALLX4/12's explicit
                // temporary say; CALLX8's own operation omits the temporary.
                // Compiled windowed code loads a long-call target into a8 and
                // then runs CALLX8 a8, so §6.1.4's order is the one hardware
                // runs (`s` was read before any write).
                self.ps = (self.ps & !CALLINC) | (n << 16);
                self.ar[(n * 4) as usize] = (n << 30) | (next & 0x3fff_ffff);
                next = if matches!(i.op, Call4 | Call8 | Call12) {
                    target(&i, self.pc).unwrap()
                } else { s };
            }
            Entry => {
                // pp. 426–427: check precedes the undefined-state test;
                // source is in the old window, destination in the new one.
                if i.s > 3 || self.ps & (WOE | EXCM) != WOE {
                    return Err(Stop::Unmodeled {
                        pc: self.pc, what: "ENTRY: operand > a3, WOE clear, or EXCM set is undefined",
                    });
                }
                let n = (self.ps >> 16) & 3;
                self.rotate_window(self.window_base.wrapping_add(n));
                self.ar[i.s as usize] = s.wrapping_sub(i.imm as u32);
                self.window_start |= 1 << self.window_base;
            }
            Retw | RetwN => {
                // pp. 247, 566–568: the nearest preceding live window,
                // if any, must match the increment encoded in a0.
                let n = self.ar[0] >> 30;
                let m = (1..=3).find(|d| self.window_live(self.window_base.wrapping_sub(*d)));
                if n == 0 || m.is_some_and(|m| m != n) || self.ps & (WOE | EXCM) != WOE {
                    return Err(Stop::Unmodeled { pc: self.pc, what: if i.op == Retw {
                        "RETW: zero/mismatched increment, WOE clear, or EXCM set is undefined"
                    } else { "RETW.N: zero/mismatched increment, WOE clear, or EXCM set is undefined" } });
                }
                let base = self.window_base.wrapping_sub(n);
                if !self.window_live(base) {
                    self.enter_window(base, (n * 2 - 1) as usize);
                    return Ok(false);
                }
                next = (self.pc & 0xc000_0000) | (self.ar[0] & 0x3fff_ffff);
                self.window_start &= !(1 << (self.window_base & 15));
                self.rotate_window(base);
                // p. 567 permits an implementation to update CALLINC;
                // choose the unchanged variant specified on pp. 247, 568.
            }
            Movsp => {
                // p. 514: AllocaCause depends on the preceding three WS
                // bits, independently of CWOE. Only overflow is WOE-gated.
                if !(1..=3).any(|d| self.window_live(self.window_base.wrapping_sub(d))) {
                    return self.fault(cause::ALLOCA, None).map(|()| false);
                }
                self.ar[i.t as usize] = s;
            }
            // pp. 578–579: signed imm4, modulo the four-bit WindowBase.
            Rotw => self.rotate_window(self.window_base.wrapping_add(i.imm as u32)),
            Rfwo | Rfwu => {
                // pp. 575–576: change the handler window's bit BEFORE
                // restoring OWB. UM and all other PS fields are preserved.
                let bit = 1 << (self.window_base & 15);
                if i.op == Rfwo { self.window_start &= !bit; }
                else { self.window_start |= bit; }
                self.rotate_window((self.ps >> 8) & 15);
                self.ps &= !EXCM;
                next = self.epc1;
            }
            Ball | Bany | Bnall | Bnone | Bbc | Bbs | Bbci | Bbsi
            | Beq | Bne | Bge | Blt | Bgeu | Bltu | Beqi | Bnei | Bgei | Blti
            | Bgeui | Bltui | Beqz | Bnez | Bgez | Bltz | BeqzN | BnezN => {
                let take = match i.op {
                    Ball => !s & t == 0,
                    Bnall => !s & t != 0,
                    Bany => s & t != 0,
                    Bnone => s & t == 0,
                    Bbc => s & (1 << (t & 31)) == 0,
                    Bbs => s & (1 << (t & 31)) != 0,
                    Bbci => s & (1 << (((i.r & 1) << 4) | i.t)) == 0,
                    Bbsi => s & (1 << (((i.r & 1) << 4) | i.t)) != 0,
                    Beq => s == t,
                    Bne => s != t,
                    Bge => (s as i32) >= (t as i32),
                    Blt => (s as i32) < (t as i32),
                    Bgeu => s >= t,
                    Bltu => s < t,
                    Beqi => s == i.imm as u32,
                    Bnei => s != i.imm as u32,
                    Bgei => (s as i32) >= i.imm,
                    Blti => (s as i32) < i.imm,
                    Bgeui => s >= i.imm as u32,
                    Bltui => s < i.imm as u32,
                    Beqz | BeqzN => s == 0,
                    Bnez | BnezN => s != 0,
                    Bgez => (s as i32) >= 0,
                    Bltz => (s as i32) < 0,
                    _ => unreachable!(),
                };
                if take { next = target(&i, self.pc).unwrap(); }
            }
            Rsr => self.ar[i.t as usize] = self.read_sr(i.imm),
            RurThreadptr => self.ar[r] = self.threadptr,
            WurThreadptr => self.threadptr = t,
            Wsr => self.write_sr(i.imm, t),
            Xsr => {
                let old = self.read_sr(i.imm);
                self.ar[i.t as usize] = old;
                // p. 278: both reads precede both writes, and the SR's
                // behavioral effect follows completion of the exchange.
                // XSR.WINDOWBASE writes the AR in the OLD window.
                self.write_sr(i.imm, t);
            }
            Rsil => {
                self.ar[i.t as usize] = self.ps;
                self.ps = (self.ps & !15) | i.imm as u32;
            }
            Waiti => {
                self.ps = (self.ps & !15) | i.imm as u32;
                self.waiting = true;
            }
            Rfe => { self.ps &= !EXCM; next = self.epc1; }
            Isync | Rsync | Esync | Dsync | Fsync | Memw | Extw | Excw | Nop | NopN => {}
            Rfde | Rfdo | Rfdd | Rfi
            | Div0S | DivnS | Recip0S | Rsqrt0S | Sqrt0S | MkdadjS | MksadjS =>
                unreachable!("unmodeled instructions stop before execution"),
        }
        self.pc = next;
        self.instret = self.instret.wrapping_add(1);
        Ok(true)
    }

    /// Stop at the first stop reason, or after exactly `max` retirements.
    /// Interrupt and exception entry do not consume the retirement budget.
    pub fn run(&mut self, bus: &mut impl Bus, max: u64) -> Stop {
        let start = self.instret;
        while self.instret.wrapping_sub(start) < max {
            if let Err(stop) = self.step(bus) { return stop; }
        }
        Stop::Limit
    }

    /// The decoded form of the instruction `bits` for a fast lane: `step`'s decode, with every
    /// gate that depends on the instruction and the configuration alone folded into its class.
    /// `None`: the encoding is illegal (`step` takes the fault).
    pub fn decode_lane(&self, bits: u32) -> Option<Decoded> {
        use Op::*;
        let (i, length) = decode(bits).ok()?;
        // Run by the reference step: configuration-gated and unmodelled instructions (their
        // fault or stop), special registers (CCOUNT, INTERRUPT, PS and the window registers are
        // read or written there, so a lane hands over with its cycles counted first), WAITI's
        // sleep, and the instructions that only trap.
        let slow = self.gated(&i) || unmodeled(i.op).is_some()
            || matches!(i.op, Rsr | Wsr | Xsr | Waiti | Ill | IllN | Syscall | Break | BreakN);
        let mut class = if slow { lane::SLOW } else { 0 };
        if fp::is_fp(i.op) { class |= lane::FP; }
        // PS writes change the interrupt level and the window gates: the lane ends after them.
        if matches!(i.op, Rsil | Rfe | Rfwo | Rfwu) { class |= lane::ENDS; }
        // ENTRY's window group depends on PS.CALLINC, so a lane computes it when it runs.
        let group = if i.op == Entry { class |= lane::ENTRY; 0 } else { window_group(&i, 0) };
        Some(Decoded { insn: i, len: length as u8, class, group })
    }

    /// Whether the window gate is on (PS.WOE set, PS.EXCM clear): `execute_decoded` then checks
    /// each instruction's window group before it runs.
    pub fn windows_checked(&self) -> bool {
        self.ps & (WOE | EXCM) == WOE
    }

    /// Whether a fast lane may start here: what `step` checks before fetching is all clear (no
    /// pending interrupt, not asleep) and no zero-overhead loop is armed, so `step`'s loop-end
    /// test cannot fire while LCOUNT stays zero (only WSR, a reference instruction, sets it).
    pub fn lane_ready(&self) -> bool {
        // `pending_level` only looks at requested, enabled lines.
        !self.waiting && self.lcount == 0 && (self.interrupt & self.intenable == 0 || self.pending_level() == 0)
    }

    /// Retirements until the first comparator match, enabled or not (a masked match still
    /// latches INTERRUPT): a lane retires at most this many before it counts its cycles in. A
    /// comparator equal to CCOUNT next matches after a full 2^32 (`advance`).
    pub fn timer_budget(&self) -> u64 {
        self.timer_lines[..self.timer_len].iter().map(|&(_, timer)| {
            let d = self.ccompare[timer as usize].wrapping_sub(self.ccount);
            if d == 0 { 1u64 << 32 } else { u64::from(d) }
        }).min().unwrap_or(u64::MAX)
    }

    /// One decoded instruction in a fast lane, after everything `step` checks before decoding
    /// was clear and with a class that is not `lane::SLOW`: `step`'s dynamic gates (the window
    /// check, the coprocessor enable), then `execute`. Ok(false): not retired, a vector was
    /// entered. CCOUNT is not advanced (`execute`).
    #[inline]
    pub fn execute_decoded(&mut self, d: &Decoded, bus: &mut impl Bus) -> Result<bool, Stop> {
        if self.ps & (WOE | EXCM) == WOE {
            let group = if d.class & lane::ENTRY != 0 { window_group(&d.insn, self.ps) } else { d.group };
            if self.window_gate(&d.insn, group)? { return Ok(false); }
        }
        if d.class & lane::FP != 0 && self.cpenable & 1 == 0 {
            return self.fault(cause::COPROCESSOR0_DISABLED, None).map(|()| false);
        }
        self.execute(&d.insn, u32::from(d.len), bus)
    }

    /// The first architectural field in which two CPUs of one configuration differ; None when
    /// they are the same. The 64 physical registers compare as `physical_ar` synchronizes them.
    pub fn same_state(&self, other: &Cpu) -> Option<&'static str> {
        // Exhaustive so new state cannot silently escape the differential proof.
        let Cpu {
            ar: _, br, fr, fcr, fsr, fsr_underflow_unknown, pc, sar, lbeg, lend, lcount, acclo, acchi, m,
            scompare1, threadptr, window_base, window_start, ps, vecbase, epc1, excsave1, exccause,
            excvaddr, cpenable, interrupt, intenable, ccount, ccompare, instret, traps,
            config: _, physical: _, lines, waiting, timer_lines: _, timer_len: _, level_lines: _, edge_lines: _,
        } = self;
        let o = other;
        if self.physical_ar() != o.physical_ar() { return Some("cpu.ar"); }
        if *pc != o.pc { return Some("cpu.pc"); }
        if *instret != o.instret { return Some("cpu.instret"); }
        if *ccount != o.ccount { return Some("cpu.ccount"); }
        if *ccompare != o.ccompare { return Some("cpu.ccompare"); }
        if *interrupt != o.interrupt { return Some("cpu.interrupt"); }
        if *intenable != o.intenable { return Some("cpu.intenable"); }
        if *lines != o.lines { return Some("cpu.lines"); }
        if *waiting != o.waiting { return Some("cpu.waiting"); }
        if *ps != o.ps { return Some("cpu.ps"); }
        if (*window_base & 15) != (o.window_base & 15) { return Some("cpu.windowbase"); }
        if *window_start != o.window_start { return Some("cpu.windowstart"); }
        if *sar != o.sar { return Some("cpu.sar"); }
        if *br != o.br { return Some("cpu.br"); }
        if *fr != o.fr { return Some("cpu.fr"); }
        if (*fcr, *fsr, *fsr_underflow_unknown) != (o.fcr, o.fsr, o.fsr_underflow_unknown) { return Some("cpu.fcr/fsr"); }
        if (*lbeg, *lend, *lcount) != (o.lbeg, o.lend, o.lcount) { return Some("cpu.loop"); }
        if (*acclo, *acchi, *m, *scompare1, *threadptr) != (o.acclo, o.acchi, o.m, o.scompare1, o.threadptr) {
            return Some("cpu.mac16/scompare1/threadptr");
        }
        if (*vecbase, *epc1, *excsave1, *exccause, *excvaddr) != (o.vecbase, o.epc1, o.excsave1, o.exccause, o.excvaddr) {
            return Some("cpu.exception registers");
        }
        if *cpenable != o.cpenable { return Some("cpu.cpenable"); }
        if *traps != o.traps { return Some("cpu.traps"); }
        None
    }
}

/// Class bits of a decoded instruction for a fast lane (`Cpu::decode_lane`).
pub mod lane {
    /// The reference `step` runs it: a lane hands over before it.
    pub const SLOW: u8 = 1;
    /// A floating-point instruction: CPENABLE bit 0 gates it.
    pub const FP: u8 = 2;
    /// It writes PS: a lane ends after it.
    pub const ENDS: u8 = 4;
    /// ENTRY: its window group is PS.CALLINC's.
    pub const ENTRY: u8 = 8;
}

/// A decoded instruction with its lane class and static window group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub insn: Insn,
    pub len: u8,
    pub class: u8,
    pub group: u8,
}

/// §6.1.3 pp. 245–246: only actual AR operands contribute to WindowCheck;
/// opcode/immediate fields, BRs and FRs do not. Taking the maximum group is
/// equivalent to the three wr/ws/wt predicates in the document.
fn window_group(i: &Insn, ps: u32) -> u8 {
    use Op::*;
    let (r, s, t) = (i.r, i.s, i.t);
    let max = match i.op {
        Call4 | Callx4 => return 1,
        Call8 | Callx8 => return 2,
        Call12 | Callx12 => return 3,
        Entry => return ((ps >> 16) & 3) as u8,
        Add | AddN | Addx2 | Addx4 | Addx8 | Sub | Subx2 | Subx4 | Subx8
        | Salt | Saltu | Min | Max | Minu | Maxu | And | Or | Xor | Src | Mull | Muluh | Mulsh
        | Quos | Quou | Rems | Remu | Moveqz | Movnez | Movltz | Movgez => r.max(s).max(t),
        AddiN | Sll | Slli | Movf | Movt => r.max(s),
        Neg | Abs | Srl | Sra | Srai | Srli => r.max(t),
        Extui => (((i.bits >> 12) & 15) as u8).max(t),
        Addi | Addmi | MovN | Movsp | L8ui | L16ui | L16si | L32i | L32iN
        | S8i | S16i | S32i | S32iN | L32e | S32e
        | Ball | Bany | Bnall | Bnone | Bbc | Bbs | Beq | Bne | Bge | Blt | Bgeu | Bltu
        | Lsx | Lsxp | Ssx | Ssxp => s.max(t),
        Beqi | Bnei | Bgei | Blti | Bgeui | Bltui | Beqz | Bnez | Bgez | Bltz
        | BeqzN | BnezN | Bbci | Bbsi | Ssl | Ssr | Ssa8l | Ssa8b | MoviN
        | Jx | Callx0 | FloatS | UfloatS | Wfr | Lsi | Lsip | Ssi | Ssip => s,
        Movi | L32r | Rsr | Wsr | Xsr | Rsil | WurFcr | WurFsr | WurThreadptr
        | MoveqzS | MovnezS | MovltzS | MovgezS => t,
        Rfr | RurFcr | RurFsr | RurThreadptr | CeilS | FloorS | RoundS | TruncS | UtruncS => r,
        // Remaining instructions reference no ARs, or only a0 (RET/CALL0).
        // Unmodeled FP operations and unsupported returns have no other ARs.
        _ => 0,
    };
    max / 4
}

fn unmodeled(op: Op) -> Option<&'static str> {
    use Op::*;
    Some(match op {
        Div0S => "DIV0.S: approximation not specified bit-exactly", // pp. 406–407
        Recip0S => "RECIP0.S: approximation not specified bit-exactly", // pp. 560–561
        Rsqrt0S => "RSQRT0.S: approximation not specified bit-exactly", // p. 584
        Sqrt0S => "SQRT0.S: approximation not specified bit-exactly", // pp. 612–613
        DivnS => "DIVN.S: special exponent interpretation not specified bit-exactly", // pp. 110–113, 408
        MkdadjS => "MKDADJ.S: divide_adjust not specified bit-exactly", // pp. 111, 493
        MksadjS => "MKSADJ.S: square_root_adjust not specified bit-exactly", // pp. 112, 495
        Rfde => "RFDE",
        Rfdo => "RFDO",
        Rfdd => "RFDD",
        Rfi => "RFI",
        _ => return None,
    })
}
