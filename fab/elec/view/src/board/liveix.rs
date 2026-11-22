//! Worker-side indices from board items to the simulation host's static texture rows.

use std::collections::BTreeMap;

use fab_elec_bench::ValueIndex;
use fab_elec_board::Board;
use fab_elec_core::{CompRef, ItemId, NetId, NetRef, PinIx, PinRef, P};

use crate::live::segs::{SegKey, TreeIn};

use super::flow::NetWires;

/// Board copper ready for the host's `SegRows::set(TreeOwner::Copper(board.unit), ..)`.
pub fn trees_in(board: &Board, wires: &[NetWires]) -> Vec<TreeIn> {
    let comps: BTreeMap<_, _> = board.placed.iter().map(|p| (p.id, p.comp)).collect();
    wires.iter().map(|w| TreeIn {
        net: NetRef { board: board.unit, net: w.net },
        nodes: w.tree.nodes,
        segs: w.tree.segs.clone(),
        keys: w.wires.iter().map(|wire| {
            SegKey::items(&[wire.track.0 as u64, wire.seg as u64, wire.piece as u64])
        }).collect(),
        pins: w.tree.pins.iter().enumerate().map(|(j, &(node, _))| {
            let (placed, pad) = w.pads[j];
            (node, PinRef { board: board.unit, comp: comps[&placed], pin: PinIx(pad.0) })
        }).collect(),
        ports: w.tree.ports.clone(),
        conductance: w.tree.conductance.clone(),
    }).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LivePiece {
    pub a: P,
    pub b: P,
    pub row: u32,
    order: u32,
}

/// One lowering's net/part numbering and one copper registration's segment rows.
/// Built on the worker; lookups are O(1) for nets and O(log items + log segments) for pieces.
#[derive(Clone, Debug, Default)]
pub struct LiveIx {
    nets: Vec<f32>,
    parts: BTreeMap<ItemId, f32>,
    tracks: BTreeMap<ItemId, BTreeMap<u32, Vec<LivePiece>>>,
}

impl LiveIx {
    /// No simulation: every instance row is -1, including the net row.
    pub fn none() -> Self {
        Self::default()
    }

    /// `rows` is parallel to `wires`, each entry parallel to that tree's segments.
    /// Construction sorts pieces by their stable path order in O(pieces log pieces).
    pub fn new(board: &Board, index: &ValueIndex, wires: &[NetWires], rows: &[Vec<u32>]) -> Self {
        assert_eq!(wires.len(), rows.len(), "one row list per copper tree");
        let mut live = Self {
            nets: board.nets.iter().enumerate().map(|(i, _)| {
                index.net(NetRef { board: board.unit, net: NetId(i as u32) }).map_or(-1.0, |row| row as f32)
            }).collect(),
            parts: board.placed.iter().map(|p| {
                (p.id, index.comp(CompRef { board: board.unit, comp: p.comp }).map_or(-1.0, |row| row as f32))
            }).collect(),
            tracks: BTreeMap::new(),
        };
        for (w, rows) in wires.iter().zip(rows) {
            assert_eq!(w.tree.segs.len(), rows.len(), "one row per copper segment");
            assert_eq!(w.wires.len(), rows.len(), "one wire id per copper segment");
            for ((wire, &(a, b)), &row) in w.wires.iter().zip(&w.tree.segs).zip(rows) {
                live.tracks.entry(wire.track).or_default().entry(wire.seg).or_default().push(LivePiece {
                    a: w.at[a as usize], b: w.at[b as usize], row, order: wire.piece,
                });
            }
        }
        for segments in live.tracks.values_mut() {
            for pieces in segments.values_mut() {
                pieces.sort_unstable_by_key(|piece| piece.order);
            }
        }
        live
    }

    pub fn net(&self, net: NetId) -> f32 {
        self.nets.get(net.ix()).copied().unwrap_or(-1.0)
    }

    pub fn part(&self, item: ItemId) -> f32 {
        self.parts.get(&item).copied().unwrap_or(-1.0)
    }

    pub(super) fn pieces(&self, item: ItemId, seg: u32) -> &[LivePiece] {
        self.tracks.get(&item).and_then(|segments| segments.get(&seg)).map_or(&[], Vec::as_slice)
    }

    /// Sorted track items whose split points or segment rows changed, including additions and
    /// removals. Merge the two ordered directories once; compare each shared track's pieces.
    pub fn changed_items(&self, old: &LiveIx) -> Vec<ItemId> {
        let mut changed = Vec::new();
        let mut before = old.tracks.iter().peekable();
        let mut after = self.tracks.iter().peekable();
        loop {
            match (before.peek(), after.peek()) {
                (Some((a, _)), Some((b, _))) if a < b => {
                    changed.push(*before.next().unwrap().0);
                }
                (Some((a, _)), Some((b, _))) if a == b => {
                    let (item, a) = before.next().unwrap();
                    let (_, b) = after.next().unwrap();
                    if a != b { changed.push(*item); }
                }
                (_, Some(_)) => changed.push(*after.next().unwrap().0),
                (Some(_), None) => changed.push(*before.next().unwrap().0),
                (None, None) => break,
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{
        batch::{batch_scene, rebatch, Templates},
        flow::net_wires,
        inst::{PadTemplate, StrokeTemplate, TriTemplate},
        scene::{scene_of, scene_of_items},
    };
    use fab_elec_board::{Anchor, BoardNet, Origin, Placed, Track};
    use fab_elec_core::{mm, Angle, BoardId, ClassId, CompId, LayerId, PadIx, Path, PolySet, Shape, Side};
    use fab_elec_parts::{Footprint, FpText, FpTextKind, Gfx, Pad, PadKind, PadProp, PadShape, PadStack, PasteRule};
    use std::sync::Arc;

    #[test]
    fn live_rows_follow_copper_splits_and_placed_components() {
        let mut board = Board::new(BoardId(3), "live copper");
        for name in ["signal", "not simulated"] {
            board.nets.push(BoardNet { name: name.into(), class: ClassId(0) });
        }
        let placed = board.alloc_id();
        let comp = CompId(7);
        let pads = [P::mm(10.0, 0.0), P::mm(5.0, 5.0)].into_iter().enumerate().map(|(i, at)| Pad {
            no: (i + 1).to_string(), kind: PadKind::Smd, stack: PadStack::smd(PadShape::Circle { d: mm(0.5) }),
            at, rot: Angle::ZERO, drill: None, mask_margin: None, paste: PasteRule::default(),
            thermal: None, zone_connect: None, die_len: 0, prop: PadProp::None,
        }).collect();
        board.placed.push(Placed {
            id: placed, comp, path: "part".into(), refdes: "J1".into(),
            footprint: Arc::new(Footprint {
                pads,
                graphics: vec![
                    (LayerId::F_SILK, Gfx::Stroke { path: Path::new(P::ZERO).line(P::mm(1.0, 0.0)), w: mm(0.1) }),
                    (LayerId::F_SILK, Gfx::Fill(Shape::Poly(PolySet::from_ring_nonzero(vec![
                        P::ZERO, P::mm(1.0, 0.0), P::mm(0.0, 1.0),
                    ])))),
                ],
                texts: vec![FpText {
                    kind: FpTextKind::Reference, text: String::new(), layer: LayerId::F_SILK,
                    at: P::mm(1.0, 1.0), rot: Angle::ZERO, size: mm(1.0), thick: mm(0.1), visible: true,
                }],
                ..Footprint::default()
            }),
            pad_nets: vec![Some(NetId(0)); 2], at: P::ZERO, rot: Angle::ZERO, side: Side::Top,
            locked: false, origin: Origin::Authored, body_z: None, ref_label: None, value_label: None, legend: None, value: None,
        });
        let trunk = board.alloc_id();
        board.tracks.push(Track {
            id: trunk, net: NetId(0), layer: LayerId::F_CU, w: mm(0.25), widths: None,
            path: Path::new(P::ZERO).line(P::mm(10.0, 0.0)),
            ends: [Anchor::Free, Anchor::Pad { placed, pad: PadIx(0) }], locked: false, origin: Origin::Authored,
        });
        let mut index = ValueIndex::new(1);
        // Number another board first: dense simulation rows must differ from this board's IDs.
        index.add_net(NetRef { board: BoardId(0), net: NetId(0) });
        index.add_comp(CompRef { board: BoardId(0), comp }, 3, None);
        let net_row = index.add_net(NetRef { board: board.unit, net: NetId(0) }) as f32;
        let (part_row, first_pin) = index.add_comp(CompRef { board: board.unit, comp }, 2, None);
        let t = Templates { stroke: StrokeTemplate::packed(), pad: PadTemplate::packed(), tri: TriTemplate::packed() };
        let fake_rows = |wires: &[NetWires]| {
            let mut next = 100;
            wires.iter().map(|w| w.tree.segs.iter().map(|_| { let row = next; next += 1; row }).collect()).collect::<Vec<Vec<u32>>>()
        };
        let before_wires = net_wires(&board, &[]);
        let before = LiveIx::new(&board, &index, &before_wires, &fake_rows(&before_wires));
        let before_batch = batch_scene(&scene_of(&board, &[]), &t, &before);
        let before_strokes = before_batch.layer(LayerId::F_CU).unwrap().strokes.publication().unwrap();
        assert_eq!(before_strokes.data()[17], 100.0, "an unsplit track carries its sole segment row");
        assert_eq!(before_batch.layer(LayerId::F_CU).unwrap().strokes.count, 1, "an unsplit track stays one instance");

        let branch = board.alloc_id();
        let junction = P::mm(5.0, 0.0);
        board.tracks.push(Track {
            id: branch, net: NetId(0), layer: LayerId::F_CU, w: mm(0.25), widths: None,
            path: Path::new(junction).line(P::mm(5.0, 5.0)),
            ends: [board.track_anchor(trunk, junction).expect("junction host"), Anchor::Free], locked: false, origin: Origin::Authored,
        });
        let wires = net_wires(&board, &[]);
        let rows = fake_rows(&wires);
        let live = LiveIx::new(&board, &index, &wires, &rows);
        let trees = trees_in(&board, &wires);
        assert_eq!(trees.len(), 1, "one host tree per routed net");
        assert_eq!(trees[0].net, NetRef { board: board.unit, net: NetId(0) }, "tree net keeps board scope");
        assert_eq!(trees[0].nodes, wires[0].tree.nodes, "host tree retains all nodes");
        assert_eq!(trees[0].segs, wires[0].tree.segs, "host tree retains oriented segments");
        assert_eq!(trees[0].ports, wires[0].tree.ports, "host tree retains ports");
        assert_eq!(trees[0].conductance, wires[0].tree.conductance, "host tree retains conductances");
        assert_eq!(trees[0].keys, vec![
            SegKey::items(&[trunk.0 as u64, 0, 0]), SegKey::items(&[trunk.0 as u64, 0, 1]),
            SegKey::items(&[branch.0 as u64, 0, 0]),
        ], "host keys identify each track, path segment and ordered piece");
        assert_eq!(trees[0].pins, vec![
            (wires[0].tree.segs[1].1, PinRef { board: board.unit, comp, pin: PinIx(0) }),
            (wires[0].tree.segs[2].1, PinRef { board: board.unit, comp, pin: PinIx(1) }),
        ], "pins use the placed component and pad index, not the board item or local current index");
        assert_eq!(index.pin(trees[0].pins[1].1), Some(first_pin + 1), "host pins resolve into the lowering's current rows");
        assert_eq!(live.net(NetId(1)), -1.0, "a net absent from the lowering has no row");
        assert_eq!(live.part(ItemId(u32::MAX)), -1.0, "an item without a placed component has no part row");
        let absent = LiveIx::new(&board, &ValueIndex::new(2), &[], &[]);
        assert_eq!(absent.part(placed), -1.0, "a component absent from the lowering has no part row");

        let scene = scene_of(&board, &[]);
        let batch = batch_scene(&scene, &t, &live);
        let front = batch.layer(LayerId::F_CU).unwrap();
        let strokes = front.strokes.publication().unwrap();
        let trunk_rows: Vec<_> = strokes.data().chunks_exact(t.stroke.slots()).filter(|r| r[10] == trunk.0 as f32).collect();
        assert_eq!(trunk_rows.len(), 2, "a T splits the trunk into two stroke instances");
        assert_eq!([trunk_rows[0][17], trunk_rows[1][17]], [rows[0][0] as f32, rows[0][1] as f32], "each piece carries its own current row");
        assert_eq!(&trunk_rows[0][19..23], &[0.0, 0.0, 5.0, 0.0], "first trunk piece ends at the T");
        assert_eq!(&trunk_rows[1][19..23], &[5.0, 0.0, 10.0, 0.0], "second trunk piece begins at the T");
        assert_eq!([trunk_rows[0][25], trunk_rows[1][25]], [0.0, 5.0], "along0 continues across the T split");
        assert!(trunk_rows.iter().all(|r| r[16] == net_row && r[18] == -1.0 && r[24] == 0.25), "track pieces retain net row and width without a part row");
        let pads = front.pads.publication().unwrap();
        assert_eq!(front.pads.count, 2, "both placed pads are batched");
        assert!(pads.data().chunks_exact(t.pad.slots()).all(|r| r[16..19] == [net_row, -1.0, part_row as f32]), "pads carry the simulation net and component rows");
        let silk = batch.layer(LayerId::F_SILK).unwrap();
        assert!(silk.strokes.count > 4 && silk.tris.count > 0, "footprint silk, text and filled graphics all emit instances");
        for kind in [&silk.strokes, &silk.tris] {
            let publication = kind.publication().unwrap();
            assert!(publication.data().chunks_exact(kind.slots).all(|r| r[18] == part_row as f32), "all placed graphics and text carry the part row");
        }
        let off = batch_scene(&scene, &t, &LiveIx::none());
        for kind in off.layers.iter().flat_map(|l| [&l.strokes, &l.pads, &l.tris]).chain([&off.holes, &off.substrate]) {
            let publication = kind.publication().unwrap();
            assert!(publication.data().chunks_exact(kind.slots).all(|r| r[16..19] == [-1.0; 3]), "without simulation every live row is absent");
        }
        assert_eq!(off.layer(LayerId::F_CU).unwrap().strokes.count, 2, "without wires each authored track segment stays one instance");
        let changed = live.changed_items(&before);
        assert_eq!(changed, vec![trunk, branch], "adding a T invalidates the neighbouring trunk and the new branch, in order");
        assert_eq!(before.changed_items(&live), changed, "removing a T invalidates the same track items");
        assert!(live.changed_items(&LiveIx::new(&board, &index, &wires, &rows)).is_empty(), "identical geometry and rows invalidate nothing");
        let mut renumbered = rows.clone();
        renumbered[0][0] += 10;
        assert_eq!(LiveIx::new(&board, &index, &wires, &renumbered).changed_items(&live), vec![trunk], "a changed segment row invalidates its track");
        let mut moved = wires.clone();
        moved[0].at[wires[0].tree.segs[0].1 as usize] = P::mm(4.0, 0.0);
        assert_eq!(LiveIx::new(&board, &index, &moved, &rows).changed_items(&live), vec![trunk, branch], "moving a split without renumbering invalidates both touching tracks");
        let updated = rebatch(&before_batch, &changed, &[], &scene_of_items(&board, &[], &changed), &t, &live);
        for layer in &batch.layers {
            let updated = updated.layer(layer.layer).unwrap();
            for (a, b) in [(&layer.strokes, &updated.strokes), (&layer.pads, &updated.pads), (&layer.tris, &updated.tris)] {
                assert_eq!(a.publication().unwrap().data(), b.publication().unwrap().data(), "rebatching changed neighbours equals a full batch with the same lowering");
            }
        }

        // Exercise both sweep directions on the same T: its chord midpoint becomes the arc's
        // mid-angle point, and a following segment must start after the complete arc length.
        for mid in [P::mm(5.0, 5.0), P::mm(5.0, -5.0), junction] {
            board.tracks[0].path = Path::new(P::ZERO).arc(mid, P::mm(10.0, 0.0)).line(P::mm(12.0, 0.0));
            board.tracks[0].ends = [Anchor::Free; 2];
            let wires = net_wires(&board, &[]);
            let rows = fake_rows(&wires);
            let live = LiveIx::new(&board, &index, &wires, &rows);
            let batch = batch_scene(&scene_of(&board, &[]), &t, &live);
            let strokes = batch.layer(LayerId::F_CU).unwrap().strokes.publication().unwrap();
            let mut pieces: Vec<_> = strokes.data().chunks_exact(t.stroke.slots()).filter(|r| r[10] == trunk.0 as f32).collect();
            pieces.sort_by(|a, b| a[25].total_cmp(&b[25]));
            assert_eq!(pieces.len(), 3, "split arc and following segment each retain their current rows");
            assert_eq!(&pieces[0][21..23], &pieces[1][19..21], "arc pieces meet without a gap");
            assert_eq!(&pieces[0][21..23], &crate::board::inst::pt(mid), "chord midpoint maps onto the original circle at mid angle");
            assert_eq!([pieces[0][17], pieces[1][17], pieces[2][17]], [100.0, 101.0, 102.0], "arc pieces and next segment use distinct host rows");
            let length = if mid == junction { 10.0 } else { 5.0 * std::f32::consts::PI };
            assert!((pieces[1][25] - length * 0.5).abs() < 1e-5, "second arc piece continues the first's arc length");
            assert!((pieces[2][25] - length).abs() < 1e-5, "next path segment continues the complete arc length");
            if mid == junction {
                assert_eq!([pieces[0][23], pieces[1][23]], [0.0; 2], "collinear arc pieces keep the straight-chord fallback");
            } else {
                let sag = 5.0 * (1.0 - 1.0 / 2.0_f32.sqrt()) * (mid.y as f32).signum();
                assert!((pieces[0][23] - sag).abs() < 1e-5 && (pieces[1][23] - sag).abs() < 1e-5, "split arcs keep the original radius and sweep direction");
            }
        }
    }
}
