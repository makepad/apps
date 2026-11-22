//! I2S0 clock master and internally clocked RX slave. Register offsets in trm.txt:38047–38085, 38096–38099.
use fab_elec_mcu_emu::board::{Board, I2sFrame, I2sStandard, Res};

use super::{gdma::{AudioChunk, Gdma}, pins::Pins, Access};
use crate::bus::Journal;

// trm.txt:13407 (I2S0 aperture), 28820–28854, 28993–29060 (SYSTEM clock/reset).
const BASE: u32 = 0x6000_f000;
const GATE: u32 = 1 << 4;
const RX: usize = 0;
const TX: usize = 1;
const GRID: u64 = 480_000_000; // M: S3Machine's board ticks per second.

// trm.txt:38196–38288, 38535–38637, 38646–38650.
const RESET: u32 = 1;
const FIFO_RESET: u32 = 2;
const START: u32 = 1 << 2;
const SLAVE: u32 = 1 << 3;
const BIG_ENDIAN: u32 = 1 << 7;
const UPDATE: u32 = 1 << 8;
const BYPASS: u32 = 1 << 12;
const TX_STOP: u32 = 1 << 13;
const LEFT_ALIGN: u32 = 1 << 15;
const TDM: u32 = 1 << 19;
const PDM: u32 = 1 << 20;
const LOOPBACK: u32 = 1 << 27;

// trm.txt:38304–38339, 38651–38691.
const MSB_SHIFT: u32 = 1 << 29;
const NO_BCK_DELAY: u32 = 1 << 30;
// trm.txt:38345–38370, 38697–38721.
const CLK_ACTIVE: u32 = 1 << 26;
const CLK_SELECT: u32 = 3 << 27;
const MCLK_SELECT: u32 = 1 << 29;
const CLK_EN: u32 = 1 << 29;
const PDM_UNKNOWN: &str = "I2S_TX_PCM2PDM_CONF1_REG bits [25:20] have no documented reset value (trm.txt:38404)";

type Hz = (u64, u64);

