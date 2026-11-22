//! The bus ignores `now`: the CPU supplies retired instructions, not S3 ticks.
//! A latched access stop outranks the CPU's resulting exception. Exception
//! entry state after an Unmodeled bus halt is not meaningful; rewind restores it.

use std::sync::Arc;

use fab_elec_mcu_emu::bus::{Bus, Kind, Width};

use crate::periph::Periph;
use crate::{CpuClock, Junk, ResetScope, S3Profile};

// facts.md §2; TRM pp.403–404, Table 4.3-1. The aliases share one store.
pub(crate) const SRAM_DATA: u32 = 0x3fc8_8000;
pub(crate) const SRAM_INSN: u32 = 0x4037_8000;
pub(crate) const SRAM_LEN: usize = 416 * 1024;
// facts.md §2, Table 4.3-2: the flash instruction and data windows, 32 MiB each.
pub(crate) const FLASH_INSN: u32 = 0x4200_0000;
const FLASH_DATA: u32 = 0x3c00_0000;
const FLASH_WINDOW: usize = 0x0200_0000;
// facts-machine.md §3; IDF soc.h:24, ext_mem_defs.h:70.
const MMU_BASE: u32 = 0x600c_5000;
const MMU_LEN: usize = 512;
// facts.md §3; TRM p.409 Table 4.3-3, pp.830,840 Registers 17.3,17.20.
const CPU_PER: u32 = 0x600c_0000 + 0x10;
const SYSCLK: u32 = 0x600c_0000 + 0x60;
// ESP-IDF v6.1.1, Apache-2.0, system_reg.h:15–41; base: facts.md:111.
const CORE1_CONTROL: [u32; 2] = [0x600c_0000, 0x600c_0004];
const CORE1_RESETING: u32 = 1 << 2;
const CORE1_CLKGATE_EN: u32 = 1 << 1;
const CORE1_RUNSTALL: u32 = 1;
const CORE1_RESET: [u32; 2] = [CORE1_RESETING, 0];
// facts-machine.md §3; IDF reg_base.h:62, extmem_reg.h:1366–1380.
const CACHE_STATE: u32 = 0x600c_4000 + 0x130;
// facts.md §4: SYSTEM_PERIP_CLK_EN0/1 and SYSTEM_PERIP_RST_EN0/1 (TRM pp.825–826,
// 831–834; IDF system_reg.h:105,299,367,561), in that order.
const PERIP: [u32; 4] = [0x600c_0018, 0x600c_001c, 0x600c_0020, 0x600c_0024];
// Gate and reset bits of the modelled blocks, per PERIP register: SYSTIMER 29,
// TIMERGROUP 13, TIMERGROUP1 15 (EN0/RST_EN0; system_reg.h:118,202,214,380,464,
// 476); USB_DEVICE 10 (EN1/RST_EN1; system_reg.h:300,562). M: the ROM leaves
// them clocked and out of reset; a change is not modelled.
const GATED: [u32; 4] = [1 << 29 | 1 << 13 | 1 << 15, 1 << 10, 1 << 29 | 1 << 13 | 1 << 15, 1 << 10];
const GATED_ON: [u32; 4] = [GATED[0], GATED[1], 0, 0];
// facts-io.md I20: these gates go to the controllers, whose ROM state is
// unknown until firmware holds reset and releases it with the clock enabled.
const I2C_GATES: u32 = (1 << 7) | (1 << 18);
// facts-io.md S17/G12, including the lead's separate SPI2_DMA requirement.
// I2S0 CLK_EN0/RST_EN0 bit 4 (trm.txt:28820–28854, 28993–29060) survives CPU reset (trm.txt:18027).
const IO_GATES: [u32; 4] = [I2C_GATES | (1 << 4) | (1 << 6) | (1 << 22), 1 << 6,
    I2C_GATES | (1 << 4) | (1 << 6) | (1 << 22), 1 << 6];
// TRM p.408–409 Table 4.3-3: the peripheral register space.
const PERIPHERALS: std::ops::RangeInclusive<u32> = 0x6000_0000..=0x600f_ffff;

/// Details accompanying an Unmodeled bus halt. `addr` is the requested
/// address; `mmu_entry` identifies the failing translation, even across pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StopDetail {
    pub addr: u32,
    pub width: Width,
    pub kind: Kind,
    pub mmu_entry: Option<u16>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct AccessStop {
    pub what: &'static str,
    pub detail: StopDetail,
}

