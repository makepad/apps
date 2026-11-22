//! Peripheral blocks behind the S3 bus: address dispatch, block events and
//! reset requests. A block sees only aligned word loads and stores (the bus
//! rejects anything else first), each with the machine tick at the start of
//! the accessing instruction. Accesses no block decodes stop the run.
//!
//! Block contract: a method returning `Err` leaves the block as it was (the
//! bus stops without a partial write), except descriptor faults which latch
//! the GDMA channel's descriptor error and halt it. `event(t)` performs
//! the event due at `t`, after which `next_event` is later than `t` or names another event due
//! at `t`; register state is the state at the access tick.
//!
//! `time` holds the time-driven blocks (SYSTIMER, RTC_CNTL's watchdogs and
//! reset state, the timer groups' watchdogs); `pins` holds GPIO, IO_MUX and
//! USB-Serial-JTAG; `intr` routes their sources to CORE0. All are cloned by
//! every mark; the matrix adds only its register bank, no event buffers.

use fab_elec_mcu_emu::{Drive, LinkOut, Note};
use fab_elec_mcu_emu::board::Board;
use fab_elec_mcu_xtensa::sim::Config;

use crate::Junk;
use crate::bus::Journal;

pub mod intr;
mod syscon;
mod i2c;
mod i2s;
mod spi;
mod gdma;
mod efuse;
mod ana;
pub mod pins;
pub mod time;

pub use pins::PinsProfile;
pub use time::TimeProfile;

/// Reset scopes, TRM pp.526–527 §§7.1.1–7.1.3. Every scope keeps SRAM; a
/// Chip Reset (power-on, brownout) is not something a model block requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetScope {
    /// CPU0 only: the CPU restarts from its reset vector.
    Cpu,
    /// The digital system except RTC.
    Core,
    /// The digital system including RTC.
    System,
}

/// A reset performed by a block at `tick`, with its CPU0 cause code from TRM
/// p.528 Table 7.1-1 (facts-machine.md §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetRequest {
    pub tick: u64,
    pub cause: u8,
    pub scope: ResetScope,
}

/// `None`: no block decodes the address. `Err`: the access reached behaviour
/// no source documents; the string names it and the run stops Unmodeled.
pub(crate) type Access<T> = Option<Result<T, &'static str>>;

#[derive(Clone)]
pub(crate) struct Periph {
    /// Private, as `time_due` is kept with it: every change of the timers goes through a method
    /// here, which takes the deadline again (`time_moved`).
    time: time::Time,
    pub pins: pins::Pins,
    pub intr: intr::Intr,
    syscon: syscon::Syscon,
    i2c: [i2c::I2c; 2],
    spi: spi::Spi,
    i2s: i2s::I2s,
    gdma: gdma::Gdma,
    efuse: efuse::Efuse,
    ana: ana::Ana,
    /// Foreign stand-ins' model choices noted this run (`note_once`), and those not yet taken.
    stand_ins: Vec<&'static str>,
    stand_in_notes: Vec<(u64, &'static str)>,
    write_generation: u64,
    /// `time.next_event()` as the timers stand: asked after every register access, and the
    /// timers' fold over their seven deadlines is the dearest of the blocks', while an access
    /// moves one only when it reaches the timers. Taken again wherever they change
    /// (`time_moved`); `next_event` checks it against the fold in debug builds.
    time_due: Option<u64>,
}

impl Periph {
    /// The first peripheral block or write counter that differs between two runs.
    pub fn same_state(&self, other: &Periph) -> Option<&'static str> {
        // (`time_due` follows `time`: no state of its own)
        let Self { time, pins, intr, syscon, i2c: [i2c0, i2c1], i2s, spi, gdma, efuse, ana, stand_ins, stand_in_notes, write_generation, time_due: _ } = self;
        if let Some(field) = time.same_state(&other.time) { return Some(field); }
        if pins != &other.pins { return Some("periph.pins"); }
        if intr != &other.intr { return Some("periph.intr"); }
        if syscon != &other.syscon { return Some("periph.syscon"); }
        if i2c0 != &other.i2c[0] { return Some("periph.i2c[0]"); }
        if i2c1 != &other.i2c[1] { return Some("periph.i2c[1]"); }
        if spi != &other.spi { return Some("periph.spi"); }
        if i2s != &other.i2s { return Some("periph.i2s"); }
        if gdma != &other.gdma { return Some("periph.gdma"); }
        if efuse != &other.efuse { return Some("periph.efuse"); }
        if ana != &other.ana { return Some("periph.ana"); }
        if stand_ins != &other.stand_ins || stand_in_notes != &other.stand_in_notes { return Some("periph.stand_ins"); }
        if write_generation != &other.write_generation { return Some("periph.write_generation"); }
        None
    }

