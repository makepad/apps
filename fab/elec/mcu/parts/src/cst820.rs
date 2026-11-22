//! Assumed CST820, using only CST816-family facts T01–T19 and O04.
//! No CST820 document was supplied. Unknown registers and behaviours are refused.

use std::collections::VecDeque;

use fab_elec_mcu_emu::board::{I2cAns, I2cOp, Payload, Res, SpiXfer};
use fab_elec_mcu_emu::{Drive, Pull};

use crate::{Cx, I2cTarget, Model, Stimulus, TargetOp};

const WIDTH: usize = 0xed; // T10: source spelling is IrqPluseWidth.
const PERIOD: usize = 0xee; // T12: NorScanPer.
const SLEEP: usize = 0xf9; // T12: AutoSleepTime.
const IRQ: usize = 0xfa; // T11: IrqCtl.
const AUTO_RESET: usize = 0xfb; // T14.
const LONG_PRESS: usize = 0xfc; // T14.
const IO: usize = 0xfd; // T13: IOCtl, complete reset byte unknown.
const DIS_SLEEP: usize = 0xfe; // T09.

// Res has static errors; keep the offending register without per-access allocation.
static REGISTER_ERRORS: [[u8; b"CST820: register 0x00 undocumented or invalid access (T03-T15)".len()]; 256] = {
    let mut errors = [*b"CST820: register 0x00 undocumented or invalid access (T03-T15)"; 256];
    let hex = b"0123456789ABCDEF";
    let mut reg = 0;
    while reg < 256 {
        errors[reg][b"CST820: register 0x".len()] = hex[reg >> 4];
        errors[reg][b"CST820: register 0x0".len()] = hex[reg & 15];
        reg += 1;
    }
    errors
};

fn register_error(reg: u8) -> &'static str {
    std::str::from_utf8(&REGISTER_ERRORS[usize::from(reg)]).expect("register errors are ASCII")
}

fn documented(reg: u8) -> bool {
    matches!(reg, 0x02..=0x06 | 0xed..=0xee | 0xf9..=0xfe) // T03–T14; T08 IDs have no values.
}

#[derive(Clone, Copy, Debug)]
enum Note {
    Family, Startup, Defaults, AutoReset, Framing, Coordinates, Initial,
    Scan, Queue, Interrupt, Overlap, Reset, ShortReset, Sleep, Wake,
    LongPress, Period, Config, Io,
}

