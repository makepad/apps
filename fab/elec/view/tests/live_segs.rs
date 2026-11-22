use fab_elec_bench::ValueIndex;
use fab_elec_core::ids::{BoardId, CompId, CompRef, NetId, NetRef, PinIx, PinRef};
use fab_elec_view::live::segs::{SegKey, SegRows, TreeIn, TreeOwner};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[test]
fn segment_rows_are_shared_stable_and_never_collide() {
    let b0 = BoardId(0);
    let net_a = NetRef { board: b0, net: NetId(7) };
    let net_b = NetRef { board: b0, net: NetId(11) };
    let comps = [CompId(5), CompId(9), CompId(2)];
    let mut index = ValueIndex::new(7);
    index.add_net(net_a);
    index.add_net(net_b);
    for comp in comps {
        index.add_comp(CompRef { board: b0, comp }, 2, None);
    }
    let pin = |comp, ix| PinRef { board: b0, comp: CompId(comp), pin: PinIx(ix) };
    let (a0, a1) = (pin(2, 0), pin(5, 0));
    let (b1, b2, b3) = (pin(2, 1), pin(5, 1), pin(9, 1));
    let absent = pin(99, 0);
    assert_eq!(index.counts(), (2, 6, 3), "the lowering has two nets and three two-pin components");
    assert_eq!(index.pin(absent), None, "the missing component is outside this lowering");
    assert_eq!(SegKey::pins(&[b2, b1]), SegKey::pins(&[b1, b2]), "pin keys ignore input order");
    assert_eq!(
        SegKey::pins(&[PinRef { board: BoardId(u16::MAX), comp: CompId(u32::MAX), pin: PinIx(u16::MAX) }]).0[1..],
        [u64::MAX],
        "pin packing preserves all sixteen board, thirty-two component and sixteen pin bits (after the kind word)"
    );
    assert_eq!(SegKey::pins(&[]).0.len(), 1, "an empty pin side's key is its kind word alone");

    let chain_key = SegKey::pins(&[a0]);
    let t_keys = [SegKey::pins(&[b1, b3]), SegKey::pins(&[b1]), SegKey::pins(&[b1, b2])];
    let sheet = TreeOwner::Sheet(b0, 0);
    let copper = TreeOwner::Copper(b0);
    let sheet_trees = vec![
        TreeIn {
            net: net_a,
            nodes: 4,
            segs: vec![(0, 1), (1, 2), (2, 3)],
            keys: vec![chain_key.clone(); 3],
            pins: vec![(0, a0), (3, a1)],
            ..TreeIn::default()
        },
        TreeIn {
            net: net_b,
            nodes: 4,
            segs: vec![(0, 1), (1, 2), (1, 3)],
            keys: t_keys.to_vec(),
            pins: vec![(0, b2), (2, b1), (3, b3)],
            ports: vec![1],
            conductance: vec![1.0, 2.0, 3.0],
        },
    ];
    let mut allocator = SegRows::default();
    assert!(allocator.is_empty(), "a new allocator has no texture rows");
    assert_eq!(allocator.in_use(), 0, "a new allocator has no live segments");
    assert_eq!(allocator.version(), 0, "a new allocator starts at version zero");
    let sheet_rows = allocator.set(sheet, sheet_trees.clone());
    let initial_len = allocator.len();
    let initial_version = allocator.version();
    assert_eq!(sheet_rows.iter().map(Vec::len).collect::<Vec<_>>(), vec![3, 3], "each sheet segment gets one row");
    assert_eq!(initial_len, 6, "the sheet allocates six texture rows");
    assert_eq!(initial_version, 1, "the first registration changes the version once");
    assert_eq!(allocator.set(sheet, sheet_trees.clone()), sheet_rows, "two panes on one sheet share exactly the same rows");
    assert_eq!(allocator.len(), initial_len, "a second pane allocates no rows");
    assert_eq!(allocator.version(), initial_version, "an identical set leaves the version unchanged");

    let copper_trees = vec![
        TreeIn {
            net: net_a,
            nodes: 2,
            segs: vec![(0, 1)],
            keys: vec![SegKey::item(20)],
            pins: vec![(0, a0), (1, a1)],
            ..TreeIn::default()
        },
        TreeIn {
            net: net_b,
            nodes: 3,
            segs: vec![(0, 1), (1, 2)],
            keys: vec![SegKey::item(21), SegKey::item(22)],
            pins: vec![(0, b1), (1, b2), (2, b3)],
            ..TreeIn::default()
        },
    ];
    let copper_rows = allocator.set(copper, copper_trees.clone());
    let sheet_set: BTreeSet<_> = sheet_rows.iter().flatten().copied().collect();
    let copper_set: BTreeSet<_> = copper_rows.iter().flatten().copied().collect();
    assert!(sheet_set.is_disjoint(&copper_set), "sheet and copper never share rows even for the same nets");
    assert_eq!(sheet_set.union(&copper_set).count(), 9, "all nine segments have distinct rows");
    assert_eq!(allocator.in_use(), 9, "in_use is the total segment count across geometries");
    assert_eq!(allocator.rows_of(sheet), sheet_set.iter().copied().collect::<Vec<_>>(), "rows_of returns ascending sheet rows");

    // Renumber old nodes [0, 1, 2, 3] to [3, 2, 0, 1] in the T, and to [2, 0, 3, 1]
    // in the chain. Reorder both the trees and their segments without changing their keys.
    let relayout = vec![
        TreeIn {
            net: net_b,
            nodes: 4,
            segs: vec![(2, 1), (3, 2), (2, 0)],
            keys: vec![t_keys[2].clone(), t_keys[0].clone(), t_keys[1].clone()],
            pins: vec![(3, b2), (0, b1), (1, b3)],
            ports: vec![2],
            conductance: vec![3.0, 1.0, 2.0],
        },
        TreeIn {
            net: net_a,
            nodes: 4,
            segs: vec![(0, 3), (3, 1), (2, 0)],
            keys: vec![chain_key.clone(); 3],
            pins: vec![(2, a0), (1, a1)],
            ..TreeIn::default()
        },
    ];
    let by_key = |trees: &[TreeIn], rows: &[Vec<u32>]| {
        let mut keyed = BTreeMap::<(NetRef, SegKey), Vec<u32>>::new();
        for (tree, rows) in trees.iter().zip(rows) {
            for (key, row) in tree.keys.iter().zip(rows) {
                keyed.entry((tree.net, key.clone())).or_default().push(*row);
            }
        }
        keyed
    };
    let before_relayout = allocator.version();
    let relayout_rows = allocator.set(sheet, relayout.clone());
    assert_eq!(by_key(&relayout, &relayout_rows), by_key(&sheet_trees, &sheet_rows), "relayout preserves rows by net, key and occurrence");
    assert_eq!(allocator.version(), before_relayout + 1, "changed geometry bumps the version once even when all rows survive");
    assert!(allocator.take_freed().is_empty(), "relayout frees no surviving segment");
    assert_eq!(allocator.len(), 9, "relayout allocates no rows");

    let mut edited = relayout.clone();
    let removed_row = relayout_rows[0][0];
    edited[0].nodes = 5;
    edited[0].segs[0] = (2, 4);
    edited[0].keys[0] = SegKey::pins(&[b3]);
    edited[0].pins.push((4, absent));
    let before_edit_len = allocator.len();
    let edited_rows = allocator.set(sheet, edited.clone());
    assert_eq!(edited_rows[0][0] as usize, before_edit_len, "a replacement allocates the next new row before any clear");
    assert_ne!(edited_rows[0][0], removed_row, "a newly removed row cannot be reused in the same set");
    assert_eq!(&edited_rows[0][1..], &relayout_rows[0][1..], "editing one segment preserves its neighbors' rows");
    assert_eq!(edited_rows[1], relayout_rows[1], "editing one net preserves the other net's rows");
    assert_eq!(allocator.in_use(), 9, "a pending clear no longer counts as in use");
    assert_eq!(allocator.take_freed(), vec![removed_row], "the removed segment is handed out for clearing once");
    assert!(allocator.take_freed().is_empty(), "a clear is never handed out twice");

    edited[0].nodes = 6;
    edited[0].segs.push((4, 5));
    edited[0].keys.push(SegKey::pins(&[b2, b3]));
    let grown_rows = allocator.set(sheet, edited.clone());
    assert_eq!(grown_rows[0].last(), Some(&removed_row), "a later segment reuses the oldest row after its clear was handed out");
    assert_eq!(allocator.len(), before_edit_len + 1, "reusing a cleared row does not grow the texture");

    edited.push(TreeIn {
        net: net_a,
        nodes: 3,
        segs: vec![(0, 1), (1, 2)],
        keys: vec![chain_key.clone(), chain_key],
        ..TreeIn::default()
    });
    let duplicate_rows = allocator.set(sheet, edited.clone());
    assert_ne!(duplicate_rows[2][0], duplicate_rows[2][1], "equal keys on one net get distinct rows");
    let chain_rows: BTreeSet<_> = duplicate_rows[1].iter().chain(&duplicate_rows[2]).copied().collect();
    assert_eq!(chain_rows.len(), 5, "occurrences span separate trees of the same net within one call");
    let duplicate_version = allocator.version();
    assert_eq!(allocator.set(sheet, edited.clone()), duplicate_rows, "equal-key occurrences retain their rows on repeat registration");
    assert_eq!(allocator.version(), duplicate_version, "a duplicate-key repeat leaves the version unchanged");

    let other_sheet = TreeOwner::Sheet(b0, 1);
    let short_keys = vec![
        TreeIn {
            net: net_a,
            nodes: 3,
            segs: vec![(0, 1), (1, 2)],
            keys: vec![SegKey(Box::new([]))],
            ..TreeIn::default()
        },
        TreeIn { net: net_a, nodes: 2, segs: vec![(0, 1)], ..TreeIn::default() },
        TreeIn { net: net_b, nodes: 2, segs: vec![(0, 1)], ..TreeIn::default() },
    ];
    let short_rows = allocator.set(other_sheet, short_keys.clone());
    assert_eq!(short_rows.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 1, 1], "missing keys still give every segment a row");
    assert_eq!(short_rows.iter().flatten().copied().collect::<BTreeSet<_>>().len(), 4, "explicit and missing empty keys have distinct occurrences and net scopes");
    let mut explicit_empty = short_keys.clone();
    for tree in &mut explicit_empty {
        tree.keys.resize_with(tree.segs.len(), || SegKey(Box::new([])));
    }
    assert_eq!(allocator.set(other_sheet, explicit_empty.clone()), short_rows, "missing keys have the same identities as explicit empty keys");
    assert!(allocator.take_freed().is_empty(), "spelling out missing keys frees no rows");

    // a view that resolves before the first lowering does so against the empty numbering; the first
    // lowering may be of generation 0 (a new design's) and is resolved again
    let empty = allocator.resolved(&ValueIndex::default());
    assert!(!empty.is_empty() && empty.iter().all(|r| r.tree.pins.is_empty()), "the empty numbering maps no pins");
    let mut first = index.clone();
    first.generation = 0;
    let at_zero = allocator.resolved(&first);
    assert!(!Arc::ptr_eq(&empty, &at_zero) && at_zero.iter().any(|r| !r.tree.pins.is_empty()),
        "a lookup made against the empty numbering is redone after the first lowering, of generation 0");
    let resolved = allocator.resolved(&index);
    let expected = [(&edited, &duplicate_rows), (&explicit_empty, &short_rows), (&copper_trees, &copper_rows)];
    let expected_count: usize = expected.iter().map(|(trees, _)| trees.len()).sum();
    assert_eq!(resolved.len(), expected_count, "resolution includes every registered tree");
    let expected_trees = expected.into_iter().flat_map(|(trees, rows)| trees.iter().zip(rows));
    for (actual, (tree, rows)) in resolved.iter().zip(expected_trees) {
        assert_eq!(actual.tree.nodes, tree.nodes, "resolved trees follow owner order then call order, preserving node counts");
        assert_eq!(actual.tree.segs, tree.segs, "resolved segment endpoints retain their direction and order");
        assert_eq!(actual.rows, *rows, "resolution returns each registered segment's own row");
        assert_eq!(actual.rows.len(), actual.tree.segs.len(), "resolved rows remain parallel to segments");
        let pins: Vec<_> = tree.pins.iter().filter_map(|&(node, pin)| index.pin(pin).map(|i| (node, i as u32))).collect();
        assert_eq!(actual.tree.pins, pins, "resolution maps design pins through ValueIndex and drops pins outside the lowering");
        assert_eq!(actual.tree.ports, tree.ports, "resolution preserves ports");
        assert_eq!(actual.tree.conductance, tree.conductance, "resolution preserves conductances including missing entries");
    }
    assert_eq!(resolved[0].tree.pins.len() + 1, edited[0].pins.len(), "the absent component's pin is dropped while the three known pins remain");
    assert!(Arc::ptr_eq(&resolved, &allocator.resolved(&index)), "unchanged trees and generation return the same Arc");
    assert_eq!(allocator.set(sheet, edited.clone()), duplicate_rows, "an identical set keeps rows after resolution");
    assert!(Arc::ptr_eq(&resolved, &allocator.resolved(&index)), "an identical set preserves the resolved cache");
    edited[0].ports.push(4);
    assert_eq!(allocator.set(sheet, edited.clone()), duplicate_rows, "changing ports preserves all segment identities");
    let changed = allocator.resolved(&index);
    assert!(!Arc::ptr_eq(&resolved, &changed), "a set that changes only ports invalidates the resolved Arc");
    assert_eq!(resolved[0].tree.ports, vec![2], "an older resolved snapshot remains immutable");
    assert_eq!(changed[0].tree.ports, vec![2, 4], "the new resolved snapshot includes the changed ports");

    let mut next_index = ValueIndex::new(index.generation + 1);
    next_index.add_net(net_b);
    next_index.add_net(net_a);
    for comp in comps.into_iter().rev() {
        next_index.add_comp(CompRef { board: b0, comp }, 2, None);
    }
    let next = allocator.resolved(&next_index);
    assert!(!Arc::ptr_eq(&changed, &next), "a new lowering generation invalidates the resolved Arc");
    assert_ne!(changed[0].tree.pins, next[0].tree.pins, "a new lowering remaps pins rather than retaining stale indices");
    let next_pins: Vec<_> = edited[0].pins.iter().filter_map(|&(node, pin)| next_index.pin(pin).map(|i| (node, i as u32))).collect();
    assert_eq!(next[0].tree.pins, next_pins, "the new generation resolves against its own pin numbering");
    assert!(Arc::ptr_eq(&next, &allocator.resolved(&next_index)), "the new generation is cached on its second call");

    let before_remove_version = allocator.version();
    let before_remove_len = allocator.len();
    let before_remove_used = allocator.in_use();
    allocator.remove(copper);
    assert_eq!(allocator.version(), before_remove_version + 1, "removing a registered owner bumps the version once");
    assert!(allocator.rows_of(copper).is_empty(), "removing copper drops all of its registered rows");
    assert_eq!(allocator.in_use(), before_remove_used - copper_set.len(), "removing copper immediately releases its live count");
    let freed_copper = allocator.take_freed();
    assert_eq!(freed_copper.iter().copied().collect::<BTreeSet<_>>(), copper_set, "removing copper frees exactly its rows");
    assert_eq!(freed_copper.len(), copper_set.len(), "removing copper frees each row only once");
    assert!(allocator.take_freed().is_empty(), "removed copper rows are handed out for clearing only once");
    let without_copper = allocator.resolved(&next_index);
    assert!(!Arc::ptr_eq(&next, &without_copper), "removing an owner invalidates the resolved Arc");
    assert_eq!(without_copper.len(), edited.len() + explicit_empty.len(), "removed trees are absent from resolution");
    allocator.remove(copper);
    assert_eq!(allocator.version(), before_remove_version + 1, "removing an absent owner changes nothing");
    assert!(Arc::ptr_eq(&without_copper, &allocator.resolved(&next_index)), "removing an absent owner preserves the cache");

    allocator.remove(other_sheet);
    let freed_sheet = allocator.take_freed();
    let fifo: Vec<_> = freed_copper.into_iter().chain(freed_sheet).collect();
    let reused = allocator.set(copper, vec![TreeIn {
        net: net_a,
        nodes: fifo.len() as u32 + 1,
        segs: (0..fifo.len() as u32).map(|i| (i, i + 1)).collect(),
        keys: (0..fifo.len() as u64).map(|i| SegKey::item(100 + i)).collect(),
        ..TreeIn::default()
    }]);
    assert_eq!(reused, vec![fifo], "cleared rows are reused in FIFO order across separate take_freed calls");
    assert_eq!(allocator.len(), before_remove_len, "removal and reuse never shrink or unnecessarily grow the texture");
    allocator.remove(sheet);
    allocator.remove(copper);
    assert_eq!(allocator.in_use(), 0, "removing every owner leaves no live rows even before clearing");
    assert_eq!(allocator.take_freed().len(), before_remove_len, "all remaining rows are eventually handed out for clearing");
    assert_eq!(allocator.len(), before_remove_len, "the texture high-water count survives complete removal");
    assert!(!allocator.is_empty(), "an allocator with previously allocated texture rows remains nonempty");
    assert!(allocator.resolved(&next_index).is_empty(), "no owners resolve to no trees");

    let mut prepared_allocator = SegRows::new();
    let mut direct_allocator = SegRows::new();
    assert_eq!(SegRows::prepare(sheet_trees.clone()).trees(), &sheet_trees[..], "prepared: keeps the trees");
    for trees in [sheet_trees, relayout, edited] {
        let prepared_rows = prepared_allocator.set_prepared(sheet, SegRows::prepare(trees.clone()));
        let direct_rows = direct_allocator.set(sheet, trees);
        assert_eq!(prepared_rows, direct_rows, "prepared: rows equal set's");
        assert_eq!(prepared_allocator.len(), direct_allocator.len(), "prepared: len");
        assert_eq!(prepared_allocator.version(), direct_allocator.version(), "prepared: version");
        assert_eq!(prepared_allocator.take_freed(), direct_allocator.take_freed(), "prepared: freed rows");
    }
}
