//! The controller and its Rust probe, mated into a two-board assembly.

use crate::ledctl;
use fab_elec_core::{mm, BoardId, CompRef, LinkId, Xform3};
use fab_elec_design::fixture::probe::probe;
use fab_elec_design::{Design, Frame, Link, LinkEnd, LinkKind, Mount, PinMap};

/// The controller with a probe mated to its debug header.
pub fn ledctl_pair() -> Design {
    let mut design = ledctl();
    let dbg = design.units[0].circuit.comp("mcu.dbg").expect("fixture has debug header").id;
    let probe = probe();
    let j = probe.circuit.comp("j").expect("fixture has probe header").id;
    design.units.push(probe);
    design.links.push(Link {
        id: LinkId(0),
        kind: LinkKind::Mate,
        a: LinkEnd::Comp(CompRef { board: BoardId(0), comp: dbg }),
        b: LinkEnd::Comp(CompRef { board: BoardId(1), comp: j }),
        pinmap: PinMap::Derived,
    });
    design.assembly.mounts.push(Mount {
        unit: BoardId(1),
        parent: Frame::Unit(BoardId(0)),
        xform: Xform3::translation([mm(10.0), 0, mm(-8.0)]),
    });
    design
}
