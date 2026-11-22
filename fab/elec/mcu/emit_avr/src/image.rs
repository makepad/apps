use fab_elec_mcu_avr::asm::{Asm, Label, Relocation, Target};
use fab_elec_mcu_avr::chip;
use fab_elec_mcu_avr::insn::{decode, Insn};
use fab_elec_mcu_emu::FwFaultCell;
use fab_elec_mcu_rt::{ports, pwm, serial::SerialSetting, watch};

use crate::{helpers::{self, Helper, Helpers}, runtime};

pub const DISARMED: u64 = (1 << 48) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Output,
    Input { pull: bool },
    Analog,
    Pwm(pwm::PwmSetting),
}

#[derive(Clone, Copy, Debug)]
pub struct PinDef {
    pub pin: u8,
    pub role: Role,
}

#[derive(Clone, Copy, Debug)]
pub struct TimerDef {
    pub entry: usize,
    pub period_ticks: u64,
    pub repeat: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct SerialDef {
    pub entry: usize,
    pub setting: SerialSetting,
    /// Bytes not yet dispatched. Compiled modules replace SerialOut.fifo with
    /// their emitted cycle bound; hand-built programs keep this explicit depth.
    pub fifo: u32,
}

/// Signed pin/timer in r16..r19, value in r20..r23, site in r24:r25. Results use
/// r16..r19. Stubs preserve r2, r6..r15 and Y; the clock owns r13..r15.
/// r3 is the overflow ISR's SREG save; r4/r5 are exclusive to the UDRE pump.
pub struct Stubs {
    pub helpers: Helpers,
    /// Constant (data, clock) pair; bits in r16..r19, value in r20..r23, site in r24:r25.
    pub shifts: Vec<((u8, u8), Label)>,
    /// Unsigned address in r16..r19, site in r24:r25; zero-extended byte (or
    /// zero on an address fault) returned in r16..r19. Clobbers r0/r1,
    /// r16..r27, Z and arithmetic flags; preserves SREG.I and the common ABI.
    pub eeprom_read: Label,
    /// Same address/site ABI; low byte of r20..r23 is stored. Returns once
    /// programming starts; clobbers the same registers as eeprom_read.
    pub eeprom_write: Label,
    pub write: Label,
    /// Signed level in r20..r23, saturated to 0..=255; queues a Duty or Fault.
    pub duty: Label,
    pub read: Label,
    pub adc: Label,
    /// ADMUX in r20; zero-extended ten-bit result in r16..r19.
    /// Clobbers r0, r16..r20, r23..r25, Z and arithmetic flags.
    pub adc_convert: Label,
    /// Inline pin/level in r16/r20, site in r24:r25; read returns a zero-extended level.
    pub record_write: Label,
    pub record_read: Label,
    pub clock_us: Label,
    pub clock_ms: Label,
    pub timer_stop: Label,
    pub timer_start: Label,
    pub timer_period: Label,
    /// Link in r16..r19, site in r24:r25; byte or -1 in r16..r19.
    pub serial_byte: Label,
    /// No arguments, site in r24:r25; reports LoopCap with argument zero.
    pub loop_cap: Label,
    pub state: u16,
    pub scratch: u16,
    pub shared: Label,
}

/// Emit a body, using labels for transfers; the builder appends RET and relaxes.
/// Entries leave interrupts enabled and use the stubs for masked stamps.
pub type Entry<'a> = &'a dyn Fn(&mut Asm, &Stubs);

pub struct Program<'a> {
    /// Direct helper calls; signed division/remainder also link unsigned division.
    pub helpers: Vec<Helper>,
    /// Distinct declared output pairs used by shift_out, in first-use order.
    pub shift_pairs: Vec<(u8, u8)>,
    /// CPU clock in whole Hz, at least one; Timer0 always uses clk/8.
    pub f_clk: u32,
    pub image_id: u32,
    pub state_init: Vec<u8>,
    pub shared: Vec<u8>,
    pub scratch_bytes: u16,
    /// Callable ports, one bit per runtime port number; ADC_READ is opt-in.
    pub ports: u32,
    /// Inline analog reads need the converter without the computed-pin ADC_READ stub.
    pub adc_convert: bool,
    /// Inline ports needing a recording routine, one bit per port; defaults to all.
    pub records: u32,
    pub pins: Vec<PinDef>,
    pub timers: Vec<TimerDef>,
    /// USART0 receive handler; its baud also drives the watch stream.
    pub serial: Option<SerialDef>,
    /// Hand-built programs default to Ports. Off leaves the UART to the program's
    /// receiver, if any, and counts faults in RAM. See Bounds for uninterrupted totals.
    pub watch: watch::Level,
    pub entries: Vec<Entry<'a>>,
    /// Optional boot entry, after the clock release and before the first pass.
    pub setup: Option<Entry<'a>>,
    /// Explicit watch overrides; absent values use serial, or 16/true.
    pub ubrr: Option<u16>,
    pub u2x: Option<bool>,
    pub flash_limit_bytes: u32,
}

