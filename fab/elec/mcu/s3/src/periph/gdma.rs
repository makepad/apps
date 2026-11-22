//! GDMA OUT to SPI2 and frame-paced I2S0 OUT/IN.

use std::sync::Arc;

use fab_elec_mcu_emu::board::Payload;

use super::Access;
use crate::bus::{Journal, SRAM_DATA, SRAM_LEN};
use crate::Junk;

const STRIDE: usize = 0xc0;
const OUT: usize = 0x60;
const RAW: usize = 0x08;
const ENA: usize = 0x10;
const LINK: usize = 0x20;
const PARK: u32 = 1 << 23;
const START: u32 = 1 << 21;
// trm.txt:12230–12263, 12339–12367, 13030–13051.
const IN_START: u32 = 1 << 22;
const IN_PARK: u32 = 1 << 24;
const I2S0: u32 = 3;

#[derive(Clone, Copy)]
enum AccessType { Rw, Ro, Raw, Clear, Link, Fifo, FifoSize }

#[derive(Clone, Copy)]
struct Register { mask: u32, reset: u32, access: AccessType }

fn register(off: usize) -> Option<Register> {
    use AccessType::*;
    // Appendix C has five identical channel banks at 0xc0 strides, each
    // with an IN half at +0 and OUT half at +0x60. Global words follow.
    let (mask, reset, access) = if off < 0x3c0 {
        let local = off % STRIDE;
        let out = local >= OUT;
        match local % OUT {
            0x00 => (if out { 0x3f } else { 0x1f }, if out { 8 } else { 0 }, Rw),
            0x04 => (if out { 0x7000 } else { 0x7fff }, if out { 0 } else { 12 }, Rw),
            0x08 => (if out { 255 } else { 1023 }, 0, Raw),
            0x0c => (if out { 255 } else { 1023 }, 0, Ro),
            0x10 => (if out { 255 } else { 1023 }, 0, Rw),
            0x14 => (if out { 255 } else { 1023 }, 0, Clear),
            0x18 => (if out { 0x07ff_ffff } else { 0x1fff_ffff },
                if out { 0x0780_002a } else { 0x0f00_003a }, Ro),
            0x1c => (if out { 0x3ff } else { 0x1fff }, if out { 0 } else { 0x800 }, Fifo),
            0x20 => (if out { 0xff_ffff } else { 0x1ff_ffff }, if out { PARK } else { 0x110_0000 }, Link),
            0x24 => (0x7f_ffff, 0, Ro),
            0x28 | 0x2c | 0x30 | 0x34 | 0x38 => (u32::MAX, 0, Ro),
            0x3c => (0xf00, 0xf00, Rw),
            0x44 => (15, 0, Rw),
            0x48 => (63, 63, Rw),
            _ => return None,
        }
    } else {
        match off {
            0x3c0 => (0x37, 0, Rw),
            0x3c4 => (0x70, 0x20, Rw),
            0x3c8 => (0x17, 0, Rw),
            0x3cc..=0x3f0 => (0x7f, 14, FifoSize),
            0x3f4 => (u32::MAX, 0, Ro),
            0x3f8 => (0xfff, 0, Ro),
            0x3fc => (1, 0, Raw),
            0x400 => (1, 0, Ro),
            0x404 => (1, 0, Rw),
            0x408 => (1, 0, Clear),
            0x40c => (u32::MAX, 0x0210_1180, Rw),
            _ => return None,
        }
    };
    Some(Register { mask, reset, access })
}

fn fields(old: u32, mask: u32, value: u32) -> u32 { (old & !mask) | (value & mask) }

