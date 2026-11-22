//! The machine boundary used by the circuit co-simulation adapter.

use crate::board::{Audio, BusEvent, NetInfo};
use crate::PcSet;
use crate::counts::{CodeMap, ExecCounts};
use std::sync::Arc;

/// A deterministic, rewindable simulated chip. Time is integer ticks since its last reset.
pub trait Machine: Clone + Send + 'static {
    /// Extra code entries in the image (handlers, statements and returns), set before running.
    fn set_code_roots(&mut self, _roots: &[u32]) {}
    /// The image's shared block and conditional map, if this machine reports execution counts.
    fn code_map(&self) -> Option<Arc<CodeMap>> { None }
    /// Appends and removes untaken execution counts. Call just before `mark`, as `take_trace`.
    fn take_counts(&mut self, _out: &mut ExecCounts) {}
    /// Ticks per second in this machine's time grid.
    fn tick_hz(&self) -> u64;
    /// Current tick since the chip's last reset.
    fn now(&self) -> u64;
    /// Next instruction's address in the ISA's unit (words on AVR, bytes on Xtensa).
    fn pc(&self) -> u32;
    /// Executes and sleeps until the first condition, with simultaneous conditions
    /// ordered: Halted, Reset, Drive, Barrier, Pc, Tick. Every return is resumable.
    /// Before executing a marked instruction without stopping there, appends
    /// `(now, pc)` to the trace. A sleep without a wake source reaches `until.tick`.
    fn run(&mut self, until: &Until) -> Ran;
    /// Saves all CPU, memory, peripheral and input state, and the trace/output lengths.
    /// Callers take trace/output only just before marking; the saved buffers are reused.
    fn mark(&mut self);
    /// A host input (a touch, a tilt) at the machine's current tick: appended to the board's input
    /// schedule and applied after this tick's chip and board events. False: this machine takes no
    /// input of this type.
    fn host_input(&mut self, _input: &dyn std::any::Any) -> bool { false }
    /// Publish committed observations, including the final frame at a pause or stop.
    /// `t` is the run's time of the committed instant, in seconds.
    fn publish(&mut self, _t: f64) {}
    /// Hands over the outputs a view reads (a panel's screen feed) to the earlier copy of this
    /// machine that a rewind restores (`put_feeds`). None: it has none. A clone carries none.
    fn take_feeds(&mut self) -> Option<Box<dyn std::any::Any + Send>> { None }
    /// Takes over the feeds `take_feeds` handed over from the machine this is an earlier copy of.
    /// Each feed sends this copy's whole state at its next publication.
    fn put_feeds(&mut self, _feeds: Box<dyn std::any::Any + Send>) {}
    /// Restores the mark and truncates untaken trace/output to their marked lengths.
    /// Panics if no mark exists.
    fn rewind(&mut self);
    /// Package names of the pins exposed to the circuit, indexed by the pin APIs.
    fn pins(&self) -> &[&'static str];
    /// The pin's current electrical drive, independent of its externally sampled input.
    fn drive(&self, pin: usize) -> Drive;
    /// Generation that changes whenever any pin's `drive` may have changed or a sample is requested.
    fn drive_gen(&self) -> u64;
    /// Appends and removes the untaken drive changes in tick order: one entry per pin whose
    /// `drive` changed, at the tick of the change, even when several fall inside one instruction.
    /// `rewind` restores the untaken changes as they were at the mark, so changes taken after a
    /// mark come again when a rewound run passes them. A reset discards the untaken changes;
    /// `drive` then gives the reset state.
    fn take_drives(&mut self, out: &mut Vec<DriveEdge>);
    /// Appends and removes the untaken sample requests in tick order. A request is made as a
    /// conversion's sample-and-hold passes; a run with `on_drive` returns `Ran::Drive { at }` at
    /// the first one, as for a drive change. `rewind` restores untaken requests as at the mark.
    fn take_samples(&mut self, _out: &mut Vec<SampleAt>) {}
    /// The pad's volts at `at.tick` and the pin's supply, volts (NaN: not in the circuit). May be
    /// given before or after the request; the conversion whose sample-and-hold is `at` takes it.
    /// A rewind to a mark before it drops it (the caller gives it again from its log).
    fn set_sample(&mut self, _at: SampleAt, _volts: f64, _supply: f64) {}
    /// A pin with no digital input (an analog-only pin): no level watches, never `set_input`.
    fn analog_only(&self, _pin: usize) -> bool { false }
    /// Sets an input pin's sampled level from `now` onward.
    fn set_input(&mut self, pin: usize, high: bool);
    /// Starts arrival of a received byte on the numbered link at `now`.
    fn link_in(&mut self, link: u8, byte: u8);
    /// Appends and removes all untaken link output. Call only just before `mark`.
    fn take_out(&mut self, out: &mut Vec<LinkOut>);
    /// Appends and removes all untaken marked-instruction trace. Call just before `mark`.
    fn take_trace(&mut self, out: &mut Vec<(u64, u32)>);
    /// Appends and removes all untaken notes: modelled assumptions the run met, such as a
    /// hardware race resolved by a documented model choice. Call just before `mark`.
    fn take_notes(&mut self, out: &mut Vec<Note>);
    /// The board's nets, for a view; a machine without a board has none.
    fn nets(&self) -> &[NetInfo] { &[] }
    /// Drains the board's records (levels, bus steps, part notes) in board time, just before a
    /// mark, as `take_out`.
    fn take_bus(&mut self, _out: &mut Vec<BusEvent>) {}
    /// Drains the sound the board's amplifiers played (`Board::take_audio`), in board time, just
    /// before a mark, as `take_bus`.
    fn take_audio(&mut self, _out: &mut Audio) {}
    /// Current running, sleeping or latched halt state.
    fn status(&self) -> Status;
    /// Why the machine halted, beyond the halt's kind and text, when the machine knows more
    /// (a stand-in's stop reason). Read at a halt, never per step.
    fn halt_detail(&self) -> Option<String> { None }
    /// Clears `out`, then fills every register by name and value.
    fn regs(&self, out: &mut Vec<(&'static str, u32)>);
    /// Copies readable RAM without side effects; returns false and leaves `out`
    /// untouched if any requested byte lies outside readable RAM.
    fn read_data(&self, addr: u32, out: &mut [u8]) -> bool;
    /// Overwrites every field with the chip's current timer, pending and wake state.
    fn chip(&self, out: &mut ChipState);
    /// The chip's non-volatile memory (an EEPROM) as its committed writes left it: what a power
    /// cycle keeps. Shared with the machine, never copied: O(1). None: this machine has none.
    fn eeprom_bytes(&self) -> Option<Arc<[u8]>> { None }
    /// Loads the non-volatile memory before running, as a programmer fills a chip's EEPROM: the
    /// bytes go into that memory only, never into the firmware's state, which reaches them only
    /// through its own EEPROM reads. False, and nothing changed: this machine has none, or
    /// `bytes` is not its size.
    fn load_eeprom(&mut self, _bytes: &[u8]) -> bool { false }
    /// Runs code in `tier` from the next `run` on. Results never depend on the tier: every tier
    /// returns at the same instruction boundaries with the same state. A machine with one tier
    /// ignores it.
    fn set_tier(&mut self, _tier: Tier) {}
    /// The first part of the whole state (CPU, memory, peripherals, board, untaken outputs and
    /// the mark) in which this machine differs from `other`, a copy of the same machine; None:
    /// the same. For differential runs of two tiers (`Lockstep`).
    fn same_state(&self, _other: &Self) -> Option<&'static str> { Some("this machine does not compare its state") }
}

/// How a machine executes its code. The reference interprets each instruction with every check
/// the chip's model makes; the faster tiers run only where those checks are known to come out
/// clear and hand every other instruction boundary to it, so results never depend on the tier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tier {
    /// The reference interpreter.
    Ref,
    /// Pre-decoded instructions under a horizon budget, run by the reference's own execute.
    #[default]
    Decoded,
    /// Native code per guest code page where a machine has compiled it (the `jit` feature), the
    /// decoded tier elsewhere: a machine without native code runs this as `Decoded`.
    Native,
}

/// Conditions bounding a run; stops are sorted and each stop is also marked.
pub struct Until<'a> {
    /// Deadline; sleep stops exactly here, execution at the first boundary at or after it.
    pub tick: u64,
    /// Addresses to trace immediately before instruction execution.
    pub marks: &'a PcSet,
    /// Sorted addresses at which to stop before executing the instruction.
    pub stops: &'a [u32],
    /// PC stops apply only when `now() > stops_after`, permitting a stopped PC to resume.
    pub stops_after: u64,
    /// Return on the first drive change or sample request during this run.
    pub on_drive: bool,
}

