//! Routed track and via barrel lengths and their circuit-declared matching tolerances.

use crate::{Anchor, Board, Connectivity, ItemKind, StackKind, Stackup};
use std::collections::{HashMap, HashSet};
use fab_elec_core::{canon_struct, ItemId, LayerId, LayerSet, NetId, Nm, Path, P};

/// A group of nets whose routed lengths must be within `tol` of the longest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LengthMatch {
    pub nets: Vec<NetId>,
    pub tol: Nm,
}
canon_struct!(LengthMatch { nets, tol });

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measurement {
    WithBarrels,
    NoStackup,
}

impl Measurement {
    pub fn text(self) -> &'static str {
        match self {
            Self::WithBarrels => "track length plus via barrels",
            Self::NoStackup => "vias not counted: no stackup",
        }
    }
}

/// Copper centre depths in half-nanometres. Each missing thickness starts a new section:
/// distances are known within a section even when another part of the stack is unspecified.
struct CopperDepths {
    centres: HashMap<LayerId, (usize, i64)>,
    measurement: Measurement,
}

impl CopperDepths {
    fn new(stack: &Stackup) -> Self {
        let mut centres = HashMap::new();
        let mut measurement = Measurement::NoStackup;
        let mut section = 0;
        let mut depth = 0;
        let mut previous_copper = None;
        for (i, layer) in stack.layers.iter().enumerate() {
            if layer.thickness <= 0 {
                section += 1;
                previous_copper = None;
                continue;
            }
            let thickness = i64::from(layer.thickness);
            if let StackKind::Copper(id) = layer.kind {
                match previous_copper {
                    // Two copper records alone give no thickness for the material between them.
                    Some(previous) if i == previous + 1 => section += 1,
                    Some(_) => measurement = Measurement::WithBarrels,
                    None => {}
                }
                centres.insert(id, (section, depth * 2 + thickness));
                previous_copper = Some(i);
            }
            depth += thickness;
        }
        Self { centres, measurement }
    }

    /// Outermost of the distinct anchored layers in physical stack order.
    fn barrel(&self, layers: LayerSet) -> Option<i64> {
        let mut layers = layers.iter();
        let &(section, first) = self.centres.get(&layers.next()?)?;
        let (mut top, mut bottom) = (first, first);
        for layer in layers {
            let &(other, depth) = self.centres.get(&layer)?;
            if section != other { return None; }
            top = top.min(depth);
            bottom = bottom.max(depth);
        }
        Some(bottom - top)
    }
}

/// Total routed length, in whole nanometres (a total can exceed one board coordinate).
#[derive(Clone, Copy, Debug)]
pub struct NetLength {
    pub nm: i64,
    pub measurement: Measurement,
    pub partial: bool,
    pub at: Option<P>,
}

/// The longest member (first in intent order on a tie) and the spread of the routed lengths.
#[derive(Clone, Copy, Debug)]
pub struct Comparison {
    pub reference: NetId,
    pub spread: i64,
    pub complete: bool,
    pub measurement: Measurement,
}

/// Measure each requested or matched net once. A board without either skips the track pass.
pub fn net_lengths(board: &Board, conn: &Connectivity, requested: &[NetId]) -> HashMap<NetId, NetLength> {
    measure(board, conn, requested, None).0
}

/// Measure the proposed path with the track's existing anchors and layer. Round once, alongside
/// the net's other copper and barrels, exactly as the committed read will.
pub fn with_path(board: &Board, conn: &Connectivity, track: &crate::Track, path: &Path) -> NetLength {
    measure(board, conn, &[track.net], Some((track.id, path))).0[&track.net]
}

