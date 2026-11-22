//! Guards the router's view of per-layer via rings (`Via::flash`, `Board::rings`): a ring a zone of
//! the via's net offers obstructs foreign copper like a ring (the next refill can flash it), and a span
//! layer with neither a ring nor an offer obstructs only with the drill.

use fab_elec_board::{
    check_to, resolve_rules, Area, Board, BoardNet, ConnItem, Copper, Flash, LayerRole, Origin, StackKind, StackLayer,
    Tent, Via, ViaKind, Zone, ZoneSettings,
};
use fab_elec_core::{mm, BoardId, ClassId, ItemId, LayerId, LayerSet, NetId, Shape, P};
use fab_elec_route::{RBase, World};
use std::ops::ControlFlow;

#[test]
fn an_offered_ring_obstructs_and_an_unflashed_layer_has_only_its_drill() {
    let (a, b) = (NetId(0), NetId(1));
    let (in1, in2) = (LayerId::inner(1), LayerId::inner(2));
    let mut board = Board::new(BoardId(0), "rings");
    let cu = |l| StackLayer {
        kind: StackKind::Copper(l), thickness: 35_000, material: "copper".into(), er: None, loss_tan: None,
        role: LayerRole::Signal, velocity: None,
    };
    let at = board.stackup.layers.iter().position(|l| l.kind == StackKind::Copper(LayerId::B_CU)).unwrap();
    board.stackup.layers.insert(at, cu(in2));
    board.stackup.layers.insert(at, cu(in1));
    board.nets = vec![BoardNet { name: "A".into(), class: ClassId(0) }, BoardNet { name: "B".into(), class: ClassId(0) }];
    let via = board.alloc_id();
    board.vias.push(Via {
        id: via, net: a, at: P::ZERO, drill: mm(0.3), pad: mm(1.2), span: (LayerId::F_CU, LayerId::B_CU),
        kind: ViaKind::Through, tent: Tent::Both, backdrill: None, locked: false, origin: Origin::Authored,
        flash: Flash::Connected,
    });
    let zone = board.alloc_id();
    board.zones.push(Zone {
        id: zone, net: Some(a), name: "pour".into(), layers: LayerSet::one(in1), area: Area::Board, priority: 0,
        settings: ZoneSettings::default(), locked: false, origin: Origin::Authored,
    });
    board.reindex();
    assert_eq!(board.rings(&board.vias[0]).offer, LayerSet::one(in1), "the zone offers the ring on In1 only");

    let rules = resolve_rules(&board, None);
    let base = RBase::build(&board);
    let world = World::new(&base);
    // A foreign step 0.45 mm from the via's centre: inside the 0.6 mm ring, clear of the drill's hole
    // clearance (0.15 mm drill radius + 0.25 mm).
    let blocked = |layer: LayerId| {
        let step = Copper {
            item: ConnItem::Track { item: ItemId(u32::MAX - 1), seg: 0 }, net: Some(b), class: ClassId(0), layer,
            shape: Shape::Segment { a: P::mm(-2.0, 0.5), b: P::mm(2.0, 0.5), w: mm(0.1) },
        };
        let mut hits = 0;
        world.query(layer, step.shape.bbox().inflate(mm(1.0)), |_, it| {
            hits += usize::from(check_to(&rules, &step, &it.obstacle(), layer).is_some());
            ControlFlow::Continue(())
        });
        hits
    };
    assert!(blocked(in1) > 0, "on the offered layer the ring obstructs the foreign step");
    assert_eq!(blocked(in2), 0, "on a span layer with no ring and no offer only the drill obstructs, and the step clears it");
}
