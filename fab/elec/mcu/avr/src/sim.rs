//! ATmega328P instruction oracle, for compiler tests only.
//!
//! Cycles count completed CPU clocks. Data loads/writes occur at instruction
//! completion; GPIO events carry that timestamp. TX events mark start bits,
//! including buffered frames starting between instruction boundaries. I/O skips
//! read at the end of their first clock, before discarding any skipped words.
//! External `write`/`set_pin`/`rx` calls occur at the current timestamp. Timer0/1
//! share a free-running prescaler; Timer2 has its own. Normal, CTC and fixed-TOP
//! fast/phase-correct PWM are modeled, plus OCRnA-TOP PWM on Timer0/1/2 and
//! Timer1 ICR1-TOP CTC/PWM and phase-and-frequency-correct PWM. Reserved WGMs
//! and Timer1 input capture interrupts outside ICR1-TOP modes are refused only
//! while clocked; external Timer0/1 clocks are refused. USART0
//! reception completes at the end of its frame (a simplification of the stop
//! bit's sample point), timed by the chip's own UBRR0/U2X0. Received frames are
//! valid: framing/parity errors are not modeled; RX parity/9-bit configurations
//! are refused. TX retains parity and 9-bit frame timing.
//! The ADC models single and free-running conversions (DS40002061B §24), with
//! exact prescaler edges, sample-and-hold and completion. External channels can
//! sample held voltages or request the circuit's voltage at their exact tick.
//! SPI master transfers (DS40002061B §19) shift and sample at exact half-clock
//! edges; slave mode models only the pin direction overrides.
//! EEPROM (DS40002061B §8.4) commits bytes at the end of a nominal RC-oscillator
//! write; reads and write starts add CPU halt clocks while peripherals advance.
//! The watchdog counter does not run;
//! WDR is a no-op and leaving WDE enabled at a configuration write is an error
//! (the WDCE unlock write is exempt so software can disable it). SLEEP
//! represents idle sleep (without requiring SMCR configuration), including the
//! chip's four wake-up clocks before its four-clock interrupt entry.
//! Co-simulation timing is opt-in: sleep waits stop on the first drive edge,
//! including when no interrupt can wake the chip, and otherwise reach their
//! deadline exactly. Instruction and wake/entry clocks are never clipped.
//!
//! The infallible public bus API latches an invalid access: reads return zero,
//! writes have no effect, and the next step/run returns `BadAddress`. Instruction
//! accesses report it in the same step. TCNT1/ICR1 low reads latch TEMP through Cell;
//! OCR1A/B reads bypass TEMP, as specified by the datasheet (writes use TEMP).
//! UDR0 reads pop the receive FIFO, including through the shared-reference API.

use std::cell::Cell;
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};
use crate::insn::{cycles, decode, pointer_base, Insn, Ptr};
use fab_elec_mcu_emu::{Block, CodeMap, Cond, CondKind, PinUse, SampleAt, Tally, TimerEvent};

pub const FLASH_WORDS: usize = 16384;
pub const SRAM: usize = 2048;
pub const EEPROM: usize = 1024;
/// Idle sleep adds four wake-up clocks beyond the normal interrupt entry.
pub const IDLE_WAKE_CYCLES: u64 = 4;

