//! GP-SPI2 master; citations refer to facts-io.md S01–S18 and Appendix B.

use fab_elec_mcu_emu::board::{Board, Payload, SpiXfer};

use super::{gdma::Gdma, pins::Pins, Access};
use crate::bus::Journal;
use crate::Junk;

const USR: u32 = 1 << 24;
const UPDATE: u32 = 1 << 23;
const DONE: u32 = 1 << 12;
const IRQ: u32 = 0x1f_ffff;
const ENA: usize = 0x34 / 4;
const RAW: usize = 0x3c / 4;
const W0: usize = 0x98 / 4;

// Appendix B: configuration fields, including unsupported fields. Keeping
// their names here makes every nondefault unsupported field an explicit stop.
struct Field {
    off: usize,
    mask: u32,
    reset: u32,
    supported: bool,
    error: &'static str,
}

macro_rules! field {
    ($off:expr, $name:ident, $mask:expr, $reset:expr, $supported:expr) => {
        Field { off: $off, mask: $mask, reset: $reset, supported: $supported,
            error: concat!(stringify!($name), " outside the GP-SPI2 subset (Appendix B)") }
    };
}

const FIELDS: &[Field] = &[
    field!(0x00, SPI_CONF_BITLEN, 0x3ffff, 0, false),
    field!(0x04, SPI_USR_ADDR_VALUE, u32::MAX, 0, true),
    field!(0x08, SPI_WR_BIT_ORDER, 3 << 25, 0, true),
    field!(0x08, SPI_RD_BIT_ORDER, 3 << 23, 0, false),
    field!(0x08, SPI_WP_POL, 1 << 21, 1 << 21, false),
    field!(0x08, SPI_HOLD_POL, 1 << 20, 1 << 20, false),
    field!(0x08, SPI_D_POL, 1 << 19, 1 << 19, false),
    field!(0x08, SPI_Q_POL, 1 << 18, 1 << 18, false),
    field!(0x08, SPI_FREAD_OCT, 1 << 16, 0, false),
    field!(0x08, SPI_FREAD_QUAD, 1 << 15, 0, false),
    field!(0x08, SPI_FREAD_DUAL, 1 << 14, 0, false),
    field!(0x08, SPI_FCMD_OCT, 1 << 10, 0, false),
    field!(0x08, SPI_FCMD_QUAD, 1 << 9, 0, true),
    field!(0x08, SPI_FCMD_DUAL, 1 << 8, 0, false),
    field!(0x08, SPI_FADDR_OCT, 1 << 7, 0, false),
    field!(0x08, SPI_FADDR_QUAD, 1 << 6, 0, true),
    field!(0x08, SPI_FADDR_DUAL, 1 << 5, 0, false),
    field!(0x08, SPI_DUMMY_OUT, 1 << 3, 0, false),
    field!(0x0c, SPI_CLK_EQU_SYSCLK, 1 << 31, 1 << 31, true),
    field!(0x0c, SPI_CLKDIV_PRE, 15 << 18, 0, true),
    field!(0x0c, SPI_CLKCNT_N, 63 << 12, 3 << 12, true),
    field!(0x0c, SPI_CLKCNT_H, 63 << 6, 1 << 6, true),
    field!(0x0c, SPI_CLKCNT_L, 63, 3, true),
    field!(0x10, SPI_USR_COMMAND, 1 << 31, 1 << 31, true),
    field!(0x10, SPI_USR_ADDR, 1 << 30, 0, true),
    field!(0x10, SPI_USR_DUMMY, 1 << 29, 0, true),
    field!(0x10, SPI_USR_MISO, 1 << 28, 0, false),
    field!(0x10, SPI_USR_MOSI, 1 << 27, 0, true),
    field!(0x10, SPI_USR_DUMMY_IDLE, 1 << 26, 0, false),
    field!(0x10, SPI_USR_MOSI_HIGHPART, 1 << 25, 0, false),
    field!(0x10, SPI_USR_MISO_HIGHPART, 1 << 24, 0, false),
    field!(0x10, SPI_SIO, 1 << 17, 0, false),
    field!(0x10, SPI_USR_CONF_NXT, 1 << 15, 0, false),
    field!(0x10, SPI_FWRITE_OCT, 1 << 14, 0, false),
    field!(0x10, SPI_FWRITE_QUAD, 1 << 13, 0, true),
    field!(0x10, SPI_FWRITE_DUAL, 1 << 12, 0, false),
    field!(0x10, SPI_CK_OUT_EDGE, 1 << 9, 0, true),
    field!(0x10, SPI_RSCK_I_EDGE, 1 << 8, 0, false),
    field!(0x10, SPI_CS_SETUP, 1 << 7, 1 << 7, true),
    field!(0x10, SPI_CS_HOLD, 1 << 6, 1 << 6, true),
    field!(0x10, SPI_TSCK_I_EDGE, 1 << 5, 0, false),
    field!(0x10, SPI_OPI_MODE, 1 << 4, 0, false),
    field!(0x10, SPI_QPI_MODE, 1 << 3, 0, false),
    field!(0x10, SPI_DOUTDIN, 1, 0, false),
    field!(0x14, SPI_USR_ADDR_BITLEN, 31 << 27, 23 << 27, true),
    field!(0x14, SPI_CS_HOLD_TIME, 31 << 22, 1 << 22, true),
    field!(0x14, SPI_CS_SETUP_TIME, 31 << 17, 0, true),
    field!(0x14, SPI_MST_WFULL_ERR_END_EN, 1 << 16, 1 << 16, false),
    field!(0x14, SPI_USR_DUMMY_CYCLELEN, 255, 7, true),
    field!(0x18, SPI_USR_COMMAND_BITLEN, 15 << 28, 7 << 28, true),
    field!(0x18, SPI_MST_REMPTY_ERR_END_EN, 1 << 27, 1 << 27, false),
    field!(0x18, SPI_USR_COMMAND_VALUE, 65535, 0, true),
    field!(0x1c, SPI_MS_DATA_BITLEN, 0x3ffff, 0, true),
    field!(0x20, SPI_QUAD_DIN_PIN_SWAP, 1 << 31, 0, false),
    field!(0x20, SPI_CS_KEEP_ACTIVE, 1 << 30, 0, false),
    field!(0x20, SPI_CK_IDLE_EDGE, 1 << 29, 0, true),
    field!(0x20, SPI_DQS_IDLE_EDGE, 1 << 24, 0, false),
    field!(0x20, SPI_SLAVE_CS_POL, 1 << 23, 0, false),
    field!(0x20, SPI_CMD_DTR_EN, 1 << 19, 0, false),
    field!(0x20, SPI_ADDR_DTR_EN, 1 << 18, 0, false),
    field!(0x20, SPI_DATA_DTR_EN, 1 << 17, 0, false),
    field!(0x20, SPI_CLK_DATA_DTR_EN, 1 << 16, 0, false),
    field!(0x20, SPI_MASTER_CS_POL, 63 << 7, 0, false),
    field!(0x20, SPI_CK_DIS, 1 << 6, 0, false),
    field!(0x20, SPI_CS5_DIS, 1 << 5, 1 << 5, false),
    field!(0x20, SPI_CS4_DIS, 1 << 4, 1 << 4, false),
    field!(0x20, SPI_CS3_DIS, 1 << 3, 1 << 3, false),
    field!(0x20, SPI_CS2_DIS, 1 << 2, 1 << 2, false),
    field!(0x20, SPI_CS1_DIS, 1 << 1, 1 << 1, false),
    field!(0x20, SPI_CS0_DIS, 1, 0, false),
    field!(0x24, SPI_TIMING_HCLK_ACTIVE, 1 << 16, 0, false),
    field!(0x24, SPI_DIN7_MODE, 3 << 14, 0, false),
    field!(0x24, SPI_DIN6_MODE, 3 << 12, 0, false),
    field!(0x24, SPI_DIN5_MODE, 3 << 10, 0, false),
    field!(0x24, SPI_DIN4_MODE, 3 << 8, 0, false),
    field!(0x24, SPI_DIN3_MODE, 3 << 6, 0, false),
    field!(0x24, SPI_DIN2_MODE, 3 << 4, 0, false),
    field!(0x24, SPI_DIN1_MODE, 3 << 2, 0, false),
    field!(0x24, SPI_DIN0_MODE, 3, 0, false),
    field!(0x28, SPI_DIN7_NUM, 3 << 14, 0, false),
    field!(0x28, SPI_DIN6_NUM, 3 << 12, 0, false),
    field!(0x28, SPI_DIN5_NUM, 3 << 10, 0, false),
    field!(0x28, SPI_DIN4_NUM, 3 << 8, 0, false),
    field!(0x28, SPI_DIN3_NUM, 3 << 6, 0, false),
    field!(0x28, SPI_DIN2_NUM, 3 << 4, 0, false),
    field!(0x28, SPI_DIN1_NUM, 3 << 2, 0, false),
    field!(0x28, SPI_DIN0_NUM, 3, 0, false),
    field!(0x2c, SPI_D_DQS_MODE, 1 << 8, 0, false),
    field!(0x2c, SPI_DOUT7_MODE, 1 << 7, 0, false),
    field!(0x2c, SPI_DOUT6_MODE, 1 << 6, 0, false),
    field!(0x2c, SPI_DOUT5_MODE, 1 << 5, 0, false),
    field!(0x2c, SPI_DOUT4_MODE, 1 << 4, 0, false),
    field!(0x2c, SPI_DOUT3_MODE, 1 << 3, 0, false),
    field!(0x2c, SPI_DOUT2_MODE, 1 << 2, 0, false),
    field!(0x2c, SPI_DOUT1_MODE, 1 << 1, 0, false),
    field!(0x2c, SPI_DOUT0_MODE, 1, 0, false),
    field!(0x30, SPI_DMA_TX_ENA, 1 << 28, 0, true),
    field!(0x30, SPI_DMA_RX_ENA, 1 << 27, 0, false),
    field!(0x30, SPI_RX_EOF_EN, 1 << 21, 0, false),
    field!(0x30, SPI_SLV_TX_SEG_TRANS_CLR_EN, 1 << 20, 0, false),
    field!(0x30, SPI_SLV_RX_SEG_TRANS_CLR_EN, 1 << 19, 0, false),
    field!(0x30, SPI_DMA_SLV_SEG_TRANS_EN, 1 << 18, 0, false),
    field!(0xe0, SPI_USR_CONF, 1 << 28, 0, false),
    field!(0xe0, SPI_SLAVE_MODE, 1 << 26, 0, false),
    field!(0xe0, SPI_DMA_SEG_MAGIC_VALUE, 15 << 22, 10 << 22, false),
    field!(0xe0, SPI_SLV_WRBUF_BITLEN_EN, 1 << 11, 0, false),
    field!(0xe0, SPI_SLV_RDBUF_BITLEN_EN, 1 << 10, 0, false),
    field!(0xe0, SPI_SLV_WRDMA_BITLEN_EN, 1 << 9, 0, false),
    field!(0xe0, SPI_SLV_RDDMA_BITLEN_EN, 1 << 8, 0, false),
    field!(0xe0, SPI_RSCK_DATA_OUT, 1 << 3, 0, false),
    field!(0xe0, SPI_CLK_MODE_13, 1 << 2, 0, false),
    field!(0xe0, SPI_CLK_MODE, 3, 0, false),
    field!(0xe4, SPI_SLV_LAST_ADDR, 63 << 26, 0, false),
    field!(0xe4, SPI_SLV_LAST_COMMAND, 255 << 18, 0, false),
    field!(0xe4, SPI_SLV_DATA_BITLEN, 0x3ffff, 0, false),
    field!(0xe8, SPI_MST_CLK_SEL, 4, 0, true),
    field!(0xe8, SPI_MST_CLK_ACTIVE, 2, 0, true),
    field!(0xe8, SPI_CLK_EN, 1, 0, false),
    field!(0xf0, SPI_DATE, 0x0fff_ffff, 0x0210_1190, false),
];