fn measure(board: &Board, conn: &Connectivity, requested: &[NetId], replacement: Option<(ItemId, &Path)>) -> (HashMap<NetId, NetLength>, Measurement) {
    #[derive(Default)]
    struct Sum {
        tracks: f64,
        barrels: i64, // half-nanometres
        missing_barrel: bool,
        at: Option<P>,
    }
    let mut sums: HashMap<_, _> = board.length_matches.iter().filter(|_| replacement.is_none()).flat_map(|m| &m.nets)
        .chain(requested).map(|&n| (n, Sum::default())).collect();
    let depths = CopperDepths::new(&board.stackup);
    let mut measurement = depths.measurement;
    if sums.is_empty() { return (HashMap::new(), measurement); }
    let mut anchored = HashMap::new();
    for track in &board.tracks {
        let Some(sum) = sums.get_mut(&track.net) else { continue };
        sum.at.get_or_insert(track.path.start);
        sum.tracks += replacement.filter(|(id, _)| *id == track.id).map_or(&track.path, |(_, p)| p).length();
        if depths.measurement == Measurement::WithBarrels {
            for anchor in track.ends {
                if let Anchor::Via(id) = anchor {
                    let layers = anchored.entry((track.net, id)).or_insert(LayerSet::EMPTY);
                    *layers = layers.with(track.layer);
                }
            }
        }
    }
    for ((net, id), layers) in anchored {
        if layers.len() < 2 { continue; }
        let Some((ItemKind::Via, i)) = board.locate(id) else { continue };
        if board.vias[i].net != net { continue; }
        let sum = sums.get_mut(&net).expect("only measured tracks contribute anchors");
        if let Some(distance) = depths.barrel(layers) {
            sum.barrels += distance;
        } else {
            sum.missing_barrel = true;
            measurement = Measurement::NoStackup;
        }
    }
    // No track can also mean fully connected through touching pads or a pour. Distinguish
    // that from a net with no board copper at all.
    let mut empty: HashSet<_> = sums.iter().filter(|(_, sum)| sum.at.is_none()).map(|(&n, _)| n).collect();
    if !empty.is_empty() {
        for cluster in &conn.clusters {
            for net in &cluster.nets { empty.remove(net); }
            if empty.is_empty() { break; }
        }
    }
    let lengths = sums.into_iter().map(|(n, sum)| (n, NetLength {
        at: sum.at,
        nm: (sum.tracks + sum.barrels as f64 * 0.5).round() as i64,
        measurement: if sum.missing_barrel { Measurement::NoStackup } else { depths.measurement },
        partial: empty.contains(&n) || conn.unrouted.binary_search(&n).is_ok(),
    })).collect();
    (lengths, measurement)
}

/// Readout shared by the Lengths list and the board read. A circuit pin may own several
/// pads, so branching counts logical pins, independently of how much copper joins them.
pub struct LengthReadout {
    pub measurement: Measurement,
    pub lengths: HashMap<NetId, NetLength>,
    pub branched: HashSet<NetId>,
}

impl LengthReadout {
    pub fn new(board: &Board, circuit: &fab_elec_circuit::Circuit, conn: &Connectivity, requested: &[NetId]) -> Self {
        let (lengths, measurement) = measure(board, conn, requested, None);
        let mut counts = HashMap::new();
        if !lengths.is_empty() {
            for comp in &circuit.comps {
                for &(_, n) in &comp.conns {
                    if lengths.contains_key(&n) { *counts.entry(n).or_insert(0usize) += 1; }
                }
            }
        }
        Self { lengths, measurement, branched: counts.into_iter().filter(|(_, count)| *count > 2).map(|(n, _)| n).collect() }
    }
}

impl LengthMatch {
    pub fn compare(&self, lengths: &HashMap<NetId, NetLength>) -> Option<Comparison> {
        let mut reference = *self.nets.first()?;
        let mut shortest = lengths.get(&reference)?.nm;
        let mut longest = shortest;
        let mut complete = true;
        let mut measurement = Measurement::WithBarrels;
        for net in &self.nets {
            let length = lengths.get(net)?;
            complete &= !length.partial;
            if length.measurement == Measurement::NoStackup { measurement = Measurement::NoStackup; }
            shortest = shortest.min(length.nm);
            if length.nm > longest {
                longest = length.nm;
                reference = *net;
            }
        }
        Some(Comparison { reference, spread: longest - shortest, complete, measurement })
    }
}

/// Two decimal places for the board's millimetre readout.
pub fn mm_text(nm: i64) -> String {
    format!("{:.2} mm", nm as f64 / 1e6)
}