impl Note {
    fn text(self) -> &'static str {
        match self {
            Self::Family => "CST820: Assumed CST816-family address framing, register meanings and documented defaults (T01, T03-T14, O04)",
            Self::Startup => "CST820: Assumed exactly 100 ms initialization from tick zero or reset release, rounded up to ticks (T18)",
            Self::Defaults => "CST820: Assumed IrqCtl resets to 0; firmware must enable interrupt pulses (T11)",
            Self::AutoReset => "CST820: Assumed AutoReset resets to 0; gesture-dependent reset is not modelled (T14)",
            Self::Framing => "CST820: Assumed register-pointer byte, persistence across STOP and repeated START, and consecutive byte increment without wrap (T02)",
            Self::Coordinates => "CST820: Assumed visible-glass pixels, origin top-left, axes aligned with the panel's display coordinates; coordinate upper nibbles are zero, not event bits (T04-T07, T15, O04)",
            Self::Initial => "CST820: Assumed no finger until a host touch; coordinate reads require a sampled stimulus, including after reset (T03-T07)",
            Self::Scan => "CST820: Assumed touch updates only at scans, first at initialization end; deadlines round up to ticks (T12, T17)",
            Self::Queue => "CST820: Assumed consecutive moves coalesce; every down/up transition occupies a separate scan and survives reset (T15-T17)",
            Self::Interrupt => "CST820: Assumed EnTouch pulses each touched scan and EnChange each changed scan; INT otherwise high-Z and needs an external pull-up (T10-T11, T16, D-T1)",
            Self::Overlap => "CST820: Assumed overlapping INT pulses remain low until the latest pulse ends (T16)",
            Self::Reset => "CST820: Assumed active-low RST pulse of at least 0.1 ms resets, treating the typical value as minimum (T18, D-T2)",
            Self::ShortReset => "CST820: Assumed RST pulse shorter than 0.1 ms is ignored (T18, D-T2)",
            Self::Sleep => "CST820: Assumed no-finger timeout starts at initialization end or the release scan; standby refuses I2C (T09, T12, T17)",
            Self::Wake => "CST820: Assumed standby touch wakes at the next normal-period scan, without an additional wake delay (T16-T17)",
            Self::LongPress => "CST820: Assumed LongPressTime counts from the first down scan and resets with 100 ms reinitialization (T14, T18)",
            Self::Period => "CST820: Assumed a NorScanPer write starts a full new scan period at byte completion (T12)",
            Self::Config => "CST820: Assumed configuration bytes are readable/writable and take effect at byte completion; reads have no side effects or snapshot latch (T02, T09-T16)",
            Self::Io => "CST820: Assumed IOCtl stores written bits; analog voltage and I2C drive selection are outside the byte bus, and host IRQ reset timing is refused (T13)",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Touch {
    x: u16,
    y: u16,
    down: bool,
}

/// Assumed single-finger target. Pins are SDA, SCL, INT, RST; supplies are implicit,
/// like the other byte-bus models. `new(addr)` takes a board-supplied seven-bit
/// address (T01), and `bind(tick_hz)` establishes simulation time exactly once.
/// Clone includes pending transitions, I2C phase, deadlines and one-time notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cst820 {
    target: I2cTarget,
    regs: [u8; 256],
    io_written: bool,
    pointer: Option<u8>,
    command: bool,
    tick_hz: Option<u64>,
    ready: u64,
    reset_low: Option<u64>,
    reset_deadline: Option<u64>,
    reset_qualified: bool,
    next_scan: Option<u64>,
    last_scan: Option<u64>,
    transition_at: Option<u64>,
    pulse_end: Option<u64>,
    long_deadline: Option<u64>,
    sleep_deadline: Option<u64>,
    down_since: Option<u64>,
    idle_since: u64,
    standby: bool,
    physical: Touch,
    physical_known: bool,
    sampled: Touch,
    coordinates_known: bool,
    pending: VecDeque<Touch>,
    notes: u32,
}

impl Cst820 {
    /// Its model pins, in the order the netlist binds; supplies are implicit.
    pub const PINS: &'static [&'static str] = &["SDA", "SCL", "INT", "RST"];

    pub fn new(addr: u8) -> Result<Self, String> {
        if addr > 0x7f { return Err("CST820: addr must be a seven-bit address (T01)".into()); }
        let mut part = Self {
            target: I2cTarget::new(addr), regs: [0; 256], io_written: false,
            pointer: None, command: false, tick_hz: None, ready: 0,
            reset_low: None, reset_deadline: None, reset_qualified: false, next_scan: None,
            last_scan: None, transition_at: None, pulse_end: None,
            long_deadline: None, sleep_deadline: None, down_since: None,
            idle_since: 0, standby: false, physical: Touch::default(),
            physical_known: false, sampled: Touch::default(), coordinates_known: false,
            pending: VecDeque::new(), notes: 0,
        };
        part.defaults();
        Ok(part)
    }

    pub fn bind(&mut self, tick_hz: u64) -> Result<(), String> {
        if tick_hz == 0 { return Err("CST820: tick rate must be nonzero".into()); }
        if self.tick_hz.is_some() { return Err("CST820: already bound".into()); }
        self.tick_hz = Some(tick_hz);
        self.ready = self.ticks(100_000_000).map_err(String::from)?; // T18.
        self.next_scan = Some(self.ready);
        self.idle_since = self.ready;
        Ok(())
    }

    fn ticks(&self, ns: u64) -> Res<u64> {
        let hz = self.tick_hz.ok_or("CST820: access before bind")?;
        u64::try_from((u128::from(hz) * u128::from(ns)).div_ceil(1_000_000_000))
            .map_err(|_| "CST820: duration exceeds tick representation")
    }

    fn deadline(&self, t: u64, ns: u64) -> Res<u64> {
        t.checked_add(self.ticks(ns)?).ok_or("CST820: deadline exceeds tick representation")
    }

    fn note(&mut self, cx: &mut Cx<'_>, note: Note) {
        let bit = 1 << note as u32;
        if self.notes & bit == 0 {
            cx.note(note.text());
            self.notes |= bit;
        }
    }

    fn assumptions(&mut self, cx: &mut Cx<'_>) {
        for note in [Note::Family, Note::Startup, Note::Defaults, Note::AutoReset,
            Note::Initial, Note::Scan, Note::Interrupt]
        {
            self.note(cx, note);
        }
    }