fn fields(old: u32, mask: u32, value: u32) -> u32 { (old & !mask) | (value & mask) }

#[derive(Clone, Copy, PartialEq, Eq)]
struct Running {
    x: SpiXfer,
    rise: u64,
    delivered: bool,
    dma: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Spi {
    regs: [u32; 64],
    active: [u32; 64],
    clocked: bool,
    reset: bool,
    held: bool,
    known: bool,
    dma_clocked: bool,
    dma_reset: bool,
    dma_held: bool,
    dma_known: bool,
    dirty: bool,
    running: Option<Running>,
    set_at: Option<u64>,
    set_bits: u32,
    noted: bool,
    notes: Vec<(u64, &'static str)>,
}

impl Spi {
    pub fn new(en: u32, rst: u32, junk: &mut Junk) -> Self {
        let regs = std::array::from_fn(|_| junk.u32());
        Self {
            regs, active: regs, clocked: en & (1 << 6) != 0, reset: rst & (1 << 6) != 0,
            held: false, known: false, dma_clocked: en & (1 << 22) != 0,
            dma_reset: rst & (1 << 22) != 0, dma_held: false, dma_known: false,
            dirty: false, running: None, set_at: None, set_bits: 0,
            noted: false, notes: Vec::new(),
        }
    }

    pub fn running(&self) -> bool { self.running.is_some() }

    pub fn check_gate(&self, en: u32, rst: u32) -> Result<(), &'static str> {
        let mask = (1 << 6) | if self.running.is_some_and(|r| r.dma) { 1 << 22 } else { 0 };
        if self.running() && (en & mask != mask || rst & mask != 0) {
            return Err("SPI2/SPI2_DMA gating or reset during a transfer is undocumented (S17)");
        }
        Ok(())
    }

    pub fn gate(&mut self, en: u32, rst: u32, reset_write: bool) {
        let reset = rst & (1 << 6) != 0;
        self.clocked = en & (1 << 6) != 0;
        if reset_write && reset { self.held = true; self.known = false; }
        if self.reset && !reset && self.held {
            if self.clocked {
                for f in FIELDS { self.regs[f.off / 4] = fields(self.regs[f.off / 4], f.mask, f.reset); }
                self.regs[0] &= !(USR | UPDATE);
                self.regs[0x30 / 4] = fields(self.regs[0x30 / 4], 0xe000_0003, 3);
                self.regs[0xe0 / 4] &= !(1 << 27);
                for off in 0x34 / 4..=0x44 / 4 { self.regs[off] &= !IRQ; }
                self.regs[W0..W0 + 16].fill(0);
                self.active = self.regs;
                self.dirty = false;
                self.set_at = None;
                self.set_bits = 0;
                self.known = true;
            }
            self.held = false;
        }
        self.reset = reset;
        // S17 lead decision: the distinct SPI2_DMA gate must also have a
        // firmware-held reset and a clocked release before a DMA transfer.
        let reset = rst & (1 << 22) != 0;
        self.dma_clocked = en & (1 << 22) != 0;
        if reset_write && reset { self.dma_held = true; self.dma_known = false; }
        if self.dma_reset && !reset && self.dma_held {
            self.dma_known = self.dma_clocked;
            self.dma_held = false;
        }
        self.dma_reset = reset;
    }

    fn accessible(&self) -> Result<(), &'static str> {
        if !self.clocked { return Err("SPI2 register access while the SYSTEM clock is off (S17)"); }
        if self.reset { return Err("SPI2 register access while SYSTEM reset is held (S17)"); }
        if !self.known { return Err("SPI2 ROM handoff state is unknown; hold reset and release it with the clock enabled (S17)"); }
        Ok(())
    }

