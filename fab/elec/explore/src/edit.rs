//! Edit targets and plans (ESCH editing design, Fable answer 11-16): what a gesture or an AI action
//! aims at on the sheet, by ids only. Pure: no Cx, no sheet geometry.

use crate::sheet::TagKind;
use fab_elec_core::{CompId, NetId, PinIx};

/// Which way from a pin's outer end the pointer sits, in world y (rails above, ground below).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Toward { Up, Down, Across }

/// What a gesture or an AI action aims at on the sheet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    None,
    /// The pin's own line (body to outer end) or its end: a new connection from or onto it.
    Pin { comp: CompId, pin: PinIx, toward: Toward },
    /// What is attached outside a connected pin's outer end (its wire's last piece or its tag):
    /// the existing connection of `pin` to `net`.
    WireEnd { comp: CompId, pin: PinIx, net: NetId },
    /// A drawn wire segment of `net`: `run` indexes `SheetLayout::wires`, `seg` the segment
    /// (`run.pts[seg]..run.pts[seg + 1]`); `at` is the point on it, snapped to the grid.
    Wire { net: NetId, run: u32, seg: u32, at: [i32; 2] },
    /// A power, ground or name tag of `net`.
    Tag { net: NetId, kind: TagKind },
    /// Empty canvas in `block` (an index into the circuit's block tree / `SheetLayout::frames`),
    /// at `cell` in that block's local grid (`to_local`).
    Canvas { block: u32, cell: [i32; 2] },
}
