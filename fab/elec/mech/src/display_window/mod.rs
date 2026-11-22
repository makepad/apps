//! A clear printed or laser-cut window over a display or LED ring, with a viewport,
//! printed segments, a centre hole and an adhesive ring.

pub mod build;
pub mod generate;
pub mod laser;
pub mod params;
pub mod screen;
pub use build::{build, layout, outline_poly};
pub use laser::{laser_svg, laser_dxf};
pub use params::*;

#[cfg(test)]
mod selftest;