const C: u8 = 1;
const Z: u8 = 2;
const N: u8 = 4;
const V: u8 = 8;
const S: u8 = 16;
const H: u8 = 32;
const T: u8 = 64;
const I: u8 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Pin { cycle: u64, port: char, bit: u8, high: bool },
    Tx { cycle: u64, byte: u8 },
    TxDone { cycle: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Break, Sleep, Cycles, BadInsn { pc: u32 }, BadAddress { pc: u32, addr: u32 },
    UdrOverrun { pc: u32 }, Watchdog { pc: u32 },
    Unmodeled { pc: u32, what: &'static str },
}

// Datasheet "Register Summary": data-space addresses (not IN/OUT offsets).
const TIMER_A: [usize; 3] = [0x44, 0x80, 0xb0];
const TIMER_B: [usize; 3] = [0x45, 0x81, 0xb1];
// Datasheet "Interrupts", "Interrupt Vectors in ATmega328P": word addresses,
// in priority order, including TIMER1 CAPT before TIMER1 COMPA.
const TIMER_IRQS: [(u32, usize, u8); 10] = [
    (14, 2, 2), (16, 2, 4), (18, 2, 1),
    (20, 1, 0x20), (22, 1, 2), (24, 1, 4), (26, 1, 1),
    (28, 0, 2), (30, 0, 4), (32, 0, 1),
];
const TIMER_SOURCES: [&str; 10] = [
    "TIMER2_COMPA", "TIMER2_COMPB", "TIMER2_OVF", "TIMER1_CAPT", "TIMER1_COMPA",
    "TIMER1_COMPB", "TIMER1_OVF", "TIMER0_COMPA", "TIMER0_COMPB", "TIMER0_OVF",
];
// Datasheet "8-bit Timer/Counter0 with PWM", "16-bit Timer/Counter1 with PWM",
// "8-bit Timer/Counter2 with PWM and Asynchronous Operation": OC pin mappings.
const OC_PINS: [[(usize, u8); 2]; 3] = [[(2, 6), (2, 5)], [(0, 1), (0, 2)], [(0, 3), (2, 3)]];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Wave { Normal, Ctc, Fast, Phase, PhaseFrequency }

#[derive(Clone, Copy, PartialEq, Eq)]
struct TimerConfig {
    wave: Wave,
    max: u16,
    fixed_top: Option<u16>,
    icr_top: bool,
    com: [u8; 2],
    toggle_a: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TimerSetup {
    divisors: [u64; 3],
    configs: [TimerConfig; 3],
    held: [bool; 2],
}

impl TimerConfig {
    fn pwm(self) -> bool { matches!(self.wave, Wave::Fast | Wave::Phase | Wave::PhaseFrequency) }
    fn connected(self, channel: usize) -> bool {
        self.com[channel] >= 2 || (self.com[channel] == 1
            && (!self.pwm() || (channel == 0 && self.toggle_a)))
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Timer {
    counter: u16,
    icr: u16,
    ocr: [u16; 2], // CPU-visible buffers; reads never expose the active latches.
    active: [u16; 2],
    down: bool,
    block_compare: bool,
    output: [bool; 2],
}

impl Timer {
    // Datasheet "8-bit Timer/Counter0 with PWM", "16-bit Timer/Counter1 with
    // PWM", and "8-bit Timer/Counter2 with PWM and Asynchronous Operation":
    // stop BEFORE the post-equality clock used by tick. Normal/CTC stop at
    // compares or MAX; Fast also stops at TOP (clear/transfer). Phase modes
    // stop at TOP (turn/transfer), and before reaching BOTTOM (TOV/transfer;
    // see Timer1's "Phase and Frequency Correct PWM Mode").
    // A missed TOP must still run to MAX, never jump across the counter wrap.
    fn quiet(&self, c: TimerConfig) -> u64 {
        let phase = matches!(c.wave, Wave::Phase | Wave::PhaseFrequency);
        if self.block_compare || (self.down && !phase) { return 0; }
        let old = self.counter;
        let mut quiet = if self.down { old.saturating_sub(1) } else { c.max - old };
        let distance = |at: u16| if self.down { old.checked_sub(at) } else { at.checked_sub(old) };
        for at in self.active {
            if let Some(ticks) = distance(at) { quiet = quiet.min(ticks); }
        }
        // ICR TOP raises ICF1 independently of compare blocking, including CTC.
        // See "ICF1: Timer/Counter1, Input Capture Flag" and "Waveform
        // Generation Mode Bit Description". OCR TOP remains the active latch.
        if c.icr_top || c.pwm() {
            if let Some(ticks) = distance(c.fixed_top.unwrap_or(self.active[0])) {
                quiet = quiet.min(ticks);
            }
        }
        quiet as u64
    }

    fn count(&mut self, c: TimerConfig, ticks: u64) {
        debug_assert!(ticks <= self.quiet(c));
        // quiet excludes every wrap, transfer and direction change.
        if self.down { self.counter -= ticks as u16; }
        else { self.counter += ticks as u16; }
    }

    fn compare_output(&mut self, c: TimerConfig, channel: usize) {
        match c.com[channel] {
            1 if c.connected(channel) => self.output[channel] = !self.output[channel],
            2 => self.output[channel] = false,
            3 => self.output[channel] = true,
            _ => {}
        }
    }

    // Datasheet "8-bit Timer/Counter0 with PWM", "16-bit Timer/Counter1 with
    // PWM", and "8-bit Timer/Counter2 with PWM and Asynchronous Operation":
    // compare equality is acted on at the following timer clock. Fast PWM's
    // TOP/MAX event clears TCNT, sets TOV and transfers OCR at BOTTOM on that
    // clock. Timer1's "Waveform Generation Mode Bit Description" and "Phase
    // and Frequency Correct PWM Mode": phase-correct transfers at TOP;
    // phase-and-frequency-correct transfers on reaching BOTTOM, with TOV.
    // "ICF1: Timer/Counter1, Input Capture Flag": use the same post-equality
    // clock as a TOP compare match for ICF1 (also when OCF1A occurs that clock).
    fn tick(&mut self, c: TimerConfig) -> u8 {
        let old = self.counter;
        let top = c.fixed_top.unwrap_or(self.active[0]);
        let matched = self.active.map(|ocr| old == ocr && !self.block_compare);
        self.block_compare = false;
        let mut flags = (u8::from(matched[0]) << 1) | (u8::from(matched[1]) << 2);
        if c.icr_top && old == top { flags |= 0x20; }
        match c.wave {
            Wave::Normal | Wave::Ctc => {
                for (channel, hit) in matched.into_iter().enumerate() {
                    if hit { self.compare_output(c, channel); }
                }
                if old == c.max { flags |= 1; }
                let clear = c.wave == Wave::Ctc
                    && (if c.icr_top { old == top } else { matched[0] });
                self.counter = if clear { 0 }
                    else { old.wrapping_add(1) & c.max };
                self.down = false;
            }
            Wave::Fast => {
                let bottom = old == top;
                self.counter = if bottom { 0 } else { old.wrapping_add(1) & c.max };
                if bottom { flags |= 1; }
                for (channel, hit) in matched.into_iter().enumerate() {
                    if c.com[channel] == 1 && hit { self.compare_output(c, channel); }
                    else if c.com[channel] >= 2 {
                        if bottom { self.output[channel] = c.com[channel] == 2; }
                        else if hit && self.active[channel] != top {
                            self.output[channel] = c.com[channel] == 3;
                        }
                    }
                }
                if bottom { self.active = self.ocr; }
                self.down = false;
            }
            Wave::Phase | Wave::PhaseFrequency => {
                let at_top = old == top;
                let descending = self.down;
                if at_top {
                    if c.wave == Wave::Phase { self.active = self.ocr; }
                    self.down = true;
                } else if old == 0 { self.down = false; }
                self.counter = if top == 0 && old == 0 { 0 }
                    else if self.down { old.wrapping_sub(1) & c.max }
                    else { old.wrapping_add(1) & c.max };
                if self.counter == 0 { flags |= 1; }
                let bottom_transfer = c.wave == Wave::PhaseFrequency && self.counter == 0;
                if bottom_transfer { self.active = self.ocr; }
                let next_top = c.fixed_top.unwrap_or(self.active[0]);
                for (channel, hit) in matched.into_iter().enumerate() {
                    if c.com[channel] == 1 && hit { self.compare_output(c, channel); }
                    else if c.com[channel] >= 2 {
                        let high = if bottom_transfer
                            && (self.active[channel] == 0 || self.active[channel] == next_top) {
                            // "16-bit Timer/Counter1 with PWM", "Phase and
                            // Frequency Correct PWM Mode": extreme duties are
                            // constant. Apply the new extreme at the BOTTOM
                            // transfer itself, overriding an old down-match;
                            // do not leave a pulse until the next TOP/match.
                            Some(self.active[channel] != 0)
                        } else if at_top {
                            // Also restores symmetry after a missed upward match,
                            // or an OCR change from TOP to a smaller duty at TOP.
                            Some(self.active[channel] == next_top)
                        } else if hit {
                            Some(self.active[channel] != 0 && descending)
                        } else { None };
                        if let Some(high) = high {
                            self.output[channel] = high ^ (c.com[channel] == 3);
                        }
                    }
                }
            }
        }
        flags
    }

    fn next_flags(mut self, c: TimerConfig, mask: u8) -> Option<(u64, u8)> {
        // Project only this tiny peripheral, using the same state transitions
        // as execution. Once the buffered writes settle, a repeated counter /
        // direction means no enabled flag can occur (e.g. CTC overflow only).
        let mut settled: Option<(u16, bool)> = None;
        let mut ticks = 0;
        loop {
            let mut quiet = self.quiet(c);
            if self.active == self.ocr {
                if let Some((counter, down)) = settled {
                    if down == self.down {
                        let distance = if down { self.counter.checked_sub(counter) }
                            else { counter.checked_sub(self.counter) };
                        if let Some(distance) = distance.filter(|&k| k != 0) {
                            quiet = quiet.min(distance as u64);
                        }
                    }
                } else {
                    // Keep the original first settled state, even inside a
                    // quiet run, and never skip its subsequent recurrence.
                    quiet = quiet.min(1);
                }
            }
            if quiet != 0 {
                self.count(c, quiet);
                ticks += quiet;
            } else {
                ticks += 1;
                let flags = self.tick(c) & mask;
                if flags != 0 { return Some((ticks, flags)); }
            }
            if self.active == self.ocr {
                let state = (self.counter, self.down);
                if settled == Some(state) { return None; }
                if settled.is_none() { settled = Some(state); }
            }
        }
    }
}

// Datasheet "USART0", Data Reception / Receive Buffer: two FIFO entries and
// a completed frame that can remain in the shift register until space opens.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Receive {
    fifo: [(u8, bool); 2], // Byte and its buffered DOR0 flag.
    len: usize,
    waiting: Option<u8>,
    pending_dor: bool,
}

impl Receive {
    fn complete(&mut self, byte: u8) {
        if self.len < 2 {
            // Error flags travel with the frame. Only a successful transfer
            // from the shifter consumes the pending overrun indication.
            self.fifo[self.len] = (byte, self.pending_dor);
            self.len += 1;
            self.pending_dor = false;
        } else { self.waiting = Some(byte); }
    }

    fn pop(&mut self) -> u8 {
        if self.len == 0 { return 0; }
        let byte = self.fifo[0].0;
        self.fifo[0] = self.fifo[1];
        self.len -= 1;
        if let Some(waiting) = self.waiting.take() { self.complete(waiting); }
        byte
    }
}

/// Flash is shared by snapshots. The oracle's existing fixture API can still
/// patch words; such an explicit edit makes a private copy. Execution never
/// writes flash (there is no self-programming instruction in this model).
#[derive(Clone)]
pub struct Flash(Arc<FlashImage>);

#[derive(Clone)]
struct FlashImage {
    words: Vec<u16>,
    roots: Vec<u32>,
    decoded: OnceLock<Arc<DecodeTable>>,
}

impl Deref for Flash {
    type Target = Vec<u16>;
    fn deref(&self) -> &Self::Target { &self.0.words }
}

impl DerefMut for Flash {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let image = Arc::make_mut(&mut self.0);
        image.decoded.take();
        &mut image.words
    }
}

impl Flash {
    pub(crate) fn decoded(&self) -> &Arc<DecodeTable> {
        self.0.decoded.get_or_init(|| Arc::new(DecodeTable::new(self)))
    }

    pub(crate) fn set_code_roots(&mut self, roots: &[u32]) {
        let mut roots = roots.to_vec();
        roots.sort_unstable();
        roots.dedup();
        if self.0.roots == roots { return; }
        let image = Arc::make_mut(&mut self.0);
        image.roots = roots;
        image.decoded.take();
    }
}

pub(crate) const LEADER: u8 = 1;
pub(crate) const CONDITIONAL: u8 = 2;
pub(crate) const LANDING: u8 = 4;

// Sixteen bytes per physical flash word: the optional instruction is eight bytes,
// ids are u16 (at most FLASH_WORDS), and width/timing/flags occupy four bytes.
// Width tags 0 and 3 preserve the two fetch errors separately from bad decoding.
#[derive(Clone, Copy)]
struct Decoded {
    insn: Option<Insn>,
    block: u16,
    cond: u16,
    width: u8,
    base: u8,
    extra: u8,
    flags: u8,
}

const _: () = assert!(std::mem::size_of::<Decoded>() == 16);

impl Decoded {
    fn width(self, pc: u32) -> Result<usize, Stop> {
        match self.width {
            1 | 2 => Ok(self.width as usize),
            _ => Err(Stop::BadAddress { pc, addr: pc + u32::from(self.width == 3) }),
        }
    }
}

pub(crate) struct DecodeTable {
    entries: Box<[Decoded]>,
    pub(crate) map: Arc<CodeMap>,
}

impl DecodeTable {
    /// Counting annotations (flags, block id, conditional id). Unmapped addresses belong to block zero.
    pub(crate) fn count_entry(&self, pc: u32) -> (u8, u32, u32) {
        self.entries.get(pc as usize).map_or((0, 0, 0), |e| (e.flags, u32::from(e.block), u32::from(e.cond)))
    }

    fn width(&self, pc: u32) -> Result<usize, Stop> {
        self.entries.get(pc as usize).ok_or(Stop::BadAddress { pc, addr: pc })?.width(pc)
    }

    fn condition(&self, pc: usize) -> Option<Cond> {
        use Insn::*;
        let fall = pc as u32 + 1;
        let (kind, taken) = match self.entries[pc].insn? {
            Brbs(_, k) | Brbc(_, k) =>
                (CondKind::Branch, (fall as u16).wrapping_add(k as i16 as u16) as u32),
            Cpse(..) | Sbrc(..) | Sbrs(..) | Sbic(..) | Sbis(..) =>
                // A failed width fetch has no executable taken edge.
                (CondKind::Skip, fall + self.width(fall).unwrap_or(0) as u32),
            _ => return None,
        };
        Some(Cond { pc: pc as u32, kind, taken, fall })
    }

    // Reachable successors and whether the instruction separates blocks. Calls have
    // both a target and a return edge; indirect calls have only the return edge.
    fn flow(&self, pc: usize) -> ([Option<u32>; 2], bool) {
        use Insn::*;
        let e = self.entries[pc];
        let next = pc as u32 + e.width as u32;
        if let Some(c) = self.condition(pc) { return ([Some(c.fall), Some(c.taken)], true); }
        match e.insn {
            Some(Rjmp(k) | Rcall(k)) => {
                let target = (next as u16).wrapping_add(k as u16) as u32;
                ([Some(target), matches!(e.insn, Some(Rcall(_))).then_some(next)], true)
            }
            Some(Jmp(k) | Call(k)) =>
                ([Some(k), matches!(e.insn, Some(Call(_))).then_some(next)], true),
            Some(Icall) => ([Some(next), None], true),
            Some(Ijmp | Ret | Reti | Break) | None => ([None, None], true),
            _ => ([Some(next), None], false),
        }
    }

    fn new(flash: &Flash) -> Self {
        let entries = (0..FLASH_WORDS).map(|pc| {
            // Exactly fetch_width's checks, in its order, then decode. This is done
            // at EVERY word, including operand words and erased words, independently
            // of the control-flow alignment used for the annotations below.
            let width = match flash.get(pc) {
                None => 0,
                Some(&w) => {
                    let long = matches!(w & 0xfe0e, 0x940c | 0x940e)
                        || matches!(w & 0xfe0f, 0x9000 | 0x9200);
                    if long && (pc + 1 >= flash.len() || pc + 1 >= FLASH_WORDS) { 3 }
                    else if long { 2 } else { 1 }
                }
            };
            let insn = if matches!(width, 1 | 2) {
                decode(&flash[pc..pc + width as usize]).map(|(insn, _)| insn)
            } else { None };
            let (base, extra) = insn.as_ref().map(cycles).unwrap_or_default();
            Decoded { insn, block: 0, cond: 0, width, base, extra,
                flags: if matches!(insn, Some(Insn::Ijmp | Insn::Icall | Insn::Ret)) { LANDING } else { 0 } }
        }).collect::<Vec<_>>().into_boxed_slice();
        let mut table = Self { entries, map: Arc::new(CodeMap::default()) };
        let extent = flash.iter().take(FLASH_WORDS).rposition(|&w| w != 0xffff).map_or(0, |pc| pc + 1);
        let mut reached = vec![false; FLASH_WORDS];
        let mut starts = vec![0u8; FLASH_WORDS];
        let mut pending: Vec<u32> = (0..=50).step_by(2).chain(flash.0.roots.iter().copied()).collect();
        for &root in &pending {
            if let Some(e) = table.entries.get_mut(root as usize) { e.flags |= LEADER; }
        }
        // Visit instruction starts, separately from covered operand words: a named
        // root or a direct target may itself enter an operand word.
        while let Some(pc) = pending.pop().map(|pc| pc as usize) {
            if pc >= FLASH_WORDS || starts[pc] != 0 { continue; }
            let e = table.entries[pc];
            let width = if e.insn.is_some() { e.width as usize } else { 1 };
            starts[pc] = width as u8;
            reached[pc..pc + width].fill(true);
            pending.extend(table.flow(pc).0.into_iter().flatten());
        }
        // A reached long instruction may end in an operand equal to 0xffff. That
        // operand belongs to its block; only the erased words after it are tail.
        let mapped_end = starts.iter().enumerate().filter(|&(pc, width)|
            *width != 0 && table.entries[pc].insn.is_some())
            .map(|(pc, &width)| pc + width as usize).max().unwrap_or(0).max(extent);
        // Align each unreached run independently. A long instruction that would
        // overlap reached code is left as one junk word, never swallowing that code.
        let mut pc = 0;
        while pc < extent {
            if reached[pc] { pc += 1; continue; }
            let end = (pc..extent).find(|&p| reached[p]).unwrap_or(extent);
            table.entries[pc].flags |= LEADER;
            while pc < end {
                let e = table.entries[pc];
                let overlaps = e.insn.is_some() && pc + e.width as usize > end;
                let width = if e.insn.is_some() && !overlaps { e.width as usize } else { 1 };
                // An incomplete swept instruction is junk, not a flow edge. Its
                // execution entry still decodes both words if a PC ever lands here.
                starts[pc] = if overlaps { 0 } else { width as u8 };
                pc += width;
            }
        }
        let mut map = CodeMap::default();
        for pc in 0..mapped_end {
            if starts[pc] == 0 { continue; }
            let (successors, split) = table.flow(pc);
            if split {
                for target in successors.into_iter().flatten().chain([pc as u32 + starts[pc] as u32]) {
                    let t = target as usize;
                    // A junk edge out of swept data never splits a reached instruction from
                    // its operand word: the block would end mid-instruction and its tail
                    // would never be entered at a leader.
                    if t < FLASH_WORDS && reached[t] && starts[t] == 0 { continue; }
                    if let Some(e) = table.entries.get_mut(t) { e.flags |= LEADER; }
                }
            }
            if let Some(cond) = table.condition(pc) {
                map.conds.push(cond);
                table.entries[pc].cond = map.conds.len() as u16;
                table.entries[pc].flags |= CONDITIONAL;
            }
        }
        // Partition by leaders. All instruction words receive their block id; the
        // erased tail stays at zero even when a vector there is marked as a leader.
        for pc in 0..mapped_end {
            if pc == 0 || table.entries[pc].flags & LEADER != 0 {
                table.entries[pc].flags |= LEADER;
                map.blocks.push(Block { start: pc as u32, end: pc as u32 });
            }
            table.entries[pc].block = map.blocks.len() as u16;
            map.blocks.last_mut().unwrap().end = pc as u32 + 1;
        }
        table.map = Arc::new(map);
        table
    }
}

// DS40002061B §24: ADC selection is locked at start, voltage at sample-and-hold.
#[derive(Clone, Copy, Debug)]
struct AnalogSample { volts: f64, supply: f64 }

impl PartialEq for AnalogSample {
    fn eq(&self, other: &Self) -> bool {
        self.volts.to_bits() == other.volts.to_bits() && self.supply.to_bits() == other.supply.to_bits()
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Conversion {
    start: u64,
    sample: u64,
    end: u64,
    channel: u8,
    reference: u8,
    first: bool,
    sampled: bool,
    value: Option<AnalogSample>,
}

impl Conversion {
    fn request(&self) -> SampleAt {
        SampleAt { tick: self.sample, pin: crate::chip::adc_row(self.channel) as u16 }
    }
}

#[derive(Clone)]
struct Adc {
    origin: Option<u64>,
    pending_start: Option<u64>,
    conversion: Option<Conversion>,
    first: bool,
    code: u16,
    locked: Cell<bool>,
    analog: [f64; 8],
    avcc: f64,
    requests: Vec<SampleAt>,
    given: Vec<(SampleAt, AnalogSample)>,
}

impl Default for Adc {
    fn default() -> Self {
        Adc {
            origin: None, pending_start: None, conversion: None, first: true,
            code: 0, locked: Cell::new(false), analog: [0.0; 8],
            // Nominal ATmega328P supply, also pins::ATMEGA328P_5V.vcc.
            avcc: crate::pins::ATMEGA328P_5V.vcc, requests: Vec::new(), given: Vec::new(),
        }
    }
}

impl Adc {
    fn copy_from(&mut self, from: &Self) {
        let Adc { origin, pending_start, conversion, first, code, locked, analog, avcc, requests, given } = from;
        self.origin = *origin;
        self.pending_start = *pending_start;
        self.conversion = *conversion;
        self.first = *first;
        self.code = *code;
        self.locked.set(locked.get());
        self.analog = *analog;
        self.avcc = *avcc;
        self.requests.clone_from(requests);
        self.given.clone_from(given);
    }

    fn same_state(&self, other: &Self) -> bool {
        self.origin == other.origin && self.pending_start == other.pending_start
            && self.conversion == other.conversion && self.first == other.first
            && self.code == other.code && self.locked == other.locked
            && self.analog.map(f64::to_bits) == other.analog.map(f64::to_bits)
            && self.avcc.to_bits() == other.avcc.to_bits()
            && self.requests == other.requests && self.given == other.given
    }

    fn next(&self) -> Option<u64> {
        self.pending_start.or_else(|| self.conversion.map(|c| if c.sampled { c.end } else { c.sample }))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SpiTransfer {
    next: u64,
    half: u64,
    edge: u8,
    control: u8,
    tx: u8,
    rx: u8,
}

impl SpiTransfer {
    fn bit(&self, index: u8) -> u8 {
        if self.control & 0x20 != 0 { index } else { 7 - index }
    }

    fn end(&self) -> u64 { self.next + u64::from(15 - self.edge) * self.half }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Spi {
    transfer: Option<SpiTransfer>,
    // Cell preserves the public shared-reference bus API's status/data read sequence.
    flags: Cell<u8>,
    clear: Cell<bool>,
    // Deterministic idle MOSI: low before the first transfer, then hold its last bit
    // until another transfer shifts it or SPE/MSTR clears.
    mosi: bool,
    sck: bool,
}

impl Spi {
    fn access_data(&self) {
        // §19: SPSR read with SPIF set arms the next SPDR read or write to clear
        // both SPIF and WCOL; an unarmed data access clears neither flag.
        if self.clear.replace(false) { self.flags.set(0); }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct EepromWrite {
    address: usize,
    value: u8,
    mode: u8,
    end: u64,
}

#[derive(Clone)]
pub struct Cpu {
    pub r: [u8; 32],
    pub sreg: u8,
    pub sp: u16,
    pub pc: u32,
    pub cycles: u64,
    // Peripherals are applied through synced; no event lies before horizon.
    synced: u64,
    horizon: u64,
    irq_cached: bool,
    deferred: bool,
    /// SRAM only; element zero is data address 0x100.
    pub sram: Vec<u8>,
    pub flash: Flash,
    pub events: Vec<Event>,
    /// Completed received bytes overwritten by a subsequent start bit.
    pub rx_lost: u64,
    io: [u8; 256],
    driven: [u8; 3],
    levels: [u8; 3],
    drive_gen: u64,
    drive_at: Option<u64>,
    /// With co-simulation timing: the circuit-facing drive word at the last change, and every
    /// change since the last take as (cycle, word before, word after).
    drive_word: [u8; 9],
    drive_log: Vec<(u64, [u8; 9], [u8; 9])>,
    cosim_timing: bool,
    adc: Adc,
    spi: Spi,
    clock_hz: u64,
    // Snapshots share committed bytes; only write completion makes a private copy.
    eeprom: Arc<[u8]>,
    eeprom_write: Option<EepromWrite>,
    eempe_until: Option<u64>,
    temp: Cell<u8>,
    timers: [Timer; 3],
    phase: [u16; 2], // synchronous (Timer0/1), asynchronous (Timer2)
    timer_setup: Cell<Option<TimerSetup>>, // None is the shared setup-dirty flag.
    irq_delay: u8,
    sleeping: bool,
    tx_buffer: Option<u8>,
    tx_end: Option<u64>,
    receive: Cell<Receive>,
    rx_queue: VecDeque<u8>,
    rx_frame: Option<(u64, Option<u8>)>, // end, byte (None after RXEN flush)
    wdt_change_until: Option<u64>,
    fault: Cell<Option<Stop>>,
}

impl Cpu {
    /// Panics if the supplied image cannot fit the physical flash.
    pub fn new(image: &[u16]) -> Cpu {
        assert!(image.len() <= FLASH_WORDS, "image exceeds ATmega328P flash");
        let mut flash = vec![0xffff; FLASH_WORDS];
        flash[..image.len()].copy_from_slice(image);
        let mut io = [0; 256];
        io[0x54] = 2; // MCUSR: external reset; WDTON fuse is unprogrammed.
        io[0xc0] = 0x20; // UCSR0A: empty transmit buffer, no completed frame.
        io[0xc2] = 6; // UCSR0C: asynchronous, eight data bits, one stop bit.
        Cpu {
            r: [0; 32], sreg: 0, sp: 0x08ff, pc: 0, cycles: 0,
            synced: 0, horizon: u64::MAX, irq_cached: false, deferred: false,
            sram: vec![0; SRAM], flash: Flash(Arc::new(FlashImage {
                words: flash, roots: Vec::new(), decoded: OnceLock::new(),
            })), events: Vec::new(), rx_lost: 0, io,
            driven: [0; 3], levels: [0; 3], drive_gen: 0, drive_at: None, drive_word: [0; 9], drive_log: Vec::new(),
            cosim_timing: false, adc: Adc::default(), spi: Spi::default(), temp: Cell::new(0),
            clock_hz: 16_000_000, eeprom: vec![0xff; EEPROM].into(), eeprom_write: None, eempe_until: None,
            timers: [Timer::default(); 3], phase: [0; 2], timer_setup: Cell::new(None),
            irq_delay: 0, sleeping: false, fault: Cell::new(None),
            tx_buffer: None, tx_end: None, wdt_change_until: None,
            receive: Cell::new(Receive::default()), rx_queue: VecDeque::new(), rx_frame: None,
        }
    }

    /// Restores every field, retaining buffer capacity and sharing flash and EEPROM images.
    pub fn copy_from(&mut self, from: &Cpu) {
        let Cpu { r, sreg, sp, pc, cycles, sram, flash, events, rx_lost, io, driven,
            levels, drive_gen, drive_at, drive_word, drive_log, cosim_timing, adc, spi, temp, timers, phase, timer_setup, irq_delay,
            sleeping, tx_buffer, tx_end, receive, rx_queue, rx_frame, wdt_change_until,
            clock_hz, eeprom, eeprom_write, eempe_until,
            fault, synced, horizon, irq_cached, deferred } = from;
        self.r = *r;
        self.sreg = *sreg;
        self.sp = *sp;
        self.pc = *pc;
        self.cycles = *cycles;
        self.synced = *synced;
        self.horizon = *horizon;
        self.irq_cached = *irq_cached;
        self.deferred = *deferred;
        self.sram.clone_from(sram);
        self.flash.clone_from(flash);
        self.events.clone_from(events);
        self.rx_lost = *rx_lost;
        self.io = *io;
        self.driven = *driven;
        self.levels = *levels;
        self.drive_gen = *drive_gen;
        self.drive_at = *drive_at;
        self.drive_word = *drive_word;
        self.drive_log.clone_from(drive_log);
        self.cosim_timing = *cosim_timing;
        self.adc.copy_from(adc);
        self.spi.clone_from(spi);
        self.clock_hz = *clock_hz;
        self.eeprom.clone_from(eeprom);
        self.eeprom_write = *eeprom_write;
        self.eempe_until = *eempe_until;
        self.temp.set(temp.get());
        self.timers = *timers;
        self.phase = *phase;
        self.timer_setup.set(timer_setup.get());
        self.irq_delay = *irq_delay;
        self.sleeping = *sleeping;
        self.tx_buffer = *tx_buffer;
        self.tx_end = *tx_end;
        self.receive.set(receive.get());
        self.rx_queue.clone_from(rx_queue);
        self.rx_frame = *rx_frame;
        self.wdt_change_until = *wdt_change_until;
        self.fault.set(fault.get());
    }

    /// First differing field after catching up both CPUs, excluding the caches: the deferred
    /// step's and the timer setup, a function of the registers that either side may hold.
    pub fn same_state(&self, other: &Cpu) -> Option<&'static str> {
        // Exhaustive so new state cannot silently escape the differential proof.
        let Cpu {
            r: _, sreg: _, sp: _, pc: _, cycles: _, sram: _, flash: _, events: _, rx_lost: _,
            io: _, driven: _, levels: _, drive_gen: _, drive_at: _, drive_word: _, drive_log: _,
            cosim_timing: _, adc: _, spi: _, temp: _, timers: _, phase: _, timer_setup: _, irq_delay: _,
            sleeping: _, tx_buffer: _, tx_end: _, receive: _, rx_queue: _, rx_frame: _,
            wdt_change_until: _, fault: _,
            clock_hz: _, eeprom: _, eeprom_write: _, eempe_until: _,
            synced: _, horizon: _, irq_cached: _, deferred: _,
        } = self;
        if self.r != other.r { return Some("r"); }
        if self.sreg != other.sreg { return Some("sreg"); }
        if self.sp != other.sp { return Some("sp"); }
        if self.pc != other.pc { return Some("pc"); }
        if self.cycles != other.cycles { return Some("cycles"); }
        if self.sram != other.sram { return Some("sram"); }
        if !Arc::ptr_eq(&self.flash.0, &other.flash.0)
            && self.flash.as_slice() != other.flash.as_slice() { return Some("flash"); }
        if self.events != other.events { return Some("events"); }
        if self.rx_lost != other.rx_lost { return Some("rx_lost"); }
        if self.io != other.io { return Some("io"); }
        if self.driven != other.driven { return Some("driven"); }
        if self.levels != other.levels { return Some("levels"); }
        if self.drive_gen != other.drive_gen { return Some("drive_gen"); }
        if self.drive_at != other.drive_at { return Some("drive_at"); }
        if self.drive_word != other.drive_word { return Some("drive_word"); }
        if self.drive_log != other.drive_log { return Some("drive_log"); }
        if self.cosim_timing != other.cosim_timing { return Some("cosim_timing"); }
        if !self.adc.same_state(&other.adc) { return Some("adc"); }
        if self.spi != other.spi { return Some("spi"); }
        if self.clock_hz != other.clock_hz { return Some("clock_hz"); }
        if !Arc::ptr_eq(&self.eeprom, &other.eeprom)
            && self.eeprom != other.eeprom { return Some("eeprom"); }
        if self.eeprom_write != other.eeprom_write { return Some("eeprom_write"); }
        if self.eempe_until != other.eempe_until { return Some("eempe_until"); }
        if self.temp.get() != other.temp.get() { return Some("temp"); }
        if self.timers != other.timers { return Some("timers"); }
        if self.phase != other.phase { return Some("phase"); }
        if self.irq_delay != other.irq_delay { return Some("irq_delay"); }
        if self.sleeping != other.sleeping { return Some("sleeping"); }
        if self.tx_buffer != other.tx_buffer { return Some("tx_buffer"); }
        if self.tx_end != other.tx_end { return Some("tx_end"); }
        if self.receive.get() != other.receive.get() { return Some("receive"); }
        if self.rx_queue != other.rx_queue { return Some("rx_queue"); }
        if self.rx_frame != other.rx_frame { return Some("rx_frame"); }
        if self.wdt_change_until != other.wdt_change_until { return Some("wdt_change_until"); }
        if self.fault.get() != other.fault.get() { return Some("fault"); }
        None
    }

    /// Set the CPU clock before running; standalone CPUs default to 16 MHz.
    pub fn set_clock_hz(&mut self, hz: u64) {
        assert!(hz != 0, "CPU clock must be nonzero");
        self.clock_hz = hz;
    }

    /// Committed EEPROM bytes; an in-flight write is not visible until completion.
    pub fn eeprom(&self) -> &[u8] { &self.eeprom }

    /// The committed EEPROM bytes shared, not copied (`eeprom`): the next write after this copies
    /// them once (`Arc::make_mut`).
    pub fn eeprom_shared(&self) -> Arc<[u8]> { self.eeprom.clone() }

    /// Load EEPROM before running. Panics unless the image is exactly 1,024 bytes.
    pub fn set_eeprom(&mut self, bytes: &[u8]) {
        assert_eq!(bytes.len(), EEPROM, "EEPROM image must be exactly 1,024 bytes");
        self.eeprom = Arc::from(bytes);
    }

    /// Enables exact sleep drive-edge cuts and deadline waits without a wake source.
    pub fn set_cosim_timing(&mut self, on: bool) {
        self.cosim_timing = on;
        self.drive_word = self.drive_word();
        self.drive_log.clear();
    }

    /// The circuit-facing drive of the three ports: DDR, the driven-high bits (timer OC
    /// and SPI overrides included) and the pulled-up input bits, per port.
    fn drive_word(&self) -> [u8; 9] {
        let pud = self.io[0x55] & 0x10 != 0;
        let mut word = [0; 9];
        for p in 0..3 {
            let ddr = self.pin_ddr(p);
            word[p] = ddr;
            word[3 + p] = self.pin_level(p) & ddr;
            word[6 + p] = if pud { 0 } else { self.io[0x25 + 3*p] & !ddr };
        }
        word
    }

    /// Without co-simulation timing, a possible drive change moves the generation; with it,
    /// `note_drive` moves it on each actual change.
    fn may_drive(&mut self) {
        if !self.cosim_timing {
            self.drive_gen = self.drive_gen.wrapping_add(1);
            self.drive_at.get_or_insert(self.cycles);
        }
    }

    /// With co-simulation timing, logs the drive word when it changed at this cycle.
    fn note_drive(&mut self) {
        if !self.cosim_timing { return; }
        let word = self.drive_word();
        if word != self.drive_word {
            self.drive_log.push((self.cycles, self.drive_word, word));
            self.drive_word = word;
            self.drive_gen = self.drive_gen.wrapping_add(1);
            self.drive_at.get_or_insert(self.cycles);
        }
    }

    /// Appends and removes the logged drive changes: (cycle, word before, word after).
    pub(crate) fn take_drive_log(&mut self, out: &mut Vec<(u64, [u8; 9], [u8; 9])>) {
        out.append(&mut self.drive_log);
    }

    /// Takes the first cycle at which drive generation changed since the last take.
    pub fn take_drive_at(&mut self) -> Option<u64> { self.drive_at.take() }

    /// Reads I/O without TEMP, USART FIFO, ADC data-lock or SPI flag-clear side effects.
    pub fn io(&self, addr: u8) -> u8 {
        match addr {
            0x4d => self.io[0x4d] | self.spi.flags.get(),
            0x78 => self.adc_data() as u8,
            0x79 => (self.adc_data() >> 8) as u8,
            _ => self.io[addr as usize],
        }
    }

    /// Pin use and PWM duty without bus side effects; ports B/C/D are 0/1/2.
    pub fn pin_use(&self, p: usize, bit: u8) -> (PinUse, f32) {
        // DS40002061B §24, DIDR0: ADC input with its digital buffer disabled.
        if p == 1 && self.io[0x7e] & (1 << bit) != 0 { return (PinUse::Peripheral, 0.0); }
        // DS40002061B §20.11.3, p. 202: RXEN0/TXEN0 override normal port
        // operation; §14.3.3, pp. 98-99 maps RXD/TXD to PD0/PD1 regardless
        // of DDR. UCSR0B's data-space address is in §36, p. 621.
        if p == 2 && ((bit == 0 && self.io[0xc1] & 0x10 != 0)
            || (bit == 1 && self.io[0xc1] & 0x08 != 0)) {
            return (PinUse::Peripheral, 0.0);
        }
        let base = 0x23 + 3*p;
        let mask = 1 << bit;
        // Table 14-1, p. 86: DDR=0 enables a pull-up only with PORT=1,
        // PUD=0. §14.4.1, p. 100 locates PUD at MCUCR bit 4.
        if self.pin_ddr(p) & mask == 0 {
            let pull_up = self.io[base + 2] & mask != 0 && self.io[0x55] & 0x10 == 0;
            return (if pull_up { PinUse::PullUp } else { PinUse::Input }, 0.0);
        }
        if p == 0 && self.spi_master() && matches!(bit, 3 | 5) {
            return (PinUse::Peripheral, 0.0);
        }
        // OC needs DDR=1: §15.6, p. 106; §16.8, pp. 130-131;
        // §18.11.1, pp. 162-163. connected() applies the COM/WGM tables.
        for (n, pins) in OC_PINS.iter().enumerate() {
            let config = self.timer_config(n);
            for (channel, &pin) in pins.iter().enumerate() {
                if pin != (p, bit) || !config.connected(channel) { continue; }
                // Tables 15-9, p. 117; 16-5, p. 143; 18-9, p. 165:
                // no clock stops the timer, so OC holds its level. §17.4.1,
                // p. 149 also halts timers while their prescaler reset is held.
                if self.divisor(n) == 0 { return (PinUse::Output, 0.0); }
                let com = config.com[channel];
                if config.pwm() && com >= 2 {
                    // §15.5, p. 105; §16.7, p. 129; §18.5, p. 153:
                    // PWM writes address a buffer; only the active latch drives OC.
                    let ocr = self.timers[n].active[channel];
                    let top = self.timer_top(n);
                    // §16.9.3-16.9.5, pp. 133-137 require TOP >= OCR and
                    // Timer1 PWM TOP >= 3. No match occurs with OCR > TOP.
                    // §15.7.4, pp. 109-110 and §18.7.4, pp. 157-158 do not
                    // resolve coincident TOP/BOTTOM (TOP=0) in dual-slope PWM.
                    // Leave these cases as Output rather than inventing a duty.
                    if ocr > top || (n == 1 && top < 3)
                        || (top == 0 && config.wave != Wave::Fast) {
                        return (PinUse::Output, 0.0);
                    }
                    let duty = if config.wave == Wave::Fast {
                        // §15.7.3, pp. 108-109; §16.9.3, pp. 132-134;
                        // §18.7.3, pp. 156-157: include the BOTTOM spike.
                        // Tables 15-3/15-6, pp. 113-114; 16-2, p. 140;
                        // 18-3/18-6, pp. 162-163 and notes: OCR=TOP is constant.
                        (f32::from(ocr) + 1.0) / (f32::from(top) + 1.0)
                    } else {
                        // §15.7.4, pp. 109-110; §16.9.4/16.9.5, pp. 134-138;
                        // §18.7.4, pp. 157-158: dual slopes, continuously low
                        // at OCR=0 and high at OCR=TOP for non-inverting PWM.
                        f32::from(ocr) / f32::from(top)
                    };
                    // The same PWM sections specify opposite polarity for COM=3.
                    return (PinUse::Pwm, if com == 3 { 1.0 - duty } else { duty });
                }
                // Tables 15-2/15-5, pp. 113-114; 16-1, p. 140;
                // 18-2/18-5, pp. 162-163: COM=1 toggles, COM=2/3 only clear/set.
                // PWM toggle-A: Tables 15-3/15-4, pp. 113-114; 16-2/16-3,
                // pp. 140-141; 18-3/18-4, pp. 162-163. Repeated toggles give 50%.
                return if com == 1 { (PinUse::Pwm, 0.5) } else { (PinUse::Output, 0.0) };
            }
        }
        // Table 14-1, p. 86: DDR=1 without an override is an ordinary output.
        (PinUse::Output, 0.0)
    }

    /// The timer's current counter, without the TCNT1 TEMP latch effect.
    pub fn timer_count(&self, n: usize) -> u16 { self.timers[n].counter }

    /// The timer's active A/B compare latches, not its CPU-visible write buffers.
    pub fn timer_compares(&self, n: usize) -> [u16; 2] { self.timers[n].active }

    /// Active configured TOP, including ICR1 or the active OCRnA latch when selected.
    pub fn timer_top(&self, n: usize) -> u16 {
        self.timer_config(n).fixed_top.unwrap_or(self.timers[n].active[0])
    }

    /// Timer clock divisor; zero when stopped or its prescaler is held.
    pub fn timer_prescale(&self, n: usize) -> u32 { self.divisor(n) as u32 }

    /// The timer's waveform mode, using the common machine interface's names.
    pub fn timer_wave(&self, n: usize) -> &'static str {
        match self.timer_config(n).wave {
            Wave::Normal => "normal", Wave::Ctc => "ctc", Wave::Fast => "fast pwm",
            Wave::Phase => "phase-correct pwm", Wave::PhaseFrequency => "phase-and-frequency-correct pwm",
        }
    }

    /// Next compare or overflow tick, even if its interrupt is disabled. Simultaneous
    /// events select A, then B, then overflow, matching timer interrupt priority.
    pub fn timer_next(&self, n: usize) -> Option<(TimerEvent, u64)> {
        self.timer_event(n, 7).map(|(clocks, flags)| {
            let event = if flags & 2 != 0 { TimerEvent::Compare(0) }
                else if flags & 4 != 0 { TimerEvent::Compare(1) } else { TimerEvent::Overflow };
            (event, self.cycles.saturating_add(clocks))
        })
    }

    pub(crate) fn fault(&self) -> Option<Stop> { self.fault.get() }
    pub(crate) fn sleeping(&self) -> bool { self.sleeping }

    pub(crate) fn rx_pending(&self) -> u32 {
        let receive = self.receive.get();
        u32::try_from(receive.len + usize::from(receive.waiting.is_some())
            + usize::from(self.rx_frame.is_some_and(|(_, byte)| byte.is_some()))
            + self.rx_queue.len()).expect("receive count exceeds u32")
    }

    fn bad_address(&self, addr: u32) -> Stop { Stop::BadAddress { pc: self.pc, addr } }

    fn check_data(&self, addr: u16) -> Result<(), Stop> {
        if addr <= 0xff || (addr <= 0x8ff && (addr as usize - 0x100) < self.sram.len()) {
            Ok(())
        } else { Err(self.bad_address(addr as u32)) }
    }

    pub(crate) fn latch(&self, stop: Stop) {
        if self.fault.get().is_none() { self.fault.set(Some(stop)); }
    }

    fn pin_ddr(&self, p: usize) -> u8 {
        let ddr = self.io[0x24 + 3*p];
        if p != 0 || self.io[0x4c] & 0x40 == 0 { return ddr; }
        // §19: master MISO is forced input; slave SS/MOSI/SCK are forced inputs.
        // Slave shifting is not modeled, so its MISO output retains PORTB4.
        ddr & !if self.spi_master() { 0x10 } else { 0x2c }
    }

    fn pin_level(&self, p: usize) -> u8 {
        let base = 0x23 + 3*p;
        let port = self.io[base+2];
        let ddr = self.pin_ddr(p);
        // SPI inputs use the ordinary PORT/PUD pull-up, including forced MISO.
        // Keep the legacy input model unchanged on pins not owned by enabled SPI.
        let pull = if p == 0 && self.io[0x4c] & 0x40 != 0 && self.io[0x55] & 0x10 != 0 {
            port & !0x3c
        } else { port };
        let input = (self.levels[p] & self.driven[p]) | (pull & !self.driven[p]);
        let mut output = port;
        for (n, pins) in OC_PINS.iter().enumerate() {
            let config = self.timer_config(n);
            for (channel, &(port_index, bit)) in pins.iter().enumerate() {
                if port_index == p && config.connected(channel) {
                    if self.timers[n].output[channel] { output |= 1 << bit; }
                    else { output &= !(1 << bit); }
                }
            }
        }
        if p == 0 && self.spi_master() {
            // SPI owns MOSI ahead of OC2A. Idle SCK follows CPOL immediately,
            // including a CPOL write between transfers.
            let sck = if self.spi.transfer.is_some() { self.spi.sck } else { self.io[0x4c] & 8 != 0 };
            output = (output & !0x28) | if self.spi.mosi { 8 } else { 0 }
                | if sck { 0x20 } else { 0 };
        }
        (output & ddr) | (input & !ddr)
    }

    /// Changes whenever GPIO configuration or a driven timer/SPI output may change; with
    /// co-simulation timing, when a pin's drive changes or an ADC sample is requested.
    /// Digital input sampling alone does not change a pin's drive.
    pub fn drive_gen(&self) -> u64 { self.drive_gen }

    /// Effective DDR, PORT and current pin levels, including timer OC and SPI overrides.
    pub fn drive(&self, port: char) -> (u8, u8, u8) {
        let p = match port { 'B' => 0, 'C' => 1, 'D' => 2,
            _ => panic!("GPIO port must be B, C or D") };
        (self.pin_ddr(p), self.io[0x25 + 3*p], self.pin_level(p))
    }

    fn pin_events(&mut self, p: usize, before: u8) {
        if p == 0 && self.check_spi_ss() {
            self.may_drive();
            self.note_drive();
        }
        let now = self.pin_level(p);
        let changed = before ^ now;
        if changed & self.pin_ddr(p) != 0 {
            self.may_drive();
            self.note_drive();
        }
        for bit in 0..8 {
            if changed & (1 << bit) != 0 {
                self.events.push(Event::Pin { cycle: self.cycles,
                    port: ['B','C','D'][p], bit, high: now & (1 << bit) != 0 });
            }
        }
    }

    pub fn set_pin(&mut self, port: char, bit: u8, high: bool) {
        debug_assert!(!self.deferred);
        let p = match port { 'B' => 0, 'C' => 1, 'D' => 2,
            _ => panic!("GPIO port must be B, C or D") };
        assert!(bit < 8, "GPIO bit must be in 0..=7");
        let before = self.pin_level(p);
        self.driven[p] |= 1 << bit;
        if high { self.levels[p] |= 1 << bit; } else { self.levels[p] &= !(1 << bit); }
        self.pin_events(p,before);
    }

    pub fn read(&self, addr: u16) -> u8 {
        if let Err(e) = self.check_data(addr) { self.latch(e); return 0; }
        match addr {
            0..=0x1f => self.r[addr as usize],
            0x23 | 0x29 => self.pin_level((addr as usize - 0x23) / 3),
            0x26 => self.pin_level(1) & !self.io[0x7e], // §24, DIDR0 only gates the digital read.
            0x5d => self.sp as u8, 0x5e => (self.sp >> 8) as u8, 0x5f => self.sreg,
            0x60 => {
                let wdce = self.wdt_change_until.is_some_and(|end| self.cycles < end);
                (self.io[0x60] & !0x10) | if wdce { 0x10 } else { 0 }
            }
            0x46 => self.timers[0].counter as u8,
            0x47 | 0x48 => self.timers[0].ocr[(addr - 0x47) as usize] as u8,
            0x4d => {
                let status = self.io(0x4d);
                if status & 0x80 != 0 { self.spi.clear.set(true); }
                status
            }
            0x4e => { self.spi.access_data(); self.io[0x4e] }
            0x78 => { self.adc.locked.set(true); self.adc_data() as u8 }
            0x79 => { self.adc.locked.set(false); (self.adc_data() >> 8) as u8 }
            0x84 => { self.temp.set((self.timers[1].counter >> 8) as u8); self.timers[1].counter as u8 }
            0x85 => self.temp.get(),
            // "16-bit Timer/Counter1 with PWM", "Accessing 16-bit Registers":
            // ICR1 shares TEMP with TCNT1 and OCR1x writes.
            0x86 => { self.temp.set((self.timers[1].icr >> 8) as u8); self.timers[1].icr as u8 }
            0x87 => self.temp.get(),
            0x88..=0x8b => (self.timers[1].ocr[((addr - 0x88) / 2) as usize] >> ((addr & 1) * 8)) as u8,
            0xb2 => self.timers[2].counter as u8,
            0xb3 | 0xb4 => self.timers[2].ocr[(addr - 0xb3) as usize] as u8,
            0xc0 => self.io[0xc0] | if self.receive.get().len != 0 { 0x80 } else { 0 }
                | if self.receive.get().len != 0 && self.receive.get().fifo[0].1 { 8 } else { 0 },
            0xc6 => {
                let mut rx = self.receive.get();
                let byte = rx.pop();
                self.receive.set(rx);
                byte
            }
            0x20..=0xff => self.io[addr as usize],
            _ => self.sram[addr as usize - 0x100],
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        debug_assert!(!self.deferred);
        self.write_data(addr, value);
    }

    fn write_data(&mut self, addr: u16, value: u8) {
        if let Err(e) = self.check_data(addr) { self.latch(e); return; }
        match addr {
            0..=0x1f => self.r[addr as usize] = value,
            0x23..=0x2b => {
                self.may_drive();
                let p = (addr as usize - 0x23) / 3;
                let before = self.pin_level(p);
                if (addr - 0x23) % 3 == 0 { self.io[addr as usize + 2] ^= value; }
                else { self.io[addr as usize] = value; }
                self.pin_events(p,before);
                self.note_drive();
            }
            // "16-bit Timer/Counter1 with PWM", "TIFR1": ICF1 is W1C too.
            0x35..=0x37 => self.io[addr as usize] &= !(value & if addr == 0x36 { 0x27 } else { 7 }),
            0x3f => self.write_eeprom_control(value, 0xff),
            0x41 | 0x42 => {
                if self.eeprom_write.is_none() {
                    self.io[addr as usize] = value & if addr == 0x42 { 3 } else { 0xff };
                }
            }
            0x43 => { // Datasheet "Timer/Counter0 and Timer/Counter1 Prescalers".
                if value & 1 != 0 { self.phase[0] = 0; }
                if value & 2 != 0 { self.phase[1] = 0; }
                self.io[0x43] = value & 0x83;
                if value & 0x80 == 0 { self.io[0x43] = 0; }
                self.timer_setup.set(None);
            }
            0x44 | 0x45 => self.write_timer_control(0, addr, value),
            0x80 | 0x81 | 0x82 => self.write_timer_control(1, addr, value),
            0xb0 | 0xb1 => self.write_timer_control(2, addr, value),
            0x46 | 0xb2 => {
                let n = if addr == 0x46 { 0 } else { 2 };
                self.timers[n].counter = value as u16;
                self.timers[n].block_compare = true;
            }
            0x47 | 0x48 => self.write_ocr(0, (addr - 0x47) as usize, value as u16),
            0x4c => {
                let before = self.pin_level(0);
                self.io[0x4c] = value;
                // Leaving master mode aborts without SPIF and releases the output
                // levels immediately. If SPE remains set, slave directions still apply.
                if !self.spi_master() { self.spi.transfer = None; self.spi.mosi = false; }
                self.may_drive();
                self.pin_events(0, before);
                self.note_drive();
            }
            0x4d => self.io[0x4d] = value & 1, // SPI2X only; SPIF/WCOL are read-only.
            0x4e => self.write_spi(value),
            0xb3 | 0xb4 => self.write_ocr(2, (addr - 0xb3) as usize, value as u16),
            0xb6 => {
                // "8-bit Timer/Counter2 with PWM and Asynchronous Operation":
                // only the synchronous clock source is modeled.
                self.io[0xb6] = value & 0x60;
                if value & 0x20 != 0 { self.unmodeled("Timer2 asynchronous clock (AS2)"); }
            }
            // Datasheet MCUSR flags clear on ZERO (unlike TIFR1's W1C).
            0x54 => self.io[0x54] &= value & 0x0f,
            // MCUCR.PUD changes the circuit-facing pull-up drive, even without
            // a DDR/PORT write or a change in the sampled PIN register.
            0x55 => {
                let before = self.pin_level(0);
                if (self.io[0x55] ^ value) & 0x10 != 0 {
                    self.may_drive();
                }
                self.io[0x55] = value;
                if self.io[0x4c] & 0x40 != 0 { self.pin_events(0, before); }
                self.note_drive();
            }
            0x5d => self.sp = (self.sp & 0xff00) | value as u16,
            0x5e => self.sp = (self.sp & 0xff) | (value as u16) << 8,
            // Only SEI/RETI have the documented one-instruction deferral.
            0x5f => self.sreg = value,
            0x60 => self.write_watchdog(value),
            0x64 => {
                self.io[0x64] = value;
                self.check_adc_mode();
                if value & 4 != 0 && self.spi.transfer.is_some() {
                    let before = self.pin_level(0);
                    // Deterministic power-down: abort without completing; retain SPE's
                    // pin overrides and last MOSI bit, and return SCK to idle CPOL.
                    self.spi.transfer = None;
                    self.pin_events(0, before);
                }
            }
            0x78 | 0x79 => {} // §24: ADC data registers are read-only.
            0x7a => self.write_adc_control(value),
            0x7b => { self.io[0x7b] = value & 0x47; self.check_adc_mode(); }
            0x7c => self.io[0x7c] = value & 0xef,
            0x7e => self.io[0x7e] = value & 0x3f,
            // "16-bit Timer/Counter1 with PWM", "TIMSK1": ICIE1 enables
            // either the modeled ICR1 TOP interrupt or unmodeled input capture.
            0x6f => { self.io[0x6f] = value; self.check_timer_mode(1); }
            0x85 | 0x87 | 0x89 | 0x8b => self.temp.set(value),
            0x84 => {
                self.timers[1].counter = (self.temp.get() as u16) << 8 | value as u16;
                self.timers[1].block_compare = true;
            }
            0x86 => {
                // "16-bit Timer/Counter1 with PWM", "ICR1H and ICR1L - Input
                // Capture Register 1": WGM must select ICR1 as TOP before a
                // write. Unlike OCR1A TOP, ICR1 is not double buffered.
                if self.timer_config(1).icr_top {
                    self.timers[1].icr = (self.temp.get() as u16) << 8 | value as u16;
                    self.timer_setup.set(None);
                }
            }
            0x88 | 0x8a => self.write_ocr(1, ((addr - 0x88) / 2) as usize,
                (self.temp.get() as u16) << 8 | value as u16),
            0xc0 => { // TXC0 is W1C; U2X0/MPCM0 writable; RXC0/DOR0 read-only.
                self.io[0xc0] = (self.io[0xc0] & 0x60 & !(value & 0x40)) | (value & 3);
            }
            0xc1 => {
                self.io[0xc1] = value;
                if value & 0x10 == 0 {
                    // Datasheet "USART0": disabling RX flushes FIFO and shifter.
                    self.receive.set(Receive::default());
                    if let Some((_, byte)) = &mut self.rx_frame { *byte = None; }
                }
                self.check_receive_format();
                if value & 8 != 0 && self.tx_end.is_none() {
                    if let Some(byte) = self.tx_buffer.take() { self.start_tx(byte); }
                }
            }
            0xc2 => { self.io[0xc2] = value; self.check_receive_format(); }
            0xc5 => self.io[0xc5] = value & 0x0f,
            0xc6 => {
                if self.tx_buffer.is_some() { self.latch(Stop::UdrOverrun { pc: self.pc }); }
                else if self.io[0xc1] & 8 != 0 && self.tx_end.is_none() { self.start_tx(value); }
                else { self.tx_buffer = Some(value); self.io[0xc0] &= !0x20; }
            }
            // Unmodeled peripherals retain plain stores.
            0x20..=0xff => self.io[addr as usize] = value,
            _ => self.sram[addr as usize - 0x100] = value,
        }
    }

    fn eeprom_address(&self) -> usize {
        usize::from(self.io[0x41]) | (usize::from(self.io[0x42]) << 8)
    }

    fn write_eeprom_control(&mut self, value: u8, mask: u8) {
        // Test the OLD EEMPE window: setting EEMPE and EEPE in one store cannot
        // unlock a write. A store at the deadline is already too late.
        let enabled = self.eempe_until.is_some_and(|end| self.cycles < end);
        let busy = self.eeprom_write.is_some();
        let writable = mask & if busy { 8 } else { 0x38 }; // EEPM is locked while busy.
        self.io[0x3f] = (self.io[0x3f] & !writable) | (value & writable);
        if mask & 4 != 0 {
            self.eempe_until = if value & 4 != 0 { Some(self.cycles.saturating_add(4)) } else { None };
            self.io[0x3f] = (self.io[0x3f] & !4) | (value & 4);
        }
        if busy { return; } // EERE and another EEPE cannot disturb the latched write.
        let mut halt = 0;
        if value & mask & 2 != 0 && enabled {
            let mode = (self.io[0x3f] >> 4) & 3;
            if mode == 3 {
                // Stated model choice for reserved EEPM=11: no write, report it
                // through the same explicit stop as other reserved peripheral modes.
                self.unmodeled("EEPROM reserved programming mode (EEPM=11)");
                return;
            }
            // Stated model choice: nominal calibrated RC oscillator write times,
            // independent of the CPU clock. Round up the conversion to CPU cycles;
            // u128 keeps multiplication safe for every accepted u64 clock setting.
            let micros = if mode == 0 { 3400 } else { 1800 };
            let clocks = (u128::from(self.clock_hz) * micros).div_ceil(1_000_000) as u64;
            self.eeprom_write = Some(EepromWrite { address: self.eeprom_address(),
                value: self.io[0x40], mode, end: self.cycles.saturating_add(clocks) });
            self.io[0x3f] |= 2;
            halt = 2;
        } else if value & mask & 1 != 0 {
            self.io[0x40] = self.eeprom[self.eeprom_address()];
            halt = 4;
        }
        // The bus store has already taken effect at this cycle, just like SPDR.
        // Use the instruction/interrupt clock-advance path for the CPU halt: no
        // instruction executes, but all peripheral events retain their own cycles.
        // Public bus writes consume these halt clocks too. EERE is a strobe, never stored.
        if halt != 0 { self.advance(halt); }
    }

    fn eeprom_event(&mut self) {
        if self.eempe_until == Some(self.cycles) {
            self.eempe_until = None;
            self.io[0x3f] &= !4;
        }
        if self.eeprom_write.is_some_and(|write| write.end == self.cycles) {
            let write = self.eeprom_write.take().unwrap();
            let byte = &mut Arc::make_mut(&mut self.eeprom)[write.address];
            *byte = match write.mode { 0 => write.value, 1 => 0xff, 2 => *byte & write.value,
                _ => unreachable!("reserved EEPROM mode never starts") };
            self.io[0x3f] &= !2;
        }
    }

    fn spi_master(&self) -> bool { self.io[0x4c] & 0x50 == 0x50 }

    fn check_spi_ss(&mut self) -> bool {
        if !self.spi_master() || self.io[0x24] & 4 != 0 || self.pin_level(0) & 4 != 0 {
            return false;
        }
        // An input SS low demotes the master immediately; this is a mode-fault
        // SPIF, not a completed receive byte. Discard the partial transfer.
        self.io[0x4c] &= !0x10;
        self.spi.transfer = None;
        self.spi.mosi = false;
        self.spi.flags.set(self.spi.flags.get() | 0x80);
        true
    }

    fn write_spi(&mut self, byte: u8) {
        self.spi.access_data();
        // PRSPI gates starts. Slave mode is direction overrides only: no transfer
        // starts or completes, and disabled/slave writes leave the receive byte alone.
        if !self.spi_master() || self.io[0x64] & 4 != 0 { return; }
        if self.spi.transfer.is_some() {
            self.spi.flags.set(self.spi.flags.get() | 0x40);
            return;
        }
        let before = self.pin_level(0);
        let control = self.io[0x4c];
        let div = [4, 16, 64, 128, 2, 8, 32, 64]
            [usize::from((self.io[0x4d] & 1) * 4 + (control & 3))];
        // Writes take effect at instruction completion (external writes at cycles),
        // just like timers/USART. Use a fresh phase: leading at start + k*div + div/2,
        // trailing at start + (k+1)*div; the eighth trailing edge completes at 8*div.
        // Deterministically latch rate, CPOL/CPHA and DORD for this whole transfer;
        // writes during it configure the next transfer (except SPE/MSTR abort).
        let transfer = SpiTransfer { next: self.cycles + div / 2, half: div / 2,
            edge: 0, control, tx: byte, rx: 0 };
        self.spi.sck = control & 8 != 0;
        // CPHA=0 presents bit zero of the sequence now; CPHA=1 waits for leading.
        if control & 4 == 0 { self.spi.mosi = byte & (1 << transfer.bit(0)) != 0; }
        self.spi.transfer = Some(transfer);
        // SPDR reads the receive buffer, which changes only at successful completion.
        self.pin_events(0, before);
    }

    fn spi_event(&mut self) {
        let Some(mut transfer) = self.spi.transfer else { return; };
        if transfer.next != self.cycles { return; }
        let before = self.pin_level(0);
        let leading = transfer.edge & 1 == 0;
        let cpha = transfer.control & 4 != 0;
        let index = transfer.edge / 2;
        self.spi.sck = (transfer.control & 8 != 0) ^ leading;
        if leading != cpha {
            // Sample the same input level PINB would read at this exact edge,
            // including external drive, forced input direction and PORT/PUD pull-up.
            if self.pin_level(0) & 0x10 != 0 { transfer.rx |= 1 << transfer.bit(index); }
        } else if cpha || index < 7 {
            let bit = transfer.bit(index + u8::from(!cpha));
            self.spi.mosi = transfer.tx & (1 << bit) != 0;
        }
        if transfer.edge == 15 {
            self.io[0x4e] = transfer.rx;
            self.spi.flags.set(self.spi.flags.get() | 0x80);
            self.spi.transfer = None;
        } else {
            transfer.edge += 1;
            transfer.next += transfer.half;
            self.spi.transfer = Some(transfer);
        }
        // Journal each half-clock separately, even inside one instruction at fosc/2.
        self.pin_events(0, before);
    }

    /// Held external-channel voltage for standalone execution, sampled only at sample-and-hold.
    pub fn set_analog(&mut self, channel: u8, volts: f64) {
        self.catch_up();
        self.adc.analog[usize::from(channel)] = volts;
    }

    /// Held AVCC for standalone execution and the internal VBG/GND channels.
    pub fn set_avcc(&mut self, volts: f64) {
        self.catch_up();
        self.adc.avcc = volts;
    }

    pub(crate) fn take_samples(&mut self, out: &mut Vec<SampleAt>) {
        out.append(&mut self.adc.requests);
    }

    pub(crate) fn set_sample(&mut self, at: SampleAt, volts: f64, supply: f64) {
        let value = AnalogSample { volts, supply };
        if let Some(c) = &mut self.adc.conversion {
            if c.channel < 8 && c.sampled && c.request() == at {
                c.value = Some(value);
                return;
            }
        }
        if let Some((_, given)) = self.adc.given.iter_mut().find(|(key, _)| *key == at) { *given = value; }
        else { self.adc.given.push((at, value)); }
    }

    fn adc_data(&self) -> u16 {
        if self.io[0x7c] & 0x20 != 0 { self.adc.code << 6 } else { self.adc.code }
    }

    fn adc_divisor(&self) -> u64 { 1 << (self.io[0x7a] & 7).max(1) }

    fn check_adc_mode(&self) {
        if self.io[0x7a] & 0x80 == 0 { return; }
        if self.io[0x64] & 1 != 0 { self.unmodeled("ADC power reduction (PRADC with ADEN)"); }
        if self.io[0x7a] & 0x20 != 0 && self.io[0x7b] & 7 != 0 {
            self.unmodeled("ADC auto trigger source (ADTS other than free running)");
        }
    }

    fn write_adc_control(&mut self, value: u8) {
        let old = self.io[0x7a];
        if old & 0x40 != 0 && value & 0x80 != 0 && (old ^ value) & 7 != 0 {
            self.unmodeled("ADC prescaler changed during conversion (ADPS)");
        }
        // DS40002061B §24, ADCSRA: ADIF is W1C; ADSC cannot be cleared by a zero write.
        self.io[0x7a] = (value & !0x50) | (old & 0x10 & !value) | ((old | value) & 0x40);
        if value & 0x80 == 0 {
            self.adc.origin = None;
            self.adc.pending_start = None;
            self.adc.conversion = None;
            self.io[0x7a] &= !0x40;
        } else {
            if old & 0x80 == 0 {
                self.adc.origin = Some(self.cycles);
                self.adc.first = true;
            }
            if value & 0x40 != 0 && old & 0x40 == 0 {
                // §24, Figure 24-3 leaves the clock phase choice open. Model the ADC clock as
                // counter bit log2(div)-1, reset when ADEN=0: rises at e+div/2+k*div. As for
                // timer-control writes, e/w are instruction COMPLETION cycles (external write:
                // current cycle). ADSC selects the first rise strictly AFTER its write.
                let div = self.adc_divisor();
                let edge = self.adc.origin.unwrap() + div / 2;
                self.adc.pending_start = Some(if self.cycles < edge { edge }
                    else { edge + ((self.cycles - edge) / div + 1) * div });
            }
        }
        self.check_adc_mode();
    }

    fn start_adc(&mut self) {
        let mux = self.io[0x7c];
        let (channel, reference) = (mux & 0x0f, mux >> 6);
        match reference {
            0 => self.unmodeled("ADC external reference (AREF)"),
            2 => self.unmodeled("ADC reserved reference (REFS=10)"),
            _ => {}
        }
        match channel {
            8 => self.unmodeled("ADC temperature sensor (MUX=8)"),
            9..=13 => self.unmodeled("ADC reserved channel (MUX=9..13)"),
            _ => {}
        }
        let first = self.adc.first;
        let div = self.adc_divisor();
        self.adc.first = false;
        self.adc.pending_start = None;
        self.adc.conversion = Some(Conversion {
            start: self.cycles, sample: self.cycles + (if first { 27 } else { 3 }) * div / 2,
            end: self.cycles + (if first { 25 } else { 13 }) * div,
            channel, reference, first, sampled: false, value: None,
        });
    }

    fn adc_event(&mut self) {
        if self.adc.pending_start == Some(self.cycles) { self.start_adc(); }
        let Some(mut c) = self.adc.conversion else { return; };
        if !c.sampled && c.sample == self.cycles {
            c.sampled = true;
            if self.cosim_timing && c.channel < 8 {
                let at = c.request();
                self.adc.requests.push(at);
                self.drive_gen = self.drive_gen.wrapping_add(1);
                self.drive_at.get_or_insert(self.cycles);
                if let Some(k) = self.adc.given.iter().position(|(key, _)| *key == at) {
                    c.value = Some(self.adc.given.remove(k).1);
                }
            } else {
                let volts = match c.channel {
                    0..=7 => self.adc.analog[usize::from(c.channel)],
                    14 => 1.1,
                    _ => 0.0,
                };
                c.value = Some(AnalogSample { volts, supply: self.adc.avcc });
            }
            self.adc.conversion = Some(c);
        }
        if c.end != self.cycles { return; }
        if let Some(value) = c.value {
            if !self.adc.locked.get() {
                // §24: ADC = floor(Vin * 1024 / Vref), saturated to ten bits. Model choice:
                // a NaN voltage (pad outside the circuit) is ground; a NaN supply is nominal 5 V.
                let vin = if value.volts.is_nan() { 0.0 } else { value.volts };
                let vref = if c.reference == 3 { 1.1 }
                    else if value.supply.is_nan() { crate::pins::ATMEGA328P_5V.vcc } else { value.supply };
                self.adc.code = (vin / vref * 1024.0).floor().clamp(0.0, 1023.0) as u16;
                self.io[0x7a] |= 0x10;
            }
        } else { self.unmodeled("ADC sample not given by the co-simulation"); }
        self.adc.conversion = None;
        if self.io[0x7a] & 0x20 != 0 && self.io[0x7b] & 7 == 0 {
            // §24: completion and the free-running successor's selection latch share a cycle.
            self.start_adc();
        } else { self.io[0x7a] &= !0x40; }
    }

    fn write_watchdog(&mut self, value: u8) {
        let old = self.io[0x60];
        // A write completing on the fourth clock is inside the timed window.
        let unlocked = self.wdt_change_until.is_some_and(|end| self.cycles <= end);
        let opening = value & 0x18 == 0x18;
        let protected = if unlocked { value & 0x2f }
            else { (old & 0x2f) | (value & 8) }; // WDE may always be SET.
        self.io[0x60] = (old & 0x80 & !value) | (value & 0x40) | protected;
        if self.io[0x54] & 8 != 0 { self.io[0x60] |= 8; } // WDRF forces WDE.
        self.wdt_change_until = if opening { Some(self.cycles.saturating_add(4)) } else { None };
        if !opening && self.io[0x60] & 8 != 0 { self.latch(Stop::Watchdog { pc: self.pc }); }
    }

    fn bit_write(&mut self, addr: u16, bit: u8, set: bool) {
        // SBI/CBI affect only the addressed bit, including in PINx and W1C
        // registers. A byte-wide read/modify/write would spuriously toggle or
        // acknowledge other bits that happened to read as one.
        if addr == 0x3f {
            // SBI EEPE must not set EEMPE again (or renew its four-cycle window).
            self.write_eeprom_control(if set { 1 << bit } else { 0 }, 1 << bit);
        } else if matches!(addr, 0x23 | 0x26 | 0x29 | 0x35..=0x37) {
            self.write_data(addr, if set { 1 << bit } else { 0 });
        } else {
            let v = self.read(addr);
            self.write_data(addr, if set { v | (1 << bit) } else { v & !(1 << bit) });
        }
    }

    fn unmodeled(&self, what: &'static str) {
        self.latch(Stop::Unmodeled { pc: self.pc, what });
    }

    fn timer_mode(&self, n: usize) -> u8 {
        let high = self.io[TIMER_B[n]];
        (self.io[TIMER_A[n]] & 3) | if n == 1 { (high >> 1) & 12 } else { (high >> 1) & 4 }
    }

    fn timer_config(&self, n: usize) -> TimerConfig {
        self.timer_setup().configs[n]
    }

    fn decode_timer_config(&self, n: usize) -> TimerConfig {
        let mode = self.timer_mode(n);
        // "16-bit Timer/Counter1 with PWM", "Waveform Generation Mode Bit
        // Description": OCR1A TOP uses the active latch; ICR1 TOP is immediate.
        let icr_top = n == 1 && matches!(mode, 8 | 10 | 12 | 14);
        let (wave, fixed_top) = if n == 1 {
            match mode {
                1..=3 => (Wave::Phase, Some((1 << (7 + mode)) - 1)),
                4 => (Wave::Ctc, None),
                5..=7 => (Wave::Fast, Some((1 << (3 + mode)) - 1)),
                8 | 9 => (Wave::PhaseFrequency, None),
                10 | 11 => (Wave::Phase, None),
                12 => (Wave::Ctc, None),
                14 | 15 => (Wave::Fast, None),
                // Reserved modes latch a stop on configuration writes only
                // while CS != 0; a stopped timer never ticks this fallback.
                _ => (Wave::Normal, Some(0xffff)),
            }
        } else {
            match mode {
                1 => (Wave::Phase, Some(255)), 2 => (Wave::Ctc, None),
                3 => (Wave::Fast, Some(255)), 5 => (Wave::Phase, None),
                7 => (Wave::Fast, None), _ => (Wave::Normal, Some(255)),
            }
        };
        let a = self.io[TIMER_A[n]];
        // "16-bit Timer/Counter1 with PWM", "Compare Output Mode, Fast PWM"
        // and "Compare Output Mode, Phase Correct and Phase and Frequency
        // Correct PWM": COM1A=01 toggles only in 9/11/14/15; never COM1B.
        TimerConfig { wave, fixed_top: if icr_top { Some(self.timers[1].icr) } else { fixed_top },
            icr_top, max: if n == 1 { 0xffff } else { 255 },
            com: [(a >> 6) & 3, (a >> 4) & 3],
            toggle_a: if n == 1 { matches!(mode, 9 | 11 | 14 | 15) } else { mode & 4 != 0 } }
    }

    fn check_timer_mode(&self, n: usize) {
        // "16-bit Timer/Counter1 with PWM", "Clock Select Bit Description"
        // and "Input Capture Unit": refuse unsupported operation only when
        // clocked, allowing stopped intermediate WGM/ICIE configurations.
        // ICP1/comparator capture and ICES1/ICNC1 are not modeled; ICIE1 is
        // supported only for the ICR1 TOP event while the timer is clocked.
        let cs = self.io[TIMER_B[n]] & 7;
        if cs == 0 { return; }
        if n != 2 && cs >= 6 { self.unmodeled("external Timer0/1 clock"); }
        let mode = self.timer_mode(n);
        if n == 1 {
            if mode == 13 { self.unmodeled("Timer1 WGM 13 (reserved)"); }
            if !self.timer_config(n).icr_top && self.io[0x6f] & 0x20 != 0 {
                self.unmodeled("Timer1 input capture");
            }
        } else if matches!(mode, 4 | 6) { self.unmodeled("reserved 8-bit timer WGM"); }
    }

    fn write_timer_control(&mut self, n: usize, addr: u16, value: u8) {
        self.may_drive();
        let before = [self.pin_level(0), self.pin_level(2)];
        let force = if (n != 1 && addr as usize == TIMER_B[n]) || addr == 0x82 {
            value & 0xc0
        } else { 0 };
        // FOC bits are strobes and always read zero; reserved bits read zero.
        self.io[addr as usize] = if addr as usize == TIMER_A[n] { value & 0xf3 }
            else if n != 1 { value & 0x0f }
            else if addr == 0x82 { 0 } else { value & 0xdf };
        self.timer_setup.set(None);
        self.check_timer_mode(n);
        let config = self.timer_config(n);
        if !config.pwm() {
            self.timers[n].active = self.timers[n].ocr;
            for channel in 0..2 {
                // Forced matches change OC only: no flag and no CTC clear.
                if force & (0x80 >> channel) != 0 {
                    self.timers[n].compare_output(config, channel);
                }
            }
        }
        self.pin_events(0, before[0]);
        self.pin_events(2, before[1]);
    }

    fn write_ocr(&mut self, n: usize, channel: usize, mut value: u16) {
        let config = self.timer_config(n);
        // "16-bit Timer/Counter1 with PWM", "OCR1A/OCR1B - Output Compare
        // Registers": unused OCR bits in fixed
        // resolution PWM are masked on writes, including the CPU-visible buffer.
        if n == 1 && matches!(self.timer_mode(n), 1..=3 | 5..=7) {
            if let Some(top) = config.fixed_top { value &= top; }
        }
        self.timers[n].ocr[channel] = value;
        if !config.pwm() { self.timers[n].active[channel] = value; }
    }

    fn prescaler_held(&self, index: usize) -> bool {
        self.io[0x43] & (0x80 | (1 << index)) == (0x80 | (1 << index))
    }

    fn divisor(&self, n: usize) -> u64 {
        self.timer_setup().divisors[n]
    }

    fn timer_setup(&self) -> TimerSetup {
        if let Some(setup) = self.timer_setup.get() { return setup; }
        // Only TCCRnA/B, GTCCR and committed ICR1 writes feed this setup.
        // The control helper also invalidates on TCCR1C's force strobes.
        // OCR-TOP stays live through Timer::active, outside this cache.
        let held = [self.prescaler_held(0), self.prescaler_held(1)];
        let divisors = [0, 1, 2].map(|n| {
            if held[usize::from(n == 2)] { return 0; }
            let cs = (self.io[TIMER_B[n]] & 7) as usize;
            if n == 2 { [0, 1, 8, 32, 64, 128, 256, 1024][cs] }
            else { [0, 1, 8, 64, 256, 1024, 0, 0][cs] }
        });
        let setup = TimerSetup {
            divisors, configs: [0, 1, 2].map(|n| self.decode_timer_config(n)), held,
        };
        self.timer_setup.set(Some(setup));
        setup
    }

    fn check_receive_format(&self) {
        if self.io[0xc1] & 0x10 != 0 {
            if self.io[0xc2] & 0x30 != 0 { self.unmodeled("USART0 receive parity"); }
            if self.io[0xc1] & 4 != 0 { self.unmodeled("USART0 receive 9-bit/reserved character size"); }
        }
    }

    /// Queue a valid USART0 frame. Its start bit begins now if the line is
    /// idle; queued frames start at the preceding frame's end, without a gap.
    /// RXEN0 gates reception, not the external sender's queued wire traffic.
    pub fn rx(&mut self, byte: u8) {
        debug_assert!(!self.deferred);
        if self.rx_frame.is_none() { self.start_rx(byte); }
        else { self.rx_queue.push_back(byte); }
    }

    fn start_rx(&mut self, byte: u8) {
        let enabled = self.io[0xc1] & 0x10 != 0;
        if enabled {
            let mut rx = self.receive.get();
            if rx.waiting.take().is_some() {
                self.rx_lost += 1;
                rx.pending_dor = true;
            }
            self.receive.set(rx);
        }
        // Only the configured 5..8 data bits are shifted into UDR0.
        let mask = (0xffu16 >> (3 - ((self.io[0xc2] >> 1) & 3))) as u8;
        self.rx_frame = Some((self.cycles.saturating_add(self.frame_clocks()),
            enabled.then_some(byte & mask)));
    }

    fn frame_clocks(&self) -> u64 {
        // UCSZ02 lives in UCSR0B; UCSZ01:0, UPM01:0 and USBS0 in UCSR0C.
        // UCSZ=4..6 is reserved; only 0..3 (5..8 bits) and 7 (9 bits) are defined.
        let size = (self.io[0xc1] & 4) | ((self.io[0xc2] >> 1) & 3);
        let data = if size == 7 { 9 } else { 5 + (size & 3) as u64 };
        let parity = u64::from(self.io[0xc2] & 0x30 != 0);
        let stops = 1 + u64::from(self.io[0xc2] & 8 != 0);
        let baud = u16::from_le_bytes([self.io[0xc4],self.io[0xc5]]) as u64;
        let oversample = if self.io[0xc0] & 2 != 0 { 8 } else { 16 };
        (1 + data + parity + stops) * oversample * (baud + 1)
    }

    fn start_tx(&mut self, byte: u8) {
        self.io[0xc0] |= 0x20;
        self.tx_end = Some(self.cycles.saturating_add(self.frame_clocks()));
        self.events.push(Event::Tx { cycle: self.cycles, byte });
    }

    fn peripheral_next(&self, mut delta: u64, setup: TimerSetup) -> (u64, [u64; 3]) {
        let TimerSetup { divisors, configs, .. } = setup;
        let mut next = [0; 3];
        for (n, &div) in divisors.iter().enumerate() {
            if div != 0 {
                // All divisors divide 1024. The first non-quiet edge is
                // quiet + 1 ticks away, including the current phase.
                next[n] = (self.timers[n].quiet(configs[n]) + 1) * div
                    - (self.phase[usize::from(n == 2)] as u64 & (div - 1));
                delta = delta.min(next[n]);
            }
        }
        if let Some(at) = self.tx_end { delta = delta.min(at - self.cycles); }
        if let Some((at, _)) = self.rx_frame { delta = delta.min(at - self.cycles); }
        if let Some(at) = self.adc.next() { delta = delta.min(at - self.cycles); }
        // Every SPI edge bounds the deferred horizon; completion shares edge 16.
        if let Some(transfer) = self.spi.transfer { delta = delta.min(transfer.next - self.cycles); }
        // Both EEPROM completion and EEMPE expiry are applied at their own cycle,
        // including across deferred instructions and CPU halt clocks.
        if let Some(write) = self.eeprom_write { delta = delta.min(write.end - self.cycles); }
        if let Some(at) = self.eempe_until { delta = delta.min(at - self.cycles); }
        (delta, next)
    }

    fn refresh(&mut self) {
        debug_assert_eq!(self.synced, self.cycles);
        let (delta, _) = self.peripheral_next(u64::MAX, self.timer_setup());
        self.horizon = self.synced.saturating_add(delta);
        self.irq_cached = self.pending().is_some();
    }

    fn deferred_advance(&mut self, clocks: u64) {
        let end = self.cycles.saturating_add(clocks);
        if end < self.horizon {
            self.cycles = end;
            return;
        }
        self.cycles = self.synced;
        self.advance(end - self.synced);
        self.synced = self.cycles;
        self.refresh();
    }

    /// Apply outstanding quiet clocks before inspecting or changing the CPU externally.
    pub fn catch_up(&mut self) {
        if !self.deferred { return; }
        if self.synced < self.cycles {
            let end = self.cycles;
            self.cycles = self.synced;
            self.advance(end - self.synced);
            self.synced = self.cycles;
        }
        self.deferred = false;
    }

    fn advance(&mut self, clocks: u64) {
        self.advance_clipped(clocks, false);
    }

    // Only sleep waits request a cut; instruction and wake/entry clocks never do.
    // Returns true even for an edge at the end, so sleep remains resumable there.
    fn advance_clipped(&mut self, clocks: u64, on_drive: bool) -> bool {
        let end = self.cycles.saturating_add(clocks);
        let generation = self.drive_gen;
        let TimerSetup { divisors, configs, held } = self.timer_setup();
        while self.cycles < end {
            let (delta, next) = self.peripheral_next(end - self.cycles, TimerSetup { divisors, configs, held });
            for (n, &div) in divisors.iter().enumerate() {
                if div != 0 {
                    let phase = self.phase[usize::from(n == 2)] as u64 & (div - 1);
                    // A running timer bounds delta by at most 65536 * 1024,
                    // so this addition cannot overflow. Divisors are powers of two.
                    let ticks = (phase + delta) >> div.trailing_zeros();
                    self.timers[n].count(configs[n], ticks - u64::from(delta == next[n]));
                }
            }
            for index in 0..2 {
                // Datasheet "Timer/Counter0 and Timer/Counter1 Prescalers"
                // and "8-bit Timer/Counter2 with PWM and Asynchronous Operation":
                // free-running modulo-1024 phases, frozen only while held.
                if !held[index] {
                    self.phase[index] = ((self.phase[index] as u64 + delta % 1024) % 1024) as u16;
                }
            }
            self.cycles += delta;
            // Only non-quiet edges can change flags or pins. Preserve each
            // timestamp and timer order, including simultaneous USART events.
            for (n, &div) in divisors.iter().enumerate() {
                if div != 0 && delta == next[n] {
                    let before = [self.pin_level(0), self.pin_level(2)];
                    self.io[0x35 + n] |= self.timers[n].tick(configs[n]);
                    self.pin_events(0, before[0]);
                    self.pin_events(2, before[1]);
                }
            }
            if self.rx_frame.is_some_and(|(at, _)| at == self.cycles) {
                let (_, byte) = self.rx_frame.take().unwrap();
                if self.io[0xc1] & 0x10 != 0 {
                    if let Some(byte) = byte {
                        let mut rx = self.receive.get();
                        rx.complete(byte);
                        self.receive.set(rx);
                    }
                }
                // Completion precedes the next start on the same clock. A full
                // FIFO therefore loses the waiting frame at this start bit.
                if let Some(byte) = self.rx_queue.pop_front() { self.start_rx(byte); }
            }
            if self.tx_end == Some(self.cycles) {
                self.tx_end = None;
                // Disabling TXEN0 takes effect only after both registers drain.
                if let Some(byte) = self.tx_buffer.take() { self.start_tx(byte); }
                else {
                    self.io[0xc0] |= 0x40;
                    self.events.push(Event::TxDone { cycle: self.cycles });
                }
            }
            // Process §24 ADC events after the existing timer/USART order, before any cut.
            self.adc_event();
            self.spi_event();
            self.eeprom_event();
            // Finish all peripherals on this clock before cutting. Otherwise a
            // simultaneous USART completion would be left at a zero delta and
            // the next advance could tick the timers twice on the same clock.
            if on_drive && self.drive_gen != generation { return true; }
        }
        false
    }

    fn pending(&self) -> Option<u32> {
        self.pending_irqs().next().map(|(vector, _)| vector)
    }

    pub(crate) fn pending_irqs(&self) -> impl Iterator<Item = (u32, &'static str)> + '_ {
        TIMER_IRQS.into_iter().zip(TIMER_SOURCES).filter_map(move |((vector, n, flag), name)| {
            (self.io[0x35 + n] & self.io[0x6e + n] & flag != 0).then_some((vector, name))
        }).chain((self.io[0x4c] & self.spi.flags.get() & 0x80 != 0).then_some((34, "SPI")))
            .chain([(36, 0x80, "USART_RX"), (38, 0x20, "USART_UDRE"), (40, 0x40, "USART_TX")]
            .into_iter().filter_map(move |(vector, flag, name)| {
                let status = self.io[0xc0] | if self.receive.get().len != 0 { 0x80 } else { 0 };
                (status & self.io[0xc1] & flag != 0).then_some((vector, name))
            })).chain((self.io[0x7a] & 0x18 == 0x18).then_some((42, "ADC")))
            .chain((self.io[0x3f] & 0x0a == 8).then_some((44, "EE_READY")))
    }

    fn irq_due(&self) -> Option<u32> {
        if self.irq_delay == 0 && self.sreg & I != 0 { self.pending() } else { None }
    }

    /// Whether the next step executes the instruction at PC, rather than sleeping,
    /// reporting a fault or entering an interrupt. Shares the step's IRQ gate.
    pub fn will_execute(&self) -> bool {
        self.fault.get().is_none() && !self.sleeping && self.irq_due().is_none()
    }

    /// The deferred step's IRQ gate; outside a deferred run the cache is not current.
    pub fn will_execute_deferred(&self) -> bool {
        if !self.deferred { return self.will_execute(); }
        self.fault.get().is_none() && !self.sleeping
            && !(self.irq_delay == 0 && self.sreg & I != 0 && self.irq_cached)
    }

    fn push(&mut self, v: u8) -> Result<(), Stop> {
        self.check_data(self.sp)?;
        self.write_data(self.sp,v);
        self.sp = self.sp.wrapping_sub(1);
        Ok(())
    }

    fn pop(&mut self) -> Result<u8, Stop> {
        let addr = self.sp.wrapping_add(1);
        self.check_data(addr)?;
        self.sp = addr;
        Ok(self.read(addr))
    }

    fn check_push_pc(&self) -> Result<(), Stop> {
        self.check_data(self.sp)?;
        self.check_data(self.sp.wrapping_sub(1))
    }

    fn push_pc(&mut self, pc: u32) -> Result<(), Stop> {
        self.push(pc as u8)?;
        self.push((pc >> 8) as u8)
    }

    fn interrupt(&mut self, vector: u32) -> Result<(), Stop> {
        self.check_push_pc()?;
        self.sreg &= !I;
        // Acknowledge the selected request before clocks advance, so a new
        // occurrence during entry is not lost.
        for (candidate, n, flag) in TIMER_IRQS {
            if vector == candidate { self.io[0x35 + n] &= !flag; }
        }
        if vector == 34 { self.spi.flags.set(self.spi.flags.get() & !0x80); } // SPI entry preserves WCOL.
        if vector == 40 { self.io[0xc0] &= !0x40; }
        if vector == 42 { self.io[0x7a] &= !0x10; }
        // RXC0, UDRE0 and EE READY are levels: entry does not acknowledge them.
        self.advance(4);
        self.push_pc(self.pc)?;
        self.pc = vector;
        self.sleeping = false;
        Ok(())
    }

    /// Clocks until any modeled enabled interrupt, including the timer clock
    /// after equality on which OCF1A is asserted. None means no wake source.
    fn wake_clocks(&self) -> Option<u64> {
        self.wake_source().map(|(clocks, _)| clocks)
    }

    /// Cycle of the next wake request while asleep; excludes the wake/entry latency.
    pub fn wake_at(&self) -> Option<u64> {
        if self.sleeping { self.wake_clocks().map(|clocks| self.cycles.saturating_add(clocks)) }
        else { None }
    }

    pub(crate) fn wake_irq(&self) -> Option<(u64, &'static str, u32)> {
        if !self.sleeping { return None; }
        let (clocks, vector) = self.wake_source()?;
        let source = TIMER_IRQS.iter().position(|&(v, _, _)| v == vector)
            .map(|n| TIMER_SOURCES[n]).unwrap_or_else(|| match vector {
                34 => "SPI", 36 => "USART_RX", 38 => "USART_UDRE", 40 => "USART_TX", 42 => "ADC",
                44 => "EE_READY",
                _ => unreachable!("projected wake source has a name"),
            });
        Some((self.cycles.saturating_add(clocks), source, vector))
    }

    // Project the request and its source together, using vector order to break ties.
    fn wake_source(&self) -> Option<(u64, u32)> {
        if self.sreg & I == 0 { return None; }
        if let Some(vector) = self.pending() { return Some((0, vector)); }
        let mut wait = None;
        let mut consider = |clocks, vector| {
            let candidate = (clocks, vector);
            wait = Some(wait.map_or(candidate, |old: (u64, u32)| old.min(candidate)));
        };
        if self.io[0x4c] & 0x80 != 0 {
            if let Some(transfer) = self.spi.transfer { consider(transfer.end() - self.cycles, 34); }
        }
        if self.io[0x3f] & 8 != 0 {
            if let Some(write) = self.eeprom_write { consider(write.end - self.cycles, 44); }
        }
        if self.io[0x7a] & 8 != 0 && !self.adc.locked.get() {
            let end = self.adc.conversion.map(|c| c.end).or_else(|| self.adc.pending_start
                .map(|start| start + (if self.adc.first { 25 } else { 13 }) * self.adc_divisor()));
            if let Some(end) = end { consider(end - self.cycles, 42); }
        }
        if let Some(end) = self.tx_end {
            if self.io[0xc1] & 0x20 != 0 { consider(end - self.cycles, 38); }
            if self.io[0xc1] & 0x40 != 0 {
                let idle = end - self.cycles + if self.tx_buffer.is_some() { self.frame_clocks() } else { 0 };
                consider(idle, 40);
            }
        }
        if self.io[0xc1] & 0x90 == 0x90 {
            if let Some((end, byte)) = self.rx_frame {
                // A flushed in-flight frame cannot wake; the next queued one can.
                if byte.is_some() || !self.rx_queue.is_empty() {
                    let rx = end - self.cycles + if byte.is_some() { 0 } else { self.frame_clocks() };
                    consider(rx, 36);
                }
            }
        }
        for n in 0..3 {
            // "16-bit Timer/Counter1 with PWM", "TIMSK1": include ICIE1
            // in the same tick projection used for the compare/overflow flags.
            let mask = self.io[0x6e + n] & if n == 1 { 0x27 } else { 7 };
            if let Some((clocks, flags)) = self.timer_event(n, mask) {
                let vector = TIMER_IRQS.iter().find(|&&(_, timer, flag)| timer == n && flags & flag != 0)
                    .expect("projected timer flag has a vector").0;
                consider(clocks, vector);
            }
        }
        wait
    }

    // Shared by sleep projection and the side-effect-free timer snapshot.
    fn timer_event(&self, n: usize, mask: u8) -> Option<(u64, u8)> {
        let div = self.divisor(n);
        if div == 0 || mask == 0 { return None; }
        self.timers[n].next_flags(self.timer_config(n), mask).map(|(ticks, flags)| {
            (ticks * div - self.phase[usize::from(n == 2)] as u64 % div, flags)
        })
    }

    fn sleep_step(&mut self, deadline: u64) -> Result<(), Stop> {
        let Some(wait) = self.wake_clocks() else {
            if !self.cosim_timing { return Err(Stop::Sleep); }
            return if self.advance_clipped(deadline.saturating_sub(self.cycles), true) { Ok(()) }
                else { Err(Stop::Cycles) };
        };
        if wait > deadline.saturating_sub(self.cycles) {
            if self.advance_clipped(deadline.saturating_sub(self.cycles), self.cosim_timing) { return Ok(()); }
            return Err(Stop::Cycles);
        }
        if self.advance_clipped(wait, self.cosim_timing) { return Ok(()); }
        if self.cycles >= deadline { return Err(Stop::Cycles); }
        self.advance(IDLE_WAKE_CYCLES);
        if let Some(vector) = self.pending() { self.interrupt(vector) }
        else { Err(Stop::Sleep) }
    }

    fn pair(&self, lo: usize) -> u16 { u16::from_le_bytes([self.r[lo],self.r[lo+1]]) }
    fn set_pair(&mut self, lo: usize, v: u16) {
        let [l,h] = v.to_le_bytes(); self.r[lo] = l; self.r[lo+1] = h;
    }

    fn flag(&mut self, mask: u8, value: bool) {
        if value { self.sreg |= mask; } else { self.sreg &= !mask; }
    }

    fn nzvs(&mut self, result: u8, overflow: bool) {
        let negative = result & 0x80 != 0;
        self.flag(Z,result == 0);
        self.flag(N,negative);
        self.flag(V,overflow);
        self.flag(S,negative ^ overflow);
    }

    fn add(&mut self, a: u8, b: u8, carry: u8) -> u8 {
        let sum = a as u16 + b as u16 + carry as u16;
        let result = sum as u8;
        self.flag(H,(a & 15) + (b & 15) + carry > 15);
        self.flag(C,sum > 255);
        self.nzvs(result,(!(a ^ b) & (a ^ result)) & 0x80 != 0);
        result
    }

    fn sub(&mut self, a: u8, b: u8, carry: u8, sticky_zero: bool) -> u8 {
        let old_z = self.sreg & Z != 0;
        let result = a.wrapping_sub(b).wrapping_sub(carry);
        self.flag(H,((a & 15) as u16) < (b & 15) as u16 + carry as u16);
        self.flag(C,(a as u16) < b as u16 + carry as u16);
        self.nzvs(result,((a ^ b) & (a ^ result)) & 0x80 != 0);
        if sticky_zero { self.flag(Z,result == 0 && old_z); }
        result
    }

    fn fetch_width(&self, pc: u32) -> Result<usize, Stop> {
        let w = *self.flash.get(pc as usize).filter(|_| pc < FLASH_WORDS as u32)
            .ok_or(Stop::BadAddress { pc, addr: pc })?;
        let long = matches!(w & 0xfe0e, 0x940c | 0x940e) || matches!(w & 0xfe0f, 0x9000 | 0x9200);
        if long && (pc as usize + 1 >= self.flash.len() || pc as usize + 1 >= FLASH_WORDS) {
            return Err(Stop::BadAddress { pc, addr: pc + 1 });
        }
        Ok(if long { 2 } else { 1 })
    }

    /// One instruction, or (when sleeping/pending) one interrupt entry.
    pub fn step(&mut self) -> Result<(), Stop> { self.step_until(u64::MAX) }

    /// Run until this many additional clocks have elapsed, stopping at instruction
    /// boundaries. The final instruction/interrupt entry may overrun the budget;
    /// sleeping waits are clipped exactly and can be resumed by another run.
    pub fn run(&mut self, max_cycles: u64) -> Stop {
        if let Some(e) = self.fault.get() { return e; }
        let deadline = self.cycles.saturating_add(max_cycles);
        while self.cycles < deadline {
            if let Err(stop) = self.step_until(deadline) { return stop; }
        }
        Stop::Cycles
    }

    /// One instruction/entry, or a sleep wait bounded by an absolute cycle deadline.
    pub fn step_until(&mut self, deadline: u64) -> Result<(), Stop> {
        debug_assert!(!self.deferred);
        if let Some(e) = self.fault.get() { return Err(e); }
        if self.sleeping { return self.sleep_step(deadline); }
        if let Some(vector) = self.irq_due() { return self.interrupt(vector); }
        let count = self.fetch_width(self.pc)?;
        let (insn,_) = decode(&self.flash[self.pc as usize..self.pc as usize + count])
            .ok_or(Stop::BadInsn { pc: self.pc })?;
        if self.irq_delay != 0 { self.irq_delay -= 1; }
        self.execute::<false>(insn, count, None, (0, 0)).map(|_| ())
    }

    /// One step sharing the oracle's semantics, with quiet peripheral clocks deferred.
    /// Call catch_up before any external access other than will_execute_deferred.
    pub fn step_deferred(&mut self, deadline: u64) -> Result<(), Stop> {
        let table = self.flash.decoded().clone();
        self.step_decoded(deadline, &table, None)
    }

    // AvrMachine resolves and retains the table once per run: no OnceLock or Arc
    // operations in its instruction loop. The public single-step API resolves per call.
    pub(crate) fn step_decoded(&mut self, deadline: u64, table: &DecodeTable,
        tally: Option<&mut Tally>) -> Result<(), Stop> {
        if !self.deferred {
            self.synced = self.cycles;
            self.refresh();
            self.deferred = true;
        }
        if let Some(e) = self.fault.get() { return Err(e); }
        if self.sleeping {
            self.catch_up();
            let result = self.sleep_step(deadline);
            self.synced = self.cycles;
            self.refresh();
            self.deferred = true;
            return result;
        }
        if self.irq_delay == 0 && self.sreg & I != 0 && self.irq_cached {
            self.catch_up();
            let vector = self.pending().expect("cached interrupt remains pending");
            let result = self.interrupt(vector);
            self.synced = self.cycles;
            self.refresh();
            self.deferred = true;
            return result;
        }
        let e = *table.entries.get(self.pc as usize)
            .ok_or(Stop::BadAddress { pc: self.pc, addr: self.pc })?;
        // The common interior instruction pays one predictable branch for counting.
        if e.flags == 0 { return self.execute_decoded(e, table).map(|_| ()); }
        if let Some(tally) = tally {
            if e.flags & LEADER != 0 { tally.block(u32::from(e.block)); }
            let taken = self.execute_decoded(e, table)?;
            if e.flags & CONDITIONAL != 0 { tally.cond(u32::from(e.cond), taken); }
            if e.flags & LANDING != 0 {
                let target = table.entries.get(self.pc as usize);
                if target.is_none_or(|e| e.flags & LEADER == 0) {
                    tally.block(target.map_or(0, |e| u32::from(e.block)));
                }
            }
            Ok(())
        } else { self.execute_decoded(e, table).map(|_| ()) }
    }

    #[inline]
    fn execute_decoded(&mut self, e: Decoded, table: &DecodeTable) -> Result<bool, Stop> {
        let count = e.width(self.pc)?;
        let insn = e.insn.ok_or(Stop::BadInsn { pc: self.pc })?;
        if self.irq_delay != 0 { self.irq_delay -= 1; }
        self.execute::<true>(insn, count, Some(table), (e.base, e.extra))
    }

    fn execute<const DEFERRED: bool>(&mut self, insn: Insn, count: usize,
        table: Option<&DecodeTable>, timing: (u8, u8)) -> Result<bool, Stop> {
        use Insn::*;
        let mut next = self.pc + count as u32;
        let (base,extra) = if DEFERRED { timing } else { cycles(&insn) };
        let carry = self.sreg & C;
        let branch = match insn {
            Brbs(b,_) => self.sreg & (1 << b) != 0,
            Brbc(b,_) => self.sreg & (1 << b) == 0, _ => false,
        };
        // SBIC/SBIS sample the bus in their first cycle. Timer flags can change
        // on that clock; any skip penalty follows the sample.
        let io_skip = matches!(insn, Sbic(..) | Sbis(..));
        let sampled = if io_skip { base as u64 } else { 0 };
        if io_skip {
            if DEFERRED { self.deferred_advance(sampled); self.catch_up(); }
            else { self.advance(sampled); }
        }
        let skip = match insn {
            Cpse(d,r) => self.r[d as usize] == self.r[r as usize],
            Sbrc(r,b) => self.r[r as usize] & (1 << b) == 0,
            Sbrs(r,b) => self.r[r as usize] & (1 << b) != 0,
            Sbic(a,b) => self.read(a as u16 + 0x20) & (1 << b) == 0,
            Sbis(a,b) => self.read(a as u16 + 0x20) & (1 << b) != 0, _ => false,
        };
        if DEFERRED && io_skip { self.refresh(); self.deferred = true; }
        let skipped = if skip {
            if DEFERRED { table.unwrap().width(next)? } else { self.fetch_width(next)? }
        } else { 0 };
        let clocks = base as u64 + if branch || skip { extra as u64 } else { 0 }
            + u64::from(skipped == 2);

        // Validate accesses before advancing time. Pointer arithmetic wraps at
        // 16 bits, then the resulting data address must exist on this device.
        let mut address = 0u16;
        match insn {
            Ld(_,p) | St(p,_) => {
                address = self.pair(pointer_base(p));
                if matches!(p, Ptr::XDec | Ptr::YDec | Ptr::ZDec) { address = address.wrapping_sub(1); }
                self.check_data(address)?;
            }
            Ldd(_,y,q) | Std(y,q,_) => {
                address = self.pair(if y { 28 } else { 30 }).wrapping_add(q as u16);
                self.check_data(address)?;
            }
            Lds(_,a) | Sts(a,_) => { address = a; self.check_data(a)?; }
            Push(_) => self.check_data(self.sp)?,
            Pop(_) => self.check_data(self.sp.wrapping_add(1))?,
            Call(_) | Rcall(_) | Icall => self.check_push_pc()?,
            Ret | Reti => {
                self.check_data(self.sp.wrapping_add(1))?;
                self.check_data(self.sp.wrapping_add(2))?;
            }
            Lpm | LpmZ(_) | LpmZInc(_) => {
                address = self.pair(30);
                if address as usize / 2 >= FLASH_WORDS || address as usize / 2 >= self.flash.len() {
                    return Err(self.bad_address(address as u32));
                }
            }
            _ => {}
        }
        if DEFERRED { self.deferred_advance(clocks - sampled); }
        else { self.advance(clocks - sampled); }
        let sync = DEFERRED && match insn {
            In(..) | Out(..) | Sbi(..) | Cbi(..) => true,
            Ld(..) | St(..) | Ldd(..) | Std(..) | Lds(..) | Sts(..) => (0x20..=0xff).contains(&address),
            Push(_) => (0x20..=0xff).contains(&self.sp),
            Pop(_) => (0x20..=0xff).contains(&self.sp.wrapping_add(1)),
            Call(_) | Rcall(_) | Icall => (0x20..=0xff).contains(&self.sp)
                || (0x20..=0xff).contains(&self.sp.wrapping_sub(1)),
            Ret | Reti => (0x20..=0xff).contains(&self.sp.wrapping_add(1))
                || (0x20..=0xff).contains(&self.sp.wrapping_add(2)),
            _ => false,
        };
        // Accesses happen at completion, after all hardware edges on this clock.
        // Leave deferred false until the access completes, including an early error.
        if sync { self.catch_up(); }
        match insn {
            Add(d,r) | Adc(d,r) => {
                let v = self.add(self.r[d as usize],self.r[r as usize], if matches!(insn, Adc(..)) { carry } else { 0 });
                self.r[d as usize] = v;
            }
            Sub(d,r) | Sbc(d,r) | Cp(d,r) | Cpc(d,r) => {
                let sticky = matches!(insn, Sbc(..) | Cpc(..));
                let v = self.sub(self.r[d as usize],self.r[r as usize],if sticky { carry } else { 0 },sticky);
                if matches!(insn, Sub(..) | Sbc(..)) { self.r[d as usize] = v; }
            }
            Subi(d,k) | Sbci(d,k) | Cpi(d,k) => {
                let sticky = matches!(insn, Sbci(..));
                let v = self.sub(self.r[d as usize],k,if sticky { carry } else { 0 },sticky);
                if !matches!(insn, Cpi(..)) { self.r[d as usize] = v; }
            }
            And(d,r) | Or(d,r) | Eor(d,r) => {
                let a = self.r[d as usize]; let b = self.r[r as usize];
                let v = match insn { And(..) => a & b, Or(..) => a | b, _ => a ^ b };
                self.r[d as usize] = v; self.nzvs(v,false);
            }
            Andi(d,k) | Ori(d,k) => {
                let v = if matches!(insn, Andi(..)) { self.r[d as usize] & k } else { self.r[d as usize] | k };
                self.r[d as usize] = v; self.nzvs(v,false);
            }
            Mov(d,r) => self.r[d as usize] = self.r[r as usize],
            Movw(d,r) => self.set_pair(d as usize,self.pair(r as usize)),
            Ldi(d,k) => self.r[d as usize] = k,
            Adiw(d,k) | Sbiw(d,k) => {
                let a = self.pair(d as usize);
                let sub = matches!(insn, Sbiw(..));
                let v = if sub { a.wrapping_sub(k as u16) } else { a.wrapping_add(k as u16) };
                let old_n = a & 0x8000 != 0; let n = v & 0x8000 != 0;
                let overflow = if sub { old_n && !n } else { !old_n && n };
                self.flag(C,if sub { !old_n && n } else { old_n && !n });
                self.flag(Z,v == 0); self.flag(N,n); self.flag(V,overflow); self.flag(S,n ^ overflow);
                self.set_pair(d as usize,v);
            }
            Mul(d,r) | Muls(d,r) | Mulsu(d,r) | Fmul(d,r) | Fmuls(d,r) | Fmulsu(d,r) => {
                let a = self.r[d as usize]; let b = self.r[r as usize];
                let signed_a = matches!(insn, Muls(..) | Mulsu(..) | Fmuls(..) | Fmulsu(..));
                let signed_b = matches!(insn, Muls(..) | Fmuls(..));
                let a = if signed_a { a as i8 as i32 } else { a as i32 };
                let b = if signed_b { b as i8 as i32 } else { b as i32 };
                let product = (a*b) as u16;
                let fractional = matches!(insn, Fmul(..) | Fmuls(..) | Fmulsu(..));
                let result = if fractional { product.wrapping_shl(1) } else { product };
                self.flag(C,product & 0x8000 != 0); self.flag(Z,result == 0);
                self.set_pair(0,result);
            }
            Com(d) | Neg(d) | Swap(d) | Inc(d) | Dec(d) | Asr(d) | Lsr(d) | Ror(d) => {
                let a = self.r[d as usize];
                let v = match insn {
                    Com(_) => { let v = !a; self.nzvs(v,false); self.flag(C,true); v }
                    Neg(_) => {
                        let v = a.wrapping_neg(); self.nzvs(v,v == 0x80);
                        self.flag(C,v != 0); self.flag(H,(v | a) & 8 != 0); v
                    }
                    Swap(_) => a.rotate_left(4),
                    Inc(_) => { let v = a.wrapping_add(1); self.nzvs(v,v == 0x80); v }
                    Dec(_) => { let v = a.wrapping_sub(1); self.nzvs(v,v == 0x7f); v }
                    _ => {
                        let top = match insn { Asr(_) => a & 0x80, Ror(_) => carry << 7, _ => 0 };
                        let v = (a >> 1) | top;
                        self.flag(C,a & 1 != 0);
                        self.nzvs(v,(v & 0x80 != 0) ^ (a & 1 != 0)); v
                    }
                };
                self.r[d as usize] = v;
            }
            Bset(b) => { self.sreg |= 1 << b; if b == 7 { self.irq_delay = 1; } }
            Bclr(b) => self.sreg &= !(1 << b),
            Bst(r,b) => self.flag(T,self.r[r as usize] & (1 << b) != 0),
            Bld(d,b) => self.r[d as usize] = (self.r[d as usize] & !(1 << b)) | (((self.sreg >> 6) & 1) << b),
            In(d,a) => self.r[d as usize] = self.read(a as u16 + 0x20),
            Out(a,r) => self.write_data(a as u16 + 0x20,self.r[r as usize]),
            Sbi(a,b) | Cbi(a,b) => self.bit_write(a as u16 + 0x20,b,matches!(insn, Sbi(..))),
            Cpse(..) | Sbrc(..) | Sbrs(..) | Sbic(..) | Sbis(..) => next += skipped as u32,
            Brbs(_,k) | Brbc(_,k) => if branch { next = (next as u16).wrapping_add(k as i16 as u16) as u32; },
            Rjmp(k) | Rcall(k) => {
                if matches!(insn, Rcall(_)) { self.push_pc(next)?; }
                next = (next as u16).wrapping_add(k as u16) as u32;
            }
            Jmp(k) | Call(k) => {
                if matches!(insn, Call(_)) { self.push_pc(next)?; }
                next = k;
            }
            Ijmp | Icall => {
                let dest = self.pair(30) as u32;
                if matches!(insn, Icall) { self.push_pc(next)?; }
                next = dest;
            }
            Ret | Reti => {
                let hi = self.pop()?; let lo = self.pop()?;
                next = u16::from_le_bytes([lo,hi]) as u32;
                if matches!(insn, Reti) { self.sreg |= I; self.irq_delay = 1; }
            }
            Ld(d,p) | St(p,d) => {
                let base = pointer_base(p);
                let store = self.r[d as usize];
                let old_pointer = self.pair(base);
                if matches!(insn, Ld(..)) { self.r[d as usize] = self.read(address); }
                else { self.write_data(address,store); }
                match p {
                    Ptr::XInc | Ptr::YInc | Ptr::ZInc => self.set_pair(base,old_pointer.wrapping_add(1)),
                    Ptr::XDec | Ptr::YDec | Ptr::ZDec => self.set_pair(base,address), _ => {}
                }
            }
            Ldd(d,_,_) | Lds(d,_) => self.r[d as usize] = self.read(address),
            Std(_,_,r) | Sts(_,r) => self.write_data(address,self.r[r as usize]),
            Lpm | LpmZ(_) | LpmZInc(_) => {
                let dest = match insn { Lpm => 0, LpmZ(d) | LpmZInc(d) => d as usize, _ => unreachable!() };
                self.r[dest] = (self.flash[address as usize / 2] >> ((address & 1)*8)) as u8;
                if matches!(insn, LpmZInc(_)) { self.set_pair(30,address.wrapping_add(1)); }
            }
            Push(r) => self.push(self.r[r as usize])?,
            Pop(d) => self.r[d as usize] = self.pop()?,
            Sleep => {
                if self.io[0x53] & 0x0e == 2 && self.io[0x7a] & 0x80 != 0 {
                    self.unmodeled("ADC Noise Reduction sleep with ADEN");
                }
                self.sleeping = true;
            }
            Nop | Wdr | Break => {}
        }
        if sync {
            // An EEPROM bus access may have advanced peripherals through CPU halt clocks.
            self.synced = self.cycles;
            self.refresh(); self.deferred = true;
        }
        if let Some(e) = self.fault.get() { return Err(e); }
        self.pc = next;
        if matches!(insn, Break) { return Err(Stop::Break); }
        if DEFERRED && self.sleeping && !self.cosim_timing {
            self.catch_up();
            self.deferred = true;
        }
        if self.sleeping && !self.cosim_timing && self.wake_clocks().is_none() { return Err(Stop::Sleep); }
        Ok(branch || skip)
    }
}