#[derive(Clone, Copy, PartialEq, Eq)]
struct Descriptor {
    address: u32,
    buffer: usize,
    len: usize,
    eof: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct Transfer {
    channel: usize,
    // Each tier decodes its own chain from SRAM. Clones share it, separate decodes compare by value.
    descriptors: Arc<[Descriptor]>,
    len: usize,
    writeback: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct AudioLink {
    channel: usize,
    desc: Descriptor,
    next: u32,
    previous: u32,
    used: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct AudioChunk {
    out: bool,
    len: usize,
    pub offset: usize,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Gdma {
    regs: [u32; 0x410 / 4],
    clocked: bool,
    reset: bool,
    held: bool,
    known: bool,
    armed: [bool; 5],
    fault: [bool; 5],
    observed_fifo: [bool; 5],
    transfer: Option<Transfer>,
    audio: [Option<AudioLink>; 2], // IN, OUT
    in_fault: [bool; 5],
    in_observed: [bool; 5],
    in_raised: [Option<(u64, u32)>; 5],
    set_at: [Option<u64>; 5],
    set_bits: [u32; 5],
    noted: bool,
    notes: Vec<(u64, &'static str)>,
}

impl Gdma {
    pub fn new(en: u32, rst: u32, junk: &mut Junk) -> Self {
        Self {
            regs: std::array::from_fn(|_| junk.u32()), clocked: en & (1 << 6) != 0,
            reset: rst & (1 << 6) != 0, held: false, known: false,
            armed: [false; 5], fault: [false; 5], observed_fifo: [false; 5], transfer: None,
            audio: [None; 2], in_fault: [false; 5], in_observed: [false; 5], in_raised: [None; 5],
            set_at: [None; 5], set_bits: [0; 5], noted: false, notes: Vec::new(),
        }
    }

    pub fn running(&self) -> bool { self.active() || self.armed.iter().any(|&armed| armed) }

    pub fn check_gate(&self, en: u32, rst: u32) -> Result<(), &'static str> {
        if self.running() && (en & (1 << 6) == 0 || rst & (1 << 6) != 0) {
            return Err("GDMA gate/reset while a channel is armed or transferring is undocumented (G12, Q04)");
        }
        Ok(())
    }

    pub fn gate(&mut self, en: u32, rst: u32, reset_write: bool) {
        // D-G3 lead decision: RST=1 holds reset, following SYSTEM's convention.
        let reset = rst & (1 << 6) != 0;
        self.clocked = en & (1 << 6) != 0;
        if reset_write && reset { self.held = true; self.known = false; }
        if self.reset && !reset && self.held {
            if self.clocked {
                for off in (0..0x410).step_by(4) {
                    if let Some(r) = register(off) { self.regs[off / 4] = fields(self.regs[off / 4], r.mask, r.reset); }
                }
                self.armed.fill(false);
                self.fault.fill(false);
                self.in_fault.fill(false);
                self.in_observed.fill(false);
                self.in_raised.fill(None);
                self.observed_fifo.fill(false);
                self.set_at.fill(None);
                self.set_bits.fill(0);
                self.known = true;
            }
            self.held = false;
        }
        self.reset = reset;
    }

    fn accessible(&self) -> Result<(), &'static str> {
        if !self.clocked { return Err("GDMA register access while the SYSTEM clock is off (G12)"); }
        if self.reset { return Err("GDMA register access while SYSTEM reset is held (G12, D-G3)"); }
        if !self.known { return Err("GDMA ROM handoff state is unknown; hold reset and release it with the clock enabled (G12)"); }
        Ok(())
    }

    fn offset(addr: u32) -> Option<usize> {
        addr.checked_sub(0x6003_f000).filter(|&off| off < 0x1000).map(|off| off as usize)
    }

    pub fn read(&self, addr: u32) -> Access<u32> {
        let off = Self::offset(addr)?;
        Some((|| {
            self.accessible()?;
            let r = register(off).ok_or("GDMA reserved register access is undocumented (Q01)")?;
            if matches!(r.access, AccessType::Clear) { return Err("GDMA WT/WO interrupt-clear readback is undocumented (Q01)"); }
            if off < 0x3c0 {
                let local = off % OUT;
                if matches!(local, 0x24 | 0x30 | 0x34 | 0x38) {
                    return Err("GDMA STATE/DSCR/DSCR_BF0/DSCR_BF1 readback pipeline is disputed (D-G4)");
                }
                if local == 0x18 && (self.observed_fifo[off / STRIDE] || (off % STRIDE < OUT && self.in_observed[off / STRIDE])) {
                    return Err("GDMA FIFO counts and flags after START require unmodelled FIFO timing (G09, Q04)");
                }
                if local == 0x0c {
                    return Ok(fields(self.regs[off / 4], r.mask, self.regs[(off - 4) / 4] & self.regs[(off + 4) / 4]));
                }
            }
            if off == 0x400 {
                return Ok(fields(self.regs[off / 4], 1, self.regs[0x3fc / 4] & self.regs[0x404 / 4]));
            }
            Ok(self.regs[off / 4])
        })())
    }

    pub fn write(&mut self, addr: u32, value: u32, tick: u64, ram: &[u8], i2s_running: bool) -> Access<()> {
        let off = Self::offset(addr)?;
        Some((|| {
            self.accessible()?;
            let r = register(off).ok_or("GDMA reserved register access is undocumented (Q01)")?;
            if off < 0x3c0 {
                let base = off - off % OUT;
                let selecting_audio = off % OUT == 0x48 && value & 63 == I2S0;
                if selecting_audio || self.regs[(base + 0x48) / 4] & 63 == I2S0 {
                    // trm.txt:12424–12465, 12590–12617. The existing GDMA bank also
                    // exposes extension IRQ bits outside these TRM registers.
                    if matches!(off % OUT, 0x10 | 0x14) {
                        let irq_mask = if off % STRIDE >= OUT { 15 } else { 63 };
                        if value & r.mask & !irq_mask != 0 { return Err("GDMA I2S reserved interrupt bits"); }
                    }
                    // trm.txt:57954–57965: preserve read-back reserved bits or write their defaults.
                    let reserved = value & !r.mask;
                    if reserved != 0 && reserved != self.regs[off / 4] & !r.mask {
                        return Err("GDMA I2S register write changes reserved bits");
                    }
                }
            }
            match r.access {
                AccessType::Ro => return Err("GDMA write to a read-only register is undocumented (Q01)"),
                AccessType::Raw => return Err("GDMA INT_RAW write is undocumented; WTC is not W1C (C01)"),
                AccessType::Clear => {
                    let raw = if off == 0x408 { 0x3fc } else { off - 12 };
                    if off < 0x3c0 && off % STRIDE >= OUT {
                        let n = off / STRIDE;
                        if self.set_at[n] == Some(tick) && value & self.set_bits[n] & r.mask != 0 {
                            return Err("GDMA simultaneous interrupt set/clear priority is undocumented (Q01)");
                        }
                    } else if off < 0x3c0 && self.in_raised[off / STRIDE].is_some_and(|(t, bits)| t == tick && bits & value != 0) {
                        return Err("GDMA simultaneous IN interrupt set/clear priority is undocumented");
                    }
                    self.regs[raw / 4] &= !(value & r.mask);
                    return Ok(());
                }
                _ => (),
            }
            if off < 0x3c0 {
                let n = off / STRIDE;
                let local = off % STRIDE;
                let out = local >= OUT;
                let audio = self.audio[usize::from(out)].is_some_and(|a| a.channel == n);
                if local % OUT == ENA {
                    if audio && value & r.mask & !(if out { 15 } else { 0x1b }) != 0 {
                        return Err("GDMA I2S FIFO or unsupported channel interrupt enable");
                    }
                    self.regs[off / 4] = fields(self.regs[off / 4], r.mask, value);
                    return Ok(());
                }
                if self.transfer.as_ref().is_some_and(|t| t.channel == n) {
                    return Err("GDMA channel configuration during a transfer is not modelled (Q04)");
                }
                if audio && (i2s_running || !(local % OUT == 0 && value & 1 != 0)) {
                    return Err("GDMA I2S channel configuration during transfer");
                }
                if local % OUT == 0x48 && value & 63 == I2S0 && self.audio_available(out) {
                    return Err("GDMA two channels select the same I2S direction");
                }
                if matches!(r.access, AccessType::Fifo) {
                    let pulse = if out { 1 << 9 } else { 1 << 12 };
                    if value & pulse != 0 { return Err("GDMA software FIFO PUSH/POP is outside the modelled subset (G08)"); }
                    if !out { return Err("GDMA INFIFO_RDATA is read-only (Q01)"); }
                }
                if matches!(r.access, AccessType::Link) {
                    let controls = if out { (1 << 20) | (1 << 22) } else { (1 << 21) | (1 << 23) };
                    if value & controls != 0 { return Err("GDMA LINK STOP/RESTART is not modelled (Q04)"); }
                    if out && self.armed[n] { return Err("GDMA LINK write on an armed channel is undocumented (Q04)"); }
                    let base = off - LINK;
                    let select = self.regs[(base + 0x48) / 4] & 63;
                    let start = value & if out { START } else { IN_START } != 0;
                    if start && select == I2S0 {
                        self.audio_start(out, n, value, ram, tick)?;
                    } else if start && !out {
                        return Err("GDMA IN start is not modelled");
                    } else if start {
                        if self.fault[n] { return Err("GDMA descriptor fault requires channel reset before START (G03)"); }
                        if self.regs[(n * STRIDE + OUT) / 4] & 1 != 0 { return Err("GDMA OUT start while OUT_RST is held (G08)"); }
                        if self.regs[(n * STRIDE + OUT + 0x48) / 4] & 63 != 0 {
                            return Err("GDMA OUT_PERI_SEL other than SPI2 is not modelled (G01)");
                        }
                        // G07 lead decision: reconstruct the internal address.
                        let first = 0x3fc0_0000 | (value & 0xfffff);
                        if !(0x3fc8_8000..=0x3fcf_ffff).contains(&first) {
                            return Err("GDMA OUTLINK address reconstructed as 0x3FC00000 | ADDR is outside internal RAM (G07)");
                        }
                        if first & 3 != 0 { return Err("GDMA descriptor address is not word-aligned (G03, G03a)"); }
                        self.armed[n] = true;
                        self.observed_fifo[n] = true;
                        self.regs[off / 4] &= !PARK;
                    }
                    let writable = if out { 0xfffff } else { 0x1fffff };
                    self.regs[off / 4] = fields(self.regs[off / 4], writable, value);
                    return Ok(()); // G07: START self-clears, at this write tick.
                }
                if local % OUT == 0 && value & 1 != 0 {
                    // G08 resets the FSM and FIFO pointers, not configuration
                    // or interrupt RAW. A fault is recoverable with 1 then 0.
                    if out {
                        self.armed[n] = false;
                        self.fault[n] = false;
                        self.observed_fifo[n] = false;
                        self.regs[(n * STRIDE + OUT + LINK) / 4] |= PARK;
                    } else {
                        self.in_fault[n] = false;
                        self.in_observed[n] = false;
                        self.regs[(n * STRIDE + LINK) / 4] |= IN_PARK;
                    }
                    if audio { self.audio[usize::from(out)] = None; }
                } else if self.armed[n] {
                    return Err("GDMA configuration of an armed channel is not modelled (Q04)");
                }
            } else if self.running() && off != 0x404 {
                return Err("GDMA global configuration while a channel is armed is not modelled (Q04)");
            }
            self.regs[off / 4] = fields(self.regs[off / 4], r.mask, value);
            Ok(())
        })())
    }

    fn range(address: u32, len: usize) -> Result<usize, &'static str> {
        if (0x3c00_0000..=0x3dff_ffff).contains(&address) { return Err("GDMA external RAM: PSRAM is not modelled (G03)"); }
        let end = u64::from(address) + len as u64;
        if address < 0x3fc8_8000 || end > 0x3fd0_0000 {
            return Err("GDMA descriptor or buffer is outside internal RAM, or crosses its boundary (G03, Q04)");
        }
        let offset = (address - SRAM_DATA) as usize;
        if offset >= SRAM_LEN || len > SRAM_LEN - offset {
            return Err("GDMA SRAM0/2 can be cache memory; the cache configuration is not modelled");
        }
        Ok(offset)
    }

    fn descriptor_fault(&mut self, n: usize, tick: u64, what: &'static str) -> &'static str {
        self.raise(n, 1 << 2, tick); // G03/G11: latch OUT_DSCR_ERR, halt channel.
        self.fault[n] = true;
        self.armed[n] = false;
        self.regs[(n * STRIDE + OUT + LINK) / 4] |= PARK;
        what
    }

    fn raise_in(&mut self, n: usize, bits: u32, tick: u64) {
        self.regs[(n * STRIDE + RAW) / 4] |= bits;
        let previous = self.in_raised[n].filter(|&(t, _)| t == tick).map_or(0, |(_, b)| b);
        self.in_raised[n] = Some((tick, previous | bits));
    }

    pub(super) fn audio_available(&self, out: bool) -> bool { self.audio[usize::from(out)].is_some() }

    pub(super) fn audio_ready(&self, out: bool) -> Result<(), &'static str> {
        self.accessible()?;
        if self.audio_available(out) { Ok(()) } else if out {
            Err("I2S TX requires an armed GDMA I2S0 OUT link")
        } else { Err("I2S RX requires an armed GDMA I2S0 IN link") }
    }

    fn audio_start(&mut self, out: bool, n: usize, value: u32, ram: &[u8], tick: u64) -> Result<(), &'static str> {
        let half = if out { OUT } else { 0 };
        let base = n * STRIDE + half;
        // trm.txt:11461–11479, 13030–13051: one channel per direction and peripheral.
        if (0..5).any(|i| i != n && self.regs[(i * STRIDE + half + 0x48) / 4] & 63 == I2S0) {
            return Err("GDMA two channels select the same I2S direction");
        }
        let fault = if out { self.fault[n] } else { self.in_fault[n] };
        if fault {
            return Err("GDMA I2S descriptor fault requires channel reset before START");
        }
        // trm.txt:12156–12208, 12269–12319, 11668–11672.
        let conf = self.regs[base / 4];
        let conf1 = self.regs[(base + 4) / 4];
        if conf & 3 != 0 || self.regs[(n * STRIDE) / 4] & (1 << 4) != 0 {
            return Err("GDMA I2S requires channel reset released, no loop test and no memory transfer mode");
        }
        if conf1 & (3 << 13) != 0 { return Err("GDMA I2S external-memory block mode is unsupported"); }
        if out && conf & 8 == 0 { return Err("GDMA I2S OUT EOF at FIFO entry is unsupported"); }
        if !out && conf1 & 4095 != 12 { return Err("GDMA I2S custom FIFO watermark is unsupported"); }
        let irq = register(base + ENA).expect("channel interrupt enable");
        if self.regs[(base + ENA) / 4] & irq.mask & !(if out { 15 } else { 0x1b }) != 0 {
            return Err("GDMA I2S FIFO or unsupported channel interrupt enable");
        }
        self.check_bus_config()?;
        // M: chapter 3 specifies fixed FIFO depths (trm.txt:11496–11499).
        // Refuse alternate FIFO allocations exposed by the existing GDMA register bank.
        for (i, &word) in self.regs.iter().enumerate().skip(0x3c0 / 4) {
            if let Some(r) = register(4 * i) {
                if matches!(r.access, AccessType::FifoSize) && word & r.mask != r.reset {
                    return Err("GDMA I2S nondefault FIFO allocation is unsupported");
                }
            }
        }
        // M: reconstruct the upper internal RAM address as for SPI (trm.txt:12253–12255, 12359–12360).
        let first = 0x3fc0_0000 | (value & 0xfffff);
        let link = self.audio_descriptor(out, n, first, 0, ram, tick)?;
        self.audio[usize::from(out)] = Some(link);
        self.regs[(base + LINK) / 4] &= !(if out { PARK } else { IN_PARK });
        if out { self.observed_fifo[n] = true; } else { self.in_observed[n] = true; }
        Ok(())
    }

    fn audio_descriptor(&mut self, out: bool, n: usize, address: u32, previous: u32, ram: &[u8], tick: u64)
        -> Result<AudioLink, &'static str>
    {
        let base = n * STRIDE + if out { OUT } else { 0 };
        // trm.txt:11414–11453, 11527–11542: only bad pointers and failed owner checks
        // are hardware descriptor faults. Valid PSRAM and unmodelled SRAM return a refusal.
        let internal = |a| (0x3fc8_8000..=0x3fcf_ffff).contains(&a);
        if !internal(address) {
            return Err(self.audio_fault(out, n, tick, "GDMA I2S descriptor pointer is outside internal RAM"));
        }
        let at = Self::range(address, 12)?;
        // M: use aligned little-endian internal RAM words as for GDMA-to-SPI.
        if at & 3 != 0 { return Err("GDMA I2S descriptor is not word-aligned"); }
        if self.in_flight(at, 12) { return Err("GDMA I2S descriptor overlaps an active transfer"); }
        let word = |i: usize| u32::from_le_bytes(ram[i..i + 4].try_into().expect("validated descriptor word"));
        let header = word(at);
        let addr = word(at + 4);
        let next = word(at + 8);
        if self.regs[(base + 4) / 4] & (1 << 12) != 0 && header & (1 << 31) == 0 {
            return Err(self.audio_fault(out, n, tick, "GDMA I2S CHECK_OWNER rejected a CPU-owned descriptor"));
        }
        if !internal(addr) && !(0x3c00_0000..=0x3dff_ffff).contains(&addr) {
            return Err(self.audio_fault(out, n, tick, "GDMA I2S buffer pointer is outside DMA memory"));
        }
        if next != 0 && !internal(next) {
            return Err(self.audio_fault(out, n, tick, "GDMA I2S next descriptor pointer is outside internal RAM"));
        }
        let length_field = if out { header >> 12 } else { header };
        let len = (length_field & 4095) as usize;
        if len == 0 { return Err("GDMA I2S descriptor has zero transfer length or capacity"); }
        // trm.txt:11446–11451: length is valid bytes within the buffer's capacity.
        if out && len > (header & 4095) as usize { return Err("GDMA I2S OUT length exceeds buffer capacity"); }
        // trm.txt:11546–11548 qualifies the last descriptor with EOF; 11659–11661 says end of list.
        if out && next == 0 && header & (1 << 30) == 0 {
            return Err("GDMA I2S terminal OUT descriptor without EOF has ambiguous TOTAL_EOF timing");
        }
        let buffer = Self::range(addr, len)?;
        if next != 0 {
            let successor = Self::range(next, 12)?;
            if next & 3 != 0 { return Err("GDMA I2S next descriptor is not word-aligned"); }
            if buffer < successor + 12 && successor < buffer + len {
                return Err("GDMA I2S buffer overlaps its next descriptor");
            }
        }
        if header & (31 << 24) != 0 || (!out && header & (1 << 30) != 0) {
            return Err("GDMA I2S descriptor reserved bits or RX EOF preset");
        }
        // trm.txt:11571–11586: receive burst requires word-aligned capacity and pointer.
        if !out && self.regs[base / 4] & 8 != 0 && (buffer | len) & 3 != 0 {
            return Err("GDMA I2S burst IN buffer address and size must be word-aligned");
        }
        if (buffer < at + 12 && at < buffer + len) || self.in_flight(buffer, len) {
            return Err("GDMA I2S overlapping active buffers or descriptors are unsupported");
        }
        Ok(AudioLink { channel: n, desc: Descriptor { address, buffer, len, eof: header & (1 << 30) != 0 },
            previous, next, used: 0 })
    }

    fn audio_fault(&mut self, out: bool, n: usize, tick: u64, why: &'static str) -> &'static str {
        // trm.txt:11527–11542, 12457–12459, 12612–12614.
        self.audio[usize::from(out)] = None;
        if out { return self.descriptor_fault(n, tick, why); }
        self.raise_in(n, 1 << 3, tick);
        self.in_fault[n] = true;
        self.regs[(n * STRIDE + LINK) / 4] |= IN_PARK;
        why
    }

    pub(super) fn audio_chunk(&self, out: bool, len: usize) -> Result<AudioChunk, &'static str> {
        let link = self.audio[usize::from(out)].ok_or(if out { "I2S TX underrun" } else {
            "I2S RX overrun: no IN descriptor, FIFO backlog timing is unsupported"
        })?;
        if len > link.desc.len - link.used { return Err("GDMA I2S descriptor boundary inside a frame is unsupported"); }
        Ok(AudioChunk { out, len, offset: link.desc.buffer + link.used })
    }

    pub(super) fn audio_commit(&mut self, chunks: [Option<AudioChunk>; 2], rx_eof: bool,
        ram: &mut [u8], journal: &mut Journal, tick: u64) -> Result<(), &'static str>
    {
        // Both halves of this WS period completed even if fetching a successor faults.
        let mut next = [None; 2];
        for chunk in chunks.into_iter().flatten() {
            next[usize::from(chunk.out)] = self.retire(chunk, !chunk.out && rx_eof, ram, journal, tick);
        }
        for (side, link) in next.into_iter().enumerate() {
            if let Some(link) = link {
                let desc = self.audio_descriptor(side != 0, link.channel, link.next, link.desc.address, ram, tick)?;
                self.audio[side] = Some(desc);
            }
        }
        Ok(())
    }

    fn retire(&mut self, chunk: AudioChunk, rx_eof: bool, ram: &mut [u8], journal: &mut Journal, tick: u64)
        -> Option<AudioLink>
    {
        let side = usize::from(chunk.out);
        let link = self.audio[side].as_mut().expect("I2S frame has a descriptor");
        link.used += chunk.len;
        if link.used < link.desc.len && !rx_eof { return None; }
        let link = self.audio[side].take().expect("completed I2S descriptor");
        let n = link.channel;
        let at = (link.desc.address - SRAM_DATA) as usize;
        let base = n * STRIDE + if chunk.out { OUT } else { 0 };
        let header = u32::from_le_bytes(ram[at..at + 4].try_into().expect("validated descriptor word"));
        // M: no FIFO lookahead; retire and fetch each successor at the completed frame boundary.
        // trm.txt:11417–11435, 11446–11455, 11560–11565, 11659–11686, 12833–12846.
        if chunk.out {
            if self.regs[base / 4] & 4 != 0 { journal.dma_word(ram, at, header & !(1 << 31)); }
            let mut bits = 1;
            if link.desc.eof {
                bits |= 2;
                self.regs[(base + 0x28) / 4] = link.desc.address;
                self.regs[(base + 0x2c) / 4] = link.previous;
            }
            if link.next == 0 { bits |= 8; }
            self.raise(n, bits, tick);
        } else {
            let writeback = (header & !(3 << 30 | 4095 << 12)) | u32::from(rx_eof) << 30 | (link.used as u32) << 12;
            journal.dma_word(ram, at, writeback);
            if rx_eof { self.regs[(base + 0x28) / 4] = link.desc.address; }
            let exhausted = !rx_eof && link.next == 0;
            self.raise_in(n, 1 | u32::from(rx_eof) << 1 | u32::from(exhausted) << 4, tick);
        }
        if link.next == 0 {
            self.regs[(base + LINK) / 4] |= if chunk.out { PARK } else { IN_PARK };
            None
        } else { Some(link) }
    }

    pub fn fetch(&mut self, bytes: usize, ram: &[u8], tick: u64) -> Result<(), &'static str> {
        self.accessible()?;
        if self.transfer.is_some() { return Err("GDMA already has a pending SPI2 transfer (G01)"); }
        let mut channels = (0..5).filter(|&n| self.regs[(n * STRIDE + OUT + 0x48) / 4] & 63 == 0);
        let n = channels.next().ok_or("GDMA has no OUT channel selecting SPI2 (G01)")?;
        if channels.next().is_some() { return Err("GDMA two OUT channels select SPI2 (G01 exclusion)"); }
        if !self.armed[n] { return Err("GDMA SPI2 OUT channel is not armed by OUTLINK START (G07)"); }
        let base = n * STRIDE + OUT;
        if self.regs[base / 4] & 2 != 0 { return Err("GDMA_OUT_LOOP_TEST is reserved (Appendix C)"); }
        if self.regs[(base + 4) / 4] & (3 << 13) != 0 { return Err("GDMA_OUT_EXT_MEM_BK_SIZE is outside the internal-RAM subset (D-G1)"); }
        if self.regs[(n * STRIDE) / 4] & (1 << 4) != 0 { return Err("GDMA_MEM_TRANS_EN is not modelled (G09)"); }
        self.check_bus_config()?;
        let size = 0x3d0 + 8 * n;
        let r = register(size).expect("GDMA OUT SRAM size register");
        if self.regs[size / 4] & r.mask != r.reset { return Err("GDMA_OUT_SRAM_SIZE outside the subset (Appendix C)"); }
        let check_owner = self.regs[(base + 4) / 4] & (1 << 12) != 0;
        let writeback = self.regs[base / 4] & 4 != 0;
        let mut address = 0x3fc0_0000 | (self.regs[(base + LINK) / 4] & 0xfffff);
        let mut descriptors = Vec::new();
        let mut total = 0;
        // Positive lengths and the 32 KiB SPI limit also bound a cyclic list:
        // it stops as an excess list before traversal can grow without bound.
        loop {
            if address & 3 != 0 {
                return Err(self.descriptor_fault(n, tick, "GDMA descriptor address is not word-aligned (G03, G03a)"));
            }
            let off = Self::range(address, 12).map_err(|what| self.descriptor_fault(n, tick, what))?;
            // G03a (Assumed): exactly the CPU's L32I little-endian SRAM words.
            let word = |offset| u32::from_le_bytes(ram[offset..offset + 4].try_into().expect("validated descriptor word"));
            let dw0 = word(off);
            let buffer_address = word(off + 4);
            let next = word(off + 8);
            if check_owner && dw0 & (1 << 31) == 0 {
                return Err(self.descriptor_fault(n, tick, "GDMA OUT_CHECK_OWNER rejected a CPU-owned descriptor (G03)"));
            }
            let len = ((dw0 >> 12) & 4095) as usize;
            if len == 0 { return Err(self.descriptor_fault(n, tick, "GDMA TX descriptor length must be 1..=4095 (G04)")); }
            let buffer = Self::range(buffer_address, len).map_err(|what| self.descriptor_fault(n, tick, what))?;
            if self.in_flight(off, 12) || self.in_flight(buffer, len) {
                return Err("GDMA SPI2 and I2S transfers overlap in SRAM");
            }
            if next != 0 {
                Self::range(next, 12).map_err(|what| self.descriptor_fault(n, tick, what))?;
                if next & 3 != 0 { return Err(self.descriptor_fault(n, tick, "GDMA DW2 descriptor address is not word-aligned (G03, G03a)")); }
            }
            total += len;
            if total > bytes { return Err("GDMA excess TX list carried to a later SPI transaction is not modelled (S05)"); }
            descriptors.push(Descriptor { address, buffer, len, eof: dw0 & (1 << 30) != 0 });
            if next == 0 { break; }
            address = next;
        }
        if total != bytes { return Err("GDMA short TX list repeating the last transferred data is not modelled (S05)"); }
        self.transfer = Some(Transfer { channel: n, descriptors: descriptors.into(), len: bytes, writeback });
        if !self.noted {
            self.notes.push((tick, "GDMA descriptor words read as the CPU reads them (G03a, Assumed)"));
            self.notes.push((tick, "GDMA OUT_EOF_MODE entering/leaving distinction is collapsed to the transfer's last edge (G10)"));
            self.noted = true;
        }
        Ok(())
    }