    fn defaults(&mut self) {
        self.regs.fill(0);
        self.regs[WIDTH] = 10; // T10, 1 ms.
        self.regs[PERIOD] = 1; // T12, 10 ms.
        self.regs[SLEEP] = 2; // T12, 2 s.
        self.regs[LONG_PRESS] = 10; // T14, 10 s.
        // T09: DisAutoSleep=0. T11/T14: IrqCtl/AutoReset=0 are Assumed.
        self.io_written = false; // T13: no complete IOCtl reset byte.
        self.pointer = None; // T02: no pointer reset value.
        self.command = false;
        self.target = I2cTarget::new(self.target.addr());
    }

    fn reset(&mut self, t: u64) -> Res {
        self.defaults();
        self.ready = self.deadline(t, 100_000_000)?; // T18.
        self.next_scan = Some(self.ready);
        self.last_scan = None;
        self.transition_at = None;
        self.pulse_end = None;
        self.long_deadline = None;
        self.sleep_deadline = None;
        self.down_since = None;
        self.idle_since = self.ready;
        self.standby = false;
        self.sampled = Touch::default();
        self.coordinates_known = false;
        // Host contact and queued edges are physical input, not resettable registers.
        Ok(())
    }

    fn timers(&mut self, t: u64) -> Res {
        self.long_deadline = if let Some(start) = self.down_since.filter(|_| self.regs[LONG_PRESS] != 0) {
            Some(self.deadline(start, u64::from(self.regs[LONG_PRESS]) * 1_000_000_000)?.max(t))
        } else { None };
        self.sleep_deadline = if !self.sampled.down && !self.standby && self.regs[DIS_SLEEP] == 0 {
            Some(self.deadline(self.idle_since, u64::from(self.regs[SLEEP]) * 1_000_000_000)?.max(t))
        } else { None };
        Ok(())
    }

    fn scan(&mut self, cx: &mut Cx<'_>) -> Res {
        self.note(cx, Note::Scan);
        if let Some(touch) = self.pending.pop_front() {
            self.physical = touch;
            self.physical_known = true;
        }
        let previous = self.sampled;
        self.sampled = self.physical;
        self.coordinates_known = self.physical_known;
        if previous.down != self.sampled.down {
            self.transition_at = Some(cx.t);
        }
        if self.sampled.down {
            if self.standby { self.note(cx, Note::Wake); }
            self.standby = false;
            self.down_since.get_or_insert(cx.t);
        } else {
            if previous.down { self.idle_since = cx.t; }
            self.down_since = None;
        }
        self.timers(cx.t)?;
        // T10/T11/T16, Assumed D-T1. Reading never acknowledges an interrupt.
        if (self.regs[IRQ] & 0x40 != 0 && self.sampled.down)
            || (self.regs[IRQ] & 0x20 != 0 && previous != self.sampled)
        {
            let end = self.deadline(cx.t, u64::from(self.regs[WIDTH]) * 100_000)?;
            if self.pulse_end.is_some() { self.note(cx, Note::Overlap); }
            self.pulse_end = Some(self.pulse_end.map_or(end, |old| old.max(end)));
        }
        self.last_scan = Some(cx.t);
        self.next_scan = Some(self.deadline(cx.t, u64::from(self.regs[PERIOD]) * 10_000_000)?);
        Ok(())
    }

    // Direct byte-bus users get the same time progression as Board::event users.
    fn catch_up(&mut self, cx: &mut Cx<'_>, inclusive: bool) -> Res {
        while let Some(t) = self.next_event().filter(|&t| t < cx.t || (inclusive && t == cx.t)) {
            self.event(&mut Cx { t, bus: cx.bus, detail: cx.detail, initializing: false })?;
        }
        Ok(())
    }

    fn accessible(&self, t: u64) -> Res {
        if self.tick_hz.is_none() { return Err("CST820: access before bind"); }
        if self.reset_low.is_some() { return Err("CST820: access while active-low RST is held (T18)"); }
        if t < self.ready { return Err("CST820: accessed during 100 ms initialization (T18)"); }
        if self.standby { return Err("CST820: I2C access in standby is undocumented (T17)"); }
        Ok(())
    }

    fn pointer(&self) -> Res<u8> {
        self.pointer.ok_or("CST820: register access before its pointer is set; power-on pointer undocumented (T02)")
    }

