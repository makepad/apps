//! ATmega328P adapter for the shared, rewindable machine interface.

use crate::chip;
use crate::insn::{decode, Insn};
use crate::sim::{Cpu, DecodeTable, Event, Stop, CONDITIONAL, LANDING, LEADER};
use std::sync::Arc;
use fab_elec_mcu_emu::{
    ChipState, Drive, DriveEdge, FwTimer, Halt, HaltKind, LinkOut, Machine, Note, Pending, Pull,
    CodeMap, CondKind, ExecCounts, PinUse, Ran, SampleAt, Status, Tally, Tier, Until, Wake,
};

// The GPIO and their rows come from the chip's pin table.
const PINS: [&str; 22] = chip::NAMES;
const REGS: [&str; 32] = [
    "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7",
    "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15",
    "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23",
    "r24", "r25", "r26", "r27", "r28", "r29", "r30", "r31",
];

fn port_bit(pin: usize) -> (char, u8) {
    let p = &chip::PINS[pin];
    (p.port, p.bit)
}

/// A pin's drive from the oracle's drive word (DDR, driven high, pulled up per port).
fn word_drive(word: &[u8; 9], p: usize, mask: u8) -> Drive {
    if word[p] & mask != 0 {
        if word[3 + p] & mask != 0 { Drive::High } else { Drive::Low }
    } else if word[6 + p] & mask != 0 { Drive::Input(Pull::Up) } else { Drive::Input(Pull::None) }
}

/// One board-clocked AVR; a tick is a CPU cycle and flash addresses are words.
#[derive(Clone)]
pub struct AvrMachine {
    cpu: Cpu,
    saved: Option<Box<Cpu>>,
    tick_hz: u64,
    trace: Vec<(u64, u32)>,
    out: Vec<LinkOut>,
    log: Vec<(u64, [u8; 9], [u8; 9])>,
    saved_lens: (usize, usize),
    tally: Tally,
    tally_map: Option<Arc<CodeMap>>,
    /// Execution strategy, outside the simulated state and its mark.
    tier: Tier,
}

impl AvrMachine {
    /// Loads an image and enables co-simulation timing at the board's crystal rate.
    pub fn new(image: &[u16], tick_hz: u64) -> AvrMachine {
        assert!(tick_hz != 0, "board clock must be nonzero");
        let mut cpu = Cpu::new(image);
        cpu.set_clock_hz(tick_hz);
        cpu.set_cosim_timing(true);
        AvrMachine { cpu, saved: None, tick_hz, trace: Vec::new(), out: Vec::new(), log: Vec::new(), saved_lens: (0, 0),
            tally: Tally::default(), tally_map: None, tier: Tier::default() }
    }

    /// Inspect the underlying CPU; machine memory/peripheral snapshots are side-effect free.
    pub fn cpu(&self) -> &Cpu { &self.cpu }

    /// Committed EEPROM bytes, including writes completed since the last run.
    pub fn eeprom(&self) -> &[u8] { self.cpu.eeprom() }

    /// Load exactly 1,024 EEPROM bytes before running; panics on any other length.
    pub fn set_eeprom(&mut self, bytes: &[u8]) { self.cpu.set_eeprom(bytes); }

    fn halt(&self, stop: Stop) -> Halt {
        let (pc, kind, what) = match stop {
            // BREAK completes and advances PC before the oracle returns its stop.
            Stop::Break => (self.cpu.pc - 1, HaltKind::Break, "BREAK"),
            Stop::BadInsn { pc } => (pc, HaltKind::BadInsn, "instruction encoding"),
            Stop::BadAddress { pc, addr } => (pc, HaltKind::BadAddress { addr }, "address"),
            Stop::UdrOverrun { pc } => (pc, HaltKind::Overrun, "USART0 transmit: UDR0 written while its buffer was full"),
            Stop::Watchdog { pc } => (pc, HaltKind::Watchdog, "watchdog"),
            Stop::Unmodeled { pc, what } => (pc, HaltKind::Unmodeled, what),
            Stop::Cycles | Stop::Sleep => unreachable!("a wait is not a latched halt"),
        };
        Halt { tick: self.cpu.cycles, pc, kind, what }
    }

