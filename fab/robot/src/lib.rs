//! Fab's arm cells, shared by the planner and the simulator: the arm models (kinematic chain,
//! link inertias, elastic drives, controller, tools), their kinematics, dynamics and clearance,
//! the as-built field, the boundary to a cell ([`io::CellLink`]: commands out, observations in)
//! and the safety types that gate motion and the laser.
//!
//! Units throughout: mm, kg, s, rad.

pub mod arm;
pub mod cell;
pub mod checks;
pub mod collide;
pub mod dynamics;
pub mod field;
pub mod io;
pub mod kin;
pub mod math;
pub mod presets;
pub mod process;
pub mod safety;

#[cfg(test)]
mod tests;
