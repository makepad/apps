//! Master FIFO I2C controllers. Citations name facts-io.md records and its
//! Appendix A; pad routing is shared with pins, never a second matrix.

use std::collections::VecDeque;

use fab_elec_mcu_emu::board::{Board, I2cAns, I2cOp, I2cTiming};

use super::{pins::Pins, Access};
use crate::Junk;

const CTR: usize = 0x04 / 4;
const FIFO: usize = 0x18 / 4;
const RAW: usize = 0x20 / 4;
const ENA: usize = 0x28 / 4;
const CLOCK: usize = 0x54 / 4;
const COMMAND: usize = 0x58 / 4;
const IRQ_BITS: u32 = (1 << 18) - 1;
const START: u32 = 1 << 5;
const FSM_RST: u32 = 1 << 10;
const UPDATE: u32 = 1 << 11;
const RX_RST: u32 = 1 << 12;
const TX_RST: u32 = 1 << 13;
const DONE: u32 = 1 << 31;
const BYTE_DONE: u32 = 1 << 4;
const COMPLETE: u32 = 1 << 7;
const TRANS_START: u32 = 1 << 9;
const NACK: u32 = 1 << 10;

// Appendix A: offset, documented mask and field defaults. Holes and reserved
// bits retain seeded junk; no whole-register zero reset is inferred.
const REGISTERS: &[(usize, u32, u32)] = &[
    (0x00, 0x1ff, 0), (0x04, 0x7fff, 0x20b),
    (0x0c, 0x3f, 16), (0x10, 0x8000_7fff, 0),
    (0x18, 0x7fff, 0x408b), (0x1c, 0xff, 0),
    (0x20, IRQ_BITS, 2), (0x24, IRQ_BITS, 0),
    (0x28, IRQ_BITS, 0), (0x2c, IRQ_BITS, 0),
    (0x30, 0x1ff, 0), (0x34, 0x1ff, 0), (0x38, 0xffff, 0),
    (0x40, 0x1ff, 8), (0x44, 0x1ff, 8),
    (0x48, 0x1ff, 8), (0x4c, 0x1ff, 8),
    (0x50, 0x3ff, 0x300), (0x54, 0x3f_ffff, 1 << 21),
    (0x58, DONE | 0x3fff, 0), (0x5c, DONE | 0x3fff, 0),
    (0x60, DONE | 0x3fff, 0), (0x64, DONE | 0x3fff, 0),
    (0x68, DONE | 0x3fff, 0), (0x6c, DONE | 0x3fff, 0),
    (0x70, DONE | 0x3fff, 0), (0x74, DONE | 0x3fff, 0),
    (0x78, 0x1f, 16), (0x7c, 0x1f, 16),
    (0x80, 0xff, 0), (0x84, 0x3fff, 0),
    (0xf8, u32::MAX, 537330177),
];

fn fields(old: u32, mask: u32, value: u32) -> u32 { (old & !mask) | (value & mask) }