    fn advance(&mut self, cx: &mut Cx<'_>, reg: u8) {
        self.note(cx, Note::Framing);
        self.pointer = reg.checked_add(1); // No invented wrap; 0xFF itself is undocumented.
    }

    fn read(&mut self, cx: &mut Cx<'_>) -> Res<u8> {
        let reg = self.pointer()?;
        let value = match reg {
            0x02 => u8::from(self.sampled.down), // T03, no invented nibble mask.
            0x03..=0x06 => {
                if !self.coordinates_known {
                    return Err("CST820: coordinate register 0x03-0x06 read before a sampled touch; reset values undocumented (T04-T07)");
                }
                self.note(cx, Note::Coordinates);
                match reg {
                    0x03 => (self.sampled.x >> 8) as u8, // T04/T15: upper nibble zero.
                    0x04 => self.sampled.x as u8, // T05.
                    0x05 => (self.sampled.y >> 8) as u8, // T06/T15.
                    _ => self.sampled.y as u8, // T07.
                }
            }
            0xfd if !self.io_written => return Err("CST820: register 0xFD IOCtl read before write; reset byte undocumented (T13)"),
            0xed..=0xee | 0xf9..=0xfe => self.regs[usize::from(reg)],
            _ => return Err(register_error(reg)), // T08/T15 include IDs and 0x07.
        };
        self.advance(cx, reg);
        Ok(value)
    }

    fn write(&mut self, cx: &mut Cx<'_>, value: u8) -> Res {
        let reg = self.pointer()?;
        match usize::from(reg) {
            WIDTH if !(1..=200).contains(&value) => return Err("CST820: register 0xED IrqPluseWidth must be 1..200 (T10)"),
            PERIOD if !(1..=30).contains(&value) => return Err("CST820: register 0xEE NorScanPer must be 1..30 (T12)"),
            IRQ => {
                if value & 0x10 != 0 { return Err("CST820: register 0xFA EnMotion is not modelled (T11, D-T3)"); }
                if value & 1 != 0 { return Err("CST820: register 0xFA OnceWLP is not modelled (T11)"); }
                if value & 0x80 != 0 { return Err("CST820: register 0xFA EnTest period is undocumented (T11)"); }
                if value & 0x0e != 0 { return Err(register_error(reg)); }
            }
            AUTO_RESET if value != 0 => return Err("CST820: register 0xFB AutoReset needs undocumented valid-gesture detection (T14)"),
            IO => {
                if value & !7 != 0 { return Err(register_error(reg)); } // T13 defines only bits 2:0.
                self.note(cx, Note::Io);
                self.io_written = true;
            }
            WIDTH | PERIOD | SLEEP | AUTO_RESET | LONG_PRESS | DIS_SLEEP => (),
            _ => return Err(register_error(reg)), // Touch-output write permissions are NOT FOUND.
        }
        self.note(cx, Note::Config);
        self.regs[usize::from(reg)] = value;
        if usize::from(reg) == PERIOD {
            self.note(cx, Note::Period);
            self.next_scan = Some(self.deadline(cx.t, u64::from(value) * 10_000_000)?);
            self.last_scan = None;
        }
        self.timers(cx.t)?;
        self.advance(cx, reg);
        Ok(())
    }
}