    /// The state at the ROM's jump to the image after power-on.
    pub fn new(time: &TimeProfile, pins: &PinsProfile, mac: [u8; 6], pvt: [u32; 4], apb_ticks: u64, gates: [u32; 4], junk: &mut Junk) -> Self {
        let time = time::Time::new(time, apb_ticks, junk);
        Self {
            time_due: time.next_event(), time, pins: pins::Pins::new(pins, junk),
            intr: intr::Intr::new(), syscon: syscon::Syscon::new(), write_generation: 0,
            i2c: std::array::from_fn(|n| i2c::I2c::new(n, apb_ticks, gates[0], gates[2], junk)),
            spi: spi::Spi::new(gates[0], gates[2], junk),
            i2s: i2s::I2s::new(gates[0], gates[2]),
            gdma: gdma::Gdma::new(gates[1], gates[3], junk),
            efuse: efuse::Efuse::new(mac, pvt, junk),
            ana: ana::Ana::new(), stand_ins: Vec::new(), stand_in_notes: Vec::new(),
        }
    }

    pub fn read(&mut self, addr: u32, tick: u64) -> Access<u32> {
        if let Some(r) = self.time.read(addr, tick) {
            self.time_moved();
            return Some(r);
        }
        if let Some(r) = self.syscon.read(addr) { return Some(r); }
        if let Some(r) = self.pins.read(addr, tick) { return Some(r); }
        if let Some(r) = self.efuse.read(addr, tick) { return Some(r); }
        if let Some(r) = self.ana.read(addr, tick) { return Some(r); }
        for i2c in &mut self.i2c {
            if let Some(r) = i2c.read(addr, tick) { return Some(r); }
        }
        if let Some(r) = self.spi.read(addr) { return Some(r); }
        if let Some(r) = self.i2s.read(addr) { return Some(r); }
        if let Some(r) = self.gdma.read(addr) { return Some(r); }
        self.intr.read(addr, self.interrupt_sources())
    }

    pub fn write(&mut self, addr: u32, value: u32, tick: u64, pll: bool, ram: &[u8]) -> Access<()> {
        if self.i2s.frame_active() && self.pins.i2s_route_write(addr, value) {
            return Some(Err("I2S pad reconfiguration during a frame is unsupported"));
        }
        if self.spi.running() && ((0x6000_4000..0x6000_5000).contains(&addr)
            || (0x6000_9000..0x6000_a000).contains(&addr))
        {
            return Some(Err("GPIO/IO_MUX configuration during SPI activity is not modelled (S16, Q03)"));
        }
        // The timers' only interrupt sources are the systimer's (`Time::interrupt_sources`), and
        // a timer write touches no other block: it re-routes the lines only when it changes them.
        // The CPU's level inputs follow `set_lines` alone, so routing unchanged sources again
        // would change nothing.
        let time_sources = self.time.interrupt_sources();
        if let Some(result) = self.time.write(addr, value, tick) {
            self.time_moved();
            if result.is_ok() && self.time.interrupt_sources() != time_sources {
                self.write_generation = self.write_generation.wrapping_add(1);
            }
            return Some(result);
        }
        if let Some(result) = self.i2s.write(addr, value, tick, pll, &self.pins, &self.gdma) {
            if result.is_ok() {
                self.write_generation = self.write_generation.wrapping_add(1);
                if let Some(note) = i2s::I2s::timing_note(addr, value) { self.note_once(tick, note); }
            }
            return Some(result);
        }
        let gdma_sources = self.gdma.interrupt_sources();
        let result = self.syscon.write(addr, value)
            .or_else(|| self.pins.write(addr, value, tick))
            .or_else(|| self.efuse.write(addr))
            .or_else(|| self.ana.write(addr, value, tick))
            .or_else(|| self.i2c[0].write(addr, value, tick, &self.pins))
            .or_else(|| self.i2c[1].write(addr, value, tick, &self.pins))
            .or_else(|| self.spi.write(addr, value, tick, pll, &mut self.pins, &mut self.gdma, ram))
            .or_else(|| self.gdma.write(addr, value, tick, ram, self.i2s.frame_active()))
            .or_else(|| self.intr.write(addr, value));
        // G03 faults latch an interrupt even though USR returns Err.
        if result == Some(Ok(())) || gdma_sources != self.gdma.interrupt_sources() {
            self.write_generation = self.write_generation.wrapping_add(1);
        }
        result
    }

