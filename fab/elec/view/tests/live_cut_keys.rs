use fab_elec_core::ids::{BoardId, CompId, PinIx, PinRef};
use fab_elec_view::live::segs::{cut_keys, SegKey};

#[test]
fn cut_keys_survive_relayout() {
    let pin = |n| PinRef { board: BoardId(0), comp: CompId(n), pin: PinIx(0) };
    let (a, b, c, d) = (pin(1), pin(2), pin(3), pin(4));

    let chain = [(0, 1), (1, 2), (2, 3)];
    assert_eq!(
        cut_keys(4, &chain, &[(0, a), (3, b)]),
        vec![SegKey::pins(&[a]); 3],
        "all segments of an unbranched chain share the smaller cut"
    );

    // Node 0 is the junction; nodes 1, 2, 3 carry a, b, c, and node 4 is a stub.
    let t = [(0, 1), (0, 2), (0, 3), (0, 4)];
    let t_pins = [(3, c), (1, a), (2, b)];
    let t_keys = cut_keys(5, &t, &t_pins);
    assert_eq!(t_keys[3], SegKey::pins(&[]), "a pinless stub uses the empty cut");
    assert_eq!(t_keys[0], SegKey::pins(&[a]), "a's branch chooses its own smaller side");
    assert_eq!(t_keys[1], SegKey::pins(&[a, c]), "b's branch chooses the opposite side");
    assert_eq!(t_keys[2], SegKey::pins(&[a, b]), "c's branch chooses the opposite side");

    let bent_t = [(0, 1), (0, 5), (5, 2), (0, 3), (0, 4)];
    let bent_keys = cut_keys(6, &bent_t, &t_pins);
    assert_eq!(
        bent_keys,
        vec![t_keys[0].clone(), t_keys[1].clone(), t_keys[1].clone(), t_keys[2].clone(), t_keys[3].clone()],
        "adding a pinless bend duplicates b's cut and preserves every other branch"
    );

    // Old nodes [0, 1, 2, 3, 4, 5] become [4, 2, 0, 5, 3, 1]. Reorder edges
    // as [4, 2, 0, 3, 1], reversing the stub, the bend-to-b edge, and c's branch.
    let relaid_t = [(3, 4), (0, 1), (4, 2), (5, 4), (4, 1)];
    let relaid_keys = cut_keys(6, &relaid_t, &[(0, b), (5, c), (2, a)]);
    assert_eq!(relaid_keys.len(), bent_keys.len(), "relayout preserves the number of segment keys");
    for (new, old) in [4, 2, 0, 3, 1].into_iter().enumerate() {
        assert_eq!(
            relaid_keys[new], bent_keys[old],
            "segment {old} retains its key after node renumbering, reordering, and reversal"
        );
    }

    let mut square = vec![(0, 1), (1, 2), (2, 3), (3, 0), (2, 4)];
    let square_pins = [(4, c), (0, a), (2, b)];
    let square_keys = cut_keys(5, &square, &square_pins);
    let loop_keys = [
        SegKey::looped(&[a]), SegKey::looped(&[b]),
        SegKey::looped(&[b]), SegKey::looped(&[a]),
    ];
    assert_eq!(&square_keys[..4], &loop_keys, "square segments use only their end nodes' pins");
    assert_eq!(
        square_keys[4], SegKey::pins(&[c]).min(SegKey::pins(&[a, b])),
        "the square's tail is a bridge and chooses the smaller side"
    );
    square.push((2, 4));
    square.push((4, 4));
    let doubled_keys = cut_keys(5, &square, &square_pins);
    assert_eq!(&doubled_keys[..4], &loop_keys, "doubling a tail preserves the square's loop keys");
    assert_eq!(doubled_keys[4], SegKey::looped(&[b, c]), "the original parallel tail is a loop");
    assert_eq!(doubled_keys[5], SegKey::looped(&[b, c]), "the second parallel tail is a loop");
    assert_eq!(doubled_keys[6], SegKey::looped(&[c]), "a self-loop counts its node's pins once");
    assert_ne!(SegKey::pins(&[a]), SegKey::looped(&[a]), "bridge and loop kinds never collide");
    assert_eq!(SegKey::item(5), SegKey::items(&[5]), "a one-word item id is the same key either way");
    assert_ne!(SegKey::items(&[0]), SegKey::pins(&[]), "item and cut kinds never collide");

    let invalid = [(0, 99), (0, 1), (99, 0)];
    let invalid_keys = cut_keys(5, &invalid, &[(0, a), (1, b), (99, d)]);
    assert_eq!(invalid_keys.len(), invalid.len(), "invalid segments still have parallel output");
    assert_eq!(invalid_keys[0], SegKey::looped(&[]), "an invalid second end uses the empty loop key");
    assert_eq!(invalid_keys[2], SegKey::looped(&[]), "an invalid first end uses the empty loop key");
    assert_eq!(
        invalid_keys, cut_keys(5, &invalid, &[(0, a), (1, b)]),
        "out-of-range pins are ignored without changing valid or invalid segment keys"
    );
    assert_eq!(
        cut_keys(0, &[(0, 0)], &[(0, a)]), vec![SegKey::looped(&[])],
        "a zero-node graph treats all ends and pins as out of range"
    );
    assert!(cut_keys(0, &[], &[]).is_empty(), "empty input gives empty output");

    assert_eq!(
        cut_keys(5, &[(0, 1), (2, 3)], &[(0, c), (1, d), (2, a), (3, b), (4, a)]),
        vec![SegKey::pins(&[c]), SegKey::pins(&[a])],
        "disconnected components and isolated pins never contribute to another part's cut"
    );
    assert_eq!(
        cut_keys(2, &[(0, 1)], &[(0, a), (1, b), (1, a), (0, a)]),
        vec![SegKey::pins(&[a, a])],
        "bridge subtraction preserves repeated pins as a multiset"
    );
    assert_eq!(
        cut_keys(2, &[(0, 1), (1, 0), (0, 0)], &[(0, c), (1, d), (0, a), (1, b), (0, a)]),
        vec![SegKey::looped(&[a, a, b, c, d]), SegKey::looped(&[a, a, b, c, d]), SegKey::looped(&[a, a, c])],
        "loop end pins are sorted and retain multiplicity while self-loops count one node"
    );

    let long_chain: Vec<_> = (0..9_999).map(|node| (node, node + 1)).collect();
    let long_keys = cut_keys(10_000, &long_chain, &[(0, a), (9_999, b)]);
    assert_eq!(long_keys.len(), 9_999, "an iterative DFS emits one key per long-chain segment");
    let expected = SegKey::pins(&[a]);
    assert!(long_keys.iter().all(|key| key == &expected), "all 9,999 long-chain cuts are equal");
}
