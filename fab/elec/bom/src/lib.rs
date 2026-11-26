//! Derived, offline BOM and board fabrication figures. The host owns snapshots and file I/O.
pub mod sheet;
pub mod board_facts;
pub mod cost;
pub mod rates;
pub mod prices;

pub use board_facts::BoardFacts;
pub use cost::{estimate_board, Estimate, Figure, Run};
pub use rates::Rates;
pub use sheet::{sheet, BomRow, BomSheet, SheetView};
#[cfg(test)]
mod tests;

pub mod import;
