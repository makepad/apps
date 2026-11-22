//! Board parsing and construction, exposed as separate stages.

pub mod build;
pub mod file;
mod parse;

pub use build::{build_board, PcbRead};
pub use file::*;
pub use parse::parse_kicad_pcb;