impl Model for Cst820 {
    fn pins(&self) -> &'static [&'static str] { Self::PINS }

    fn drive(&self, pin: u8) -> Drive {
        match pin {
            0 | 1 | 3 => Drive::Input(Pull::None),
            2 => Drive::OpenDrain { low: self.pulse_end.is_some(), pull: Pull::None },
            _ => panic!("CST820: unknown pin"),
        }
    }

    fn pin(&mut self, cx: &mut Cx<'_>, pin: u8, high: bool) -> Res {
        self.assumptions(cx);
        match pin {
            0 | 1 => Ok(()),
            2 => {
                if !high && self.pulse_end.is_none() && self.io_written && self.regs[IO] & 4 != 0 {
                    return Err("CST820: IOCtl host IRQ soft-reset timing is undocumented (T13)");
                }
                Ok(())
            }
            3 => {
                if !high && self.reset_low.is_none() {
                    self.note(cx, Note::Reset);
                    self.reset_deadline = Some(self.deadline(cx.t, 100_000)?);
                    self.reset_low = Some(cx.t);
                } else if high {
                    if let Some(start) = self.reset_low.take() {
                        if cx.t - start >= self.ticks(100_000)? { // T18/D-T2.
                            self.reset(cx.t)?;
                        } else {
                            self.note(cx, Note::ShortReset);
                        }
                        self.reset_qualified = false;
                        self.reset_deadline = None;
                    }
                }
                Ok(())
            }
            _ => Err("CST820: unknown pin"),
        }
    }

    fn next_event(&self) -> Option<u64> {
        [self.reset_deadline, self.next_scan, self.pulse_end, self.long_deadline, self.sleep_deadline]
            .into_iter().flatten().min()
    }

    fn event(&mut self, cx: &mut Cx<'_>) -> Res {
        self.assumptions(cx);
        if self.reset_deadline == Some(cx.t) {
            self.reset(cx.t)?;
            self.reset_deadline = None;
            self.reset_qualified = true;
            self.next_scan = None; // Reinitialization begins only on release (T18).
            return Ok(());
        }
        if self.long_deadline == Some(cx.t) {
            self.note(cx, Note::LongPress);
            self.reset(cx.t)?;
            return Ok(());
        }
        if self.pulse_end == Some(cx.t) { self.pulse_end = None; }
        if self.next_scan == Some(cx.t) { self.scan(cx)?; }
        if self.sleep_deadline == Some(cx.t) {
            self.note(cx, Note::Sleep);
            self.standby = true;
            self.sleep_deadline = None;
        }
        Ok(())
    }

    fn i2c_addr(&self) -> Option<u8> { Some(self.target.addr()) }

    fn i2c(&mut self, cx: &mut Cx<'_>, op: I2cOp) -> Res<I2cAns> {
        // T19 cannot be checked: this byte seam carries no SCL/SDA edge timing.
        self.catch_up(cx, true)?;
        let selected = self.target.step(op)?;
        if selected == TargetOp::None { return Ok(I2cAns::None); }
        self.assumptions(cx);
        self.accessible(cx.t)?;
        self.note(cx, Note::Framing);
        self.note(cx, Note::Config);
        match selected {
            TargetOp::None => Ok(I2cAns::None),
            TargetOp::Address { read } => {
                if read { self.pointer()?; }
                self.command = !read;
                Ok(I2cAns::Ack(true))
            }
            TargetOp::Write(value) => {
                if self.command {
                    if !documented(value) { return Err(register_error(value)); }
                    self.pointer = Some(value);
                    self.command = false;
                } else { self.write(cx, value)?; }
                Ok(I2cAns::Ack(true))
            }
            TargetOp::Read { .. } => self.read(cx).map(I2cAns::Byte),
        }
    }

    fn input(&mut self, cx: &mut Cx<'_>, ev: &Stimulus) -> Res {
        let Stimulus::Touch { x, y, down, .. } = *ev else {
            return Err("CST820 takes only touch stimulus");
        };
        if x > 0x0fff || y > 0x0fff {
            return Err("CST820: touch coordinates exceed documented 12-bit fields (T04-T07)");
        }
        self.catch_up(cx, false)?;
        // A release with no finger on the glass is no event: the host may end a contact this
        // part never saw (its process started again while the finger was held).
        let contact = self.pending.back().map_or(self.physical.down, |touch| touch.down);
        if !down && !contact { return self.catch_up(cx, true); }
        self.note(cx, Note::Coordinates);
        self.note(cx, Note::Queue);
        let touch = Touch { x, y, down };
        if let Some(last) = self.pending.back_mut().filter(|last| last.down == down) {
            *last = touch;
        } else { self.pending.push_back(touch); }
        // S1 runs board events before equal-tick host input. An idle scan at this
        // tick can still sample that input. Moves can update that same scan, but
        // once an edge occupied it the next edge waits one period, including
        // down+up at exactly the same tick.
        let same_contact = self.pending.front().is_some_and(|touch| touch.down == self.sampled.down);
        if self.last_scan == Some(cx.t) && !self.reset_qualified
            && (self.transition_at != Some(cx.t) || same_contact)
        {
            self.scan(cx)?;
        }
        self.catch_up(cx, true)
    }

    fn spi(&mut self, _cx: &mut Cx<'_>, _x: &SpiXfer, _tx: &dyn Payload, _rx: &mut Vec<u8>) -> Res {
        Err("CST820 has no modelled SPI")
    }
}