impl Default for Program<'_> {
    fn default() -> Self {
        // Keep existing hand-built digital images unchanged; analog and EEPROM callers opt in.
        Self {
            helpers: Vec::new(),
            shift_pairs: Vec::new(),
            f_clk: 16_000_000, image_id: 0, state_init: Vec::new(), shared: Vec::new(), scratch_bytes: 0,
            ports: u32::MAX & !((1 << ports::ADC_READ) | (1 << ports::EEPROM_READ) | (1 << ports::EEPROM_WRITE)), adc_convert: false, records: u32::MAX, pins: Vec::new(), timers: Vec::new(), entries: Vec::new(), setup: None,
            serial: None, watch: watch::Level::Ports,
            ubrr: None, u2x: None, flash_limit_bytes: 32768,
        }
    }
}

impl Program<'_> {
    pub(crate) fn shift_spi(&self, pair: (u8, u8)) -> bool {
        let drives = |pin| self.pins.iter().any(|p| p.pin == pin && matches!(p.role, Role::Output | Role::Pwm(_)));
        pair == (11, 13) && drives(10) && !drives(12)
    }
    pub(crate) fn uses_port(&self, port: u16) -> bool { self.ports & (1 << port) != 0 }
    pub(crate) fn uses_adc(&self) -> bool { self.adc_convert || self.uses_port(ports::ADC_READ) }
    pub(crate) fn records_port(&self, port: u16) -> bool {
        self.watch == watch::Level::Ports && self.records & (1 << port) != 0
    }

    /// Without serial or explicit overrides, UBRR stays 16 with U2X at
    /// every CPU clock: the watch stream's baud is f_clk / 136.
    pub(crate) fn baud(&self) -> (u16, bool) {
        self.serial.map_or((self.ubrr.unwrap_or(16), self.u2x.unwrap_or(true)),
            |s| (s.setting.ubrr, s.setting.u2x))
    }

    pub(crate) fn uart(&self, drain: bool) -> u8 {
        (if self.watch != watch::Level::Off { 8 } else { 0 }) | (if self.serial.is_some() { 0x90 } else { 0 })
            | (if drain && self.watch != watch::Level::Off { 0x20 } else { 0 })
    }

    /// Reachable fault codes and the number of emitted paths that raise each one.
    fn fault_paths(&self) -> [(u8, usize); 9] {
        let count = |ports: &[u16]| ports.iter().filter(|&&port| self.uses_port(port)).count();
        [
            (watch::FAULT_PIN, count(&[ports::PIN_WRITE, ports::PIN_READ, ports::ADC_READ, ports::PWM_DUTY])),
            (watch::FAULT_TIMER, count(&[ports::TIMER_STOP, ports::TIMER_START, ports::TIMER_PERIOD])),
            (watch::FAULT_OVERRUN, usize::from(self.timers.iter().any(|t| t.repeat))),
            (watch::FAULT_LOOP_CAP, usize::from(self.uses_port(ports::LOOP_CAP))),
            (watch::FAULT_SERIAL, usize::from(self.uses_port(ports::SERIAL_BYTE))),
            (watch::FAULT_SERIAL_LOST, usize::from(self.serial.is_some())),
            (watch::FAULT_SERIAL_LOST_AT_LEAST, usize::from(self.serial.is_some())),
            (watch::FAULT_SHIFT_BITS, self.shift_pairs.len()),
            (watch::FAULT_EEPROM_ADDR, count(&[ports::EEPROM_READ, ports::EEPROM_WRITE])),
        ]
    }
}