    /// Whether `len` bytes at SRAM offset `offset` overlap a descriptor or buffer of the
    /// transfer in flight.
    /// Whether a transfer is under way (`in_flight` can be true only then).
    pub fn active(&self) -> bool { self.transfer.is_some() || self.audio.iter().any(Option::is_some) }

    pub fn in_flight(&self, offset: usize, len: usize) -> bool {
        let overlaps = |start: usize, n: usize| offset < start + n && start < offset + len;
        self.transfer.as_ref().is_some_and(|t| t.descriptors.iter().any(|d|
            overlaps((d.address - SRAM_DATA) as usize, 12) || overlaps(d.buffer, d.len)))
            || self.audio.iter().flatten().any(|a| overlaps((a.desc.address - SRAM_DATA) as usize, 12)
                || overlaps(a.desc.buffer, a.desc.len)
                // M: the successor pre-read has no exact tick (trm.txt:12957–12959).
                // Protect its metadata for the lifetime of the current descriptor.
                || (a.next != 0 && overlaps((a.next - SRAM_DATA) as usize, 12)))
    }

    fn check_bus_config(&self) -> Result<(), &'static str> {
        // trm.txt:11496–11499, 11809–11810, 12373–12423. M: retain the existing GDMA
        // model's reset-only bus/FIFO extensions; chapter 3 does not document their programming.
        for (off, what) in [(0x3c0, "GDMA_AHB_TEST outside the subset (Appendix C)"),
            (0x3c4, "GDMA_PD_CONF outside the subset (Appendix C)"),
            (0x3c8, "GDMA_MISC_CONF outside the subset (Appendix C)")] {
            let r = register(off).expect("global register exists");
            if self.regs[off / 4] & r.mask != r.reset { return Err(what); }
        }
        Ok(())
    }

    pub fn payload<'a>(&'a self, ram: &'a [u8]) -> impl Payload + 'a {
        let transfer = self.transfer.as_ref().expect("SPI DMA has a fetched descriptor list");
        DmaPayload { ram, descriptors: &transfer.descriptors, len: transfer.len }
    }

    fn raise(&mut self, n: usize, bits: u32, tick: u64) {
        self.regs[(n * STRIDE + OUT + RAW) / 4] |= bits;
        if self.set_at[n] != Some(tick) { self.set_bits[n] = 0; }
        self.set_at[n] = Some(tick);
        self.set_bits[n] |= bits;
    }

    pub fn complete(&mut self, ram: &mut [u8], journal: &mut Journal, tick: u64) {
        let transfer = self.transfer.take().expect("GDMA completion requires a fetched list");
        let n = transfer.channel;
        let base = n * STRIDE + OUT;
        let mut previous = 0;
        for d in transfer.descriptors.iter() {
            if transfer.writeback {
                // G03a/G06: clear only owner, in the same shared SRAM word.
                let offset = (d.address - SRAM_DATA) as usize;
                let old = u32::from_le_bytes(ram[offset..offset + 4].try_into().expect("validated descriptor word"));
                journal.dma_word(ram, offset, old & !(1 << 31));
            }
            self.raise(n, 1, tick); // G10 OUT_DONE for each descriptor at t1.
            if d.eof {
                self.regs[(base + 0x28) / 4] = d.address;
                self.regs[(base + 0x2c) / 4] = previous;
                self.raise(n, 2, tick);
            }
            previous = d.address;
        }
        self.raise(n, 8, tick); // G06/G10: end of list, OUT_TOTAL_EOF.
        self.regs[(base + LINK) / 4] |= PARK;
        self.armed[n] = false;
    }

    pub fn interrupt_sources(&self) -> u128 {
        if !self.known || !self.clocked || self.reset { return 0; }
        let mut sources = 0;
        for n in 0..5 {
            // trm.txt:18545–18550, 12424–12465: independent GDMA IN channel sources.
            let base = n * STRIDE;
            if self.regs[(base + RAW) / 4] & self.regs[(base + ENA) / 4] & 0x3ff != 0 {
                sources |= 1u128 << (66 + n);
            }
            let base = n * STRIDE + OUT;
            if self.regs[(base + RAW) / 4] & self.regs[(base + ENA) / 4] & 255 != 0 {
                sources |= 1u128 << (71 + n); // G11/G12: level sources.
            }
        }
        sources
    }

    pub fn rehandoff(&mut self, gates: [u32; 2], junk: &mut Junk) {
        let notes = std::mem::take(&mut self.notes);
        let noted = self.noted;
        *self = Self::new(gates[0], gates[1], junk);
        self.notes = notes;
        self.noted = noted;
    }

    pub fn new_epoch(&mut self) { self.set_at.fill(None); self.set_bits.fill(0); self.in_raised.fill(None); }
    pub fn take_notes(&mut self, out: &mut Vec<(u64, &'static str)>) { out.append(&mut self.notes); }
}

struct DmaPayload<'a> { ram: &'a [u8], descriptors: &'a [Descriptor], len: usize }

impl Payload for DmaPayload<'_> {
    fn len(&self) -> usize { self.len }
    fn chunks(&self, f: &mut dyn FnMut(&[u8])) {
        for d in self.descriptors { f(&self.ram[d.buffer..d.buffer + d.len]); }
    }
}