/// Give a near-limit difference enough decimal places to show which side of the tolerance it falls on.
pub fn difference_text(nm: i64, tol: Nm) -> (String, String) {
    let actual = nm as f64 / 1e6;
    let allowed = f64::from(tol) / 1e6;
    for precision in 2..=6 {
        let a = format!("{actual:.precision$} mm");
        let t = format!("{allowed:.precision$} mm");
        if nm == i64::from(tol) || a != t || precision == 6 { return (a, t); }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect, Anchor, Origin, Track};
    use fab_elec_core::{mm, BoardId, ItemId, LayerId, Path, P};

    #[test]
    fn line_and_semicircle_length_with_a_layer_change() {
        use crate::{drc, resolve_rules, Flash, NetPair, Tent, Via, ViaKind};
        use fab_elec_circuit::CheckKind;
        let mut board = Board::new(BoardId(0), "line and arc");
        board.nets.push(crate::BoardNet { name: "A".into(), class: fab_elec_core::ClassId(0) });
        board.tracks.push(Track {
            id: ItemId(0), net: NetId(0), layer: LayerId::F_CU,
            w: mm(0.2), widths: None, ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
            path: Path::new(P::ZERO).line(P::mm(3.0, 4.0)).arc(P::mm(4.0, 5.0), P::mm(5.0, 4.0)),
        });
        // A 3-4-5 triangle's hypotenuse plus half a circle of radius 1 mm.
        let expected = 5_000_000 + (std::f64::consts::PI * 1_000_000.0).round() as i64;
        assert_eq!(net_lengths(&board, &connect(&board, &[]), &[NetId(0)])[&NetId(0)].nm, expected);

        let (a, b) = (NetId(0), NetId(1));
        board.nets.push(crate::BoardNet { name: "B".into(), class: fab_elec_core::ClassId(0) });
        let whole = board.tracks[0].clone();
        board.tracks.push(Track {
            id: ItemId(1), net: b, path: whole.path.translated(P::mm(0.0, 8.0)), ..whole.clone()
        });
        let turn = P::mm(3.0, 4.0);
        let via = ItemId(3);
        board.tracks[0].path = Path::new(P::ZERO).line(turn);
        board.tracks[0].ends[1] = Anchor::Via(via);
        board.tracks.push(Track {
            id: ItemId(2), layer: LayerId::B_CU, ends: [Anchor::Via(via), Anchor::Free],
            path: Path::new(turn).arc(P::mm(4.0, 5.0), P::mm(5.0, 4.0)), ..whole
        });
        board.vias.push(Via {
            id: via, net: a, at: turn, span: (LayerId::F_CU, LayerId::B_CU), drill: mm(0.3),
            pad: mm(0.6), kind: ViaKind::Through, tent: Tent::Both, backdrill: None,
            locked: false, origin: Origin::Authored, flash: Flash::All,
        });
        board.reindex();
        board.pairs.push(NetPair { p: a, n: b, gap: None });
        board.length_matches.push(LengthMatch { nets: vec![a, b], tol: mm(1.5) });
        let findings = |board: &Board| {
            drc(board, &connect(board, &[]), &resolve_rules(board, None), &[], None).violations.into_iter()
                .filter(|v| matches!(v.kind, CheckKind::Length | CheckKind::Skew)).collect::<Vec<_>>()
        };
        let read = |board: &Board| net_lengths(board, &connect(board, &[]), &[]);
        // Standard stack: half of each 35 µm copper foil plus its 1510 µm core.
        let thickness = |kind| i64::from(board.stackup.layers.iter().find(|l| l.kind == kind).unwrap().thickness);
        let barrel = thickness(StackKind::Copper(LayerId::F_CU)) / 2
            + thickness(StackKind::Dielectric { core: true }) + thickness(StackKind::Copper(LayerId::B_CU)) / 2;
        assert_eq!(barrel, 1_545_000);
        let lengths = read(&board);
        assert_eq!(lengths[&a].nm, expected + barrel);
        assert_eq!(lengths[&b].nm, expected);
        assert_eq!(lengths[&a].measurement.text(), "track length plus via barrels");
        let skew = findings(&board);
        assert_eq!(skew.len(), 1);
        assert_eq!(skew[0].kind, CheckKind::Skew);
        board.length_matches[0].tol = mm(1.55);
        assert!(findings(&board).is_empty());
        board.length_matches[0].tol = mm(1.5);
        board.tracks[2].layer = LayerId::F_CU;
        assert_eq!(read(&board)[&a].nm, expected, "two anchors on the same layer leave the barrel unused");
        board.tracks[2].layer = LayerId::B_CU;

        // A through via used only down to an inner layer leaves its bottom stub unused.
        let mut multilayer = board.clone();
        let bottom = multilayer.stackup.layers.iter().position(|l| l.kind == StackKind::Copper(LayerId::B_CU)).unwrap();
        let mut inner = multilayer.stackup.layers[bottom].clone();
        inner.kind = StackKind::Copper(LayerId::inner(1));
        inner.thickness = 17_000;
        let mut prepreg = multilayer.stackup.layers[bottom - 1].clone();
        prepreg.thickness = 200_000;
        multilayer.stackup.layers.splice(bottom - 1..bottom - 1, [prepreg, inner]);
        multilayer.tracks[2].layer = LayerId::inner(1);
        multilayer.reindex();
        assert_eq!(read(&multilayer)[&a].nm, expected + 17_500 + 200_000 + 8_500);

        // Keep the nominal board and copper thicknesses, but remove the core thickness.
        board.stackup.layers.iter_mut().find(|l| matches!(l.kind, StackKind::Dielectric { .. })).unwrap().thickness = 0;
        let lengths = read(&board);
        assert_eq!(lengths[&a].measurement.text(), "vias not counted: no stackup");
        assert_eq!(lengths[&a].nm, lengths[&b].nm);
        assert!(findings(&board).is_empty());
        board.stackup.layers.retain(|l| matches!(l.kind, StackKind::Copper(_)));
        assert_eq!(read(&board)[&a].measurement, Measurement::NoStackup);
        board.tracks[1].path = Path::new(P::mm(0.0, 8.0)).line(P::mm(3.0, 12.0));
        for kind in [CheckKind::Skew, CheckKind::Length] {
            let found = findings(&board);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].kind, kind);
            assert!(found[0].message.contains("vias not counted: no stackup"));
            board.pairs.clear();
        }
    }
    #[test]
    fn matches_follow_connectivity_and_replace_remote_findings() {
        use crate::{drc, drc::drc_indexed, resolve_rules, BoardDelta, BoardNet, NetPair, PieceIndex};
        use fab_elec_circuit::CheckKind;
        use fab_elec_core::{ClassId, Contour, NetRef, Sel};

        let mut board = Board::new(BoardId(0), "lengths");
        board.outline.push(Contour::rect(P::ZERO, P::mm(20.0, 20.0), 0));
        let (p, n, q) = (NetId(0), NetId(1), NetId(2));
        for (i, (name, len)) in [("P", 8.0), ("N", 6.0), ("Q", 7.9)].into_iter().enumerate() {
            board.nets.push(BoardNet { name: name.into(), class: ClassId(0) });
            let y = 2.0 + i as f64;
            board.tracks.push(Track {
                id: ItemId(i as u32), net: NetId(i as u32), layer: LayerId::F_CU,
                w: mm(0.2), widths: None, ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
                path: Path::new(P::mm(1.0, y)).line(P::mm(1.0 + len, y)),
            });
        }
        board.pairs.push(NetPair { p, n, gap: None });
        board.length_matches.push(LengthMatch { nets: vec![n, p], tol: mm(0.1) });
        let run = |board: &Board, prev: &[_]| {
            let conn = connect(board, &[]);
            let rules = resolve_rules(board, None);
            let full = drc(board, &conn, &rules, &[], None);
            let delta = BoardDelta::default();
            let indexed = drc_indexed(prev, &PieceIndex::build(board, &[]), board, &conn, &rules, &delta, &[]);
            let only = |rows: Vec<fab_elec_circuit::Violation>| rows.into_iter()
                .filter(|v| matches!(v.kind, CheckKind::Skew | CheckKind::Length)).collect::<Vec<_>>();
            let found = only(full.violations);
            assert_eq!(only(indexed), found);
            found
        };
        let broken = run(&board, &[]);
        let net = |net| Sel::Net(NetRef { board: BoardId(0), net });
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].kind, CheckKind::Skew);
        assert_eq!(broken[0].items, vec![net(p), net(n)]);
        board.length_matches[0].tol = mm(2.0);
        assert!(run(&board, &broken).is_empty(), "exactly at tolerance passes");
        board.length_matches[0] = LengthMatch { nets: vec![q, n, p], tol: mm(0.2) };
        let short = run(&board, &broken);
        assert_eq!(short.len(), 1);
        assert_eq!(short[0].kind, CheckKind::Length);
        assert_eq!(short[0].items, vec![net(n)]);
        assert!(short[0].message.contains("N is 2.00 mm shorter than P"));
        let mut island = board.tracks[2].clone();
        island.id = ItemId(3);
        island.path = island.path.translated(P::mm(0.0, 4.0));
        board.tracks.push(island);
        assert!(run(&board, &short).is_empty(), "one open member suspends the whole group");
        board.length_matches[0] = LengthMatch { nets: vec![p, q], tol: mm(0.1) };
        board.pairs[0].n = q;
        assert!(run(&board, &broken).is_empty(), "one open half suspends the pair");
    }

}