    /// Whether `addr` is one of SPI2's registers (its read handler's block).
    pub(super) fn claims(addr: u32) -> bool { Self::offset(addr).is_some() }

    fn offset(addr: u32) -> Option<usize> {
        addr.checked_sub(0x6002_4000).filter(|&off| off < 0x1000).map(|off| off as usize)
    }

    fn mask(off: usize) -> Option<u32> {
        match off {
            0 => Some(USR | UPDATE | 0x3ffff),
            0x30 => Some(0xf83c_0003),
            0x34..=0x44 => Some(IRQ),
            0x98..=0xd4 => Some(u32::MAX),
            0xe0 => Some(0x1fc0_0f0f),
            _ => FIELDS.iter().filter(|f| f.off == off).map(|f| f.mask).reduce(|a, b| a | b),
        }
    }

    pub fn read(&self, addr: u32) -> Access<u32> {
        let off = Self::offset(addr)?;
        Some((|| {
            self.accessible()?;
            Self::mask(off).ok_or("SPI2 reserved register access is undocumented (Q01)")?;
            match off {
                0 => Err("SPI_UPDATE WT readback is undocumented (S06, Q01)"),
                0x30 => Err("SPI DMA_CONF WT readback and FIFO observation timing are undocumented (Q01, Q03)"),
                0x38 | 0x44 => Err("SPI interrupt WT alias readback is undocumented (Q01)"),
                0xe0 => Err("SPI_SOFT_RESET WT readback is undocumented (Q01)"),
                0x40 => Ok(fields(self.regs[off / 4], IRQ, self.regs[RAW] & self.regs[ENA])),
                _ => Ok(self.regs[off / 4]),
            }
        })())
    }

