//! A light pipe carries light from a PCB LED to a tip in a panel hole. A shoulder and tapered
//! body connect the tip to the LED pocket; legs, pegs or panel ribs hold the part in place.
//! The PCB top is z = 0, +z points toward the panel, and the legs lie along x. The mould pulls
//! along z at the shoulder, with pass-through cores forming the snap catches without side actions.

pub mod params;
pub use params::*;
pub mod build;
pub mod derived;
pub use build::build;
pub mod presets;

#[cfg(test)]
mod selftest;
