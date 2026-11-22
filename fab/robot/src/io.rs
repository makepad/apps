//! The boundary between the planner and a cell: commands out, observations in, nothing else.
//! The virtual cell implements [`CellLink`]; a real cell will implement it by composing one
//! driver per device class ([`ArmDriver`], [`TrackerDriver`], [`ImuDriver`], [`ScannerDriver`],
//! [`CameraDriver`], [`LaserDriver`], [`CutterDriver`], [`ExtruderDriver`]); a dry run composes real arms and
//! sensors with the process drivers disabled, or real sensors over a simulated plant.
//!
//! Time is the cell's clock in seconds ([`Stamp`]): simulated time in a simulation, the
//! hardware's clock (synced to the host, [`ClockSync`]) on a real cell. Every observation
//! carries the stamp it was captured at; it arrives later by the sensor's latency, and the
//! planner compensates by that stamp, never by when it arrived.

use crate::arm::*;
use crate::cell::{Positioner, Substrate};
use crate::math::*;
use crate::safety::{Interlocks, Mode, StopKind};

/// A time on the cell's clock (s).
pub type Stamp = f64;

/// One streamed setpoint for an arm's drives (impedance style: motor angle and speed to hold,
/// a feed-forward torque and the gains, link side).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointCmd {
    pub th: Q,
    pub thd: Q,
    pub tau: Q,
    pub kp: Q,
    pub kd: Q,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// A drive setpoint, valid from `at` until the next.
    Joints { arm: ArmId, at: Stamp, cmd: JointCmd },
    /// Laser power (W; 0 off).
    Laser { arm: ArmId, at: Stamp, watts: f64 },
    /// Wire feed (mm/s; 0 stopped).
    Wire { arm: ArmId, at: Stamp, mm_s: f64 },
    /// An extruder's melt flow (mm³/s; 0 stopped).
    Extrude { arm: ArmId, at: Stamp, mm3_s: f64 },
    /// The beam's offset across the tool axis (tool frame x, y; mm), for a head with scanner
    /// mirrors ([`Galvo`]).
    Galvo { arm: ArmId, at: Stamp, offset: [f64; 2] },
    /// Put head `head` of the arm to work (a mode or beam switch; [`Observation::HeadReady`]
    /// says when it is done).
    Head { arm: ArmId, at: Stamp, head: u8 },
    /// The positioner's axes (tilt, turn; rad, rad/s).
    Positioner { at: Stamp, q: [f64; 2], qd: [f64; 2] },
    /// The trimming tool on or off (cutting laser, spindle or ablation source).
    Cutter { arm: ArmId, at: Stamp, on: bool },
    /// The planner is alive (the cell's watchdog stops everything when these stop).
    Heartbeat { at: Stamp },
    /// Stop in a controlled way, process off first.
    Stop { at: Stamp, why: StopKind },
}

/// A camera frame: what a vision model reads (depth, segmentation, pose).
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Row-major RGB, 8 bits per channel (empty for a depth-only frame).
    pub rgb: Vec<u8>,
    /// Row-major depth along the view (mm; 0 where none), if the camera measures it.
    pub depth: Vec<f32>,
    /// Pinhole intrinsics: focal lengths and centre (pixels).
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
}

/// A drive's state as its controller reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveState {
    Disabled,
    Enabled,
    /// Stopped by a fault (following error, overspeed, overtorque, lost setpoints).
    Fault(DriveFault),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveFault {
    Following { joint: u8 },
    Overspeed { joint: u8 },
    Limit { joint: u8 },
    Watchdog,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Observation {
    /// The drives' own readings (motor side, link referred): angle, speed, torque.
    Joints { arm: ArmId, at: Stamp, th: Q, thd: Q, tau: Q },
    /// A tracked target on the tool, in the cell frame: where the TCP really is.
    Tracker { arm: ArmId, at: Stamp, tcp: Pose },
    /// The tool's inertial sensor (its frame): specific force (mm/s², gravity included) and
    /// angular rate.
    Imu { arm: ArmId, at: Stamp, accel: V3, gyro: V3 },
    /// Points a height scanner on the tool measured, in the scanner's frame (mm).
    Profile { arm: ArmId, at: Stamp, points: Vec<V3> },
    /// The melt pool camera's bead width (mm).
    BeadWidth { arm: ArmId, at: Stamp, width: f64 },
    /// A camera frame and where the camera was (its pose as the cell knows it, cell frame).
    Frame { arm: Option<ArmId>, at: Stamp, pose: Pose, image: Image },
    Drive { arm: ArmId, at: Stamp, state: DriveState },
    /// The arm's head `head` is at work.
    HeadReady { arm: ArmId, at: Stamp, head: u8 },
    /// The positioner's encoders (tilt, turn).
    Positioner { at: Stamp, q: [f64; 2] },
    /// A thermocouple on the plate: how far it stands over ambient (K).
    Temperature { at: Stamp, rise: f64 },
    Safety { at: Stamp, interlocks: Interlocks },
}

impl Observation {
    pub fn at(&self) -> Stamp {
        match self {
            Observation::Joints { at, .. }
            | Observation::Tracker { at, .. }
            | Observation::Imu { at, .. }
            | Observation::Profile { at, .. }
            | Observation::BeadWidth { at, .. }
            | Observation::Frame { at, .. }
            | Observation::Drive { at, .. }
            | Observation::HeadReady { at, .. }
            | Observation::Positioner { at, .. }
            | Observation::Temperature { at, .. }
            | Observation::Safety { at, .. } => *at,
        }
    }
}

/// A sensor class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SensorKind {
    Encoders,
    Tracker,
    Imu,
    Scanner,
    MeltPool,
    Camera,
    /// A thermocouple on the plate (K).
    Thermocouple,
}

