//! What gates motion and the laser, as types every mode shares.
//!
//! - [`Mode`]: a simulation, a dry run (real arms and sensors, process disabled; or real sensors
//!   over a simulated plant) or a real build.
//! - [`Interlocks`]: the inputs: emergency stop, door, laser key, shielding gas, extraction.
//! - [`Limits`]: speed and force limits and the workspace fence.
//! - [`Watchdog`]: a lost heartbeat or a stale sensor stops the cell in a controlled way.
//! - [`Gate`]: arming. Motion needs the interlocks, an operator's confirmation, a successful
//!   simulation of the same plan (outside a simulation) and a real-time path that is compiled
//!   ahead (Real and dry-run modes, see [`RtReady`]); the laser needs motion plus its own chain.

use crate::math::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Sim,
    DryRun,
    Real,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Sim => "Sim",
            Mode::DryRun => "Dry run",
            Mode::Real => "Real",
        }
    }
}

/// The safety inputs, as the cell reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interlocks {
    pub estop: bool,
    pub door_closed: bool,
    pub laser_key: bool,
    pub gas_flow: bool,
    pub extraction: bool,
}

impl Interlocks {
    /// A simulation's: everything in order.
    pub const SIM: Interlocks = Interlocks { estop: false, door_closed: true, laser_key: true, gas_flow: true, extraction: true };
}

/// Why the cell stopped (or refuses to start).
#[derive(Clone, Debug, PartialEq)]
pub enum StopKind {
    EStop,
    Interlock(&'static str),
    Heartbeat,
    StaleSensor(&'static str),
    Fence { arm: u8 },
    Speed { arm: u8 },
    Force { arm: u8 },
    Fault(String),
    Operator,
    /// The build is done.
    Finished,
}

/// The cell's limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// Fastest TCP speed (mm/s).
    pub tcp_speed: f64,
    /// Largest process force on a tool (crate force units).
    pub force: f64,
    /// The box every TCP stays inside (cell frame, mm).
    pub fence_lo: V3,
    pub fence_hi: V3,
}

impl Limits {
    pub fn inside(&self, p: V3) -> bool {
        (0..3).all(|i| p[i] >= self.fence_lo[i] && p[i] <= self.fence_hi[i])
    }
}

/// Heartbeat and sensor freshness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Watchdog {
    /// The planner's heartbeat may be at most this old (s).
    pub heartbeat: f64,
    /// A sensor the loop needs may be at most this stale (s).
    pub sensor: f64,
}

impl Default for Watchdog {
    fn default() -> Self {
        Watchdog { heartbeat: 0.05, sensor: 0.1 }
    }
}

impl Watchdog {
    /// Why to stop at `now`, given when the heartbeat and each needed sensor last arrived.
    pub fn check(&self, now: f64, heartbeat: f64, sensors: &[(&'static str, f64)]) -> Option<StopKind> {
        if now - heartbeat > self.heartbeat {
            return Some(StopKind::Heartbeat);
        }
        sensors.iter().find(|(_, t)| now - t > self.sensor).map(|(n, _)| StopKind::StaleSensor(n))
    }
}

/// Whether the real-time path (control and estimation loops) is compiled ahead of motion.
/// Native builds are; under the JIT the loop's owner pre-compiles that path and reports here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtReady {
    Ready,
    /// Something on the path would still compile lazily.
    Lazy,
    Unknown,
}

/// What arming asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct Gate {
    pub mode: Mode,
    pub interlocks: Interlocks,
    /// The operator confirmed arming.
    pub confirmed: bool,
    /// The plan's hash that last ran to the end in a simulation (Real and dry run need it).
    pub simulated: Option<u64>,
    pub rt: RtReady,
}

impl Gate {
    /// Motion for plan `plan`: Ok, or why not.
    pub fn motion(&self, plan: u64) -> Result<(), String> {
        if self.interlocks.estop {
            return Err("Emergency stop pressed".into());
        }
        if !self.interlocks.door_closed && self.mode == Mode::Real {
            return Err("Cell door open".into());
        }
        if self.mode == Mode::Sim {
            return Ok(());
        }
        if !self.confirmed {
            return Err("The operator has not confirmed arming".into());
        }
        if self.simulated != Some(plan) {
            return Err("Run this plan in the simulator first".into());
        }
        if self.rt != RtReady::Ready {
            return Err("The real-time path is not compiled ahead".into());
        }
        Ok(())
    }

    /// The laser for plan `plan`, while the arm moves.
    pub fn laser(&self, plan: u64, moving: bool) -> Result<(), String> {
        self.motion(plan)?;
        if self.mode == Mode::DryRun {
            return Err("Dry run: the laser stays off".into());
        }
        let i = &self.interlocks;
        for (ok, why) in [(i.laser_key, "Laser key off"), (i.door_closed, "Cell door open"), (i.gas_flow, "No shielding gas"), (i.extraction, "No fume extraction")] {
            if !ok {
                return Err(why.into());
            }
        }
        if !moving {
            return Err("The laser fires only while the tool moves".into());
        }
        Ok(())
    }
}
