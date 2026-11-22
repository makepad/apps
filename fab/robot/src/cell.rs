//! A cell's layout: one or two arms with their heads, the positioner under the part if there is
//! one, the fixtures holding pieces, and what the hardware costs.

use crate::arm::*;
use crate::math::*;

/// Two axes under the part: a tilt about the base's x axis, then a turn about the table's z.
/// It orients the part for the arm (a second arm's job in a two-arm cell), but holds and braces
/// nothing: pieces stay put by [`Fixture`]s or by tacking them first.
#[derive(Clone, Debug, PartialEq)]
pub struct Positioner {
    pub name: String,
    /// The cell frame to the positioner's base: the tilt axis runs along its x through its
    /// origin.
    pub base: Pose,
    /// The tilted frame to the table's top centre, about whose z the turn goes.
    pub table: Pose,
    /// The tilt's range (rad); the turn is endless.
    pub tilt: [f64; 2],
    /// Fastest speed and acceleration per axis (rad/s, rad/s²).
    pub vmax: [f64; 2],
    pub accel: [f64; 2],
    /// The most it carries (kg).
    pub payload: f64,
    /// Each axis' position error (one sigma, rad; estimate).
    pub sigma: f64,
    /// How its servo follows a setpoint: a first-order lag (s; estimate).
    pub lag: f64,
    /// How long a setpoint takes to reach it (s; estimate).
    pub latency: f64,
    /// USD, estimate.
    pub cost: f64,
}

impl Positioner {
    /// The cell frame to the table's top at axes `q` (tilt, turn).
    pub fn table_frame(&self, q: [f64; 2]) -> Pose {
        self.base.mul(&Pose::new(rot_x(q[0]), ZERO)).mul(&self.table).mul(&Pose::new(rot_z(q[1]), ZERO))
    }
}

/// A rigid hold on a piece (a clamp, a locating block): a box in the part frame the part may
/// not move in and the tools must keep clear of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fixture {
    pub centre: V3,
    pub half: V3,
}

/// What the part is built on, in the part frame: a plate (a bed, a table) `half` its size in x
/// and y about the frame's z axis and `thick` deep, the part on its top (z = 0) or on a stub
/// standing on it. Past the plate's edge there is nothing: an arm standing beside it reaches
/// round and under its edge, and a beam leaving it runs on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Substrate {
    pub half: [f64; 2],
    pub thick: f64,
    pub stub: Option<Stub>,
}

/// A round stub of `radius` standing `height` on the plate, the part on its top: the part
/// overhangs it, so a beam leaving the part's lower faces runs on past the stub instead of
/// into the plate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stub {
    pub radius: f64,
    pub height: f64,
}

impl Substrate {
    /// A plate, the part on its top.
    pub fn plate(half: [f64; 2], thick: f64) -> Substrate {
        Substrate { half, thick, stub: None }
    }

    /// The part on a stub standing on the plate instead.
    pub fn on_stub(self, radius: f64, height: f64) -> Substrate {
        Substrate { stub: Some(Stub { radius, height }), ..self }
    }

    /// The plate's top (z).
    pub fn plate_top(&self) -> f64 {
        -self.stub.map_or(0.0, |s| s.height)
    }

    /// Its signed distance.
    pub fn distance(&self, p: V3) -> f64 {
        let mid = self.plate_top() - 0.5 * self.thick;
        let o = [p[0].abs() - self.half[0], p[1].abs() - self.half[1], (p[2] - mid).abs() - 0.5 * self.thick];
        let plate = len(o.map(|v| v.max(0.0))) + o[0].max(o[1]).max(o[2]).min(0.0);
        match self.stub {
            None => plate,
            Some(Stub { radius, height }) => {
                let r = p[0].hypot(p[1]) - radius;
                let up = (p[2] + 0.5 * height).abs() - 0.5 * height;
                let stub = r.max(0.0).hypot(up.max(0.0)) + r.max(up).min(0.0);
                stub.min(plate)
            }
        }
    }

    /// Its top under (x, y), over the plate (where the part is).
    pub fn top(&self, x: f64, y: f64) -> f64 {
        match self.stub {
            Some(s) if x * x + y * y <= s.radius * s.radius => 0.0,
            _ => self.plate_top(),
        }
    }
}

/// A whole cell to build with.
#[derive(Clone, Debug)]
pub struct CellSetup {
    pub label: String,
    pub arms: Vec<ArmModel>,
    pub positioner: Option<Positioner>,
    pub fixtures: Vec<Fixture>,
    /// The part frame on the table (with a positioner) or in the cell (without).
    pub plate: Pose,
    pub substrate: Substrate,
    /// What the rest costs beyond the arms, heads and positioner: sources, beam switch,
    /// trackers, enclosure (USD, estimate).
    pub other_cost: f64,
    /// The arms' own prices (USD, estimate), one per arm.
    pub arm_cost: Vec<f64>,
}

impl CellSetup {
    /// The part frame in the cell frame with the positioner (if any) at axes `q` (tilt, turn).
    pub fn part_frame(&self, q: [f64; 2]) -> Pose {
        match &self.positioner {
            Some(p) => p.table_frame(q).mul(&self.plate),
            None => self.plate,
        }
    }

    /// The hardware's rough price (USD, estimate): arms, heads, positioner and the rest.
    pub fn cost(&self) -> f64 {
        let heads: f64 = self.arms.iter().flat_map(|a| a.heads.iter()).map(|h| h.cost).sum();
        self.arm_cost.iter().sum::<f64>() + heads + self.positioner.as_ref().map_or(0.0, |p| p.cost) + self.other_cost
    }
}