/// A sensor as the planner is told of it: where it sits and how it reports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SensorInfo {
    pub kind: SensorKind,
    pub arm: Option<ArmId>,
    /// Its frame on the arm's flange (or in the cell, for a fixed sensor).
    pub mount: Pose,
    pub hz: f64,
    /// From capture to arrival (s).
    pub latency: f64,
    /// Its noise (one sigma: mm, or rad, or mm/s² by kind).
    pub sigma: f64,
}

/// What a cell tells the planner about itself: the nominal models (never the truth), its
/// sensors and its mode.
#[derive(Clone, Debug)]
pub struct CellInfo {
    pub mode: Mode,
    /// One arm or two.
    pub arms: Vec<ArmModel>,
    /// The tilt and turn axes under the part, if the cell has them.
    pub positioner: Option<Positioner>,
    pub sensors: Vec<SensorInfo>,
    /// The part frame: the build plate's top centre, z up, in the cell frame (or, with a
    /// positioner, on its table). The design and the as-built field live in this frame.
    pub plate: Pose,
    pub substrate: Substrate,
    /// The cell's gravity vector (mm/s²).
    pub gravity: V3,
}

impl CellInfo {
    pub fn arm(&self, a: ArmId) -> &ArmModel {
        &self.arms[a.index()]
    }

    /// The part frame in the cell frame, the positioner at `axes`.
    pub fn part_frame(&self, axes: [f64; 2]) -> Pose {
        match &self.positioner {
            Some(p) => p.table_frame(axes).mul(&self.plate),
            None => self.plate,
        }
    }
    pub fn sensor(&self, kind: SensorKind, arm: Option<ArmId>) -> Option<&SensorInfo> {
        self.sensors.iter().find(|s| s.kind == kind && s.arm == arm)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LinkError {
    /// The cell refused it (stopped, not armed, out of limits).
    Refused(String),
    /// The command queue is full: retry on a later tick.
    Busy,
}

/// A cell as the planner reaches it.
pub trait CellLink: Send {
    fn info(&self) -> &CellInfo;
    /// The cell's time now.
    fn now(&self) -> Stamp;
    /// Hand a command to the cell (never blocks).
    fn send(&mut self, cmd: Command) -> Result<(), LinkError>;
    /// Let the cell run to `until` (a simulation steps its plant; a real cell gathers what
    /// arrived) and append the observations that have arrived by then.
    fn advance(&mut self, until: Stamp, out: &mut Vec<Observation>);
}

/// One arm's servo interface (a real arm: joint streaming over its bus).
pub trait ArmDriver: Send {
    fn send(&mut self, at: Stamp, cmd: &JointCmd) -> Result<(), LinkError>;
    fn enable(&mut self, on: bool) -> Result<(), LinkError>;
    fn state(&self) -> DriveState;
    /// Readings and state changes since the last poll.
    fn poll(&mut self, out: &mut Vec<Observation>);
}

pub trait TrackerDriver: Send {
    fn poll(&mut self, out: &mut Vec<Observation>);
}

pub trait ImuDriver: Send {
    fn poll(&mut self, out: &mut Vec<Observation>);
}

pub trait ScannerDriver: Send {
    fn poll(&mut self, out: &mut Vec<Observation>);
}

pub trait CameraDriver: Send {
    fn poll(&mut self, out: &mut Vec<Observation>);
}

/// The laser and its wire feed. A real driver sits behind the laser-enable chain
/// ([`crate::safety::Gate::laser`]).
pub trait LaserDriver: Send {
    fn set(&mut self, at: Stamp, watts: f64, wire_mm_s: f64) -> Result<(), LinkError>;
}

pub trait CutterDriver: Send {
    fn set(&mut self, at: Stamp, on: bool) -> Result<(), LinkError>;
}

/// A plastic extruder's drive (filament or screw) and its hot end.
pub trait ExtruderDriver: Send {
    fn set(&mut self, at: Stamp, mm3_s: f64) -> Result<(), LinkError>;
}

/// A device's clock mapped onto the cell's: `cell = device + offset`, kept from pairs of
/// (device stamp, host receive time) by the least-latency pairs (a message never arrives before
/// it was sent, so the smallest apparent delay is the closest to the true offset).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClockSync {
    pub offset: f64,
    /// The pairs taken so far.
    pub pairs: u64,
    /// The smallest apparent delay seen in the current window, and the window's start.
    best: f64,
    window_start: f64,
}

impl ClockSync {
    /// The window over which the least-delay pair is taken (s).
    pub const WINDOW: f64 = 1.0;

    /// One message stamped `device` arrived at host time `host` (cell clock).
    pub fn observe(&mut self, device: Stamp, host: Stamp) {
        let delay = host - device;
        if self.pairs == 0 || host - self.window_start > Self::WINDOW {
            self.window_start = host;
            self.best = delay;
            self.offset = delay;
        } else if delay < self.best {
            self.best = delay;
            self.offset = delay;
        }
        self.pairs += 1;
    }

    pub fn to_cell(&self, device: Stamp) -> Stamp {
        device + self.offset
    }
}