    pub fn write(&mut self, addr: u32, value: u32, tick: u64, pll: bool,
        pins: &mut Pins, gdma: &mut Gdma, ram: &[u8]) -> Access<()>
    {
        let off = Self::offset(addr)?;
        Some((|| {
            self.accessible()?;
            let mask = Self::mask(off).ok_or("SPI2 reserved register access is undocumented (Q01)")?;
            match off {
                0x3c => return Err("SPI INT_RAW write is undocumented; WTC is not W1C (C01)"),
                0x40 => return Err("SPI INT_ST is read-only (Q01)"),
                0x38 => {
                    if self.set_at == Some(tick) && value & self.set_bits & IRQ != 0 {
                        return Err("SPI simultaneous interrupt set/clear priority is undocumented (Q01)");
                    }
                    self.regs[RAW] &= !(value & IRQ);
                    return Ok(());
                }
                0x44 => { self.raise(value & IRQ, tick); return Ok(()); }
                0x34 => { self.regs[ENA] = fields(self.regs[ENA], IRQ, value); return Ok(()); }
                0x30 if value & 0xe000_0000 != 0 => return Err("SPI DMA_AFIFO_RST/BUF_AFIFO_RST/RX_AFIFO_RST outside the subset (S05)"),
                0xe0 if value & (1 << 27) != 0 => return Err("SPI_SOFT_RESET outside the subset (Appendix B)"),
                _ => (),
            }
            if self.running() { return Err("SPI configuration or USR write during a transfer is undocumented (S06)"); }
            if off == 0 {
                // Stage the configuration locally: a rejected USR leaves SPI
                // unchanged. G03 descriptor faults deliberately latch in GDMA.
                let conf = fields(self.regs[0], 0x3ffff, value);
                let dirty = self.dirty || conf != self.regs[0] || value & (USR | UPDATE) == 0;
                let mut active = self.active;
                if value & UPDATE != 0 { active = self.regs; active[0] = conf; }
                if value & USR != 0 {
                    if dirty && value & UPDATE == 0 {
                        return Err("configuration written without SPI_UPDATE (S06)");
                    }
                    let (run, cs) = self.prepare(tick, pll, pins, &active)?;
                    if run.dma {
                        if !self.dma_known || !self.dma_clocked || self.dma_reset {
                            return Err("SPI2_DMA needs clock enabled and a held-then-released reset (S17 lead decision)");
                        }
                        gdma.fetch(run.x.tx_bits as usize / 8, ram, tick)?;
                    }
                    self.running = Some(run);
                    pins.spi_select(cs, run.x.mode >= 2);
                    if !self.noted {
                        self.notes.push((tick, "GP-SPI2 start and completion overheads are not documented (Q03): modelled as zero"));
                        self.noted = true;
                    }
                }
                self.regs[0] = conf | (value & USR);
                self.active = active;
                self.dirty = dirty && value & UPDATE == 0;
            } else {
                // RO FIFO flags retain their reset state; they cannot be read
                // as a claim about the unmodelled cross-clock FIFO pipeline.
                let writable = if off == 0x30 { mask & !3 } else { mask };
                self.regs[off / 4] = fields(self.regs[off / 4], writable, value);
                self.dirty = true;
            }
            Ok(())
        })())
    }

