use fab_elec_board::{Board, BoardKeepout, Hole, Item, ItemKind, Origin};
use fab_elec_core::{mm, BBox, BoardId, FeatureRef, ItemId, LayerId, LayerSet, PolySet, P};
use fab_elec_parts::Forbid;
use std::sync::Arc;

#[test]
fn derived_layer_replaces_whole_and_skips_equal_sets() {
    let mut d = fab_elec_fixtures::ledctl();
    let unit = BoardId(0);
    let a = FeatureRef::Generated("case.boss".into());
    let b = FeatureRef::Generated("case.lid".into());
    let authored = |board: &Board| -> Vec<Item> {
        board.placed.iter().cloned().map(Item::Placed)
            .chain(board.tracks.iter().cloned().map(Item::Track))
            .chain(board.vias.iter().cloned().map(Item::Via))
            .chain(board.zones.iter().cloned().map(Item::Zone))
            .chain(board.keepouts.iter().cloned().map(Item::Keepout))
            .chain(board.holes.iter().cloned().map(Item::Hole))
            .chain(board.cutouts.iter().cloned().map(Item::Cutout))
            .chain(board.graphics.iter().cloned().map(Item::Graphic))
            .chain(board.texts.iter().cloned().map(Item::Text))
            .chain(board.tunings.iter().cloned().map(Item::Tuning))
            .filter(|item| !matches!(item.origin(), Origin::Derived { .. }))
            .collect()
    };
    let before = authored(d.unit(unit).expect("the fixture unit exists").board.as_ref().expect("the fixture has a board"));
    let mut first = Hole {
        id: ItemId(900), name: "boss1".into(), at: P::mm(6.0, 6.0), drill: mm(2.0),
        pad: None, net: None, plated: false, origin: Origin::Authored,
    };
    let second = Hole {
        id: ItemId(901), name: "boss2".into(), at: P::mm(14.0, 6.0), drill: mm(2.0),
        pad: None, net: None, plated: false, origin: Origin::Authored,
    };
    let keepout = BoardKeepout {
        id: ItemId(902), area: PolySet::rect(BBox::new(P::mm(4.0, 4.0), P::mm(8.0, 8.0))),
        layers: LayerSet::of(&[LayerId::F_CU, LayerId::B_CU]), forbid: Forbid::ALL,
        reason: "boss clearance".into(), origin: Origin::Authored, z: None,
    };
    // Deliberately interleave kinds: sorting must retain the caller's hole order.
    let items = vec![Item::Hole(first.clone()), Item::Keepout(keepout), Item::Hole(second)];
    let delta = d.set_derived(unit, &a, items.clone())
        .expect("source A's initial layer applies").expect("source A's initial layer produces a delta");
    assert_eq!(delta.added.len(), 3, "the initial layer adds exactly three ids");
    let layer_a = d.derived(unit, &a);
    assert_eq!(layer_a.len(), 3, "source A has three derived items");
    assert!(layer_a.iter().all(|item| item.origin() == &Origin::Derived { from: a.clone() }), "every item takes source A's derived origin");
    assert_eq!(layer_a.iter().map(Item::kind).collect::<Vec<_>>(), vec![ItemKind::Keepout, ItemKind::Hole, ItemKind::Hole], "derived items follow board kind order");
    let hole_id = layer_a[1].id();
    assert_eq!(layer_a[1], Item::Hole(first.clone()).with_id(hole_id).with_origin(Origin::Derived { from: a.clone() }), "stable kind sorting retains the first hole");
    let mut a_ids: Vec<_> = layer_a.iter().map(Item::id).collect();
    a_ids.sort();
    assert_eq!(delta.added, a_ids, "the initial delta names exactly source A's ids");

    let generation = d.generation;
    let board_before_equal = d.unit(unit).expect("source A's unit remains present").board.as_ref().expect("source A's board remains present").clone();
    let again = items.iter().cloned().enumerate().map(|(i, item)| {
        item.with_id(ItemId(1000 + i as u32)).with_origin(Origin::Derived { from: b.clone() })
    }).collect();
    assert_eq!(d.set_derived(unit, &a, again).expect("equal items with other ids and origins are accepted"), None, "an equal set writes nothing");
    assert_eq!(d.generation, generation, "an equal set writes nothing: design generation is unchanged");
    let board_after_equal = d.unit(unit).expect("the equal set keeps its unit").board.as_ref().expect("the equal set keeps its board");
    assert!(Arc::ptr_eq(&board_before_equal, board_after_equal), "an equal set writes nothing: the shared board is not copied");
    assert_eq!(board_after_equal.generation, board_before_equal.generation, "an equal set writes nothing: board generation is unchanged");
    assert_eq!(board_after_equal.next_item, board_before_equal.next_item, "an equal set writes nothing: no id is allocated");

    let lid = Hole {
        id: ItemId(903), name: "lid".into(), at: P::mm(22.0, 6.0), drill: mm(2.0),
        pad: None, net: None, plated: false, origin: Origin::Authored,
    };
    d.set_derived(unit, &b, vec![Item::Hole(lid)])
        .expect("source B's layer applies").expect("source B's layer produces a delta");
    let layer_b = d.derived(unit, &b);
    assert_eq!(layer_b.len(), 1, "source B has one derived hole");
    let mut moved = items;
    let Item::Hole(hole) = &mut moved[0] else { panic!("the first supplied item is a hole") };
    hole.at.x += mm(1.0);
    let delta = d.set_derived(unit, &a, moved)
        .expect("moving source A's first hole applies").expect("moving a derived hole produces a delta");
    assert_eq!(delta.changed, vec![hole_id], "only the moved hole changes and its id stays stable");
    assert!(delta.added.is_empty(), "moving a paired hole adds no items");
    assert!(delta.removed.is_empty(), "moving a paired hole removes no items");
    first.at.x += mm(1.0);
    assert_eq!(d.derived(unit, &a)[1], Item::Hole(first).with_id(hole_id).with_origin(Origin::Derived { from: a.clone() }), "the paired hole moves by one millimetre under its original id");
    assert_eq!(d.derived(unit, &b), layer_b, "another source's layer is untouched");

    let delta = d.set_derived(unit, &a, Vec::new())
        .expect("clearing source A's layer applies").expect("clearing a nonempty layer produces a delta");
    assert_eq!(delta.removed, a_ids, "clearing source A removes exactly its three ids");
    assert!(d.derived(unit, &a).is_empty(), "source A's derived layer is empty after clearing");
    assert_eq!(d.derived(unit, &b), layer_b, "clearing source A leaves source B's hole unchanged");
    assert_eq!(authored(d.unit(unit).expect("the fixture unit survives derived edits").board.as_ref().expect("the fixture board survives derived edits")), before, "all fixture items whose origin is not Derived remain unchanged");
}
