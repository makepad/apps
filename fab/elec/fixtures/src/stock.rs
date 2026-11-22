//! Stock boards: designs Fab ships, which a case script reads by name (`board("ledctl")`) without the person
//! designing them. Each is built when a script first reads it; the edited design's boards come first.

use fab_elec_design::fixture::{devboard, sbc};
use fab_elec_design::options::{Field, Refused};
use fab_elec_design::Design;

/// A stock board: `build` makes a design whose board unit is named `name`.
pub struct StockBoard {
    pub name: &'static str,
    pub build: fn() -> Design,
    /// The board built over a case's override record, as its leaves (dotted key, value), for a
    /// board whose measurements a script sets (`board("devboard", {usb: {height: 7.9mm}})`);
    /// None: the board takes no record.
    pub with: Option<fn(&[(String, Field)]) -> Result<Design, Refused>>,
}

/// Every stock board, by name (sorted).
pub fn boards() -> &'static [StockBoard] {
    &[
        StockBoard { name: "devboard", build: devboard, with: Some(devboard::devboard_over) },
        StockBoard { name: "ledctl", build: crate::ledctl, with: None },
        StockBoard { name: "sbc", build: sbc, with: None },
    ]
}

/// The stock board named `name`.
pub fn board(name: &str) -> Option<&'static StockBoard> {
    boards().iter().find(|entry| entry.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_names_are_sorted_unique_and_name_boards() {
        for pair in boards().windows(2) {
            assert!(pair[0].name < pair[1].name, "stock board '{}' must sort before '{}': names must be sorted and unique", pair[0].name, pair[1].name);
        }
        for entry in boards() {
            let design = (entry.build)();
            assert!(design.units.iter().any(|unit| unit.name == entry.name && unit.board.is_some()), "stock board '{}' must build a board unit with that name", entry.name);
        }
    }
}