#[derive(Clone, PartialEq)]
pub(crate) struct Registers {
    /// Shared with marks and clones until written: firmware maps its flash at boot.
    mmu: Arc<Mmu>,
    sysclk: u32,
    cpu_per: u32,
    perip: [u32; 4],
    core1: [u32; 2],
    pub stop: Option<AccessStop>,
}

/// The flash MMU's entries, and which were written since reset.
#[derive(Clone, PartialEq, Hash)]
pub(crate) struct Mmu {
    entry: [u32; MMU_LEN],
    known: [bool; MMU_LEN],
}

impl Registers {
    /// The ROM handoff state: junk except what the profile fixes.
    fn handoff(profile: &S3Profile, junk: &mut Junk) -> Self {
        // The junk words in the order the reference draws them: the clock registers first.
        let (sysclk, cpu_per) = (junk.u32(), junk.u32());
        let mut mmu = Mmu { entry: [0; MMU_LEN], known: [false; MMU_LEN] };
        for (n, entry) in mmu.entry.iter_mut().enumerate() {
            *entry = junk.u32();
            if n < profile.handoff_mapped_pages as usize {
                // M: emcu-s3.md:28–30. Valid flash target is bits 14/15 clear
                // (facts-machine.md §3; ext_mem_defs.h:46–63).
                *entry = n as u32;
                mmu.known[n] = true;
            }
        }
        let mut regs = Registers {
            mmu: Arc::new(mmu),
            sysclk, cpu_per, perip: [0; 4], stop: None,
            core1: CORE1_RESET,
        };
        // Only fields specifying the handoff clock replace the junk. These
        // are M handoff choices, not claims about ROM's final register writes.
        match profile.handoff_cpu {
            CpuClock::Xtal { div } => {
                assert!((1..=1024).contains(&div), "XTAL divisor must be 1..=1024");
                regs.sysclk = (regs.sysclk & !0xfff) | (div - 1);
            }
            CpuClock::Pll480 { period } => {
                assert!(period <= 2, "480 MHz PLL period must be 0..=2");
                regs.sysclk = (regs.sysclk & !(3 << 10)) | (1 << 10);
                regs.cpu_per = (regs.cpu_per & !7) | 4 | period;
            }
            CpuClock::Pll320 { period } => {
                assert!(period <= 1, "320 MHz PLL period must be 0..=1");
                regs.sysclk = (regs.sysclk & !(3 << 10)) | (1 << 10);
                regs.cpu_per = (regs.cpu_per & !7) | period;
            }
        }
        for n in 0..4 {
            regs.perip[n] = (junk.u32() & !GATED[n]) | GATED_ON[n];
        }
        regs
    }

    pub fn ticks_per_cycle(&self) -> u64 {
        clock_ticks(self.sysclk, self.cpu_per).expect("clock writes are validated atomically")
    }

    pub fn apb_ticks(&self) -> u64 { apb_ticks(self.sysclk, self.ticks_per_cycle()) }

    pub fn gates(&self) -> [u32; 4] { self.perip }

    pub fn pll(&self) -> bool { (self.sysclk >> 10) & 3 == 1 }

    /// Whether both map the flash windows the same way: decoded code stays valid across a
    /// rewind between them.
    pub fn same_mmu(&self, other: &Registers) -> bool { Arc::ptr_eq(&self.mmu, &other.mmu) || self.mmu == other.mmu }

    /// The MMU table, for keys of code built from what it maps.
    #[cfg(feature = "jit")]
    pub(crate) fn mmu(&self) -> &Arc<Mmu> { &self.mmu }
}

// esp-hal (soc/esp32s3/clocks.rs, ensure_voltage_raised) raises the core regulator before a
// 240 MHz CPU and only sets LDO slaves below it; no document gives the voltage at ROM handoff.
const PLL_REGULATOR: &str = "CPU on the PLL: Assumed the core regulator voltage at ROM handoff carries it; the regulator and LDO slaves are not modelled";
// IDF esp_hw_support/port/esp32s3/rtc_clk.c:233-240 writes EXT_RTC_DREG and EXT_DIG_DREG before the CPU goes to
// 240 MHz; nothing documents the chip at 240 MHz on the ROM's regulator setting.
const UNRAISED_240: &str = "CPU at 240 MHz before both core regulators were written is not modelled";

