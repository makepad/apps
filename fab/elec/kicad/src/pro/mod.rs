//! Project design settings, net classes and compiled assignment patterns.
pub mod file;
pub mod parse;
pub mod pattern;
pub use file::*;
pub use parse::parse_kicad_pro;

#[cfg(test)]
mod tests;