    fn step_ref(&mut self, deadline: u64, table: &DecodeTable) -> Result<(), Stop> {
        let pc = self.cpu.pc;
        let (flags, block, cond) = table.count_entry(pc);
        if flags == 0 || !self.cpu.will_execute() { return self.cpu.step_until(deadline); }

        // A leader counts on entry, including an instruction whose fetch or execution faults.
        if flags & LEADER != 0 { self.tally.block(block); }
        self.cpu.step_until(deadline)?;
        if flags & CONDITIONAL != 0 {
            let edge = &table.map.conds[cond as usize - 1];
            let taken = if edge.kind == CondKind::Branch && edge.taken == edge.fall {
                // A zero-offset branch preserves SREG and reaches the same PC either way.
                match decode(&self.cpu.flash[pc as usize..]) {
                    Some((Insn::Brbs(bit, _), _)) => self.cpu.sreg & (1 << bit) != 0,
                    Some((Insn::Brbc(bit, _), _)) => self.cpu.sreg & (1 << bit) == 0,
                    _ => unreachable!("branch annotation requires a branch instruction"),
                }
            } else { self.cpu.pc != edge.fall };
            self.tally.cond(cond, taken);
        }
        if flags & LANDING != 0 {
            let (flags, block, _) = table.count_entry(self.cpu.pc);
            if flags & LEADER == 0 { self.tally.block(block); }
        }
        Ok(())
    }

    fn run_tier<const DECODED: bool>(&mut self, until: &Until) -> Ran {
        let table = self.cpu.flash.decoded().clone();
        if !self.tally_map.as_ref().is_some_and(|map| Arc::ptr_eq(map, &table.map)) {
            self.tally.resize(&table.map);
            self.tally_map = Some(table.map.clone());
        }
        let generation = self.cpu.drive_gen();
        let _ = self.cpu.take_drive_at();
        loop {
            if self.cpu.fault().is_some() { self.cpu.catch_up(); return Ran::Halted; }
            if until.on_drive && self.cpu.drive_gen() != generation {
                self.cpu.catch_up();
                return Ran::Drive { at: self.cpu.take_drive_at().expect("drive change has a timestamp") };
            }
            // A marked PC stops or traces only when its instruction is next to execute.
            let marked = until.marks.contains(self.cpu.pc)
                && if DECODED { self.cpu.will_execute_deferred() } else { self.cpu.will_execute() };
            if marked && self.cpu.cycles > until.stops_after
                && until.stops.binary_search(&self.cpu.pc).is_ok() { self.cpu.catch_up(); return Ran::Pc; }
            if self.cpu.cycles >= until.tick { self.cpu.catch_up(); return Ran::Tick; }
            if marked { self.trace.push((self.cpu.cycles, self.cpu.pc)); }
            let result = if DECODED {
                self.cpu.step_decoded(until.tick, &table, Some(&mut self.tally))
            } else {
                self.step_ref(until.tick, &table)
            };
            if !self.cpu.events.is_empty() {
                for event in self.cpu.events.drain(..) {
                    if let Event::Tx { cycle, byte } = event {
                        self.out.push(LinkOut { link: 0, tick: cycle, byte });
                    }
                }
            }
            if let Err(stop) = result {
                if !matches!(stop, Stop::Cycles | Stop::Sleep) { self.cpu.latch(stop); }
            }
        }
    }
}

impl Machine for AvrMachine {
    fn set_tier(&mut self, tier: Tier) { self.tier = tier; }