/// Why a resumable run returned; precedence is Halted, Reset, Drive, Barrier, Pc, Tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ran {
    /// Deadline reached: exact during sleep, otherwise the first instruction boundary after it.
    Tick,
    /// Next instruction is a stop and `now > stops_after`; none of it has executed.
    Pc,
    /// First drive change or sample request during this call, only with `on_drive`; `at <= now()`.
    /// Sleep waits are cut at the edge, so a change during a sleep wait has `at == now()`.
    Drive { at: u64 },
    /// About to perform an effect rewind cannot undo; the caller marks, then runs again.
    Barrier,
    /// Chip reset at `at` in the old epoch; `now()` restarts at zero.
    Reset { at: u64 },
    /// Fault or model stop is latched, and `status()` reports `Halted`.
    Halted,
}

/// The chip's current execution state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Ready to execute instructions or enter an interrupt.
    Running,
    /// Asleep, with an optional absolute tick of the next wake request.
    Sleeping { wake: Option<u64> },
    /// Stopped on a latched fault or explicit model stop.
    Halted(Halt),
}

/// A weak bias on a pin not actively driven high or low.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    /// No bias.
    None,
    /// Pull toward the positive supply.
    Up,
    /// Pull toward ground.
    Down,
}

/// Electrical drive presented to the circuit solver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drive {
    /// Actively drives low.
    Low,
    /// Actively drives high.
    High,
    /// High impedance except for the specified weak pull.
    Input(Pull),
    /// Sinks current when `low`; otherwise releases the pin with the specified pull.
    OpenDrain { low: bool, pull: Pull },
}

