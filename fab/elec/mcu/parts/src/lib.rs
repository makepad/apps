//! Chip-independent, rewindable board wiring and electronic parts.

mod co5300;
mod cst820;
mod es8311;
mod factory;
mod i2c;
pub mod motion;
mod netlist;
mod qmi8658c;
pub mod screen;
mod simple;
mod tca9554;

pub use co5300::Co5300;
pub use cst820::Cst820;
pub use es8311::Es8311;
pub use factory::model;
pub use i2c::{I2cTarget, TargetOp};
pub use motion::{MotionSample, MotionTrack, SoundTrack, Track, TrackRead, TrackRef, TrackSample};
pub use netlist::Netlist;
pub use qmi8658c::Qmi8658c;
pub use simple::{Amp, Key, Not};
pub use tca9554::Tca9554;

/// Screen readers announced once per newly made device, never shared with a remake.
pub struct BoardPorts {
    pub screens: Vec<(u16, screen::ScreenReader)>,
}

use std::sync::Arc;

use fab_elec_mcu_emu::board::{BusEvent, I2cAns, I2cOp, I2sFrame, Payload, Res, SpiXfer};
use fab_elec_mcu_emu::{Drive, Pull};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardDesc {
    /// Reproducible power-on RAM junk for parts whose initial contents are random.
    pub seed: u64,
    pub parts: Vec<PartDesc>,
    pub nets: Vec<NetDesc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartDesc {
    pub refdes: Arc<str>,
    pub kind: PartKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    /// Model key CO5300: glass_x, glass_y, glass_w, glass_h (integer memory texels).
    /// Memory is fixed at 480x480 (P19); glass is [x, y, width, height].
    Co5300 { glass: [u16; 4] },
    /// Model key TCA9554: addr (integer seven-bit address, 0x20..=0x27).
    Tca9554 { addr: u8 },
    /// Model key QMI8658C: addr (integer seven-bit address, 0x6a or 0x6b).
    Qmi8658c { addr: u8 },
    /// Model key CST820: addr (integer seven-bit board address; T01 gives no strap restriction).
    Cst820 { addr: u8 },
    /// Model key ES8311: addr is the CE-strapped seven-bit address (es8311-datasheet.txt:303-307).
    Es8311 { addr: u8 },
    /// Model key AMP: enable (the EN level that turns it on, 0 or 1), gain_db (integer dB,
    /// -40..=40) and start_ms (its start time after EN enables it, 0..=1000): a speaker
    /// amplifier on a codec's output (`Amp`).
    Amp { enable_high: bool, gain_db: i8, start_ms: u16 },
    Key,
    Not,
}

impl PartKind {
    /// The model's pin names in the order the netlist binds; supply pins are not listed.
    pub fn pins(&self) -> &'static [&'static str] {
        match self {
            Self::Co5300 { .. } => Co5300::PINS,
            Self::Tca9554 { .. } => Tca9554::PINS,
            Self::Qmi8658c { .. } => Qmi8658c::PINS,
            Self::Cst820 { .. } => Cst820::PINS,
            Self::Es8311 { .. } => Es8311::PINS,
            Self::Amp { .. } => Amp::PINS,
            Self::Key => &["A"],
            Self::Not => &["G", "D"],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetDesc {
    pub name: Arc<str>,
    pub pull: Pull,
    /// Time from 0 V to the net's input threshold through its pull-up, in
    /// picoseconds, rounded up. None when the pull is not up or its resistance,
    /// capacitance or threshold is unknown.
    pub rise_ps: Option<u64>,
    pub pins: Vec<PinRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinRef {
    Chip(Arc<str>),
    Part { part: u16, pin: Arc<str> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stimulus {
    Key { part: u16, down: bool },
    /// Sensor-axis acceleration in micro-g and angular rate in milli-degrees/s, from now on. The
    /// newest input wins: it ends a track the sensor read (`Track`).
    Motion { part: u16, accel_ug: [i32; 3], gyro_mdps: [i32; 3] },
    /// Touch-controller part index and visible-glass pixels, origin top-left,
    /// axes aligned with the panel's display coordinates (Assumed T04-T07/T15/O04).
    /// The touch IC does not apply the display controller's memory offset.
    /// A move is another down at new coordinates; release is down=false.
    Touch { part: u16, x: u16, y: u16, down: bool },
    /// A motion sensor's motion from now on: the track's samples at its instants (`MotionTrack`,
    /// on the device's board clock). It replaces a track the sensor read before; a read before
    /// the new track's origin reads the motion the sensor holds apart from any track (a motion
    /// stimulus's, or at rest face up).
    Track { part: u16, track: TrackRef },
    /// The codec reads this mono recording from the next frame's first edge.
    Sound { part: u16, track: TrackRef<i16> },
}

/// A part's tick and record sink for one call.
pub struct Cx<'a> {
    pub t: u64,
    bus: &'a mut Vec<BusEvent>,
    detail: &'a mut Option<String>,
    initializing: bool,
}

impl Cx<'_> {
    pub fn note(&mut self, what: &'static str) {
        self.bus.push(BusEvent::Note { t: self.t, what });
    }

    pub fn refuse(&mut self, what: &'static str, detail: String) -> &'static str {
        *self.detail = Some(detail);
        what
    }
}

/// The electrical and protocol behavior of a part.
pub trait Model {
    fn pins(&self) -> &'static [&'static str];
    fn drive(&self, pin: u8) -> Drive;
    fn pin(&mut self, cx: &mut Cx<'_>, pin: u8, high: bool) -> Res;
    fn next_event(&self) -> Option<u64>;
    fn event(&mut self, cx: &mut Cx<'_>) -> Res;

    fn i2c_addr(&self) -> Option<u8> {
        None
    }

    fn i2c(&mut self, cx: &mut Cx<'_>, op: I2cOp) -> Res<I2cAns>;
    fn i2s(&mut self, _cx: &mut Cx<'_>, _frame: &I2sFrame) -> Res<[i32; 2]> {
        Err("the part on the I2S pads is not an I2S device")
    }
    fn spi(&mut self, cx: &mut Cx<'_>, x: &SpiXfer, tx: &dyn Payload, rx: &mut Vec<u8>) -> Res;
    fn input(&mut self, cx: &mut Cx<'_>, ev: &Stimulus) -> Res;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Part {
    Co5300(Co5300),
    Tca9554(Tca9554),
    Qmi8658c(Qmi8658c),
    Cst820(Cst820),
    Es8311(Es8311),
    Amp(Amp),
    Key(Key),
    Not(Not),
}

impl Clone for Part {
    fn clone(&self) -> Self {
        match self {
            Self::Co5300(part) => Self::Co5300(part.clone()),
            Self::Tca9554(part) => Self::Tca9554(part.clone()),
            Self::Qmi8658c(part) => Self::Qmi8658c(part.clone()),
            Self::Cst820(part) => Self::Cst820(part.clone()),
            Self::Es8311(part) => Self::Es8311(part.clone()),
            Self::Amp(part) => Self::Amp(*part),
            Self::Key(part) => Self::Key(*part),
            Self::Not(part) => Self::Not(*part),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        match (self, source) {
            (Self::Co5300(part), Self::Co5300(source)) => part.clone_from(source),
            (Self::Tca9554(part), Self::Tca9554(source)) => part.clone_from(source),
            (Self::Qmi8658c(part), Self::Qmi8658c(source)) => part.clone_from(source),
            (Self::Cst820(part), Self::Cst820(source)) => part.clone_from(source),
            (Self::Es8311(part), Self::Es8311(source)) => part.clone_from(source),
            (part, source) => *part = source.clone(),
        }
    }
}

impl Part {
    fn bind(&mut self, tick_hz: u64) -> Result<(), String> {
        match self {
            Self::Co5300(part) => part.bind(tick_hz),
            Self::Tca9554(part) => part.bind(tick_hz),
            Self::Qmi8658c(part) => part.bind(tick_hz),
            Self::Cst820(part) => part.bind(tick_hz),
            Self::Es8311(part) => part.bind(tick_hz),
            Self::Amp(part) => part.bind(tick_hz),
            Self::Key(_) | Self::Not(_) => Ok(()),
        }
    }

    fn mark(&mut self) -> PartState {
        match self {
            Self::Co5300(part) => PartState::Co5300(part.mark()),
            Self::Tca9554(part) => PartState::Tca9554(part.clone()),
            Self::Qmi8658c(part) => PartState::Qmi8658c(part.clone()),
            Self::Cst820(part) => PartState::Cst820(part.clone()),
            Self::Es8311(part) => PartState::Es8311(part.mark()),
            Self::Amp(part) => PartState::Amp(*part),
            Self::Key(part) => PartState::Key(*part),
            Self::Not(part) => PartState::Not(*part),
        }
    }

    fn rewind(&mut self, saved: &PartState) {
        match (self, saved) {
            (Self::Co5300(part), PartState::Co5300(saved)) => part.rewind(*saved),
            (Self::Tca9554(part), PartState::Tca9554(saved)) => part.clone_from(saved),
            (Self::Qmi8658c(part), PartState::Qmi8658c(saved)) => part.clone_from(saved),
            (Self::Cst820(part), PartState::Cst820(saved)) => part.clone_from(saved),
            (Self::Es8311(part), PartState::Es8311(saved)) => part.rewind(saved),
            (Self::Amp(part), PartState::Amp(saved)) => *part = *saved,
            (Self::Key(part), PartState::Key(saved)) => *part = *saved,
            (Self::Not(part), PartState::Not(saved)) => *part = *saved,
            _ => unreachable!("a board mark retains its part kinds"),
        }
    }

    fn barrier(&self) -> bool {
        matches!(self, Self::Co5300(part) if part.barrier())
    }
}

/// Panel pixels keep their rewind history in the part.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PartState {
    Co5300(co5300::State),
    Tca9554(Tca9554),
    Qmi8658c(Qmi8658c),
    Cst820(Cst820),
    Es8311(es8311::Control),
    Amp(Amp),
    Key(Key),
    Not(Not),
}

macro_rules! dispatch {
    ($self:ident, $method:ident $(, $arg:expr)*) => {
        match $self {
            Part::Co5300(part) => part.$method($($arg),*),
            Part::Tca9554(part) => part.$method($($arg),*),
            Part::Qmi8658c(part) => part.$method($($arg),*),
            Part::Cst820(part) => part.$method($($arg),*),
            Part::Es8311(part) => part.$method($($arg),*),
            Part::Amp(part) => part.$method($($arg),*),
            Part::Key(part) => part.$method($($arg),*),
            Part::Not(part) => part.$method($($arg),*),
        }
    };
}

impl Model for Part {
    fn pins(&self) -> &'static [&'static str] {
        dispatch!(self, pins)
    }

    fn drive(&self, pin: u8) -> Drive {
        dispatch!(self, drive, pin)
    }

    fn pin(&mut self, cx: &mut Cx<'_>, pin: u8, high: bool) -> Res {
        dispatch!(self, pin, cx, pin, high)
    }

    fn next_event(&self) -> Option<u64> {
        dispatch!(self, next_event)
    }

    fn event(&mut self, cx: &mut Cx<'_>) -> Res {
        dispatch!(self, event, cx)
    }

    fn i2c_addr(&self) -> Option<u8> {
        dispatch!(self, i2c_addr)
    }

    fn i2c(&mut self, cx: &mut Cx<'_>, op: I2cOp) -> Res<I2cAns> {
        dispatch!(self, i2c, cx, op)
    }

    fn i2s(&mut self, cx: &mut Cx<'_>, frame: &I2sFrame) -> Res<[i32; 2]> {
        dispatch!(self, i2s, cx, frame)
    }

    fn input(&mut self, cx: &mut Cx<'_>, ev: &Stimulus) -> Res {
        dispatch!(self, input, cx, ev)
    }

    fn spi(&mut self, cx: &mut Cx<'_>, x: &SpiXfer, tx: &dyn Payload, rx: &mut Vec<u8>) -> Res {
        dispatch!(self, spi, cx, x, tx, rx)
    }
}