    fn same_state(&self, other: &Self) -> Option<&'static str> {
        // tally_map caches the decode map identity for resize. Each clone can build its own map.
        let Self { cpu, saved, tick_hz, trace, out, log, saved_lens, tally, tally_map: _, tier: _ } = self;
        if let Some(field) = cpu.same_state(&other.cpu) { return Some(field); }
        match (saved, &other.saved) {
            (Some(a), Some(b)) => {
                if a.same_state(b).is_some() { return Some("saved cpu"); }
            }
            (None, None) => {}
            _ => return Some("mark"),
        }
        if *tick_hz != other.tick_hz { return Some("tick_hz"); }
        if *trace != other.trace { return Some("trace"); }
        if *out != other.out { return Some("link output"); }
        if *log != other.log { return Some("drive log"); }
        if *saved_lens != other.saved_lens { return Some("saved_lens"); }
        // Tally exposes its two layers through take and rewind. Inspect copies so the caller
        // can still take the counts, including the last outcome on either side of the mark.
        fn counts(tally: &Tally, rewind: bool) -> ExecCounts {
            let mut copy = tally.clone();
            if rewind { copy.rewind(); }
            let mut out = ExecCounts::default();
            copy.take(&mut out);
            out
        }
        if counts(tally, false) != counts(&other.tally, false) { return Some("tally"); }
        if saved.is_some() && counts(tally, true) != counts(&other.tally, true) { return Some("marked tally"); }
        None
    }

    fn set_code_roots(&mut self, roots: &[u32]) { self.cpu.flash.set_code_roots(roots); }

    fn code_map(&self) -> Option<Arc<CodeMap>> { Some(self.cpu.flash.decoded().map.clone()) }

    fn take_counts(&mut self, out: &mut ExecCounts) { self.tally.take(out); }

    fn tick_hz(&self) -> u64 { self.tick_hz }
    fn now(&self) -> u64 { self.cpu.cycles }
    fn pc(&self) -> u32 { self.cpu.pc }

    fn run(&mut self, until: &Until) -> Ran {
        // Select once per run. Every return catches up the CPU, so the next run may use either tier.
        match self.tier {
            Tier::Ref => self.run_tier::<false>(until),
            Tier::Decoded | Tier::Native => self.run_tier::<true>(until),
        }
    }

    fn mark(&mut self) {
        self.tally.mark();
        if let Some(saved) = &mut self.saved { saved.copy_from(&self.cpu); }
        else { self.saved = Some(Box::new(self.cpu.clone())); }
        self.saved_lens = (self.trace.len(), self.out.len());
    }

    fn eeprom_bytes(&self) -> Option<Arc<[u8]>> { Some(self.cpu.eeprom_shared()) }

    fn load_eeprom(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() != crate::sim::EEPROM {
            return false;
        }
        self.cpu.set_eeprom(bytes);
        // a programmer's load is outside the run: a rewind to a mark made before it keeps it
        if let Some(saved) = &mut self.saved {
            saved.set_eeprom(bytes);
        }
        true
    }

    fn rewind(&mut self) {
        self.cpu.copy_from(self.saved.as_ref().expect("rewind requires a mark"));
        self.tally.rewind();
        self.trace.truncate(self.saved_lens.0);
        self.out.truncate(self.saved_lens.1);
    }

    fn pins(&self) -> &[&'static str] { &PINS }

    fn drive(&self, pin: usize) -> Drive {
        if self.analog_only(pin) { return Drive::Input(Pull::None); }
        let (port, bit) = port_bit(pin);
        let mask = 1 << bit;
        let (ddr, output, level) = self.cpu.drive(port);
        if ddr & mask != 0 {
            if level & mask != 0 { Drive::High } else { Drive::Low }
        } else if output & mask != 0 && self.cpu.io(0x55) & 0x10 == 0 {
            Drive::Input(Pull::Up)
        } else { Drive::Input(Pull::None) }
    }

    fn drive_gen(&self) -> u64 { self.cpu.drive_gen() }

    fn set_input(&mut self, pin: usize, high: bool) {
        if self.analog_only(pin) { return; }
        let (port, bit) = port_bit(pin);
        self.cpu.set_pin(port, bit, high);
    }

    fn link_in(&mut self, link: u8, byte: u8) {
        assert_eq!(link, 0, "ATmega328P has only USART0");
        self.cpu.rx(byte);
    }

    fn take_out(&mut self, out: &mut Vec<LinkOut>) { out.append(&mut self.out); }
    fn take_trace(&mut self, out: &mut Vec<(u64, u32)>) { out.append(&mut self.trace); }
    // The oracle documents every behaviour it models; it makes no assumption a note would carry.
    fn take_notes(&mut self, _out: &mut Vec<Note>) {}

    fn take_drives(&mut self, out: &mut Vec<DriveEdge>) {
        self.cpu.take_drive_log(&mut self.log);
        for (tick, before, after) in self.log.drain(..) {
            for pin in 0..chip::PINS.len() {
                let (port, bit) = port_bit(pin);
                let p = usize::from(port as u8 - b'B');
                let mask = 1 << bit;
                let changed = (before[p] ^ after[p]) | (before[3 + p] ^ after[3 + p]) | (before[6 + p] ^ after[6 + p]);
                if changed & mask == 0 { continue; }
                out.push(DriveEdge { tick, pin: pin as u16, drive: word_drive(&after, p, mask) });
            }
        }
    }

    fn take_samples(&mut self, out: &mut Vec<SampleAt>) { self.cpu.take_samples(out); }
    fn set_sample(&mut self, at: SampleAt, volts: f64, supply: f64) { self.cpu.set_sample(at, volts, supply); }
    fn analog_only(&self, pin: usize) -> bool { chip::ANALOG_ONLY.iter().any(|p| p.row == pin) }

    fn status(&self) -> Status {
        if let Some(stop) = self.cpu.fault() { Status::Halted(self.halt(stop)) }
        else if self.cpu.sleeping() { Status::Sleeping { wake: self.cpu.wake_at() } }
        else { Status::Running }
    }

    fn regs(&self, out: &mut Vec<(&'static str, u32)>) {
        out.clear();
        out.extend(REGS.into_iter().zip(self.cpu.r).map(|(name, value)| (name, u32::from(value))));
        out.extend([("sreg", u32::from(self.cpu.sreg)), ("sp", u32::from(self.cpu.sp)), ("pc", self.cpu.pc)]);
    }

    fn read_data(&self, addr: u32, out: &mut [u8]) -> bool {
        let Some(offset) = addr.checked_sub(0x100).map(|n| n as usize) else { return false; };
        let len = self.cpu.sram.len().min(0x800);
        if offset >= len || out.len() > len - offset { return false; }
        out.copy_from_slice(&self.cpu.sram[offset..offset + out.len()]);
        true
    }

    fn chip(&self, out: &mut ChipState) {
        out.pins.clear();
        for pin in &chip::PINS {
            let p = match pin.port { 'B' => 0, 'C' => 1, 'D' => 2,
                _ => unreachable!("GPIO port must be B, C or D") };
            out.pins.push(self.cpu.pin_use(p, pin.bit));
        }
        out.pins.extend(chip::ANALOG_ONLY.iter().map(|_| (PinUse::Peripheral, 0.0)));
        out.timers.resize_with(3, || FwTimer {
            name: "", mode: "", prescale: 0, top: 0, count: 0, compares: Vec::new(), next: None,
        });
        for (n, timer) in out.timers.iter_mut().enumerate() {
            timer.name = ["Timer0", "Timer1", "Timer2"][n];
            timer.mode = self.cpu.timer_wave(n);
            timer.prescale = self.cpu.timer_prescale(n);
            timer.top = u32::from(self.cpu.timer_top(n));
            timer.count = u32::from(self.cpu.timer_count(n));
            timer.compares.clear();
            timer.compares.extend(self.cpu.timer_compares(n).map(u32::from));
            timer.next = self.cpu.timer_next(n);
        }
        out.pending.clear();
        out.pending.extend(self.cpu.pending_irqs().map(|(vector, source)| Pending::Irq { source, vector }));
        let bytes = self.cpu.rx_pending();
        if bytes != 0 { out.pending.push(Pending::Rx { link: 0, bytes }); }
        out.wake = self.cpu.wake_irq().map(|(tick, source, vector)| Wake { tick, source, vector });
    }
}