    fn prepare(&self, tick: u64, pll: bool, pins: &Pins, r: &[u32; 64]) -> Result<(Running, u16), &'static str> {
        for f in FIELDS {
            if !f.supported && r[f.off / 4] & f.mask != f.reset { return Err(f.error); }
        }
        let (ctrl, clock, user, user1, user2, misc, gate) =
            (r[2], r[3], r[4], r[5], r[6], r[8], r[0xe8 / 4]);
        if gate & 2 == 0 { return Err("SPI_MST_CLK_ACTIVE is disabled (S08)"); }
        if gate & 4 != 0 && !pll {
            return Err("PLL_F80M availability with the CPU on XTAL is not modelled");
        }
        // S08/D-S1: use the selected module clock, not the contradictory
        // APB wording in CLOCK's field descriptions.
        let mut bit_ticks = if gate & 4 != 0 { 6 } else { 12 };
        if clock & (1 << 31) == 0 {
            let n = (clock >> 12) & 63;
            if n == 0 || clock & 63 != n || (clock >> 6) & 63 != (n + 1) / 2 - 1 {
                return Err("SPI_CLKCNT_L/H do not satisfy master settings (S08)");
            }
            bit_ticks *= (n + 1) * (((clock >> 18) & 15) + 1);
        }
        let lines = [if ctrl & (1 << 9) != 0 { 4 } else { 1 },
            if ctrl & (1 << 6) != 0 { 4 } else { 1 },
            if user & (1 << 13) != 0 { 4 } else { 1 }];
        let cmd_bits = if user & (1 << 31) != 0 {
            if user2 >> 28 != 7 { return Err("SPI_USR_COMMAND_BITLEN other than 7 is not modelled (lead decision D-S3)"); }
            8
        } else { 0 };
        let addr_bits = if user & (1 << 30) != 0 { ((user1 >> 27) + 1) as u8 } else { 0 };
        let tx_bits = if user & (1 << 27) != 0 { (r[7] & 0x3ffff) + 1 } else { 0 };
        if tx_bits % 8 != 0 { return Err("SPI_MS_DATA_BITLEN is not a whole number of bytes (S03)"); }
        if addr_bits % lines[1] != 0 { return Err("SPI address ends in a partial lane group (Q03)"); }
        let widths = [u32::from(cmd_bits), u32::from(addr_bits), tx_bits];
        let lanes = widths.iter().zip(lines).filter(|(bits, _)| **bits != 0)
            .map(|(_, lines)| lines).max().unwrap_or(0);
        match (ctrl >> 25) & 3 {
            1 | 3 => return Err("SPI_WR_BIT_ORDER LSB-first values are not modelled (D-S2)"),
            2 if lanes > 1 => return Err("SPI_WR_BIT_ORDER 2 with a quad phase is not modelled (lead decision D-S4)"),
            _ => (),
        }
        let dummy = if user & (1 << 29) != 0 { ((user1 & 255) + 1) as u16 } else { 0 };
        // S12 INFERRED, extended to the enabled parallel lane count.
        let clocks = u64::from(cmd_bits / lines[0]) + u64::from(addr_bits / lines[1])
            + u64::from(dummy) + u64::from(tx_bits / u32::from(lines[2]));
        if clocks == 0 { return Err("SPI transfer with no clocked phase is undocumented (Q03)"); }
        let half = u64::from(bit_ticks / 2);
        let setup = half * if user & (1 << 7) != 0 { 2 * u64::from((user1 >> 17) & 31) + 3 } else { 1 };
        let hold = half * if user & (1 << 6) != 0 { 2 * u64::from((user1 >> 22) & 31) + 3 } else { 1 };
        let t0 = tick.checked_add(setup).ok_or("SPI tick overflow")?;
        // S10 measures setup/hold to latch edges: N latch edges have N-1
        // complete periods between the first and the last.
        let t1 = t0.checked_add((clocks - 1) * u64::from(bit_ticks)).ok_or("SPI tick overflow")?;
        let rise = t1.checked_add(hold).ok_or("SPI tick overflow")?;
        let sck = pins.output_pad(101)?;
        let cs = pins.output_pad(110)?;
        let mut d = [None; 4];
        // S11, Table 30.5-5: WR_BIT_ORDER=0's first quad clock puts
        // B4/B5/B6/B7 on D/Q/WP/HD. Payload byte order remains unchanged.
        for (slot, signal) in d.iter_mut().zip([103, 102, 105, 104]).take(lanes as usize) {
            *slot = Some(pins.output_pad(signal)?);
        }
        let idle = misc & (1 << 29) != 0;
        let out = user & (1 << 9) != 0;
        let mode = match (idle, out) { (false, false) => 0, (false, true) => 1, (true, true) => 2, (true, false) => 3 };
        Ok((Running {
            x: SpiXfer { t0, t1, sck, d, bit_ticks, mode, lsb_first: false,
                cmd: (cmd_bits, if cmd_bits == 0 { 0 } else { (user2 & 255) as u16 }),
                addr: (addr_bits, if addr_bits == 0 { 0 } else { r[1] >> (32 - addr_bits) }),
                dummy, lines, tx_bits, rx_bits: 0, full_duplex: false },
            rise, delivered: false, dma: r[0x30 / 4] & (1 << 28) != 0,
        }, cs))
    }

    fn raise(&mut self, bits: u32, tick: u64) {
        self.regs[RAW] |= bits;
        if self.set_at != Some(tick) { self.set_bits = 0; }
        self.set_at = Some(tick);
        self.set_bits |= bits;
    }

    pub fn next_event(&self) -> Option<u64> {
        self.running.map(|run| if run.delivered { run.rise } else { run.x.t1 })
    }

    pub fn event<B: Board>(&mut self, tick: u64, board: &mut B, board_t0: u64,
        pins: &mut Pins, gdma: &mut Gdma, ram: &mut [u8], journal: &mut Journal) -> Result<(), &'static str>
    {
        let mut run = self.running.expect("SPI event requires a transfer");
        if !run.delivered {
            let mut x = run.x;
            x.t0 = board_t0.checked_add(x.t0).ok_or("SPI board tick overflow")?;
            x.t1 = board_t0.checked_add(x.t1).ok_or("SPI board tick overflow")?;
            let mut rx = Vec::new();
            if run.dma {
                board.spi(&x, &gdma.payload(ram), &mut rx)?;
                gdma.complete(ram, journal, tick);
            } else {
                let mut bytes = [0; 64];
                for (word, bytes) in self.active[W0..W0 + 16].iter().zip(bytes.chunks_exact_mut(4)) {
                    bytes.copy_from_slice(&word.to_le_bytes());
                }
                board.spi(&x, &CpuPayload { bytes: &bytes, len: x.tx_bits as usize / 8 }, &mut rx)?;
            }
            run.delivered = true;
            self.running = Some(run);
        } else {
            pins.spi_release();
            self.running = None;
            self.regs[0] &= !USR;
            self.raise(DONE, tick); // S14: completion at CS rise, after hold.
        }
        Ok(())
    }

    pub fn interrupt_sources(&self) -> u128 {
        u128::from(self.known && self.clocked && !self.reset && self.regs[RAW] & self.regs[ENA] & IRQ != 0) << 21
    }

    pub fn rehandoff(&mut self, gates: [u32; 2], junk: &mut Junk) {
        let notes = std::mem::take(&mut self.notes);
        let noted = self.noted;
        *self = Self::new(gates[0], gates[1], junk);
        self.notes = notes;
        self.noted = noted;
    }

    pub fn new_epoch(&mut self) { self.set_at = None; self.set_bits = 0; }
    pub fn take_notes(&mut self, out: &mut Vec<(u64, &'static str)>) { out.append(&mut self.notes); }
}

struct CpuPayload<'a> { bytes: &'a [u8; 64], len: usize }

impl Payload for CpuPayload<'_> {
    fn len(&self) -> usize { self.len }
    fn chunks(&self, f: &mut dyn FnMut(&[u8])) {
        // S04: byte zero upward, wrapping the sixteen W registers.
        let mut left = self.len;
        while left != 0 { let n = left.min(64); f(&self.bytes[..n]); left -= n; }
    }
}