    pub fn write_gen(&self) -> u64 { self.write_generation }

    /// Whether a read of `addr` is stable for the spin skip: its handler takes no tick and no
    /// `&mut self`, so the value is a function of block state, which changes only at an event
    /// or a write. SPI2 and I2S0 have this property. Never ANA (BUSY until a tick), eFuse,
    /// I2C, pins or USB, whose reads take the tick or change state.
    pub fn stable_read(addr: u32) -> bool { spi::Spi::claims(addr) || i2s::I2s::claims(addr) }

    /// Whether a write to `addr` can be idle in a polling cycle (`idle_write` decides).
    pub fn idle_target(addr: u32) -> bool { time::Time::idle_target(addr) }

    /// See `Time::idle_write`: None when the write is not idle now, else the tick below which
    /// it stays so.
    pub fn idle_write(&self, addr: u32, value: u32) -> Option<u64> {
        if time::Time::idle_target(addr) { self.time.idle_write(addr, value) } else { None }
    }

    /// Whether `self` equals `other` in everything but the declared time fields an idle cycle
    /// changes: the write generation and the RWDT's anchor and due. Notes included; built on
    /// `same_state`, which is exhaustive, so a new field cannot escape.
    pub fn same_but_time(&self, other: &Periph) -> bool {
        let mut a = self.clone();
        a.write_generation = other.write_generation;
        a.time.take_rwdt_time(&other.time);
        a.same_state(other).is_none()
    }

    /// Counts in the write generation of `n` skipped writes.
    pub fn add_writes(&mut self, n: u64) { self.write_generation = self.write_generation.wrapping_add(n); }

    pub fn interrupt_sources(&self) -> u128 {
        // GPIO NMI and watchdog IRQs remain outside the modelled sources.
        self.time.interrupt_sources() | self.pins.interrupt_sources()
            | self.i2c[0].interrupt_sources() | self.i2c[1].interrupt_sources()
            | self.spi.interrupt_sources() | self.i2s.interrupt_sources() | self.gdma.interrupt_sources()
    }

    /// Register `reg` of analog I2C block `block` through `host`, as the ROM reads it.
    pub fn regi2c(&self, block: u8, host: u8, reg: u8) -> Result<u8, &'static str> { self.ana.regi2c(block, host, reg) }

    /// Whether the ROM's write of `data` to that register is one the analog model takes.
    pub fn regi2c_writable(&self, block: u8, host: u8, reg: u8, data: u8) -> Result<(), &'static str> { self.ana.regi2c_writable(block, host, reg, data) }

    /// The write `regi2c_writable` allowed.
    pub fn regi2c_set(&mut self, block: u8, reg: u8, data: u8) { self.ana.regi2c_set(block, reg, data) }

    /// Whether the image wrote both core regulators (the 240 MHz raise).
    pub fn regulators_written(&self) -> bool { self.ana.regulators_written() }

    /// A foreign stand-in's model choice, noted once a run at `tick`.
    pub fn note_once(&mut self, tick: u64, what: &'static str) {
        if !self.stand_ins.contains(&what) {
            self.stand_ins.push(what);
            self.stand_in_notes.push((tick, what));
        }
    }