// facts.md §3: APB is 80 MHz (6 ticks) when the CPU runs from the PLL, and
// equals the CPU clock from XTAL (TRM p.530 Table 7.2-2, p.533 Table 7.2-4).
fn apb_ticks(sysclk: u32, cpu_ticks: u64) -> u64 {
    if (sysclk >> 10) & 3 == 1 { 6 } else { cpu_ticks }
}

// facts.md §3 CPU clock selection: TRM p.530 Table 7.2-2; pp.830,840.
// SOC_CLK_SEL[11:10], PRE_DIV_CNT[9:0], PLL_FREQ_SEL[2], CPUPERIOD_SEL[1:0].
// All ratios below are on the machine's 480 MHz grid; no PLL lock timing.
fn clock_ticks(sysclk: u32, cpu_per: u32) -> Option<u64> {
    match (sysclk >> 10) & 3 {
        0 => Some(12 * u64::from((sysclk & 0x3ff) + 1)),
        1 => match (cpu_per & 4 != 0, cpu_per & 3) {
            (_, 0) => Some(6),
            (_, 1) => Some(3),
            (true, 2) => Some(2),
            _ => None,
        },
        _ => None,
    }
}

/// SRAM undo by page: the first store to a 4 KiB page since the mark copies the page aside, and
/// a rewind copies the saved pages back. Its cost is bounded by the memory a run writes, never by
/// a count of stores, so a store never ends a run. One flags byte per page carries `SAVED`.
#[derive(Clone, PartialEq)]
pub(crate) struct Journal {
    flags: Vec<u8>,
    /// Pages saved since the mark, in the order saved, and their contents at the mark.
    saved: Vec<u16>,
    copies: Vec<u8>,
}

/// The undo's SRAM page, and its flag.
pub(crate) const PAGE: usize = 4096;
/// A journal flag: the page's content at the mark is saved.
pub(crate) const SAVED: u8 = 1;

impl Journal {
    fn new() -> Self { Journal { flags: vec![0; SRAM_LEN / PAGE], saved: Vec::new(), copies: Vec::new() } }

    /// The page flags, one byte per SRAM page (`SAVED`), for code that tests them before a store.
    #[cfg(feature = "jit")]
    pub(crate) fn flags(&self) -> *const u8 { self.flags.as_ptr() }

    /// Before a write of `len` bytes at SRAM `offset`: saves the pages it touches that are not
    /// saved since the mark.
    #[inline]
    pub fn touch(&mut self, ram: &[u8], offset: usize, len: usize) {
        let (first, last) = (offset / PAGE, (offset + len - 1) / PAGE);
        if self.flags[first] & self.flags[last] & SAVED == 0 { self.save(ram, first, last); }
    }

    #[inline(never)]
    fn save(&mut self, ram: &[u8], first: usize, last: usize) {
        for page in first..=last {
            if self.flags[page] & SAVED == 0 {
                self.flags[page] |= SAVED;
                self.saved.push(page as u16);
                self.copies.extend_from_slice(&ram[page * PAGE..(page + 1) * PAGE]);
            }
        }
    }

    /// The memory as it is becomes the rewind point. The copies keep their room.
    fn mark(&mut self) {
        for &page in &self.saved { self.flags[usize::from(page)] &= !SAVED; }
        self.saved.clear();
        self.copies.clear();
    }

    fn rewind(&mut self, ram: &mut [u8]) {
        for (n, &page) in self.saved.iter().enumerate() {
            let page = usize::from(page);
            ram[page * PAGE..(page + 1) * PAGE].copy_from_slice(&self.copies[n * PAGE..(n + 1) * PAGE]);
        }
        self.mark();
    }