#[derive(Clone, Copy, PartialEq, Eq)]
struct Running {
    command: usize,
    left: u8,
    pads: [u16; 2],
    op: I2cOp,
    due: u64,
    start_ticks: u64,
    byte_ticks: u64,
    stop_ticks: u64,
    timing: I2cTiming,
    generated_stop: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct I2c {
    index: usize,
    apb: u64,
    clocked: bool,
    reset: bool,
    held: bool,
    known: bool,
    regs: [u32; 64],
    active: [u32; 64],
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    running: Option<Running>,
    busy: bool,
    set_at: Option<u64>,
    set_bits: u32,
    noted: u8,
    notes: Vec<(u64, &'static str)>,
}

impl I2c {
    pub fn new(index: usize, apb: u64, en: u32, rst: u32, junk: &mut Junk) -> Self {
        let regs = std::array::from_fn(|_| junk.u32());
        let bit = 1 << [7, 18][index]; // I20.
        Self {
            index, apb, clocked: en & bit != 0, reset: rst & bit != 0,
            held: false, known: false, regs, active: regs,
            tx: VecDeque::new(), rx: VecDeque::new(), running: None,
            busy: false, set_at: None, set_bits: 0, noted: 0, notes: Vec::new(),
        }
    }

    fn offset(&self, addr: u32) -> Option<usize> {
        // I01, I06: include the direct RAM windows so they stop explicitly.
        addr.checked_sub([0x6001_3000, 0x6002_7000][self.index])
            .filter(|&off| off < 0x200).map(|off| off as usize)
    }

    pub fn running(&self) -> bool { self.running.is_some() }

    pub fn check_gate(&self, en: u32, rst: u32) -> Result<(), &'static str> {
        let bit = 1 << [7, 18][self.index];
        if self.running() && (en & bit == 0 || rst & bit != 0) {
            return Err("I2C clock gating or reset during a command list is undocumented (Q01)");
        }
        Ok(())
    }

    pub fn gate(&mut self, en: u32, rst: u32, reset_write: bool) {
        let bit = 1 << [7, 18][self.index];
        let reset = rst & bit != 0;
        self.clocked = en & bit != 0;
        // The ROM's controller state is unknown. Only a firmware-held reset
        // followed by a clocked release establishes Appendix A defaults (I20).
        if reset_write && reset { self.held = true; self.known = false; }
        if self.reset && !reset && self.held {
            if self.clocked {
                for &(off, mask, value) in REGISTERS {
                    self.regs[off / 4] = fields(self.regs[off / 4], mask, value);
                }
                self.active = self.regs;
                self.tx.clear();
                self.rx.clear();
                self.busy = false;
                self.set_at = None;
                self.set_bits = 0;
                self.known = true;
            }
            self.held = false;
        }
        self.reset = reset;
    }

    fn accessible(&self) -> Result<(), &'static str> {
        if !self.clocked { return Err("I2C register access while the SYSTEM clock is off"); }
        if self.reset { return Err("I2C register access while SYSTEM reset is held"); }
        if !self.known { return Err("I2C ROM handoff state is unknown; hold reset and release it with the clock enabled"); }
        Ok(())
    }

    fn transact<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, &'static str>) -> Result<T, &'static str> {
        // As in time.rs, rejected accesses/events preserve the block and its
        // notes without copying the accumulated note history on every access.
        let notes = std::mem::take(&mut self.notes);
        let mut next = self.clone();
        self.notes = notes;
        let result = f(&mut next)?;
        self.notes.append(&mut next.notes);
        std::mem::swap(&mut self.notes, &mut next.notes);
        *self = next;
        Ok(result)
    }

    fn note(&mut self, bit: u8, tick: u64, what: &'static str) {
        if self.noted & bit == 0 { self.notes.push((tick, what)); self.noted |= bit; }
    }

    fn raise(&mut self, bits: u32, tick: u64) {
        self.regs[RAW] |= bits;
        if self.set_at != Some(tick) { self.set_bits = 0; }
        self.set_at = Some(tick);
        self.set_bits |= bits;
    }

    fn watermark_bits(&self) -> u32 {
        let fifo = self.regs[FIFO];
        if fifo & (1 << 14) == 0 { return 0; }
        // I07: strictly below for TX, strictly above for RX.
        u32::from(self.rx.len() > (fifo & 31) as usize)
            | (u32::from(self.tx.len() < ((fifo >> 5) & 31) as usize) << 1)
    }

    fn watermarks(&mut self, tick: u64) { self.raise(self.watermark_bits(), tick); }