fn ratio(n: u64, d: u64) -> Hz {
    let (mut a, mut b) = (n, d);
    while b != 0 { (a, b) = (b, a % b); }
    (n / a, d / a)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Config {
    conf: u32,
    fmt: u32,
    clkm: u32,
    div: u32,
    tdm: u32,
}

impl Config {
    fn reset(dir: usize) -> Self {
        Self {
            // trm.txt:38219–38263, 38560–38608.
            conf: LEFT_ALIGN | BYPASS | (1 << 9) | if dir == TX { TX_STOP } else { 1 << 10 },
            // trm.txt:38312–38326, 38660–38676.
            fmt: MSB_SHIFT | if dir == TX { NO_BCK_DELAY } else { 0 }
                | (15 << 24) | (15 << 18) | (15 << 13) | (6 << 7),
            clkm: 2, // trm.txt:38351–38365, 38704–38717.
            div: 1 << 9, // trm.txt:38837–38853, 38941–38957.
            tdm: 0xffff, // trm.txt:38440–38476, 38750–38785.
        }
    }

    fn clocks(&self, pll: bool) -> Res<(Hz, Hz)> {
        // trm.txt:37470–37494, 38365–38370, 38717–38721.
        // M: use PLL_F160M only while the bus confirms PLL operation (trm.txt:18202–18204).
        if self.clkm & CLK_ACTIVE == 0 { return Err("I2S unit clock is disabled"); }
        let source = match (self.clkm & CLK_SELECT) >> 27 {
            0 => 40_000_000,
            2 if pll => 160_000_000,
            2 => return Err("I2S PLL_F160M availability is unknown while the CPU uses XTAL"),
            // trm.txt:18205–18206: PLL_D2 depends on the PLL frequency. The bus supplies only pll/xtal.
            1 => return Err("I2S PLL_D2 source requires the PLL frequency"),
            _ => return Err("I2S external MCLK input is unsupported"),
        };
        let n = match u64::from(self.clkm & 255) { 0 => 256, 1 => 2, n => n };
        // trm.txt:37495–37506, 37512–37516, 38837–38864, 38941–38968.
        let x = u64::from((self.div >> 18) & 511);
        let y = u64::from((self.div >> 9) & 511);
        let z = u64::from(self.div & 511);
        let complement = self.div & (1 << 27) != 0;
        let (a, b) = if x == 0 && y == 1 && z == 0 {
            (1, 0)
        } else {
            let a = (x + 1) * z + y;
            if z == 0 || y >= z || 2 * z > a || (complement && 2 * z == a) {
                return Err("I2S fractional divider is outside the TRM X/Y/Z/YN1 encoding");
            }
            (a, if complement { a - z } else { z })
        };
        // trm.txt:37517–37536: BCK divisor is the field plus one, field 1 prohibited.
        let bck = u64::from((self.fmt >> 7) & 63);
        if bck == 1 { return Err("I2S BCK_DIV_NUM must not be 1"); }
        let mclk = ratio(source * a, n * a + b);
        Ok((mclk, ratio(mclk.0, mclk.1 * (bck + 1))))
    }

    fn format(&self, dir: usize) -> Res<(I2sStandard, u8, u8)> {
        if self.conf & TDM == 0 { return Err("I2S requires TDM enabled for serial audio"); }
        // trm.txt:37678–37682, 37862–37871, 38517–38518, 38826–38831.
        if self.tdm >> 16 & 15 != 1 { return Err("I2S frame requires exactly two TDM slots"); }
        if self.tdm & 3 != 3 { return Err("I2S masked channels are unsupported"); }
        // trm.txt:38327–38339, 38677–38691, 37695, 37877. HALF_SAMPLE_BITS is a count.
        let slot = ((self.fmt >> 24) & 31) as u8 + 1;
        let data = ((self.fmt >> 13) & 31) as u8 + 1;
        if !matches!(data, 8 | 16 | 24 | 32) { return Err("I2S valid data width must be 8, 16, 24 or 32"); }
        if (self.fmt >> 18) & 63 != u32::from(slot) { return Err("I2S WS period must span two complete slots"); }
        if data > slot || (data < slot && self.conf & LEFT_ALIGN == 0) {
            return Err("I2S truncated or right-aligned sub-slot samples are unsupported");
        }
        // trm.txt:37603–37613, 37650–37657, 37932–37952. In a 24-bit slot
        // padded TX sends the low 24 bits; RX zero-extends them to a word.
        if data == 24 && slot != data && self.conf & (1 << 16) != 0 {
            return Err("I2S padded 24-bit DMA in a wider wire slot is unsupported");
        }
        if self.fmt & 127 != u32::from(slot - 1) { return Err("I2S WS width must equal one slot for Philips/MSB alignment"); }
        if dir == TX && self.fmt & NO_BCK_DELAY == 0 { return Err("I2S delayed TX BCK edges are unsupported"); }
        // trm.txt:37420–37433, 37439–37449. Companding/PCM is refused at the control write.
        let standard = if self.fmt & MSB_SHIFT != 0 { I2sStandard::Philips } else { I2sStandard::MsbAlignment };
        Ok((standard, slot, data))
    }

    fn sample_bytes(&self, bits: u8) -> usize {
        if bits == 24 && self.conf & (1 << 16) != 0 { 4 } else { usize::from(bits / 8) }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Unit {
    apb: Config,
    synced: Option<Config>,
    update_due: Option<u64>,
    reset_pulses: u32,
    start: bool,
    running: bool,
}

impl Unit {
    fn new(dir: usize) -> Self {
        Self { apb: Config::reset(dir), synced: None, update_due: None, reset_pulses: 0, start: false, running: false }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PendingFrame {
    tx: Option<AudioChunk>,
    rx: Option<AudioChunk>,
    samples: [i32; 2],
    rx_eof: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stream {
    frame: I2sFrame,
    driver: usize,
    // M: first edge one BCK after START. Ceil absolute edges on the machine grid;
    // trm.txt:37515–37518 gives average clocks and no fractional jitter waveform.
    origin: u64,
    edge: u64,
    due: u64,
    participating: [bool; 2],
    pending: Option<PendingFrame>,
}

impl Stream {
    fn join(&mut self, dir: usize, mut frame: I2sFrame, before_edge: bool) -> Res {
        frame.t = self.frame.t;
        frame.din = frame.din.or(self.frame.din);
        frame.dout = frame.dout.or(self.frame.dout);
        let mut other = self.frame;
        other.din = frame.din;
        other.dout = frame.dout;
        if other != frame { return Err("I2S duplex clocks, pads and slot formats must agree"); }
        if frame.din == frame.dout { return Err("I2S duplex requires distinct data pads"); }
        self.frame = frame;
        self.participating[dir] = before_edge;
        Ok(())
    }

    fn time(&self, edge: u64) -> Res<u64> {
        let (hz, den) = self.frame.bclk_hz;
        let ticks = (u128::from(edge) * u128::from(GRID) * u128::from(den)).div_ceil(u128::from(hz));
        u64::try_from(ticks).ok().and_then(|n| self.origin.checked_add(n)).ok_or("I2S frame deadline overflow")
    }

    fn set_window(&mut self, tick: u64) -> Res {
        self.frame.t = tick;
        self.edge = self.edge.checked_add(2 * u64::from(self.frame.slot_bits)).ok_or("I2S edge counter overflow")?;
        self.due = self.time(self.edge)?;
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct I2s {
    units: [Unit; 2],
    clocked: bool,
    reset: bool,
    held: bool,
    known: bool,
    raw: u32,
    ena: u32,
    hung: u32,
    eof: u32,
    rx_count: u32,
    single: u32,
    date: u32,
    stream: Option<Stream>,
    last_tx: Option<[i32; 2]>,
    raised: Option<(u64, u32)>,
}

impl I2s {
    pub fn new(en: u32, rst: u32) -> Self {
        Self {
            units: [Unit::new(RX), Unit::new(TX)], clocked: en & GATE != 0, reset: rst & GATE != 0,
            held: false, known: false, raw: 0, ena: 0, stream: None, last_tx: None, raised: None,
            // trm.txt:38101–38143, 38149–38190, 39024–39037, 38519–38529, 39056–39079.
            hung: 0x810, eof: 0x40, rx_count: 0, single: 0, date: 0x0200_9070,
        }
    }

    pub fn busy(&self) -> bool { self.next_event().is_some() }
    pub fn frame_active(&self) -> bool { self.stream.is_some() }

    pub fn check_gate(&self, en: u32, rst: u32) -> Res {
        if self.busy() && (en & GATE == 0 || rst & GATE != 0) { return Err("I2S clock gating/reset during activity is unsupported"); }
        Ok(())
    }

    pub fn gate(&mut self, en: u32, rst: u32, reset_write: bool) {
        let reset = rst & GATE != 0;
        // M: as for SPI2, ROM handoff is unknown until a clocked release of firmware-held reset.
        if self.reset && !reset && self.held {
            *self = Self::new(en, rst);
            self.known = self.clocked;
        }
        if reset_write && reset { self.held = true; self.known = false; }
        self.reset = reset;
        self.clocked = en & GATE != 0;
    }

    pub fn new_epoch(&mut self) { self.raised = None; }

    fn accessible(&self) -> Res {
        if !self.clocked { return Err("I2S0 SYSTEM clock is disabled"); }
        if self.reset { return Err("I2S0 SYSTEM reset is held"); }
        if !self.known { return Err("I2S0 needs a clocked release of SYSTEM reset after ROM handoff"); }
        Ok(())
    }

    fn offset(addr: u32) -> Option<u32> { addr.checked_sub(BASE).filter(|&off| off < 0x1000) }
    pub fn claims(addr: u32) -> bool { Self::offset(addr).is_some() }

    pub fn timing_note(addr: u32, value: u32) -> Option<&'static str> {
        if !matches!(Self::offset(addr), Some(0x20 | 0x24)) { return None; }
        if value & UPDATE != 0 {
            Some("I2S UPDATE latency unspecified: one destination clock rounded up (trm.txt:38271, 38619)")
        } else if value & START != 0 {
            Some("I2S first edge one BCK after START, average fractional clock rounded up on the tick grid; DMA retires at full-frame boundaries (trm.txt:37515, 37559, 37856)")
        } else { None }
    }

    pub fn read(&self, addr: u32) -> Access<u32> {
        let off = Self::offset(addr)?;
        if off == 0x44 { return Some(Err(PDM_UNKNOWN)); }
        Some(self.accessible().and_then(|()| {
            let u = &self.units[((off >> 2) & 1) as usize];
            Ok(match off {
                0x0c => self.raw,
                0x10 => self.raw & self.ena,
                0x14 => self.ena,
                0x18 => return Err("I2S interrupt-clear register is write-trigger only"),
                // M: the two WT pulses have no stored state in a mixed R/W read (trm.txt:57935–57938).
                0x20 | 0x24 => u.apb.conf | u32::from(u.start) * START | u32::from(u.update_due.is_some()) * UPDATE,
                0x28 | 0x2c => u.apb.fmt,
                0x30 | 0x34 => u.apb.clkm,
                0x38 | 0x3c => u.apb.div,
                0x40 => 2 << 1, // trm.txt:38371–38398.
                0x50 | 0x54 => u.apb.tdm,
                0x58 | 0x5c => 0, // trm.txt:38870–38935, 38974–39023: delay bypass.
                0x60 => self.hung,
                0x64 => self.eof,
                0x68 => self.single,
                0x6c => u32::from(!self.units[TX].running), // trm.txt:39062–39070.
                0x80 => self.date,
                _ => return Err("I2S0 reserved register read"),
            })
        }))
    }

    fn conf_fields(&self, dir: usize, value: u32) -> Res {
        // trm.txt:38196–38288, 38296–38303, 38535–38637, 38646–38650.
        let mask = if dir == TX { 0x0f1f_bfef } else { 0x007f_ffaf };
        if value & !mask != 0 { return Err("I2S control write changes reserved bits"); }
        if value & PDM != 0 { return Err("I2S PDM mode is unsupported"); }
        if dir == TX && value & SLAVE != 0 { return Err("I2S TX slave mode is unsupported"); }
        // DERIVED: RX waits for master BCK (§28.8.2, trm.txt:37575-37582)
        // and SIG_LOOPBACK shares WS and BCK (38649-38650). NOT FOUND: an
        // explicit requirement to put RX in slave mode for loopback.
        if dir == RX && value & SLAVE != 0 && !self.shared_clocks() {
            return Err("I2S RX slave requires synchronized TX SIG_LOOPBACK");
        }
        if value & BYPASS == 0 { return Err("I2S PCM A-law/mu-law conversion is unsupported"); }
        if value & (1 << 5) != 0 { return Err("I2S mono mode is unsupported"); }
        if value & (1 << 17) != 0 { return Err("I2S inverted WS polarity is unsupported"); }
        if value & (1 << 18) != 0 { return Err("I2S reversed data bit order is unsupported"); }
        if dir == RX && value & (3 << 21) != 0 { return Err("I2S PDM receive conversion is unsupported"); }
        if dir == RX && ((value >> 13) & 3) >= 2 { return Err("I2S RX FIFO-full/reserved stop mode is unsupported"); }
        if dir == TX && value & (7 << 24) != 0 { return Err("I2S PDM transmit channel modes are unsupported"); }
        Ok(())
    }

    pub fn write(&mut self, addr: u32, value: u32, tick: u64, pll: bool, pins: &Pins, gdma: &Gdma) -> Access<()> {
        let off = Self::offset(addr)?;
        if off == 0x44 { return Some(Err(PDM_UNKNOWN)); }
        Some(self.accessible().and_then(|()| {
            let dir = ((off >> 2) & 1) as usize;
            match off {
                // trm.txt:38118–38122, 38140–38143, 38166–38190, 57935–57938.
                0x0c | 0x10 | 0x6c => return Err("I2S write to read-only register"),
                0x14 | 0x18 => {
                    if value & !15 != 0 { return Err("I2S interrupt write changes reserved bits"); }
                    if off == 0x14 { self.ena = value; }
                    else {
                        if self.raised.is_some_and(|(t, bits)| tick == t && bits & value != 0) {
                            return Err("I2S simultaneous interrupt set and clear priority is undocumented");
                        }
                        self.raw &= !value;
                    }
                }
                0x20 | 0x24 => return self.control(dir, value, tick, pll, pins, gdma),
                0x28..=0x3c | 0x50 | 0x54 => {
                    if off == 0x34 && self.units[RX].update_due.is_some()
                        && (self.units[TX].apb.clkm ^ value) & CLK_EN != 0 {
                        return Err("I2S module clock gate changed during RX synchronization");
                    }
                    let u = &mut self.units[dir];
                    if u.update_due.is_some() || u.running || self.stream.is_some() {
                        return Err("I2S configuration during synchronization or a frame is unsupported");
                    }
                    match off {
                        0x28 | 0x2c => {
                            let mask = if dir == TX { 0x7fff_ffff } else { 0x3fff_ffff };
                            if value & !mask != 0 { return Err("I2S CONF1 reserved bits"); }
                            if value >> 7 & 63 == 1 { return Err("I2S BCK_DIV_NUM must not be 1"); }
                            u.apb.fmt = value;
                        }
                        0x30 | 0x34 => {
                            if value & !(255 | CLK_ACTIVE | CLK_SELECT | CLK_EN) != 0 { return Err("I2S CLKM reserved bits"); }
                            if value & CLK_SELECT == CLK_SELECT { return Err("I2S external MCLK input is unsupported"); }
                            if value & CLK_SELECT == 1 << 27 { return Err("I2S PLL_D2 source requires the PLL frequency"); }
                            u.apb.clkm = value;
                        }
                        0x38 | 0x3c => {
                            if value >> 28 != 0 { return Err("I2S fractional divider reserved bits"); }
                            u.apb.div = value;
                        }
                        _ => {
                            let mask = if dir == TX { 0x1f_ffff } else { 0xf_ffff };
                            if value & !mask != 0 { return Err("I2S TDM control reserved bits"); }
                            if value >> 16 & 15 > 1 { return Err("I2S TDM with more than two slots is unsupported"); }
                            u.apb.tdm = value;
                        }
                    }
                }
                0x40 if value == 2 << 1 => (),
                0x40 => return Err("I2S TX PCM-to-PDM configuration is unsupported"),
                0x58 | 0x5c if value == 0 => (),
                0x58 | 0x5c => return Err("I2S nondefault edge delays or timing-register reserved bits"),
                0x60 if value <= 0xfff => self.hung = value, // trm.txt:39024–39050 (slave timeout).
                0x64 if value <= 0xfff && !self.units[RX].running => self.eof = value,
                0x64 if self.units[RX].running => return Err("I2S RX EOF change while receiving"),
                0x68 => self.single = value, // trm.txt:37688–37691: unused when both channels enabled.
                0x80 if value < 1 << 28 => self.date = value,
                _ => return Err("I2S0 reserved register or field write"),
            }
            Ok(())
        }))
    }

    fn control(&mut self, dir: usize, value: u32, tick: u64, pll: bool, pins: &Pins, gdma: &Gdma) -> Res {
        self.conf_fields(dir, value)?;
        let u = &self.units[dir];
        let conf = value & !(START | UPDATE | RESET | FIFO_RESET);
        let slave = dir == RX && conf & SLAVE != 0;
        if dir == TX && conf & LOOPBACK == 0 && self.units[RX].running && self.units[RX].apb.conf & SLAVE != 0 {
            return Err("I2S TX cannot remove the clock route of an armed RX slave");
        }
        let clock = if slave { &self.units[TX].apb } else { &u.apb };
        let start = value & START != 0;
        let pulses = value & (RESET | FIFO_RESET);
        if u.update_due.is_some() { return Err("I2S control write before UPDATE cleared"); }
        // M: trm.txt:38721 calls CLK_EN the module clock gate; require it for both clock domains.
        let clock_gate = self.units[TX].apb.clkm & CLK_EN != 0;
        if value & UPDATE != 0 {
            if self.stream.is_some() || u.running || start || pulses != 0 { return Err("I2S UPDATE overlaps START, a reset pulse or an active frame"); }
            if conf & TDM == 0 { return Err("I2S UPDATE requires one enabled data mode (trm.txt:37676, 37861)"); }
            if !clock_gate { return Err("I2S module CLK_EN is clear during UPDATE"); }
            let (mclk, _) = clock.clocks(pll)?;
            // M: synchronization completes on the late side of one destination-clock period.
            // trm.txt:38271–38272, 38619–38620 specify completion, without a latency.
            let delay = (GRID * mclk.1).div_ceil(mclk.0);
            let due = tick.checked_add(delay).ok_or("I2S UPDATE deadline overflow")?;
            let u = &mut self.units[dir];
            u.apb.conf = conf;
            u.start = false;
            u.update_due = Some(due);
            u.reset_pulses = 0;
            return Ok(());
        }
        if pulses != 0 {
            // trm.txt:37539–37543, 37990–37997, 38017–38026, 57935–57936.
            if start || u.running { return Err("I2S unit/FIFO reset during START is unsupported"); }
            if !clock_gate { return Err("I2S module CLK_EN is clear during unit/FIFO reset"); }
            clock.clocks(pll)?;
        }
        let change = conf ^ u.apb.conf;
        if (u.running || self.stream.is_some()) && change & !(if dir == TX { TX_STOP } else { 0 }) != 0 {
            return Err("I2S format or loopback change during an active frame");
        }
        if start && !u.start {
            if u.running { return Err("I2S restart before the previous frame completed"); }
            if self.units[1 - dir].update_due.is_some() { return Err("I2S START overlaps peer synchronization"); }
            // trm.txt:38005–38007 explicitly permits changing TX_STOP_EN after UPDATE.
            let mut active = u.synced.ok_or("I2S START requires UPDATE")?;
            if dir == TX { active.conf = (active.conf & !TX_STOP) | (conf & TX_STOP); }
            let mut requested = u.apb;
            requested.conf = conf;
            if requested != active { return Err("I2S START with unsynchronized configuration"); }
            if u.reset_pulses != (RESET | FIFO_RESET) { return Err("I2S START requires unit and FIFO reset after UPDATE"); }
            if !clock_gate { return Err("I2S module CLK_EN is clear during START"); }
            if dir == RX && self.eof & 1 == 0 { return Err("I2S RX EOF inside a two-slot frame is unsupported"); }
            gdma.audio_ready(dir == TX)?;
            if let Some(mut stream) = self.stream {
                if !self.shared_clocks() { return Err("I2S duplex requires SIG_LOOPBACK for shared WS/BCK"); }
                if !slave && active.clocks(pll)? != self.units[stream.driver].apb.clocks(pll)? {
                    return Err("I2S shared clock units require matching MCLK and BCK");
                }
                // M: a direction enabled after the first edge joins at the next complete WS period.
                stream.join(dir, self.frame(dir, &active, pins, pll)?, tick < stream.frame.t)?;
                self.stream = Some(stream);
            } else if !slave {
                let frame = self.frame(dir, &active, pins, pll)?;
                let mut stream = Stream { frame, driver: dir, origin: tick, edge: 1, due: 0, participating: [false; 2], pending: None };
                stream.participating[dir] = true;
                stream.due = stream.time(stream.edge)?;
                stream.frame.t = stream.due;
                // An armed slave waited without scheduling frames. The master's
                // first BCK starts its first complete WS period (TRM §28.8.2, 37575-37582).
                if dir == TX && self.units[RX].running {
                    stream.join(RX, self.frame(RX, &self.units[RX].apb, pins, pll)?, true)?;
                }
                self.stream = Some(stream);
            }
            self.units[dir].running = true;
            if dir == RX { self.rx_count = 0; } else { self.last_tx = None; }
        } else if !start && u.start && u.running {
            // M: clearing START drains only a frame whose first edge already occurred;
            // partial-slot stop timing is unspecified (trm.txt:37565, 37579).
            if let Some(s) = &mut self.stream {
                if tick < s.frame.t || !s.participating[dir] {
                    if s.driver == TX && dir == TX && self.units[RX].apb.conf & SLAVE != 0 {
                        s.participating[RX] = false;
                    } else if s.driver == dir && self.units[1 - dir].running {
                        return Err("I2S shared clock driver stopped before the first frame");
                    }
                    self.units[dir].running = false;
                    s.participating[dir] = false;
                }
            } else { self.units[dir].running = false; }
            if self.stream.is_some_and(|s| !self.units[s.driver].running) { self.stream = None; }
        }
        let u = &mut self.units[dir];
        u.apb.conf = conf;
        u.reset_pulses |= pulses;
        u.start = start;
        if pulses != 0 {
            if dir == RX { self.rx_count = 0; } else { self.last_tx = None; }
        }
        Ok(())
    }

    fn shared_clocks(&self) -> bool {
        self.units[TX].synced.is_some_and(|cfg| cfg.conf & LOOPBACK != 0)
    }

    fn frame(&self, dir: usize, active: &Config, pins: &Pins, pll: bool) -> Res<I2sFrame> {
        let slave = dir == RX && active.conf & SLAVE != 0;
        let (_, bclk_hz) = if slave { self.units[TX].apb.clocks(pll)? } else { active.clocks(pll)? };
        let (standard, slot_bits, data_bits) = active.format(dir)?;
        // trm.txt:37397–37416, 38369–38370, 38649–38650.
        // A slave always follows TX. With two masters the first START owns the
        // physical pair; the manual specifies no arbitration in that case.
        let driver = self.stream.map_or(if slave { TX } else { dir }, |s| s.driver);
        let (bclk, ws) = pins.i2s_clocks(driver == TX, self.shared_clocks())?;
        let mclk = pins.i2s_output(23)?;
        let mclk_hz = if mclk.is_some() {
            let source = if self.units[RX].apb.clkm & MCLK_SELECT != 0 { RX } else { TX };
            Some(self.units[source].apb.clocks(pll)?.0)
        } else { None };
        // trm.txt:16185–16187, 16205–16207. An unrouted SD output has no board pad.
        let dout = if dir == TX { pins.i2s_output(25)? } else { None };
        let din = if dir == RX { Some(pins.i2s_input()?) } else { None };
        Ok(I2sFrame { t: 0, bclk, ws, mclk, dout, din, mclk_hz, bclk_hz, standard, slot_bits, data_bits, tx: [0; 2] })
    }

    fn done(&mut self, dir: usize, tick: u64) {
        // trm.txt:38045–38046, 38107–38119. M: DONE at the last complete frame.
        self.units[dir].running = false;
        let bits = 1 << dir;
        self.raw |= bits;
        let prev = self.raised.filter(|&(t, _)| t == tick).map_or(0, |(_, bits)| bits);
        self.raised = Some((tick, prev | bits));
    }

    pub fn next_event(&self) -> Option<u64> {
        self.units.iter().filter_map(|u| u.update_due).chain(self.stream.map(|s| s.due)).min()
    }

    pub fn interrupt_sources(&self) -> u128 {
        // trm.txt:18495, 38140–38143: CORE0 I2S0 level is the masked status.
        if self.known && self.clocked && !self.reset && self.raw & self.ena != 0 { 1 << 25 } else { 0 }
    }

    pub fn event<B: Board>(&mut self, tick: u64, board: &mut B, board_t0: u64, gdma: &mut Gdma,
        ram: &mut [u8], journal: &mut Journal) -> Res
    {
        for u in &mut self.units {
            if u.update_due == Some(tick) {
                u.synced = Some(u.apb);
                u.update_due = None;
                return Ok(());
            }
        }
        let mut stream = self.stream.expect("I2S frame is due");
        if let Some(pending) = stream.pending.take() {
            self.finish_frame(&mut stream, pending, gdma, ram, journal, tick)?;
            if !stream.participating.contains(&true) {
                self.stream = None;
                return Ok(());
            }
        }
        stream.set_window(tick)?;
        let mut frame = stream.frame;
        frame.t = board_t0.checked_add(frame.t).ok_or("I2S board timestamp overflow")?;
        let mut rx_eof = false;
        // M: present the frame on its first edge; publish received bytes at its end.
        // trm.txt:37594–37599, 37856–37857: FIFO/DMA ordering; no bus service latency is given.
        let rx = if stream.participating[RX] {
            let bytes = self.units[RX].apb.sample_bytes(frame.data_bits);
            rx_eof = self.rx_count + 2 == self.eof + 1; // trm.txt:38527–38529, count channel samples.
            Some(gdma.audio_chunk(false, 2 * bytes)?)
        } else { frame.din = None; None };
        let tx = if stream.participating[TX] && gdma.audio_available(true) {
            let bytes = self.units[TX].apb.sample_bytes(frame.data_bits);
            let chunk = gdma.audio_chunk(true, 2 * bytes)?;
            let data = &ram[chunk.offset..chunk.offset + 2 * bytes];
            // trm.txt:37614–37634, 37665–37670: channel 0 then 1, little/big endian.
            let big = self.units[TX].apb.conf & BIG_ENDIAN != 0;
            for (sample, data) in frame.tx.iter_mut().zip(data.chunks_exact(bytes)) {
                let mut word = 0u32;
                for (i, &b) in data.iter().enumerate() {
                    word |= u32::from(b) << (8 * if big { bytes - 1 - i } else { i });
                }
                *sample = ((word << (32 - frame.data_bits)) as i32) >> (32 - frame.data_bits);
            }
            Some(chunk)
        } else {
            if stream.participating[TX] {
                // trm.txt:37559–37565: continuous TX repeats the last complete frame.
                frame.tx = self.last_tx.ok_or("I2S TX underrun before the first complete frame")?;
            } else { frame.dout = None; }
            None
        };
        let shifted = frame.tx;
        if frame.dout.is_none() { frame.tx = [0; 2]; }
        let samples = board.i2s(&frame)?;
        if rx.is_some() && samples.iter().any(|&v| (v << (32 - frame.data_bits)) >> (32 - frame.data_bits) != v) {
            return Err("I2S board supplied a sample outside the signed data width");
        }
        if tx.is_some() { self.last_tx = Some(shifted); }
        stream.pending = Some(PendingFrame { tx, rx, samples, rx_eof });
        self.stream = Some(stream);
        Ok(())
    }

    fn finish_frame(&mut self, stream: &mut Stream, pending: PendingFrame, gdma: &mut Gdma,
        ram: &mut [u8], journal: &mut Journal, tick: u64) -> Res
    {
        if let Some(chunk) = pending.rx {
            let bytes = self.units[RX].apb.sample_bytes(stream.frame.data_bits);
            let big = self.units[RX].apb.conf & BIG_ENDIAN != 0;
            let len = bytes * 2;
            journal.touch(ram, chunk.offset, len);
            // trm.txt:37923–37931, 37945–37956, 37962–37974: store low valid bits in channel order.
            for (dst, sample) in ram[chunk.offset..chunk.offset + len].chunks_exact_mut(bytes).zip(pending.samples) {
                let sample = sample as u32 & (u32::MAX >> (32 - stream.frame.data_bits));
                for (i, b) in dst.iter_mut().enumerate() {
                    *b = (sample >> (8 * if big { bytes - 1 - i } else { i })) as u8;
                }
            }
            self.rx_count = if pending.rx_eof { 0 } else { self.rx_count + 2 };
        }
        gdma.audio_commit([pending.rx, pending.tx], pending.rx_eof, ram, journal, tick)?;
        for dir in [RX, TX] {
            let u = &self.units[dir];
            let empty_stop = dir == TX && stream.participating[TX] && u.apb.conf & TX_STOP != 0 && !gdma.audio_available(true);
            let eof_stop = dir == RX && pending.rx_eof && u.apb.conf & (1 << 13) != 0; // trm.txt:38279–38281.
            if u.running && (!u.start || empty_stop || eof_stop) { self.done(dir, tick); }
            stream.participating[dir] = self.units[dir].running;
        }
        if !stream.participating[stream.driver] && stream.participating.contains(&true) {
            if stream.driver == TX && self.units[RX].apb.conf & SLAVE != 0 {
                stream.participating[RX] = false; // START stays armed until TX supplies BCK again.
            } else { return Err("I2S shared clock driver stopped while the other unit still requests frames"); }
        }
        Ok(())
    }
}