    pub fn dma_word(&mut self, ram: &mut [u8], offset: usize, value: u32) {
        self.touch(ram, offset, 4);
        ram[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}

#[derive(Clone)]
pub(crate) struct S3Bus {
    flash: Arc<Vec<u8>>,
    flash_bytes: usize,
    pub sram: Vec<u8>,
    pub regs: Registers,
    pub periph: Periph,
    /// Machine tick at the start of the instruction now accessing the bus.
    pub tick: u64,
    pub journal: Journal,
    /// Changes whenever code read through the flash windows may decode differently (an MMU
    /// entry written, a handoff): decoded instructions from before it are void.
    pub code_gen: u64,
}

impl S3Bus {
    pub fn new(flash: Arc<Vec<u8>>, profile: &S3Profile, junk: &mut Junk) -> Self {
        assert!(flash.len() <= profile.flash_bytes, "image exceeds board flash");
        assert!(profile.handoff_mapped_pages <= MMU_LEN as u32, "handoff exceeds MMU table");
        let regs = Registers::handoff(profile, junk);
        let mut sram = vec![0; SRAM_LEN];
        for chunk in sram.chunks_mut(8) {
            chunk.copy_from_slice(&junk.word().to_le_bytes()[..chunk.len()]);
        }
        let periph = Periph::new(&profile.time, &profile.pins, profile.mac, profile.pvt, regs.apb_ticks(), regs.gates(), junk);
        Self {
            flash, flash_bytes: profile.flash_bytes, sram, regs, periph, tick: 0,
            journal: Journal::new(),
            code_gen: 0,
        }
    }

    /// The ROM's next handoff after a reset: SRAM and its journal are kept
    /// (TRM pp.526–527: every reset but Chip Reset retains internal memory);
    /// MMU, clock and gate registers return to their handoff state.
    pub fn rehandoff(&mut self, profile: &S3Profile, scope: ResetScope, junk: &mut Junk) {
        let core1 = self.regs.core1;
        let gates = self.regs.gates();
        self.regs = Registers::handoff(profile, junk);
        self.code_gen += 1;
        // INFERRED from reset domains, facts-machine.md:356: CPU0 reset
        // leaves SYSTEM controls intact; Core/System reset the digital block.
        if scope == ResetScope::Cpu {
            self.regs.core1 = core1;
            // CPU reset retains the digital I/O blocks and their SYSTEM gates
            // (facts-machine.md §4); other gates keep the existing handoff policy.
            for (n, old) in gates.into_iter().enumerate() {
                self.regs.perip[n] = (self.regs.perip[n] & !IO_GATES[n]) | (old & IO_GATES[n]);
            }
        }
    }

    pub fn flash(&self) -> &[u8] { &self.flash }

    pub fn mark(&mut self) { self.journal.mark(); }

    pub fn rewind(&mut self) { self.journal.rewind(&mut self.sram); }

    fn stop<T>(&mut self, what: &'static str, addr: u32, width: Width, kind: Kind, mmu_entry: Option<u16>) -> Result<T, ()> {
        if self.regs.stop.is_none() {
            self.regs.stop = Some(AccessStop { what, detail: StopDetail { addr, width, kind, mmu_entry } });
        }
        Err(())
    }

    fn precheck(&mut self, addr: u32, width: Width, kind: Kind) -> Result<(), ()> {
        match addr {
            // facts.md §2; TRM p.403 Table 4.3-1. Policy: Q6 §1.2.
            0x4000_0000..=0x4005_ffff | 0x3ff0_0000..=0x3ff1_ffff =>
                self.stop("ROM access", addr, width, kind, None),
            // facts.md §2; TRM pp.403–405: SRAM0/2 may belong to caches.
            0x4037_0000..=0x4037_7fff | 0x3fcf_0000..=0x3fcf_ffff =>
                self.stop("SRAM0/2 can be cache memory; the cache configuration is not modelled", addr, width, kind, None),
            // facts.md §2; TRM pp.401–402,405. Store stop precedes alignment.
            0x4200_0000..=0x43ff_ffff | 0x3c00_0000..=0x3dff_ffff if kind == Kind::Store =>
                self.stop("store to a flash window", addr, width, kind, None),
            0x4000_0000..=0x4fff_ffff if kind != Kind::Fetch && (width != Width::W || addr & 3 != 0) =>
                self.stop("instruction-bus data access is not an aligned word", addr, width, kind, None),
            _ => Ok(()),
        }
    }

    /// The flash offset a window address maps to through its MMU entry; Err names why the access
    /// stops, with the entry.
    fn translate(&self, vaddr: u32) -> Result<usize, (&'static str, u16)> {
        // facts-machine.md §3; mmu_ll.h:143–146; ext_mem_defs.h:46–68.
        let index = ((vaddr & 0x01ff_ffff) >> 16) as usize;
        let entry = self.regs.mmu.entry[index];
        let why = if !self.regs.mmu.known[index] {
            Some("MMU entry not written since reset")
        } else if entry & (1 << 14) != 0 {
            Some("access through an invalid MMU entry")
        } else if entry & (1 << 15) != 0 {
            Some("PSRAM is not modelled")
        } else { None };
        if let Some(why) = why {
            return Err((why, index as u16));
        }
        let physical = (((entry & 0x3fff) << 16) | (vaddr & 0xffff)) as usize;
        if physical >= self.flash_bytes {
            return Err(("flash address beyond the board's flash", index as u16));
        }
        Ok(physical)
    }

    /// The flash byte a window address reads; Err as `translate`.
    fn flash_byte(&self, vaddr: u32) -> Result<u8, (&'static str, u16)> {
        // Erased bytes beyond the supplied image, up to the board limit.
        self.translate(vaddr).map(|physical| self.flash.get(physical).copied().unwrap_or(0xff))
    }

    fn flash_read(&mut self, addr: u32, width: Width, kind: Kind) -> Result<u32, ()> {
        let bytes = width.bytes();
        // The bytes of an access inside one 64 KiB page share its entry: one translation, and
        // the board's flash must hold the last byte too (as the byte-wise check below finds).
        if (addr & 0xffff) as usize + bytes <= 0x1_0000 {
            let physical = match self.translate(addr) {
                Ok(physical) => physical,
                Err((why, index)) => return self.stop(why, addr, width, kind, Some(index)),
            };
            if physical + bytes > self.flash_bytes {
                let index = ((addr & 0x01ff_ffff) >> 16) as u16;
                return self.stop("flash address beyond the board's flash", addr, width, kind, Some(index));
            }
            return Ok(match self.flash.get(physical..physical + bytes) {
                Some(b) => le(b, width),
                None => (0..bytes).fold(0, |value, byte| {
                    value | u32::from(self.flash.get(physical + byte).copied().unwrap_or(0xff)) << (8 * byte)
                }),
            });
        }
        let mut value = 0;
        for byte in 0..bytes {
            let vaddr = addr + byte as u32; // caller checked the complete window span
            match self.flash_byte(vaddr) {
                Ok(b) => value |= u32::from(b) << (8 * byte),
                Err((why, index)) => return self.stop(why, addr, width, kind, Some(index)),
            }
        }
        Ok(value)
    }

    /// A load from the SRAM data window, as `read` makes it: None for any other access.
    #[inline]
    pub fn sram_load(&self, addr: u32, width: Width) -> Option<u32> {
        let n = offset(addr, SRAM_DATA, SRAM_LEN, width)?;
        Some(le(&self.sram[n..], width))
    }

    /// A store to the SRAM data window, as `write` makes it: None for any other access.
    #[inline]
    pub fn sram_store(&mut self, addr: u32, width: Width, value: u32) -> Option<Result<(), ()>> {
        let n = offset(addr, SRAM_DATA, SRAM_LEN, width)?;
        Some(self.store_at(n, addr, width, value))
    }

    #[inline]
    fn store_at(&mut self, n: usize, addr: u32, width: Width, value: u32) -> Result<(), ()> {
        // Adverse side: which bytes go out when the CPU changes a list or buffer the
        // transfer is reading is undocumented (S05, G03), so the store stops the run.
        if self.periph.dma_in_flight(n, width.bytes()) {
            return self.stop("CPU store into a GDMA descriptor or buffer during its transfer (S05, G03)",
                addr, width, Kind::Store, None);
        }
        self.journal.touch(&self.sram, n, width.bytes());
        match width {
            Width::B => self.sram[n] = value as u8,
            Width::H => self.sram[n..n + 2].copy_from_slice(&(value as u16).to_le_bytes()),
            Width::W => self.sram[n..n + 4].copy_from_slice(&value.to_le_bytes()),
        }
        Ok(())
    }

    /// The byte an instruction fetch from the flash instruction window reads, without effects;
    /// None: the fetch would stop (the reference fetch then latches why).
    pub fn peek_fetch(&self, addr: u32) -> Option<u8> {
        offset(addr, FLASH_INSN, FLASH_WINDOW, Width::B)?;
        self.flash_byte(addr).ok()
    }

    /// The word an aligned load at `addr` in the flash instruction window reads (L32R's
    /// literals), as `read` gives it: None for any other address or where the load would stop.
    /// It stays what `read` gives while the flash image and the MMU table stay.
    #[cfg(feature = "jit")]
    pub(crate) fn peek_literal(&self, addr: u32) -> Option<u32> {
        if addr & 3 != 0 { return None; }
        offset(addr, FLASH_INSN, FLASH_WINDOW, Width::W)?;
        (0..4).try_fold(0, |value, byte| Some(value | u32::from(self.flash_byte(addr + byte).ok()?) << (8 * byte)))
    }

    /// The flash image.
    #[cfg(feature = "jit")]
    pub(crate) fn flash_image(&self) -> &Arc<Vec<u8>> { &self.flash }

    /// Whether an access to `addr` only reads or writes memory (SRAM or a flash window): never a
    /// register, so it changes no peripheral, event or interrupt line.
    pub fn plain(addr: u32) -> bool {
        addr.wrapping_sub(SRAM_DATA) < SRAM_LEN as u32 || addr.wrapping_sub(SRAM_INSN) < SRAM_LEN as u32
            || addr.wrapping_sub(FLASH_INSN) < FLASH_WINDOW as u32 || addr.wrapping_sub(FLASH_DATA) < FLASH_WINDOW as u32
    }

    /// The first part of the bus state that differs from `other`'s. `tick` is scratch for the
    /// instruction now accessing the bus and is not state.
    pub fn same_state(&self, other: &S3Bus) -> Option<&'static str> {
        let S3Bus { flash, flash_bytes, sram, regs, periph, tick: _, journal, code_gen: _ } = self;
        if !Arc::ptr_eq(flash, &other.flash) || *flash_bytes != other.flash_bytes { return Some("bus.flash"); }
        if *sram != other.sram { return Some("bus.sram"); }
        if *regs != other.regs { return Some("bus.regs"); }
        if *journal != other.journal { return Some("bus.journal"); }
        periph.same_state(&other.periph)
    }

    fn register_access(&mut self, addr: u32, width: Width, kind: Kind) -> Result<(), ()> {
        if width != Width::W || addr & 3 != 0 || kind == Kind::Fetch {
            return self.stop("register access is not an aligned word load or store", addr, width, kind, None);
        }
        Ok(())
    }
}

/// The little-endian value of `width` bytes at the start of `b` (fixed-size reads: no copy call).
#[inline]
fn le(b: &[u8], width: Width) -> u32 {
    match width {
        Width::B => u32::from(b[0]),
        Width::H => u32::from(u16::from_le_bytes([b[0], b[1]])),
        Width::W => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
    }
}

fn offset(addr: u32, base: u32, len: usize, width: Width) -> Option<usize> {
    let offset = addr.checked_sub(base)? as usize;
    (offset < len && width.bytes() <= len - offset).then_some(offset)
}

impl Bus for S3Bus {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, _now: u64) -> Result<u32, ()> {
        self.precheck(addr, width, kind)?;
        if let Some(n) = offset(addr, SRAM_INSN, SRAM_LEN, width)
            .or_else(|| if kind == Kind::Load { offset(addr, SRAM_DATA, SRAM_LEN, width) } else { None })
        {
            return Ok(le(&self.sram[n..], width));
        }
        // facts.md §2, Table 4.3-2: instruction and data windows, 32 MiB each.
        if offset(addr, FLASH_INSN, FLASH_WINDOW, width).is_some()
            || (kind == Kind::Load && offset(addr, FLASH_DATA, FLASH_WINDOW, width).is_some())
        {
            return self.flash_read(addr, width, kind);
        }
        // facts-machine.md §3: MMU table ends at base + 512 * 4 - 1.
        if (MMU_BASE..MMU_BASE + (MMU_LEN * 4) as u32).contains(&addr) {
            self.register_access(addr, width, kind)?;
            return Ok(self.regs.mmu.entry[((addr - MMU_BASE) / 4) as usize]);
        }
        if matches!(addr, SYSCLK | CPU_PER | CACHE_STATE) {
            self.register_access(addr, width, kind)?;
            return Ok(match addr {
                SYSCLK => self.regs.sysclk,
                CPU_PER => self.regs.cpu_per,
                // facts-machine.md §3; extmem_reg.h:1366–1380: both idle.
                _ => 0x0000_1001,
            });
        }
        if let Some(n) = PERIP.iter().position(|&a| a == addr) {
            self.register_access(addr, width, kind)?;
            return Ok(self.regs.perip[n]);
        }
        if let Some(n) = CORE1_CONTROL.iter().position(|&a| a == addr) {
            self.register_access(addr, width, kind)?;
            return Ok(self.regs.core1[n]);
        }
        if PERIPHERALS.contains(&addr) {
            self.register_access(addr, width, kind)?;
            match self.periph.read(addr, self.tick) {
                Some(Ok(value)) => return Ok(value),
                Some(Err(what)) => return self.stop(what, addr, width, kind, None),
                None => {}
            }
        }
        self.stop("unmodelled peripheral or address", addr, width, kind, None)
    }