    fn fifo_access(&self, reset: u32) -> Result<(), &'static str> {
        if self.regs[FIFO] & (1 << 10) != 0 { return Err("I2C non-FIFO RAM access is not modelled"); }
        if self.regs[FIFO] & reset != 0 { return Err("I2C FIFO push or pop while its reset bit is set"); }
        Ok(())
    }

    pub fn read(&mut self, addr: u32, tick: u64) -> Access<u32> {
        let off = self.offset(addr)?;
        Some(self.transact(|next| {
            next.accessible()?;
            match off {
                0x08 => Err("I2C_SR FIFO counts are documented both ways (D-I9)"),
                0x14 => Err("I2C FIFO pointer encodings are not modelled"),
                // M: I16 leaves idle FSM counting unknown. Do not model idle
                // timeouts or expose RAW bits 13/14; ENA keeps STATUS known.
                0x20 => Err("I2C_INT_RAW bits 13/14: whether an idle FSM times out is undocumented (I16)"),
                0x24 => Err("I2C INT_CLR WT readback is undocumented (Q01)"),
                0x1c => {
                    next.fifo_access(RX_RST)?;
                    let byte = next.rx.pop_front().ok_or("I2C empty RX FIFO read value is undocumented (Q02)")?;
                    next.watermarks(tick);
                    Ok(fields(next.regs[off / 4], 0xff, u32::from(byte)))
                }
                0x2c => Ok(fields(next.regs[off / 4], IRQ_BITS, next.regs[RAW] & next.regs[ENA])),
                0x100..=0x1fc => Err("I2C non-FIFO RAM access is not modelled"),
                _ if REGISTERS.iter().any(|&(r, _, _)| r == off) => Ok(next.regs[off / 4]),
                _ => Err("I2C reserved register access is undocumented (Q01)"),
            }
        }))
    }

    pub fn write(&mut self, addr: u32, value: u32, tick: u64, pins: &Pins) -> Access<()> {
        let off = self.offset(addr)?;
        Some(self.transact(|next| next.write_inner(off, value, tick, pins)))
    }

    fn write_inner(&mut self, off: usize, value: u32, tick: u64, pins: &Pins) -> Result<(), &'static str> {
        self.accessible()?;
        match off {
            0x08 | 0x14 | 0x2c => return Err("I2C write to read-only register is undocumented (Q01)"),
            0x0c if value & (1 << 5) != 0 => return Err("I2C SCL timeout: its clock domain is disputed and its idle behaviour undocumented (I16, D-I3)"),
            0x20 => return Err("I2C INT_RAW write is undocumented; WTC is not W1C (C01)"),
            0x28 if value & ((1 << 13) | (1 << 14)) != 0 => return Err("I2C_INT_RAW bits 13/14: whether an idle FSM times out is undocumented (I16)"),
            0x100..=0x1fc => return Err("I2C non-FIFO RAM access is not modelled"),
            0x1c => {
                self.fifo_access(TX_RST)?;
                if self.tx.len() == 32 { return Err("I2C full TX FIFO write disposition is undocumented (Q02)"); }
                self.tx.push_back(value as u8); // I06: DATA pushes the low byte.
                self.watermarks(tick);
                return Ok(());
            }
            0x24 => {
                if self.set_at == Some(tick) && value & self.set_bits & IRQ_BITS != 0 {
                    return Err("I2C simultaneous interrupt set/clear priority is undocumented (Q01)");
                }
                if value & self.watermark_bits() != 0 {
                    return Err("I2C clearing an active FIFO watermark has undocumented reassertion timing (Q01)");
                }
                self.regs[RAW] &= !(value & IRQ_BITS);
                return Ok(());
            }
            _ => (),
        }
        let &(_, mask, _) = REGISTERS.iter().find(|&&(r, _, _)| r == off)
            .ok_or("I2C reserved register access is undocumented (Q01)")?;
        if self.running() && off != 0x28 {
            return Err("I2C configuration or command change during an active list is undocumented (I09, Q02)");
        }
        // Unsupported feature enables are refused even before synchronization.
        match off {
            0x04 if value & (1 << 12) != 0 => return Err("I2C slave auto-start is not modelled"),
            0x04 if value & (1 << 13) != 0 => return Err("I2C 10-bit address checking is not modelled"),
            0x04 if value & (1 << 14) != 0 => return Err("I2C broadcast addressing is not modelled"),
            0x04 if value & (1 << 8) != 0 => return Err("I2C_CLK_EN meaning is disputed (D-I2)"),
            0x10 if value & (1 << 31) != 0 => return Err("I2C 10-bit addressing is not modelled (D-I10)"),
            0x10 if value & 0x7fff != 0 => return Err("I2C slave addressing is not modelled"),
            0x18 if value & (1 << 11) != 0 => return Err("I2C slave FIFO address configuration is not modelled"),
            0x80 if value & 0x3f != 0 => return Err("I2C SCL-reset-slave pulses are not modelled"),
            0x80 if value & 0xc0 != 0 => return Err("I2C forced SCL/SDA low depends on disputed FORCE_OUT polarity (D-I1)"),
            0x84 if value & 0x3c00 != 0 => return Err("I2C slave stretching and byte ACK control are not modelled"),
            _ => (),
        }
        let pulse = if off == 0x04 { START | FSM_RST | UPDATE } else { 0 };
        self.regs[off / 4] = fields(self.regs[off / 4], mask, value & !pulse);
        if off == 0x18 {
            if value & RX_RST != 0 { self.rx.clear(); }
            if value & TX_RST != 0 { self.tx.clear(); }
            self.watermarks(tick);
        }
        if off == 0x04 {
            if value & UPDATE != 0 {
                // M: I09 leaves synchronization latency open. Apply its exact
                // list at this write's tick; CLK_CONF and COMD are unsynchronized.
                for &(offset, mask, _) in REGISTERS {
                    let sync = match offset {
                        0x04 => 0x72df,
                        0x18 => 1 << 11,
                        0x00 | 0x0c | 0x10 | 0x30..=0x50 | 0x78..=0x84 => mask,
                        _ => 0,
                    };
                    self.active[offset / 4] = fields(self.active[offset / 4], sync, self.regs[offset / 4]);
                }
                self.note(1, tick, "I2C CONF_UPGATE takes effect at the write tick; synchronization latency undocumented (I09)");
            }
            // FSM_RST is a pulse with no effect while idle (I08).
            if value & START != 0 { self.start(tick, pins)?; }
        }
        Ok(())
    }

    fn timing(&self) -> Result<(u64, u64, u64, I2cTiming), &'static str> {
        let clock = self.regs[CLOCK]; // I09 excludes CLK_CONF from synchronization.
        if clock & (1 << 20) != 0 { return Err("I2C RC_FAST clock is disputed (D-I6)"); }
        if clock & (1 << 21) == 0 { return Err("I2C SCLK_ACTIVE is disabled"); }
        if clock & (63 << 8) != 0 { return Err("I2C fractional divider pattern is undocumented (I11)"); }
        let t = 12 * u64::from((clock & 255) + 1); // I11; facts.md §3: 40 MHz XTAL.
        let f = |off: usize, mask| u64::from(self.active[off / 4] & mask);
        let (low, high, wait) = (f(0, 511), f(0x38, 511), f(0x38, 0xfe00) >> 9);
        let (hold, sample, start, restart) = (f(0x30, 511), f(0x34, 511), f(0x40, 511), f(0x44, 511));
        let period = low + high + wait + 3;
        if period <= 20 { return Err("I2C main-clock/SCL ratio must exceed 20 (I11)"); }
        // I13, with frequencies cross-multiplied into integer tick periods.
        if !(6..=480).contains(&self.apb) || hold < 4 || 3 * self.apb > (hold - 4) * t
            || hold + start <= (f(0x50, 0xf0) >> 4) + 3
            || sample <= wait || sample >= high || sample >= wait + start + restart
            || f(0x84, 1023) + hold <= low
        { return Err("I2C timing constraint violation; behaviour undocumented (I13)"); }
        if low <= hold {
            return Err("I2C data setup (SCL_LOW_PERIOD - SDA_HOLD_TIME) is not positive; behaviour undocumented (I12)");
        }
        let (start_cycles, byte_cycles, stop_cycles) =
            (restart + start + 2, 9 * period, f(0x4c, 511) + f(0x48, 511) + 2);
        // M: I16/D-I3: use TRM §27.4.8's 2^v I2C_SCLK cycles, not
        // §27.6's direct counts, and refuse values above the disputed limit 22.
        // Bound every FSM state by the longest single step in SCLK cycles
        // (RSTART, STOP or one whole byte); neither timeout may be reached.
        let timeouts = [f(0x78, 31), f(0x7c, 31)]; // I09 synchronized fields.
        if timeouts.iter().any(|&v| v > 22) {
            return Err("I2C FSM timeout above 22 is disputed (D-I3)");
        }
        let bound = start_cycles.max(byte_cycles).max(stop_cycles);
        if timeouts.iter().any(|&v| bound >= (1u64 << v)) {
            return Err("an I2C FSM timeout could fire during this list; its effect is not modelled (I16, D-I3)");
        }
        let timing = I2cTiming { setup: (low - hold) * t, allowance: (wait + 1) * t };
        Ok((start_cycles * t, byte_cycles * t, stop_cycles * t, timing))
    }

    fn start(&mut self, tick: u64, pins: &Pins) -> Result<(), &'static str> {
        if self.busy { return Err("I2C TRANS_START while the bus is busy is undocumented (Q01)"); }
        if self.active[CTR] & (1 << 4) == 0 { return Err("slave mode is not modelled"); }
        if self.regs[FIFO] & (1 << 10) != 0 { return Err("I2C non-FIFO RAM access is not modelled"); }
        let pads = pins.i2c_pads(self.index)?;
        let (start_ticks, byte_ticks, stop_ticks, timing) = self.timing()?;
        self.running = Some(Running {
            command: 0, left: 0, pads, op: I2cOp::Start, due: tick,
            start_ticks, byte_ticks, stop_ticks, timing, generated_stop: false,
        });
        self.schedule(tick, true)?;
        // M: I12 durations, Q02 fetch timing unspecified. No additional fetch,
        // filtering or synchronization delay is added between commands/bytes.
        self.note(2, tick, "I2C command times from TRM §27.4.7: commands back to back from TRANS_START, no fetch, filter or sync delay");
        self.note(8, tick, "I2C FSM timeouts read as 2^v I2C_SCLK cycles (TRM §27.4.8); §27.6's direct count disagrees (D-I3); no step of this list reaches them");
        Ok(())
    }

    fn schedule(&mut self, tick: u64, new_command: bool) -> Result<(), &'static str> {
        let mut run = self.running.expect("scheduling a running list");
        if run.command == 8 { return Err("I2C command list has no STOP among eight commands (I03)"); }
        let command = self.regs[COMMAND + run.command];
        let opcode = (command >> 11) & 7; // I02a.
        if new_command { run.left = command as u8; }
        let duration = match opcode {
            6 => { run.op = I2cOp::Start; run.start_ticks }
            2 => { run.op = I2cOp::Stop; run.stop_ticks }
            1 | 3 => {
                if run.left == 0 { return Err("I2C zero-byte READ/WRITE behaviour is undocumented (I04, Q02)"); }
                if opcode == 1 {
                    self.fifo_access(TX_RST)?;
                    // I06, Q02: consume at byte START, never at its end tick.
                    let byte = self.tx.pop_front().ok_or("I2C TX FIFO underflow disposition is undocumented (Q02)")?;
                    run.op = I2cOp::Write(if self.active[CTR] & (1 << 6) != 0 { byte.reverse_bits() } else { byte });
                    self.watermarks(tick);
                } else { run.op = I2cOp::Read { ack: command & (1 << 10) == 0 }; }
                run.byte_ticks
            }
            4 => return Err("I2C END holds SCL low; suspension is not modelled"),
            _ => return Err("I2C undefined command opcode (I03, Q01)"),
        };
        run.due = tick.checked_add(duration).ok_or("I2C event tick overflow")?;
        self.running = Some(run);
        Ok(())
    }

    pub fn next_event(&self) -> Option<u64> { self.running.map(|run| run.due) }

    pub fn event<B: Board>(&mut self, tick: u64, board: &mut B, board_t0: u64) -> Result<(), &'static str> {
        // Do not stage the board: an Err latches an Unmodeled halt, and rewind
        // restores the board from its own mark. Keep the block transactional.
        self.transact(|next| next.event_inner(tick, board, board_t0))
    }

    fn event_inner<B: Board>(&mut self, tick: u64, board: &mut B, board_t0: u64) -> Result<(), &'static str> {
        let mut run = self.running.expect("I2C event requires a running command");
        if let I2cOp::Read { .. } = run.op {
            self.fifo_access(RX_RST)?;
            if self.rx.len() == 32 { return Err("I2C RX FIFO overflow disposition is undocumented (Q02)"); }
            // M: Appendix A RX_FULL_ACK_LEVEL leaves the threshold and
            // "reached" ambiguous. Stop at post-push count >= RXFIFO_WM_THRHD,
            // regardless of FIFO_PRT_EN, before sending the byte to the board.
            if self.rx.len() + 1 >= (self.regs[FIFO] & 31) as usize {
                return Err("I2C master ACK at the RX FIFO threshold (RX_FULL_ACK_LEVEL) is not modelled");
            }
        }
        let at = board_t0.checked_add(tick).ok_or("I2C board tick overflow")?;
        if run.command == 0 && run.op == I2cOp::Start {
            board.i2c_timing(at, run.pads[0], run.pads[1], run.timing)?;
        }
        let answer = board.i2c(at, run.pads[0], run.pads[1], run.op)?;
        let command = self.regs[COMMAND + run.command];
        match (run.op, answer) {
            (I2cOp::Start, I2cAns::None) => { self.busy = true; self.raise(TRANS_START, tick); }
            (I2cOp::Stop, I2cAns::None) => {
                self.busy = false;
                self.raise(COMPLETE, tick);
                if !run.generated_stop { self.regs[COMMAND + run.command] |= DONE; }
                self.running = None;
                return Ok(());
            }
            (I2cOp::Write(_), I2cAns::Ack(acked)) => {
                self.raise(BYTE_DONE, tick);
                run.left -= 1;
                if command & (1 << 8) != 0 && !acked != (command & (1 << 9) != 0) {
                    self.raise(NACK, tick);
                    // M: I04 generates STOP; I17 leaves aborted DONE effects
                    // open. A fully sent command is done; later commands stay clear.
                    if run.left == 0 { self.regs[COMMAND + run.command] |= DONE; }
                    run.generated_stop = true;
                    run.op = I2cOp::Stop;
                    run.due = tick.checked_add(run.stop_ticks).ok_or("I2C event tick overflow")?;
                    self.running = Some(run);
                    self.note(4, tick, "I2C NACK: the generated STOP uses STOP_SETUP/STOP_HOLD; later commands stay not done (I04, I17 NOT FOUND)");
                    return Ok(());
                }
            }
            (I2cOp::Read { .. }, I2cAns::Byte(byte)) => {
                self.rx.push_back(if self.active[CTR] & (1 << 7) != 0 { byte.reverse_bits() } else { byte });
                self.watermarks(tick);
                self.raise(BYTE_DONE, tick);
                run.left -= 1;
            }
            _ => return Err("I2C board answer does not match its operation"),
        }
        let finished = matches!(run.op, I2cOp::Start) || run.left == 0;
        if finished { self.regs[COMMAND + run.command] |= DONE; run.command += 1; }
        self.running = Some(run);
        self.schedule(tick, finished)
    }

    pub fn interrupt_sources(&self) -> u128 {
        // I18/I19: only documented RAW AND ENA bits hold sources 42/43.
        u128::from(self.known && self.clocked && !self.reset && self.regs[RAW] & self.regs[ENA] & IRQ_BITS != 0)
            << (42 + self.index)
    }

    pub fn apb_changed(&mut self, apb: u64) { self.apb = apb; }

    pub fn rehandoff(&mut self, apb: u64, gates: [u32; 2], junk: &mut Junk) {
        let notes = std::mem::take(&mut self.notes);
        let noted = self.noted;
        *self = Self::new(self.index, apb, gates[0], gates[1], junk);
        self.notes = notes;
        self.noted = noted;
    }

    pub fn new_epoch(&mut self, apb: u64) {
        // A reset with a running list was refused in Periph::events (Q01).
        self.apb = apb;
        self.set_at = None;
        self.set_bits = 0;
    }

    pub fn take_notes(&mut self, out: &mut Vec<(u64, &'static str)>) { out.append(&mut self.notes); }
}