    /// Whether a CPU store to these SRAM bytes would race a GDMA transfer in flight.
    pub fn dma_in_flight(&self, offset: usize, len: usize) -> bool { self.gdma.in_flight(offset, len) }

    /// Whether a GDMA transfer is under way: without one, no CPU store meets `dma_in_flight`.
    pub fn dma_active(&self) -> bool { self.gdma.active() }

    pub fn interrupt_lines(&self, config: &Config) -> u32 {
        self.intr.lines(self.interrupt_sources(), config)
    }

    pub fn event_source(&self, tick: u64) -> Option<usize> {
        self.time.interrupt_event_source(tick)
            .or_else(|| (self.pins.next_event() == Some(tick)).then_some(96))
            .or_else(|| (self.spi.next_event() == Some(tick)).then_some(21))
            .or_else(|| (self.i2s.next_event() == Some(tick)).then_some(25)) // trm.txt:18495.
            .or_else(|| self.i2c.iter().position(|i| i.next_event() == Some(tick)).map(|n| 42 + n))
    }

    /// The earliest tick at which a block changes state by itself. O(1): the timers' deadline is
    /// kept (`time_due`), the other blocks' are a field or two each.
    pub fn next_event(&self) -> Option<u64> {
        debug_assert_eq!(self.time_due, self.time.next_event(), "the timers changed without `time_moved`");
        fn earlier(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        let i2c = earlier(self.i2c[0].next_event(), self.i2c[1].next_event());
        earlier(earlier(earlier(self.time_due, self.pins.next_event()), i2c), earlier(self.spi.next_event(), self.i2s.next_event()))
    }

    /// The timers may have changed: their deadline again (`time_due`). Called by every method
    /// that reaches them mutably, on every way out.
    fn time_moved(&mut self) {
        self.time_due = self.time.next_event();
    }

    /// Runs one due event: time first at equal ticks, then pins, SPI, I2S, I2C0/1.
    /// Returning after each event lets the machine feed board inputs and see a
    /// newly scheduled board deadline before advancing to another block event.
    pub fn events<B: Board>(&mut self, tick: u64, out: &mut Vec<LinkOut>, board: &mut B, board_t0: u64,
        ram: &mut [u8], journal: &mut Journal) -> Result<Option<ResetRequest>, &'static str>
    {
        let ran = self.event_due(tick, out, board, board_t0, ram, journal);
        self.time_moved();
        ran
    }

    fn event_due<B: Board>(&mut self, tick: u64, out: &mut Vec<LinkOut>, board: &mut B, board_t0: u64,
        ram: &mut [u8], journal: &mut Journal) -> Result<Option<ResetRequest>, &'static str>
    {
        let Some(due) = self.next_event().filter(|&due| due <= tick) else { return Ok(None); };
        if self.time_due == Some(due) {
            if self.i2c.iter().any(i2c::I2c::running) || self.spi.running() || self.gdma.running() || self.i2s.busy() {
                let mut next = self.time.clone();
                if next.event(due)?.is_some() {
                    return Err(if self.i2c.iter().any(i2c::I2c::running) {
                        "I2C reset during bus activity is undocumented (Q01)"
                    } else if self.i2s.busy() { "reset during I2S activity is unsupported" }
                    else { "reset during SPI/GDMA activity is undocumented (Q01, Q04)" });
                }
                self.time = next;
            } else { return self.time.event(due); }
        } else if self.pins.next_event() == Some(due) {
            self.pins.event(due, out)?;
        } else if self.spi.next_event() == Some(due) {
            self.spi.event(due, board, board_t0, &mut self.pins, &mut self.gdma, ram, journal)?;
        } else if self.i2s.next_event() == Some(due) {
            self.i2s.event(due, board, board_t0, &mut self.gdma, ram, journal)?;
        } else {
            let i2c = self.i2c.iter_mut().find(|i| i.next_event() == Some(due)).expect("a due I2C event exists");
            i2c.event(due, board, board_t0)?;
        }
        Ok(None)
    }

    /// The state after `reset`, at the ROM's next jump to the image.
    pub fn reset(&mut self, reset: ResetRequest, time: &TimeProfile, pins: &PinsProfile, apb_ticks: u64, gates: [u32; 4], junk: &mut Junk) {
        self.time.reset(reset, time, apb_ticks, junk);
        self.time_moved();
        self.pins.reset(reset, pins, junk);
        self.intr.reset(reset.scope);
        self.syscon.reset(reset.scope);
        if reset.scope != ResetScope::Cpu {
            for i2c in &mut self.i2c { i2c.rehandoff(apb_ticks, [gates[0], gates[2]], junk); }
            self.spi.rehandoff([gates[0], gates[2]], junk);
            self.i2s = i2s::I2s::new(gates[0], gates[2]);
            self.gdma.rehandoff([gates[1], gates[3]], junk);
        } else {
            for i2c in &mut self.i2c { i2c.new_epoch(apb_ticks); }
            self.spi.new_epoch();
            self.i2s.new_epoch();
            self.gdma.new_epoch();
        }
    }

    /// The APB clock now lasts `apb_ticks` per cycle, from `tick` on.
    pub fn apb_changed(&mut self, apb_ticks: u64, tick: u64) -> Result<(), &'static str> {
        if self.i2s.busy() { return Err("clock source change during I2S activity is unsupported"); }
        if self.spi.running() || self.gdma.running() {
            return Err("clock source change during SPI/GDMA activity is not modelled (S08, Q04)");
        }
        if self.i2c.iter().any(i2c::I2c::running) {
            return Err("I2C APB clock change during a command list is undocumented (I09, I13)");
        }
        let changed = self.time.apb_changed(apb_ticks, tick);
        self.time_moved();
        changed?;
        self.pins.apb_changed(apb_ticks, tick)?;
        for i2c in &mut self.i2c { i2c.apb_changed(apb_ticks); }
        Ok(())
    }