/// A modelled assumption the run met, for the run's events: where the hardware's behaviour is
/// undocumented and the model chose one deterministic outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Note {
    pub tick: u64,
    pub what: &'static str,
}

/// One pin's drive change: the tick it took effect and the drive from then on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriveEdge {
    pub tick: u64,
    /// Index into `Machine::pins`.
    pub pin: u16,
    pub drive: Drive,
}

/// One analog sample a conversion takes: the machine pin and the tick of its sample-and-hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleAt {
    pub tick: u64,
    pub pin: u16,
}

/// One transmitted link byte, timestamped when transmission starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkOut {
    /// Machine-specific link index.
    pub link: u8,
    /// Start tick in the current reset epoch.
    pub tick: u64,
    /// Transmitted byte.
    pub byte: u8,
}

/// Machine-independent reason for a halt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaltKind {
    /// Explicit break instruction.
    Break,
    /// Invalid or unsupported instruction encoding.
    BadInsn,
    /// Access outside the implemented address space.
    BadAddress { addr: u32 },
    /// A peripheral buffer overrun.
    Overrun,
    /// Watchdog stop.
    Watchdog,
    /// Requested behavior is not modeled.
    Unmodeled,
}

/// A latched stop, including the machine location and human-readable cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Halt {
    /// Tick at which the stop occurred.
    pub tick: u64,
    /// Offending instruction's address in the machine's ISA unit.
    pub pc: u32,
    /// Portable stop category.
    pub kind: HaltKind,
    /// Static description of the stopped operation or unmodeled feature.
    pub what: &'static str,
}

/// Side-effect-free peripheral state for inspection; `chip` overwrites all fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChipState {
    /// Firmware-visible timers in chip order.
    pub timers: Vec<FwTimer>,
    /// Enabled flagged interrupts and received bytes awaiting consumption.
    pub pending: Vec<Pending>,
    /// Next wake request while sleeping, if any.
    pub wake: Option<Wake>,
    /// Each pin's use and PWM duty (0..=1, zero unless `Pwm`), by machine row; empty when the
    /// machine does not report them (the device then reads the use from the pin's drive).
    pub pins: Vec<(crate::PinUse, f32)>,
}

/// A timer's active configuration and next hardware event.
#[derive(Clone, Debug, PartialEq)]
pub struct FwTimer {
    /// Datasheet name, such as "Timer0".
    pub name: &'static str,
    /// "normal", "ctc", "fast pwm", "phase-correct pwm", or "phase-and-frequency-correct pwm".
    pub mode: &'static str,
    /// Clock divisor, zero when stopped.
    pub prescale: u32,
    /// Active configured counter TOP.
    pub top: u32,
    /// Current counter.
    pub count: u32,
    /// Active compare latches in channel order, not pending buffered writes.
    pub compares: Vec<u32>,
    /// Next compare or overflow and its absolute tick, when running.
    pub next: Option<(TimerEvent, u64)>,
}

/// Hardware timer event, independent of whether its interrupt is enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerEvent {
    /// Compare match on the zero-based channel.
    Compare(u8),
    /// Counter overflow at the mode's overflow boundary.
    Overflow,
}

/// Work waiting for firmware attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    /// Enabled interrupt with its flag set, using the datasheet name and ISA vector unit.
    Irq { source: &'static str, vector: u32 },
    /// Bytes in the receive FIFO, shifter or arrival queue for a link.
    Rx { link: u8, bytes: u32 },
}

/// Interrupt request projected to wake the sleeping CPU (before wake/entry latency).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wake {
    /// Absolute tick of the wake request.
    pub tick: u64,
    /// Datasheet interrupt source name.
    pub source: &'static str,
    /// Interrupt vector in the machine's ISA unit.
    pub vector: u32,
}