/// Watch-only addresses are zero when absent at Off; never use them as data addresses then.
#[derive(Clone, Debug)]
pub struct Layout {
    pub state: u16,
    pub state_bytes: u16,
    pub scratch: u16,
    pub scratch_bytes: u16,
    pub timers: u16,
    pub timer_count: u8,
    pub head: u16,
    pub tail: u16,
    pub lost: u16,
    pub lost_first: u16,
    pub lost_last: u16,
    pub seq: u16,
    pub tx: u16,
    pub tx_len: u16,
    pub tx_pos: u16,
    pub hello_pending: u16,
    pub time_sent: u16,
    pub sink: u16,
    pub clock_hi: u16,
    pub clock_b31: u16,
    pub mcusr: u16,
    pub wdtcsr: u16,
    pub stamp: u16,
    pub due: u16,
    /// Next far wake requiring a pass, modulo the 24-bit served overflow count.
    pub far_until: u16,
    pub selected: u16,
    pub phase: u16,
    pub args: u16,
    pub record: u16,
    pub record_count: u16,
    /// Invalid timer-pointer fallback, retained by the exact bookkeeping prefix.
    pub timer_sink: u16,
    pub body: u16,
    pub rx_head: u16,
    pub rx_tail: u16,
    pub rx_count: u16,
    pub rx_lost: u16,
    pub rx_overrun: u16,
    pub rx_report: u16,
    pub rx_report_overrun: u16,
    pub rx_byte: u16,
    pub rx_active: u16,
    /// Six-byte selected event stamp, published inside the FIFO scan mask.
    pub rx_cut: u16,
    /// Sticky 0/255 flag for an accepted byte strictly before the cut.
    pub rx_early: u16,
    pub rx_fifo: u16,
    pub rx_depth: u16,
    pub ring: u16,
    pub records: u8,
    pub fault_cells: Vec<FwFaultCell>,
    /// Share the increment routine only when at least two paths use it.
    pub(crate) shared_fault: bool,
    /// End of allocated RAM, excluding the reserved stack.
    pub ram_end: u16,
    pub stack_top: u16,
    pub stack_bytes: u16,
    pub w: u32,
}