    pub fn gates(&mut self, gates: [u32; 4], written: usize) -> Result<(), &'static str> {
        let (en, rst) = (gates[0], gates[2]);
        // Validate every controller before applying any gate (I20, S17, G12).
        for i2c in &self.i2c { i2c.check_gate(en, rst)?; }
        self.spi.check_gate(en, rst)?;
        self.i2s.check_gate(en, rst)?;
        self.gdma.check_gate(gates[1], gates[3])?;
        for i2c in &mut self.i2c { i2c.gate(en, rst, written == 2); }
        self.spi.gate(en, rst, written == 2);
        self.i2s.gate(en, rst, written == 2);
        self.gdma.gate(gates[1], gates[3], written == 3);
        self.write_generation = self.write_generation.wrapping_add(1);
        Ok(())
    }

    pub fn drive(&self, pin: usize) -> Drive { self.pins.drive(pin) }

    /// Appends and removes the blocks' notes: the hardware races they met, each resolved by the
    /// model choice its note names, at the access or event tick.
    pub fn take_notes(&mut self, out: &mut Vec<Note>) {
        let mut notes = Vec::new();
        self.time.take_notes(&mut notes);
        self.time_moved();
        self.pins.take_notes(&mut notes);
        self.efuse.take_notes(&mut notes);
        self.ana.take_notes(&mut notes);
        for i2c in &mut self.i2c { i2c.take_notes(&mut notes); }
        self.spi.take_notes(&mut notes);
        self.gdma.take_notes(&mut notes);
        notes.append(&mut self.stand_in_notes);
        // in tick order; equal ticks keep the blocks' order
        notes.sort_by_key(|n| n.0);
        out.extend(notes.into_iter().map(|(tick, what)| Note { tick, what }));
    }
    pub fn drive_gen(&self) -> u64 { self.pins.drive_gen() }
    /// A note of the timers' at `tick` (`Time::note`).
    pub fn note_time(&mut self, tick: u64, what: &'static str) {
        self.time.note(tick, what);
        self.time_moved();
    }
    pub fn set_input(&mut self, pin: usize, high: bool, tick: u64) { self.pins.set_input(pin, high, tick) }
    pub fn link_in(&mut self, link: u8, byte: u8, tick: u64) { self.pins.link_in(link, byte, tick) }
}
