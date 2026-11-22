//! The microsplash runtime: firmware written in Splash, run as a module of
//! flat AIR (the Splash compute core's IR) by the same runtime on a chip and
//! inside the circuit simulator.
//!
//! ```text
//! module words --[module::load: decode, VERIFY]--> Module
//!     Module + state + scratch --[exec: the interpreter]--> port calls
//!     sched::Device: timers and events in order, handlers to completion,
//!                    ports onto a Hal (pins, clock) at their instants
//! ```
//!
//! - A module is verified before anything runs: every index, type and region
//!   access, every loop's cap, every port's arity, every declared pin, and
//!   the worst case of every handler against the chip's [`Profile`].
//! - Handlers run to completion, one at a time (cooperative). Between handlers
//!   the device is plain data (state words, timers, pin registers).
//! - The interpreter is held bit-identical to the compute core's reference
//!   (`ir::run`); it allocates nothing while it runs: a handler's registers,
//!   frame and control stack live in the caller's scratch words.
//!
//! - [`watch`] is the wire protocol a device streams to the host (pin activity
//!   with its source site, faults, lost counts), with no allocation.
//! - [`bundle`] packs firmware files and reads them in place from flash images.
//!
//! `no_std` with `alloc` (a module's tables are decoded once at load).
#![no_std]

extern crate alloc;

pub mod analog;
pub mod bundle;
mod display;
pub mod exec;
pub mod fmath;
pub mod format;
pub mod kernel;
pub mod module;
pub mod net;
pub mod ports;
pub mod pwm;
pub mod sched;
pub mod serial;
pub mod watch;

/// The entry that runs once at boot, before any event.
pub const SETUP: &str = "setup";

/// Interrupt policy supplied by a kernel image's board, not by its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelLines {
    pub protected: u32,
    pub raise: u32,
}

/// The board's panel dimensions; the loader requires non-zero, even sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayProfile {
    pub width: u32,
    pub height: u32,
}

/// Raw motion counts exposed by the board's configured full scales.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionProfile {
    pub accel_lsb_per_g: u32,
    pub gyro_lsb_per_dps: u32,
}

/// Sample rates the board can clock exactly, and whether it has a capture path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioProfile {
    pub rates: &'static [u32],
    pub mic: bool,
}

/// What a chip allows a module: the verifier checks every module against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// The chip profile a module is compiled for (0 in a module: any).
    pub id: u32,
    /// Pin numbers are `0..pins`.
    pub pins: u32,
    /// Worst-case ops of one handler run.
    pub max_entry_ops: u64,
    /// Keep helpers above this cost as shared functions; None inlines them.
    pub call_ops: Option<u64>,
    /// Words of RAM for the module: its state plus the largest handler's
    /// scratch plus serial receive buffers (one word per byte), and the
    /// range-fault bitset when buffers are declared (one bit per port site).
    pub ram_words: u32,
    /// RAM bytes reserved for writable buffers, each rounded up to whole words.
    /// Read-only buffers live with the module (in flash on a chip).
    pub buffer_bytes: u32,
    /// Bytes of EEPROM; zero means EEPROM ports are unavailable.
    pub eeprom: u32,
    /// Time one EEPROM access can wait for a write in flight, in nanoseconds.
    /// In a module containing a write, every EEPROM access is charged this
    /// time rounded up to whole ops; read-only modules keep one op per read.
    /// Zero means no wait. The charged time (ops × op_ns) must cover the
    /// slowest real write, and the emulator must never take longer than that
    /// charged time. The emulator may use a shorter nominal programming time.
    pub eeprom_write_ns: u32,
    /// f64 values allowed.
    pub f64: bool,
    /// Nominal nanoseconds per interpreted op (the simulator's handler
    /// timing; hardware-timed things are exact).
    pub op_ns: u32,
    /// The CPU clock; PWM periods are cycles of it.
    pub clock_hz: u32,
    /// The chip's PWM pins and timer settings.
    pub pwm: &'static pwm::PwmTable,
    /// The chip's analog inputs.
    pub analog: &'static analog::AnalogTable,
    /// The chip's serial links and receiver settings.
    pub serial: &'static serial::SerialTable,
    /// The board's panel, when present.
    pub display: Option<DisplayProfile>,
    pub motion: Option<MotionProfile>,
    /// Whether the board supplies touch reports in display coordinates.
    pub touch: bool,
    pub audio: Option<AudioProfile>,
}

impl Profile {
    /// Cost of an EEPROM access in a module that writes. A positive wait
    /// cannot fit a zero-duration op: saturate so the cost limit refuses it.
    pub fn eeprom_access_ops(&self) -> u64 {
        if self.eeprom_write_ns == 0 { 1 }
        else if self.op_ns == 0 { u64::MAX }
        else { u64::from(self.eeprom_write_ns).div_ceil(u64::from(self.op_ns)) }
    }
}