impl Layout {
    fn new(p: &Program<'_>, tb: &runtime::Timebase, adc: Option<&runtime::Adc>) -> Result<(Self, runtime::Bounds), String> {
        let watching = p.watch != watch::Level::Off;
        let mut next = 0x100u32;
        let mut take = |n: usize| -> Result<u16, String> {
            let address = next;
            next = next.checked_add(u32::try_from(n).map_err(|_| "RAM size overflow")?)
                .ok_or("RAM size overflow")?;
            if next > 0x900 { return Err("state, scratch and runtime exceed RAM".into()); }
            Ok(address as u16)
        };
        let state = take(p.state_init.len())?;
        let scratch = take(p.scratch_bytes as usize)?;
        let timers = take(p.timers.len() * 12)?;
        let head = if watching { take(1)? } else { 0 };
        let tail = if watching { take(1)? } else { 0 };
        let lost = if watching { take(4)? } else { 0 };
        let lost_first = if watching { take(4)? } else { 0 };
        let lost_last = if watching { take(4)? } else { 0 };
        let seq = if watching { take(1)? } else { 0 };
        let tx = if watching { take(watch::MAX_WIRE)? } else { 0 };
        let tx_len = if watching { take(1)? } else { 0 };
        let tx_pos = if watching { take(1)? } else { 0 };
        let hello_pending = if watching { take(1)? } else { 0 };
        let time_sent = if watching { take(3)? } else { 0 };
        let sink = if watching { take(8)? } else { 0 };
        let clock_hi = take(2)?;
        let clock_b31 = take(1)?;
        let mcusr = if watching { take(1)? } else { 0 };
        let wdtcsr = if watching { take(1)? } else { 0 };
        let stamp = take(6)?;
        let due = take(6)?;
        let far_until = take(3)?;
        let selected = take(1)?;
        let phase = take(1)?;
        let args = take(if watching { 11 } else { 8 })?;
        let record = if watching { take(16)? } else { 0 };
        let record_count = if watching { take(1)? } else { 0 };
        let timer_sink = take(12)?;
        let body = if watching { take(watch::MAX_BODY)? } else { 0 };
        let mut fault_cells = Vec::new();
        let mut fault_paths = 0;
        if !watching {
            for (code, paths) in p.fault_paths() {
                if paths == 0 { continue; }
                fault_cells.push(FwFaultCell { code, addr: u32::from(take(4)?) });
                fault_paths += paths;
            }
        }
        let serial = usize::from(p.serial.is_some());
        let rx_head = take(2 * serial)?;
        let rx_tail = take(2 * serial)?;
        let rx_count = take(2 * serial)?;
        let rx_lost = take(4 * serial)?;
        let rx_overrun = take(serial)?;
        let rx_report = take(4 * serial)?;
        let rx_report_overrun = take(serial)?;
        let rx_byte = take(serial)?;
        let rx_active = take(serial)?;
        let rx_cut = take(6 * serial)?;
        let rx_early = take(serial)?;
        let rx_fifo = take(0)?;
        // Entry -> stub -> stamp, four saved A registers, saved status, IRQ
        // return, sixteen RX registers and SREG; plus a two-byte margin.
        // The deepest interruptible point is push's local append call: entry, stub, push and
        // append returns (8) and the saved A (4), under the RX interrupt's 19: 31 of these 32.
        // Off without RX needs at most the entry/stub/now48 returns, saved A/status and TOV
        // return: thirteen bytes. Keep the watch allocation unchanged too: ring
        // capacity is transmitted in Hello, even though the pump now saves only Z.
        // Bookkeeping and dispatch call the sled at pass depth, after saved Z is popped:
        // two return bytes, at most 21 with RX, 6 with UDRE or 4 with TOV. Dispatch is
        // masked, so its two bytes cannot overlap an ISR or the following entry CALL.
        let stack_bytes = if watching || p.serial.is_some() { 3 * 2 + 4 + 1 + 2 + 16 + 1 + 2 } else { 16 };
        let stack_bytes = stack_bytes + if p.helpers.is_empty() { 0 } else { helpers::STACK_BYTES };
        let fifo_available = 0x900u32.saturating_sub(u32::from(rx_fifo) + stack_bytes + if watching { 4 * 8 } else { 0 });
        let depth = p.serial.map_or(0, |s| s.fifo);
        let fifo_bytes = u64::from(depth) * 7;
        if fifo_bytes > u64::from(fifo_available) {
            return Err(format!("RX FIFO needs {fifo_bytes} bytes ({depth} entries); RAM has {fifo_available} bytes available"));
        }
        take(fifo_bytes as usize)?;
        let allocated = take(0)?;
        if u32::from(allocated) + stack_bytes > 0x900 { return Err("runtime and stack exceed RAM".into()); }
        let ring = if watching { allocated } else { 0 };
        let available = 0x900u32.saturating_sub(u32::from(allocated) + stack_bytes);
        let records = if watching { (available / 8).min(255) as u8 } else { 0 };
        if watching && records < 4 { return Err(format!("ring has {records} records; at least 4 are required")); }
        let ram_end = allocated + u16::from(records) * 8;
        let mut layout = Self {
            state, state_bytes: p.state_init.len() as u16, scratch,
            scratch_bytes: p.scratch_bytes, timers, timer_count: p.timers.len() as u8,
            head, tail, lost, lost_first, lost_last, seq, tx, tx_len, tx_pos,
            hello_pending, time_sent, sink, clock_hi, clock_b31, mcusr, wdtcsr,
            stamp, due, far_until, selected, phase, args, record, record_count, timer_sink,
            body, rx_head, rx_tail, rx_count, rx_lost, rx_overrun, rx_report, rx_report_overrun, rx_byte, rx_active,
            rx_cut, rx_early, rx_fifo, rx_depth: depth as u16,
            ring, records, fault_cells, shared_fault: fault_paths > 1, ram_end,
            stack_top: 0x8ff, stack_bytes: stack_bytes as u16, w: 0,
        };
        // W only changes immediates in the pass and far_until. Count the runtime
        // with a provisional W before emitting the final image; no entries run.
        let (mut a, labels, stubs) = runtime_asm(p, &layout);
        let bounds = runtime::emit(&mut a, p, tb, adc, &layout, &labels, &stubs, &mut Symbols::default());
        for (routine, bound) in [("UDRE", bounds.udre), ("RX interrupt", bounds.rx_int),
            ("RX scan", bounds.rx_scan_mask), ("RX take", bounds.rx_take)] {
            if bound >= 2040 {
                return Err(format!("{routine}: masked bound {bound} cycles reaches the 2040-cycle limit"));
            }
        }
        let (routine, w) = if p.serial.is_some() { ("RX pass", bounds.rx_window()) }
            else { ("pass", bounds.window()) };
        if w >= 1 << 12 {
            return Err(format!("{routine}: window W={w} must fit in 12 bits (pass prefix {} cycles)", bounds.pass_prefix));
        }
        if w >= 1 << 15 || (8 * w + 64 - runtime::DELAY_BASE) / 4 > u16::MAX as u32 {
            return Err(format!("delay: {routine} window W={w} exceeds the delay counter range"));
        }
        layout.w = w;
        Ok((layout, bounds))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Symbols {
    pub mul32: u32,
    pub udivmod32: u32,
    pub sdiv32: u32,
    pub srem32: u32,
    pub shl32: u32,
    pub shr32: u32,
    pub sar32: u32,
    /// Word ranges of the linked EEPROM stubs, including RET.
    pub eeprom_read: u32,
    pub eeprom_read_end: u32,
    pub eeprom_write: u32,
    pub eeprom_write_end: u32,
    pub shift_spi: u32,
    pub shift_spi_end: u32,
    /// Pair, start, exclusive end; one bounded routine per bit-banged pair.
    pub shift_bitbang: Vec<((u8, u8), u32, u32)>,
    pub t0: u32,
    pub pass: u32,
    pub sleep: u32,
    pub entries: Vec<u32>,
    pub setup: u32,
    /// Word address of the reset routine's CALL to setup (an empty RET if absent).
    pub setup_call: u32,
    pub write: u32,
    pub duty: u32,
    pub read: u32,
    pub adc: u32,
    pub adc_convert: u32,
    pub record_write: u32,
    pub record_read: u32,
    pub clock_us: u32,
    pub clock_ms: u32,
    pub timer_stop: u32,
    pub timer_start: u32,
    pub timer_period: u32,
    pub serial_byte: u32,
    pub loop_cap: u32,
    pub push: u32,
    pub fault: u32,
    pub rx: u32,
    pub rx_end: u32,
    pub rx_sample: u32,
    pub rx_scan: u32,
    pub rx_take: u32,
    pub rx_check: u32,
    pub rx_defer: u32,
    pub bookkeep: u32,
    /// Bounded timer-overrun arm, through its IJMP (exclusive end).
    pub overrun: u32,
    pub overrun_end: u32,
    pub now48: u32,
    pub now48_end: u32,
    pub delay: u32,
    pub delay_end: u32,
    pub lock: u32,
    pub lock_end: u32,
    pub samples: [u32; 9],
    pub tov: u32,
    pub udre: u32,
    pub idle_check: u32,
    pub idle_mask: u32,
    /// Encode SEI and out-of-line body (one inline instance, no CALL/RET).
    pub encode: u32,
    pub encode_body: u32,
    pub encode_end: u32,
    pub tx_publish: u32,
    pub dispatch_call: Vec<u32>,
    pub dispatch: Vec<u32>,
    pub write_effect: u32,
    pub duty_effect: u32,
    /// The shared pad sled (Code::pad's long padding).
    pub pad: u32,
    /// Exclusive word end of the sled, including its RET.
    pub pad_end: u32,
}

pub struct Image {
    pub words: Vec<u16>,
    pub layout: Layout,
    pub symbols: Symbols,
    pub bounds: runtime::Bounds,
}

impl Image {
    pub fn bytes(&self) -> Vec<u8> { Asm::bytes(&self.words) }
}

pub(crate) struct Labels {
    pub reset: Label,
    pub pass: Label,
    pub tov: Label,
    pub udre: Label,
    pub rx: Label,
    pub now48: Label,
    pub delay: Label,
    pub push: Label,
    pub fault: Label,
    pub roles: Label,
    pub analog_pins: Label,
    pub pwm_pins: Label,
    pub pwm_levels: Label,
    pub timer_table: Label,
    pub state_init: Label,
    pub setup: Label,
    pub entries: Vec<Label>,
    pub dispatch: Vec<Label>,
    /// The pad sled's entries: `sled[k]` runs k RJMP .+0 words before its RET.
    pub sled: Vec<Label>,
}

fn runtime_asm(p: &Program<'_>, layout: &Layout) -> (Asm, Labels, Stubs) {
    let mut a = Asm::new();
    let labels = Labels {
        reset: a.label(), pass: a.label(), tov: a.label(), udre: a.label(), rx: a.label(),
        now48: a.label(), delay: a.label(), push: a.label(), fault: a.label(), roles: a.label(), analog_pins: a.label(),
        pwm_pins: a.label(), pwm_levels: a.label(),
        timer_table: a.label(), state_init: a.label(), setup: a.label(),
        entries: p.entries.iter().map(|_| a.label()).collect(),
        dispatch: p.entries.iter().map(|_| a.label()).collect(),
        sled: (0..=runtime::SLED).map(|_| a.label()).collect(),
    };
    let stubs = Stubs {
        helpers: Helpers::new(&mut a),
        shifts: p.shift_pairs.iter().map(|&pair| (pair, a.label())).collect(),
        write: a.label(), duty: a.label(), read: a.label(), record_write: a.label(), record_read: a.label(),
        adc: a.label(), adc_convert: a.label(),
        eeprom_read: a.label(), eeprom_write: a.label(),
        clock_us: a.label(), clock_ms: a.label(),
        timer_stop: a.label(), timer_start: a.label(), timer_period: a.label(), serial_byte: a.label(),
        loop_cap: a.label(), state: layout.state, scratch: layout.scratch, shared: a.label(),
    };
    (a, labels, stubs)
}

pub fn build(p: &Program<'_>) -> Result<Image, String> {
    if p.f_clk == 0 { return Err("CPU clock must be at least one Hz".into()); }
    build_with_timebase(p, &runtime::Timebase::new(p.f_clk)).map(|(image, _)| image)
}

pub(crate) fn build_with_timebase(p: &Program<'_>, tb: &runtime::Timebase) -> Result<(Image, Relocation), String> {
    let mut declared = [false; 22];
    let mut pwm_timers = [None; 3];
    for pin in &p.pins {
        if let Role::Pwm(setting) = pin.role {
            let output = pwm::ATMEGA328P.pins.iter().find(|out| out.pin == u32::from(pin.pin))
                .ok_or_else(|| format!("pin {} has no OC output or PWM timer (PWM pins: 3, 5, 6, 9, 10, 11)", pin.pin))?;
            let timer = &pwm::ATMEGA328P.timers[output.timer as usize];
            let name = format!("pin {} ({}) on {}", pin.pin, output.output, timer.name);
            if setting.timer != output.timer {
                return Err(format!("{name}: setting names Timer{} instead", setting.timer));
            }
            if timer.clock && (setting.mode != pwm::PwmMode::Fast || setting.prescale != 8 || setting.top != 255) {
                return Err(format!("{name}: device clock requires Fast, clk/8, TOP 255"));
            }
            if setting.mode != timer.mode {
                return Err(format!("{name}: mode must be {:?}", timer.mode));
            }
            if !timer.prescalers.contains(&setting.prescale) {
                return Err(format!("{name}: prescaler {} is not in {:?}", setting.prescale, timer.prescalers));
            }
            if setting.top != timer.top && !(3..=timer.free_top).contains(&setting.top) {
                return Err(format!("{name}: TOP {} is outside {}", setting.top,
                    if timer.free_top == 0 { "255 only" } else { "3..=65535 (255 uses WGM 1)" }));
            }
            if let Some((other_pin, other_output, other_setting)) = pwm_timers[output.timer as usize] {
                if setting != other_setting {
                    return Err(format!("{name}: setting differs from pin {other_pin} ({other_output}) on the same {}", timer.name));
                }
            } else {
                pwm_timers[output.timer as usize] = Some((pin.pin, output.output, setting));
            }
        }
        let first_pin = if p.watch != watch::Level::Off { 2 } else if p.serial.is_some() { 1 } else { 0 };
        if !(first_pin..22).contains(&pin.pin) { return Err(format!("pin {} is unavailable", pin.pin)); }
        if pin.role == Role::Analog {
            if chip::adc_channel(u32::from(pin.pin)).is_none() {
                return Err(format!("pin {} has no analog input (analog pins: 14..=21)", pin.pin));
            }
        } else if pin.pin >= 20 {
            return Err(format!("pin {} is an analog-only pin: declare it with `analog(..)`", pin.pin));
        }
        if std::mem::replace(&mut declared[pin.pin as usize], true) {
            return Err(format!("pin {} has two roles", pin.pin));
        }
    }
    let adc = if p.uses_adc() || p.pins.iter().any(|pin| pin.role == Role::Analog) {
        Some(runtime::Adc::new(p.f_clk)?)
    } else { None };
    for &(data, clock) in &p.shift_pairs {
        for pin in [data, clock] {
            if !p.pins.iter().any(|p| p.pin == pin && p.role == Role::Output) {
                return Err(format!("shift_out pin {pin} must be a declared output"));
            }
        }
    }
    for (k, timer) in p.timers.iter().enumerate() {
        if timer.period_ticks == 0 || timer.period_ticks >= 1 << 48 {
            return Err(format!("timer {k} period must be in 1..2^48"));
        }
        if timer.entry >= p.entries.len() { return Err(format!("timer {k} has no entry {}", timer.entry)); }
    }
    if let Some(s) = p.serial {
        if s.entry >= p.entries.len() { return Err(format!("serial has no entry {}", s.entry)); }
        if s.setting.link != 0 { return Err(format!("USART0 cannot receive link {}", s.setting.link)); }
        if s.fifo == 0 { return Err("RX FIFO must contain at least one entry".into()); }
        if p.ubrr.is_some_and(|b| b != s.setting.ubrr) || p.u2x.is_some_and(|b| b != s.setting.u2x) {
            return Err(format!("watch UBRR/U2X {:?}/{:?} conflicts with serial {}/{}",
                p.ubrr, p.u2x, s.setting.ubrr, s.setting.u2x));
        }
    }
    if p.timers.len() > 255 || p.baud().0 > 4095 { return Err("timer count or baud divisor does not fit".into()); }
    let (layout, counted) = Layout::new(p, tb, adc.as_ref())?;
    let (mut a, labels, stubs) = runtime_asm(p, &layout);
    let mut symbols = Symbols::default();
    let mut bounds = runtime::emit(&mut a, p, tb, adc.as_ref(), &layout, &labels, &stubs, &mut symbols);
    assert_eq!(bounds, counted, "runtime bounds must not depend on W or code addresses");
    for helper in Helper::ALL {
        let dependency = helper == Helper::Udivmod32 && p.helpers.iter().any(|h| matches!(h, Helper::Sdiv32 | Helper::Srem32));
        if !p.helpers.contains(&helper) && !dependency { continue; }
        let (symbol, bound) = match helper {
            Helper::Mul32 => (&mut symbols.mul32, &mut bounds.mul32),
            Helper::Udivmod32 => (&mut symbols.udivmod32, &mut bounds.udivmod32),
            Helper::Sdiv32 => (&mut symbols.sdiv32, &mut bounds.sdiv32),
            Helper::Srem32 => (&mut symbols.srem32, &mut bounds.srem32),
            Helper::Shl32 => (&mut symbols.shl32, &mut bounds.shl32),
            Helper::Shr32 => (&mut symbols.shr32, &mut bounds.shr32),
            Helper::Sar32 => (&mut symbols.sar32, &mut bounds.sar32),
        };
        *symbol = a.here();
        *bound = helper.cycles() - 4; // Stub table excludes the caller's CALL.
        helper.emit(&mut a, &stubs.helpers);
    }
    a.bind(labels.setup);
    assert_eq!(a.relaxable(), 0, "runtime transfers must keep their fixed forms");
    symbols.setup = a.here();
    if let Some(setup) = p.setup { setup(&mut a, &stubs); }
    a.emit(Insn::Ret);
    for (label, entry) in labels.entries.iter().zip(&p.entries) {
        a.bind(*label);
        symbols.entries.push(a.here());
        entry(&mut a, &stubs);
        a.emit(Insn::Ret);
    }
    a.bind(labels.state_init);
    bytes(&mut a, &p.state_init);
    a.bind(stubs.shared);
    bytes(&mut a, &p.shared);
    if p.uses_port(ports::PIN_WRITE) || p.uses_port(ports::PIN_READ) {
        a.bind(labels.roles);
        let mut roles = vec![0; 21 * 4];
        for def in &p.pins {
            if def.role == Role::Analog { continue; }
            let gpio = chip::by_number(u32::from(def.pin)).expect("build checked the pin");
            // the PORT register: PIN, DDR, PORT
            let (port, bit) = (chip::pin_register(gpio.port) as u8 + 2, gpio.bit);
            let at = def.pin as usize * 4;
            let role = match def.role { Role::Input { .. } => 1, Role::Output => 2, Role::Pwm(_) => 3,
                Role::Analog => unreachable!("analog pins have no digital row") };
            roles[at..at + 4].copy_from_slice(&[1 << bit, port, role, port - 2]);
        }
        bytes(&mut a, &roles);
    }
    if p.uses_port(ports::ADC_READ) {
        a.bind(labels.analog_pins);
        // One ADMUX byte per pin, followed by the zero range sentinel.
        let mut analog_pins = [0; 23];
        for def in &p.pins {
            if def.role == Role::Analog {
                analog_pins[def.pin as usize] = 0x40 | chip::adc_channel(u32::from(def.pin)).unwrap();
            }
        }
        bytes(&mut a, &analog_pins);
    }
    if p.uses_port(ports::PWM_DUTY) {
        a.bind(labels.pwm_pins);
        // OCR low, OCR high (zero for eight-bit timers), COM clear/connect masks.
        // Undeclared pins and the range sentinel have all-zero rows.
        let mut pwm_pins = vec![0; 21 * 4];
        for def in &p.pins {
            if matches!(def.role, Role::Pwm(_)) {
                let (ocr, high, clear, connect) = pwm_registers(def.pin);
                let at = def.pin as usize * 4;
                pwm_pins[at..at + 4].copy_from_slice(&[ocr, high, clear, connect]);
            }
        }
        bytes(&mut a, &pwm_pins);
        a.bind(labels.pwm_levels);
        if let Some(setting) = scaled_pwm(p) {
            // One shared Timer1 table: exact rounded scaling, two flash reads at runtime.
            for level in 0..=255 { a.word(setting.ocr(level)); }
        }
    }
    a.bind(labels.timer_table);
    // Eight bytes per timer: repeat, padding, entry word address, padding.
    // LDI pairs are decoded as relocations after assembly, outside executable code.
    let table_start = a.here();
    for timer in &p.timers {
        a.word(u16::from(timer.repeat));
        a.ldi_label(30, 31, labels.dispatch[timer.entry]);
        a.word(0);
    }
    let (mut words, relocation) = a.finish_relocated()?;
    assert_eq!(relocation.at(symbols.setup), symbols.setup, "runtime end must not move");
    symbols.setup = relocation.at(symbols.setup);
    for entry in &mut symbols.entries { *entry = relocation.at(*entry); }
    let table_start = relocation.at(table_start);
    for (i, _) in p.timers.iter().enumerate() {
        let at = table_start as usize + i * 4 + 1;
        let lo = match decode(&words[at..]) { Some((Insn::Ldi(_, x), _)) => x, _ => unreachable!() };
        let hi = match decode(&words[at + 1..]) { Some((Insn::Ldi(_, x), _)) => x, _ => unreachable!() };
        words[at] = u16::from_le_bytes([lo, hi]);
        words[at + 1] = 0;
    }
    let size = words.len() as u32 * 2;
    if size > p.flash_limit_bytes || size > 32768 {
        return Err(format!("image is {size} bytes; flash limit is {} bytes", p.flash_limit_bytes.min(32768)));
    }
    Ok((Image { words, layout, symbols, bounds }, relocation))
}

fn bytes(a: &mut Asm, data: &[u8]) {
    for pair in data.chunks(2) { a.word(u16::from_le_bytes([pair[0], *pair.get(1).unwrap_or(&0)])); }
}

pub(crate) fn scaled_pwm(p: &Program<'_>) -> Option<pwm::PwmSetting> {
    p.pins.iter().find_map(|pin| match pin.role {
        Role::Pwm(setting) if setting.timer == 1 && setting.top != 255 => Some(setting),
        _ => None,
    })
}

pub(crate) fn pwm_registers(pin: u8) -> (u8, u8, u8, u8) {
    match pin {
        3 => (0xb4, 0, 0, 0), 5 => (0x48, 0, 0x30, 0x20),
        6 => (0x47, 0, 0xc0, 0x80), 9 => (0x88, 0x89, 0, 0),
        10 => (0x8a, 0x8b, 0, 0), 11 => (0xb3, 0, 0, 0),
        _ => unreachable!("PWM pins are validated before emission"),
    }
}

pub fn blink_entry(a: &mut Asm, s: &Stubs) {
    a.emit(Insn::Lds(20, s.state));
    a.emit(Insn::Ldi(21, 1));
    a.emit(Insn::Eor(20, 21));
    a.emit(Insn::Sts(s.state, 20));
    a.emit(Insn::Ldi(16, 13));
    for r in 17..20 { a.emit(Insn::Ldi(r, 0)); }
    a.emit(Insn::Ldi(24, 1));
    a.emit(Insn::Ldi(25, 0));
    a.jump(Target::Call, s.write);
}