    fn write(&mut self, addr: u32, width: Width, value: u32, _now: u64) -> Result<(), ()> {
        self.precheck(addr, width, Kind::Store)?;
        if let Some(n) = offset(addr, SRAM_DATA, SRAM_LEN, width)
            .or_else(|| offset(addr, SRAM_INSN, SRAM_LEN, width))
        {
            return self.store_at(n, addr, width, value);
        }
        if (MMU_BASE..MMU_BASE + (MMU_LEN * 4) as u32).contains(&addr) {
            self.register_access(addr, width, Kind::Store)?;
            let n = ((addr - MMU_BASE) / 4) as usize;
            let mmu = Arc::make_mut(&mut self.regs.mmu);
            mmu.entry[n] = value;
            mmu.known[n] = true;
            self.code_gen += 1;
            return Ok(());
        }
        if matches!(addr, SYSCLK | CPU_PER) {
            self.register_access(addr, width, Kind::Store)?;
            let sysclk = if addr == SYSCLK { value } else { self.regs.sysclk };
            let cpu_per = if addr == CPU_PER { value } else { self.regs.cpu_per };
            // Unsupported selections fail without a partial register write,
            // as required by Bus. Only the active source determines the rate.
            let Some(cpu_ticks) = clock_ticks(sysclk, cpu_per) else {
                return self.stop("CPU clock source not modelled", addr, width, Kind::Store, None);
            };
            if cpu_ticks == 2 && !self.periph.regulators_written() {
                return self.stop(UNRAISED_240, addr, width, Kind::Store, None);
            }
            let apb = apb_ticks(sysclk, cpu_ticks);
            if apb != self.regs.apb_ticks() || ((sysclk >> 10) & 3 == 1) != self.regs.pll() {
                if let Err(what) = self.periph.apb_changed(apb, self.tick) {
                    return self.stop(what, addr, width, Kind::Store, None);
                }
            }
            if (sysclk >> 10) & 3 == 1 && !self.regs.pll() {
                self.periph.note_time(self.tick, PLL_REGULATOR);
            }
            self.regs.sysclk = sysclk;
            self.regs.cpu_per = cpu_per;
            return Ok(());
        }
        if addr == CACHE_STATE {
            return self.stop("write to read-only cache state", addr, width, Kind::Store, None);
        }
        if let Some(n) = CORE1_CONTROL.iter().position(|&a| a == addr) {
            self.register_access(addr, width, Kind::Store)?;
            if n == 0 {
                let fields = CORE1_RESETING | CORE1_CLKGATE_EN | CORE1_RUNSTALL;
                if value & !fields != 0 {
                    return self.stop("reserved CPU1 control fields are undocumented", addr, width, Kind::Store, None);
                }
                if value & fields == CORE1_CLKGATE_EN {
                    return self.stop("CPU1 is not modelled", addr, width, Kind::Store, None);
                }
            }
            self.regs.core1[n] = value;
            return Ok(());
        }
        if let Some(n) = PERIP.iter().position(|&a| a == addr) {
            self.register_access(addr, width, Kind::Store)?;
            if (value ^ self.regs.perip[n]) & GATED[n] != 0 {
                return self.stop("clock gate or reset of a modelled peripheral changed", addr, width, Kind::Store, None);
            }
            let mut gates = self.regs.perip;
            gates[n] = value;
            if let Err(what) = self.periph.gates(gates, n) {
                return self.stop(what, addr, width, Kind::Store, None);
            }
            self.regs.perip[n] = value;
            return Ok(());
        }
        if PERIPHERALS.contains(&addr) {
            self.register_access(addr, width, Kind::Store)?;
            match self.periph.write(addr, value, self.tick, self.regs.pll(), &self.sram) {
                Some(Ok(())) => return Ok(()),
                Some(Err(what)) => return self.stop(what, addr, width, Kind::Store, None),
                None => {}
            }
        }
        self.stop("unmodelled peripheral or address", addr, width, Kind::Store, None)
    }
}
