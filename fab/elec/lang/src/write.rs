//! The board writer (C5): a `Board` as its board document, in canonical form (E1 2.5).
//!
//! Used by importers and by "normalise"; span splices keep a person's formatting otherwise.
//! Canonical form: the header statements (`board`, `stackup`, `outline`, `rules`,
//! waivers, custom rules and checks), then the items
//! in the board's field order (placements, tracks, vias, zones, keepouts, holes, cutouts,
//! graphics, texts, tunings), each kind in its list order, which is the order lowering issues
//! ids in; only items with statements are written (`Origin::Authored`, `Solved` and detached
//! `Block` items, E4 Q7).

use fab_elec_board::Board;
use fab_elec_circuit::Circuit;

/// The board document of `board`, whose circuit is `circuit` (block references are written as
/// instance paths).
pub fn write(board: &Board, circuit: &Circuit) -> String {
    crate::board_doc::write_with(board, circuit, &|_| None)
}
