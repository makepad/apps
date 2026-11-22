//! A pour-only pad whose narrow escape can be cut by a signal route.

use fab_elec_board::{
    Area, Board, BoardNet, Origin, Placed, RuleSet, StackKind, Zone, ZoneSettings,
};
use fab_elec_circuit::{ClassRules, NetClass, ViaSize};
use fab_elec_core::{mm, Angle, BoardId, ClassId, CompId, Contour, LayerId, LayerSet, NetId, Side, P};
use fab_elec_design::fixture::fp;
use fab_elec_parts::{Footprint, FpAttrs, FpMount, PadShape, ZoneConnect};
use std::sync::Arc;

/// One copper layer with a GND pour, a pad inside a U-shaped fence and a crossing signal.
/// Returns the board and its GND, signal and fence nets, in that order.
pub fn pour_neck() -> (Board, NetId, NetId, NetId) {
    let (gnd, signal, fence) = (NetId(0), NetId(1), NetId(2));
    let mut board = Board::new(BoardId(0), "pour neck");
    board.outline = vec![Contour::new(fp::rect_path(P::ZERO, P::mm(8.1, 8.1)))];
    board.stackup.layers.retain(|layer| matches!(layer.kind, StackKind::Copper(LayerId::F_CU)));
    board.rules.base = RuleSet {
        track: Some(mm(0.25)),
        clearance: Some(mm(0.20)),
        via: Some(ViaSize { drill: mm(0.40), pad: mm(0.80) }),
        ..RuleSet::standard()
    };
    board.classes.push(NetClass {
        id: ClassId(0),
        name: "Default".into(),
        rules: ClassRules {
            track: board.rules.base.track,
            clearance: board.rules.base.clearance,
            via: board.rules.base.via,
            ..Default::default()
        },
    });
    for name in ["GND", "S", "X"] {
        board.nets.push(BoardNet { name: name.into(), class: ClassId(0) });
    }
    // X is a solid U: inner sides x=3.50/4.60, bottom y=3.50, mouth y=4.70.
    // P's 0.60 mm pad has a 0.70 mm pour pocket after clearance, too small for a
    // 0.80 mm via. The sole exit is upward; the grid still has access inside P.
    for (refdes, at, size, net) in [
        ("P", P::mm(4.05, 4.05), P::mm(0.6, 0.6), gnd),
        ("G", P::mm(4.05, 7.05), P::mm(0.6, 0.6), gnd),
        ("XL", P::mm(3.30, 3.90), P::mm(0.4, 1.6), fence),
        ("XR", P::mm(4.80, 3.90), P::mm(0.4, 1.6), fence),
        ("XB", P::mm(4.05, 3.30), P::mm(1.9, 0.4), fence),
        ("S1", P::mm(1.35, 5.25), P::mm(0.6, 0.6), signal),
        ("S2", P::mm(6.75, 5.25), P::mm(0.6, 0.6), signal),
    ] {
        let mut pad = fp::smd("1", P::ZERO, size);
        pad.stack.front = PadShape::Rect { size };
        let id = board.alloc_id();
        board.placed.push(Placed {
            id,
            comp: CompId(board.placed.len() as u32),
            path: refdes.into(),
            refdes: refdes.into(),
            footprint: Arc::new(Footprint {
                name: refdes.into(),
                pads: vec![pad],
                attrs: FpAttrs { mount: FpMount::Smd, ..Default::default() },
                ..Default::default()
            }),
            pad_nets: vec![Some(net)],
            at,
            rot: Angle::ZERO,
            side: Side::Top,
            locked: true,
            origin: Origin::Authored,
            body_z: None,
            value: None,
            ref_label: None,
            value_label: None, legend: None,
        });
    }
    let id = board.alloc_id();
    board.zones.push(Zone {
        id,
        net: Some(gnd),
        name: "GND".into(),
        layers: LayerSet::EMPTY.with(LayerId::F_CU),
        area: Area::Board,
        priority: 0,
        settings: ZoneSettings { connect: ZoneConnect::Solid, ..ZoneSettings::default() },
        locked: false,
        origin: Origin::Authored,
    });
    board.reindex();
    (board, gnd, signal, fence)
}
