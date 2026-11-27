//! Negotiated congestion must try another corridor after rollback, rip every blockade of a net
//! cut off in several places, pass a displacement on (a ripped net rips in turn), stream removals,
//! move the job's own pour repairs, and preserve authored and locked copper.

use fab_elec_board::drc::sel_of;
use fab_elec_board::zone::fill_all;
use fab_elec_board::{
    connect, drc, resolve_rules, Anchor, Area, Board, BoardNet, ConnItem, Connectivity, Item, Origin, Placed,
    RuleSet, StackKind, Track, Zone, ZoneSettings,
};
use fab_elec_circuit::{CheckKind, ClassRules, NetClass, ViaSize};
use fab_elec_core::{
    mm, Angle, BBox, BoardId, ClassId, CompId, Contour, LayerId, LayerSet, NetId, Nm, PadIx, Path, PolySet, Seg,
    Sel, Side, P,
};
use fab_elec_design::fixture::fp;
use fab_elec_parts::{Footprint, FpAttrs, FpMount, Pad, PadShape};
use fab_elec_route::auto::GridRouter;
use fab_elec_route::{
    problem, scope_nets, solvers, Autorouter, Base, Effort, Progress, RBase, Reason, RetryWhy,
    RouteProblem, RouteScope, RouteSolution, Style,
};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const M1: NetId = NetId(0);
const M2: NetId = NetId(1);
const N: NetId = NetId(2);

/// A one-layer board `size` mm, with the Wide (0.5 mm) and Narrow (0.25 mm) classes at 0.2 mm
/// clearance and `nets` in order (M1, M2, N).
fn one_layer(name: &str, size: P, nets: [(&str, ClassId); 3]) -> Board {
    let mut board = Board::new(BoardId(0), name);
    board.outline = vec![Contour::new(fp::rect_path(P::ZERO, size))];
    board.stackup.layers.retain(|layer| matches!(layer.kind, StackKind::Copper(LayerId::F_CU)));
    board.rules.base = RuleSet {
        track: Some(mm(0.25)),
        clearance: Some(mm(0.20)),
        via: Some(ViaSize { drill: mm(0.40), pad: mm(0.80) }),
        ..RuleSet::standard()
    };
    for (id, name, w) in [(ClassId(0), "Wide", 0.50), (ClassId(1), "Narrow", 0.25)] {
        board.classes.push(NetClass {
            id,
            name: name.into(),
            rules: ClassRules {
                track: Some(mm(w)),
                clearance: Some(mm(0.20)),
                via: board.rules.base.via,
                ..Default::default()
            },
        });
    }
    for (name, class) in nets {
        board.nets.push(BoardNet { name: name.into(), class });
    }
    board
}

/// A wall of netless rectangular pads, one per (centre, size).
fn wall(pads: &[(P, P)]) -> Vec<Pad> {
    pads.iter().enumerate().map(|(i, &(at, size))| {
        let mut pad = fp::smd(&(i + 1).to_string(), at, size);
        pad.stack.front = PadShape::Rect { size };
        pad
    }).collect()
}

/// Place one locked part per (name, at, pads, pad nets); a net's single pad is 0.6 mm square.
fn place(board: &mut Board, parts: Vec<(&str, P, Vec<Pad>, Vec<Option<NetId>>)>) {
    for (name, at, pads, pad_nets) in parts {
        let id = board.alloc_id();
        board.placed.push(Placed {
            id,
            comp: CompId(board.placed.len() as u32),
            path: name.into(),
            refdes: name.into(),
            footprint: Arc::new(Footprint {
                name: name.into(),
                pads,
                attrs: FpAttrs { mount: FpMount::Smd, ..Default::default() },
                ..Default::default()
            }),
            pad_nets,
            at,
            rot: Angle::ZERO,
            side: Side::Top,
            locked: true,
            origin: Origin::Authored,
            body_z: None,
            ref_label: None,
            value_label: None, legend: None,
            value: None,
        });
    }
}

fn pin(name: &str, at: P, net: NetId) -> (&str, P, Vec<Pad>, Vec<Option<NetId>>) {
    square_pin(name, at, net, 0.6)
}

/// One part with a single square pad `side` mm wide on `net`.
fn square_pin(name: &str, at: P, net: NetId, side: f64) -> (&str, P, Vec<Pad>, Vec<Option<NetId>>) {
    let size = P::mm(side, side);
    let mut pad = fp::smd("1", P::ZERO, size);
    pad.stack.front = PadShape::Rect { size };
    (name, at, vec![pad], vec![Some(net)])
}

/// One wall across a 12 x 12 mm board at x = 6 mm, with three gaps: g1 at y = 6.4..7.6 and g2 at
/// y = 4.4..5.6 (1.2 mm each), where a wide net fits alone (it needs 0.9 mm) but not beside a
/// narrow track (1.35 mm), and g3 at y = 1.0..1.85 (0.85 mm) that only a narrow track fits
/// (0.65 mm). The wide M1 takes g1 and the narrow M2 g2, their shorter ways, which leaves the
/// wide N none. `m2_track` gives M2 a track through g2 from the start, with its origin and lock.
fn gaps(m2_track: Option<(Origin, bool)>) -> Board {
    let mut board = one_layer("rip-up gaps", P::mm(12.0, 12.0), [("M1", ClassId(0)), ("M2", ClassId(1)), ("N", ClassId(0))]);
    let wall = wall(&[
        (P::mm(6.0, 0.5), P::mm(0.8, 1.0)),
        (P::mm(6.0, 3.125), P::mm(0.8, 2.55)),
        (P::mm(6.0, 6.0), P::mm(0.8, 0.8)),
        (P::mm(6.0, 9.8), P::mm(0.8, 4.4)),
    ]);
    place(&mut board, vec![
        ("WALL", P::ZERO, wall, vec![None; 4]),
        pin("M1L", P::mm(3.0, 9.5), M1),
        pin("M1R", P::mm(9.0, 9.5), M1),
        pin("M2L", P::mm(3.0, 3.6), M2),
        pin("M2R", P::mm(9.0, 3.6), M2),
        pin("NL", P::mm(1.0, 7.0), N),
        pin("NR", P::mm(11.0, 7.0), N),
    ]);
    if let Some((origin, locked)) = m2_track {
        let ends = ["M2L", "M2R"].map(|name| {
            let placed = board.placed.iter().find(|placed| placed.refdes == name)
                .expect("fixture: each M2 track endpoint has a placed pad").id;
            Anchor::Pad { placed, pad: PadIx(0) }
        });
        let id = board.alloc_id();
        board.tracks.push(Track {
            id,
            net: M2,
            layer: LayerId::F_CU,
            w: mm(0.25),
            widths: None,
            path: Path::new(P::mm(3.0, 3.6))
                .line(P::mm(4.4, 5.0)).line(P::mm(7.6, 5.0)).line(P::mm(9.0, 3.6)),
            ends,
            locked,
            origin,
        });
    }
    board.reindex();
    board
}

/// Two walls across an 18 x 12 mm board, at x = 6 and x = 12 mm. Each has a 1.2 mm gap at
/// y = 5.4..6.6, where the wide N fits alone (it needs 0.9 mm) but not beside a narrow track
/// (1.35 mm), and a 0.85 mm gap at y = 1.0..1.85 that only a narrow track fits (0.65 mm). N
/// (wide) has a pad before, between and beyond the walls; the narrow M1 and M2 each cross one
/// wall, M1 the first and M2 the second, by the 1.2 mm gap while it is free (their shorter way).
fn walls() -> Board {
    let mut board = one_layer("rip-up walls", P::mm(18.0, 12.0), [("M1", ClassId(1)), ("M2", ClassId(1)), ("N", ClassId(0))]);
    let pads = |x: f64| [
        (P::mm(x, 0.5), P::mm(0.8, 1.0)),
        (P::mm(x, 3.625), P::mm(0.8, 3.55)),
        (P::mm(x, 9.3), P::mm(0.8, 5.4)),
    ];
    place(&mut board, vec![
        ("WALL1", P::ZERO, wall(&pads(6.0)), vec![None; 3]),
        ("WALL2", P::ZERO, wall(&pads(12.0)), vec![None; 3]),
        pin("M1L", P::mm(4.0, 4.5), M1),
        pin("M1R", P::mm(8.0, 4.5), M1),
        pin("M2L", P::mm(10.0, 4.5), M2),
        pin("M2R", P::mm(14.0, 4.5), M2),
        pin("NL", P::mm(1.0, 6.0), N),
        pin("NM", P::mm(9.0, 6.0), N),
        pin("NR", P::mm(17.0, 6.0), N),
    ]);
    board.reindex();
    board
}

/// One wall across a 12 x 14 mm board at x = 6 mm, with three gaps: g1 at y = 10.0..12.22
/// (2.22 mm), g2 at y = 5.0..6.62 (1.62 mm) and g3 at y = 1.0..2.05 (1.05 mm). The bus N (1.4 mm
/// track, needs 1.8 mm) fits g1 only, and not beside a narrow track (2.25 mm). M1 (0.8 mm, needs
/// 1.2 mm) fits g1 or g2, but not g2 beside a narrow track (1.65 mm). The narrow M2 (0.25 mm,
/// needs 0.65 mm) fits every gap. Each fit leaves the grid at least two pitches. M2, shortest and first in plan order, takes g2, its way; M1 then has
/// only g1, its long way, which N needs.
fn corridor() -> Board {
    let mut board = one_layer("rip-up corridor", P::mm(12.0, 14.0), [("M1", ClassId(2)), ("M2", ClassId(1)), ("N", ClassId(3))]);
    for (id, name, w) in [(ClassId(2), "Mid", 0.80), (ClassId(3), "Bus", 1.40)] {
        board.classes.push(NetClass {
            id,
            name: name.into(),
            rules: ClassRules {
                track: Some(mm(w)),
                clearance: Some(mm(0.20)),
                via: board.rules.base.via,
                ..Default::default()
            },
        });
    }
    let wall = wall(&[
        (P::mm(6.0, 0.5), P::mm(0.8, 1.0)),
        (P::mm(6.0, 3.525), P::mm(0.8, 2.95)),
        (P::mm(6.0, 8.31), P::mm(0.8, 3.38)),
        (P::mm(6.0, 13.11), P::mm(0.8, 1.78)),
    ]);
    place(&mut board, vec![
        ("WALL", P::ZERO, wall, vec![None; 4]),
        square_pin("M1L", P::mm(4.0, 7.5), M1, 1.0),
        square_pin("M1R", P::mm(8.0, 7.5), M1, 1.0),
        pin("M2L", P::mm(4.2, 4.5), M2),
        pin("M2R", P::mm(7.8, 4.5), M2),
        square_pin("NL", P::mm(1.2, 11.1), N, 1.6),
        square_pin("NR", P::mm(10.8, 11.1), N, 1.6),
    ]);
    board.reindex();
    board
}

const G: NetId = NetId(0);
const A: NetId = NetId(1);
const B: NetId = NetId(3);

/// A 14 x 16 mm board with a pour of G on a strip at x = 6..8 mm that joins G's three pads. The
/// short A crosses the strip at y = 6 and cuts it, so the job repairs G, by the short way round
/// A's left pad. Two walls box N's pad in beside that pad, open to the left, where the repair
/// passes and closes the box. The long way for G is a 0.9 mm gap at x = 12.0..12.9 in a wall from
/// A's right pad to the edge. N's other pad sits in the corner, where nothing passes, so once N
/// routes, G has only the long way left. The short B crosses the strip at y = 12, far from N, and
/// the job repairs that cut too. Vias are wider than the strip, so no via drops into it (on one
/// layer such a via would join one layer only) and a repair joins G's pads by track.
fn repair_box() -> Board {
    let mut board = one_layer("rip-up repair box", P::mm(14.0, 16.0), [("G", ClassId(1)), ("A", ClassId(1)), ("N", ClassId(1))]);
    board.nets.push(BoardNet { name: "B".into(), class: ClassId(1) });
    let wide = Some(ViaSize { drill: mm(0.40), pad: mm(2.4) });
    board.rules.base.via = wide;
    for class in &mut board.classes {
        class.rules.via = wide;
    }
    let wall = wall(&[
        (P::mm(4.8, 5.25), P::mm(1.4, 0.5)),
        (P::mm(4.8, 6.75), P::mm(1.4, 0.5)),
        (P::mm(10.65, 6.0), P::mm(2.7, 0.8)),
        (P::mm(13.2, 6.0), P::mm(0.6, 0.8)),
    ]);
    place(&mut board, vec![
        ("WALL", P::ZERO, wall, vec![None; 4]),
        pin("G1", P::mm(7.0, 2.5), G),
        pin("G2", P::mm(7.0, 9.5), G),
        pin("A1", P::mm(5.2, 6.0), A),
        pin("A2", P::mm(8.8, 6.0), A),
        pin("N1", P::mm(4.4, 6.0), N),
        pin("N2", P::mm(0.8, 0.8), N),
        pin("G3", P::mm(7.0, 13.8), G),
        pin("B1", P::mm(5.2, 12.0), B),
        pin("B2", P::mm(8.8, 12.0), B),
    ]);
    let id = board.alloc_id();
    board.zones.push(Zone {
        id, net: Some(G), name: "G".into(), layers: LayerSet::one(LayerId::F_CU),
        area: Area::Poly(PolySet::rect(BBox::new(P::mm(6.0, 1.0), P::mm(8.0, 15.0)))), priority: 0,
        settings: ZoneSettings::default(), locked: false, origin: Origin::Authored,
    });
    board.reindex();
    board
}

/// A 14 x 16 mm board with a pour of G on a strip at x = 6..8 mm under G's pads G1, G2 and G3,
/// and G's pad GX off the strip, so G is in the problem and routes last (its pour serves it).
/// The short A and B cross the strip at y = 6 and y = 12 and cut it into three islands before G
/// routes. G's pad GB, placed before the others, sits in a closed ring of netless pads: nothing
/// reaches it. Vias are wider than the strip, as in `repair_box`.
fn islands() -> Board {
    let mut board = one_layer("unreachable pin", P::mm(14.0, 16.0), [("G", ClassId(1)), ("A", ClassId(1)), ("B", ClassId(1))]);
    let wide = Some(ViaSize { drill: mm(0.40), pad: mm(2.4) });
    board.rules.base.via = wide;
    for class in &mut board.classes {
        class.rules.via = wide;
    }
    let ring = wall(&[
        (P::mm(0.9, 13.0), P::mm(0.4, 2.6)),
        (P::mm(3.1, 13.0), P::mm(0.4, 2.6)),
        (P::mm(2.0, 11.9), P::mm(2.6, 0.4)),
        (P::mm(2.0, 14.1), P::mm(2.6, 0.4)),
    ]);
    place(&mut board, vec![
        pin("GB", P::mm(2.0, 13.0), G),
        ("RING", P::ZERO, ring, vec![None; 4]),
        pin("G1", P::mm(7.0, 2.5), G),
        pin("G2", P::mm(7.0, 9.0), G),
        pin("G3", P::mm(7.0, 14.5), G),
        pin("GX", P::mm(11.5, 9.0), G),
        pin("A1", P::mm(5.2, 6.0), A),
        pin("A2", P::mm(8.8, 6.0), A),
        pin("B1", P::mm(5.2, 12.0), NetId(2)),
        pin("B2", P::mm(8.8, 12.0), NetId(2)),
    ]);
    let id = board.alloc_id();
    board.zones.push(Zone {
        id, net: Some(G), name: "G".into(), layers: LayerSet::one(LayerId::F_CU),
        area: Area::Poly(PolySet::rect(BBox::new(P::mm(6.0, 1.0), P::mm(8.0, 15.0)))), priority: 0,
        settings: ZoneSettings::default(), locked: false, origin: Origin::Authored,
    });
    board.reindex();
    board
}

fn setup(board: Board) -> RouteProblem {
    let board = Arc::new(board);
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let base = Base::new(board, rules, conn, fills);
    let nets = scope_nets(&base.board, &base.conn, &RouteScope::All);
    let world = Arc::new(RBase::build(&base.board));
    problem(&base, world, &nets, Style::FortyFive, Effort::default())
}

/// The router passes where its padded plane says nothing can: its search takes the exact step rule,
/// so a 0.70 mm opening that every padded cell calls hard still routes. And a via pad, legal under a
/// keepout that forbids only tracks, joins copper on both sides of it, and the router's stubs off
/// that pad stay legal once the board snaps them to its centre. (Both boards disproved a capacity
/// bound that claimed these nets must stay open.)
#[test]
fn a_route_crosses_an_opening_the_padded_plane_calls_hard_and_a_track_keepout_by_a_via_pad() {
    use fab_elec_route::grid::{Cell, Grid, GridSpec, Occ, Probe};
    use fab_elec_route::{World, WorldSource};

    let narrow = ClassId(1);
    let mut board = one_layer("narrow gate", P::mm(24.0, 16.8), [("signal", narrow); 3]);
    board.nets.truncate(1);
    // The right wall has a 0.70 mm opening centred on a grid row.
    let ring = wall(&[
        (P::mm(8.4, 4.8), P::mm(7.8, 0.6)),
        (P::mm(8.4, 12.0), P::mm(7.8, 0.6)),
        (P::mm(4.8, 8.4), P::mm(0.6, 7.2)),
        (P::mm(12.0, 6.425), P::mm(0.6, 3.25)),
        (P::mm(12.0, 10.375), P::mm(0.6, 3.25)),
    ]);
    place(&mut board, vec![
        pin("U1", P::mm(8.4, 8.4), M1),
        pin("J1", P::mm(18.9, 8.4), M1),
        ("WALL", P::ZERO, ring, vec![None; 5]),
    ]);
    board.reindex();
    let mut p = setup(board);
    p.effort = Effort::Fast;
    let [net] = &p.nets[..] else { panic!("exactly one net crosses the ring") };
    let probe = Probe { class: net.class, w: net.w[0], drill: None };
    let key = fab_elec_board::RuleKey { class: net.class, item: fab_elec_board::RuleItem::Track };
    let pitch = GridSpec::pitch_for(probe.w + p.rules.clearance(key, key, LayerId::F_CU));
    let spec = GridSpec::covering(p.board.outline_bbox(), pitch, p.stack.layers.clone());
    assert_eq!(pitch, mm(0.15), "the gate lies on the router's grid");
    let world = World::new(&p.base);
    let owned = |it: &fab_elec_route::RItem| p.owned(it);
    let source = WorldSource::new(&world, &p.rules, &p.board, &owned);
    let low = spec.cell_at(P::mm(4.8, 4.8));
    let high = spec.cell_at(P::mm(12.0, 12.0));
    let gap = spec.cell_at(P::mm(12.0, 8.4));
    let mut grid = Grid::new(spec);
    let plane = grid.plane(probe);
    for i in low.x..=high.x {
        for cell in [Cell { x: i, y: low.y }, Cell { x: i, y: high.y }, Cell { x: low.x, y: i }, Cell { x: high.x, y: i }] {
            assert_eq!(grid.state(&source, plane, 0, cell).fixed, Occ::HARD.fixed,
                "even the opening is hard in the padded plane, closing the 8-neighbour cut at {cell:?}");
        }
    }
    assert_eq!(grid.step(&source, plane, 0, gap, gap).fixed, Occ::FREE.fixed,
        "the exact track centre in the opening clears the fixed wall");
    let sol = GridRouter::default().route(&p, &Progress::new());
    assert!(sol.open.is_empty(), "the router can pass through the opening: {:?}", sol.open);
    let (_, conn) = apply_clean(&p, &sol);
    assert!(conn.unrouted.is_empty(), "the verified route connects the terminals across the padded cut");

    // A ring can forbid tracks yet permit a via's annular copper. Its disc can join copper
    // on opposite sides even though no track centreline can cross the ring on either layer.
    let mut board = one_layer("via annulus across a track keepout", P::mm(21.0, 18.0), [("signal", narrow); 3]);
    board.nets.truncate(1);
    board.stackup = fab_elec_board::Stackup::standard_2_layer();
    let size = ViaSize { pad: mm(9.0), drill: mm(0.4) };
    board.classes[narrow.ix()].rules.via = Some(size);
    place(&mut board, vec![
        square_pin("U1", P::mm(10.5, 9.0), M1, 0.3),
        square_pin("J1", P::mm(10.5, 9.0), M1, 0.3),
        square_pin("J2", P::mm(13.95, 9.0), M1, 0.3),
    ]);
    board.placed[1].side = Side::Bottom;
    let outer = BBox::new(P::mm(8.88, 7.38), P::mm(12.12, 10.62));
    let inner = BBox::new(P::mm(9.72, 8.22), P::mm(11.28, 9.78));
    let id = board.alloc_id();
    board.keepouts.push(fab_elec_board::BoardKeepout {
        id, area: PolySet::rect(outer).difference(&PolySet::rect(inner)), layers: board.copper_set(),
        forbid: fab_elec_parts::Forbid::TRACKS, reason: String::new(), origin: Origin::Authored, z: None,
    });
    board.reindex();
    let mut p = setup(board);
    p.effort = Effort::Fast;
    // Its stubs leave the via from the centre, where the board puts an anchored end, so the
    // applied copper is the copper the router checked (apply_clean runs DRC on it).
    let route = GridRouter::default().route(&p, &Progress::new());
    let (_, connected) = apply_clean(&p, &route);
    assert!(connected.unrouted.is_empty(), "the router's via pad joins all three terminals across the track keepout: {:?}", route.open);
    assert!(route.open.is_empty(), "the router reports the net joined by its via pad as routed: {:?}", route.open);
    assert_eq!(route.metrics.nets_open as usize, route.open.len(), "the open-net count matches the via-pad result");
}


/// A zone island that touches no copper of its net is discarded until the router's tracks reach it:
/// the refill then keeps it, and it conducts across a ring that forbids tracks and vias but not
/// zone copper. The settled board is connected and passes DRC.
#[test]
fn a_refill_keeps_the_island_the_routed_tracks_reach_across_a_keepout() {
    let narrow = ClassId(1);
    let mut board = one_layer("retained copper across a routing keepout", P::mm(16.0, 15.0), [("signal", narrow); 3]);
    board.nets.truncate(1);
    place(&mut board, vec![
        pin("U1", P::mm(6.0, 6.0), M1), pin("U2", P::mm(6.0, 9.0), M1),
        pin("J1", P::mm(13.2, 6.0), M1), pin("J2", P::mm(13.2, 9.0), M1),
    ]);
    let outer = PolySet::rect(BBox::new(P::mm(3.0, 4.5), P::mm(9.0, 10.5)));
    let inner = PolySet::rect(BBox::new(P::mm(3.8, 5.3), P::mm(8.2, 9.7)));
    let id = board.alloc_id();
    board.keepouts.push(fab_elec_board::BoardKeepout {
        id, area: outer.difference(&inner), layers: board.copper_set(),
        forbid: fab_elec_parts::Forbid::TRACKS.with(fab_elec_parts::Forbid::VIAS),
        reason: String::new(), origin: Origin::Authored, z: None,
    });
    let zone = board.alloc_id();
    board.zones.push(Zone {
        id: zone, net: Some(M1), name: "signal".into(), layers: board.copper_set(),
        area: Area::Poly(PolySet::rect(BBox::new(P::mm(4.5, 7.0), P::mm(14.4, 8.0)))),
        priority: 0, settings: ZoneSettings::default(), locked: false, origin: Origin::Authored,
    });
    board.reindex();
    let mut p = setup(board);
    p.effort = Effort::Fast;
    assert!(p.nets[0].terminals.iter().all(|t| t.items.iter().all(|ci| !matches!(ci, ConnItem::Zone { .. }))),
        "the island starts discarded, so no terminal includes it");
    let sol = GridRouter::default().route(&p, &Progress::new());
    let (after, conn) = apply_clean(&p, &sol);
    let fills = fill_all(&after, &p.rules);
    let in_wall = P::mm(8.6, 7.5);
    assert!(p.fills[0].is_empty() && fills[0].layer(LayerId::F_CU).unwrap().kept().any(|(_, poly)| poly.contains(in_wall)),
        "refilling keeps copper inside the track-and-via keepout that the first fill discarded");
    assert!(conn.unrouted.is_empty(), "the retained island electrically connects the router's tracks on both sides");
    assert!(sol.open.is_empty(), "the router reports the net joined by the refilled island as routed: {:?}", sol.open);
    assert_eq!(sol.metrics.nets_open as usize, sol.open.len(), "the open-net count matches the refilled result");
    assert_eq!(sol.metrics.nets_routed, 1, "the routed-net count includes the net joined by the refill");
    let drc = drc(&after, &conn, &p.rules, &fills, None);
    assert!(drc.violations.is_empty(), "the complete refilled board passes DRC: {:?}", drc.violations);
}

fn has_copper(sol: &RouteSolution, net: NetId) -> bool {
    sol.add.iter().any(|item| match item {
        Item::Track(track) => track.net == net,
        Item::Via(via) => via.net == net,
        _ => false,
    })
}

fn blocked_by(sol: &RouteSolution, net: NetId, blockers: &[NetId]) -> bool {
    sol.open.iter().any(|(open, reason)| {
        *open == net && matches!(reason, Reason::Blocked { by, .. } if by.as_slice() == blockers)
    })
}

fn apply_clean(p: &RouteProblem, sol: &RouteSolution) -> (Board, Connectivity) {
    let mut after = (*p.board).clone();
    let applied = after.apply_all(&sol.ops());
    assert!(applied.is_ok(), "applies: rip-up operations must apply to the original board: {applied:?}");
    let fills = fill_all(&after, &p.rules);
    let conn = connect(&after, &fills);
    let added_ids: BTreeSet<_> = sol.add.iter().map(Item::id).collect();
    let mut added_sels: BTreeSet<_> = added_ids.iter().map(|&id| Sel::Item(p.board.unit, id)).collect();
    added_sels.extend(conn.clusters.iter().flat_map(|cluster| cluster.items.iter())
        .filter(|item| added_ids.contains(&item.item())).map(|&item| sel_of(&after, item)));
    let result = drc(&after, &conn, &p.rules, &fills, None);
    let offending: Vec<_> = result.violations.iter().filter(|v| {
        matches!(
            v.kind,
            CheckKind::Clearance
                | CheckKind::EdgeClearance
                | CheckKind::HoleClearance
                | CheckKind::HoleToHole
                | CheckKind::Keepout
                | CheckKind::Short
                | CheckKind::TrackWidth
                | CheckKind::ViaSize
                | CheckKind::AnnularRing
                | CheckKind::Drill
                | CheckKind::Dangling
                | CheckKind::DanglingVia
                | CheckKind::ViaInPad
        ) && v.items.iter().any(|sel| added_sels.contains(sel))
    }).collect();
    assert!(offending.is_empty(), "DRC: added rip-up copper must have no offending findings: {offending:#?}");
    (after, conn)
}

/// Every crossing of net `net`'s tracks with the vertical line at `x` lies in y = `lo`..`hi`.
fn assert_crosses_gap(sol: &RouteSolution, net: NetId, x: Nm, lo: Nm, hi: Nm) {
    let mut crossings = Vec::new();
    for item in &sol.add {
        let Item::Track(track) = item else { continue };
        if track.net != net {
            continue;
        }
        for (a, seg) in track.path.steps() {
            let Seg::Line(b) = seg else {
                panic!("gap geometry: FortyFive tracks must contain only lines, net={net:?}");
            };
            if x < a.x.min(b.x) || x > a.x.max(b.x) {
                continue;
            }
            if a.x == b.x {
                crossings.extend([i64::from(a.y), i64::from(b.y)]);
            } else {
                let y = i64::from(a.y)
                    + i64::from(x - a.x) * i64::from(b.y - a.y) / i64::from(b.x - a.x);
                crossings.push(y);
            }
        }
    }
    assert!(!crossings.is_empty(), "gap geometry: net {net:?} must cross x={x}");
    assert!(
        crossings.iter().all(|&y| i64::from(lo) < y && y < i64::from(hi)),
        "gap geometry: net {net:?} must cross x={x} inside y={lo}..{hi}, crossings={crossings:?}"
    );
}

#[test]
fn rip_up_moves_a_net_out_of_the_way_and_streams_it() {
    let p = setup(gaps(None));
    assert_eq!(p.stack.layers, vec![LayerId::F_CU], "fixture: routing must use only F.Cu");
    assert_eq!(
        p.nets.iter().map(|net| net.net).collect::<Vec<_>>(), vec![M1, M2, N],
        "fixture: short M1 and M2 must precede long N in plan order"
    );
    let partials = Arc::new(Mutex::new(Vec::new()));
    let output = partials.clone();
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let status_output = statuses.clone();
    let progress = Progress::with_sink(Duration::ZERO, move |step, partial| {
        output.lock().unwrap().push((step, partial));
    }).with_status(move |status| status_output.lock().unwrap().push(status));
    let sol = GridRouter::default().route(&p, &progress);
    let partials = std::mem::take(&mut *partials.lock().unwrap());
    let statuses = std::mem::take(&mut *statuses.lock().unwrap());
    let first = &partials.iter().find(|(step, _)| *step == 3)
        .expect("premise: step 3 must offer the completed first pass").1;
    assert_eq!(first.open.len(), 1, "premise: the first pass must leave exactly one net open");
    assert!(blocked_by(first, N, &[M1]), "premise: N must initially be blocked by M1 alone: {:?}", first.open);
    assert!(has_copper(first, M1), "premise: the first pass must add M1 copper");
    assert!(has_copper(first, M2), "premise: the first pass must add M2 copper");

    assert!(sol.open.is_empty(), "result: negotiation must route every net: {:?}", sol.open);
    let (_, conn) = apply_clean(&p, &sol);
    assert!(conn.unrouted.is_empty(), "connectivity: the solution must connect every net: {:?}", conn.unrouted);
    assert_crosses_gap(&sol, N, mm(6.0), mm(4.4), mm(5.6));
    assert_crosses_gap(&sol, M2, mm(6.0), mm(1.0), mm(1.85));
    assert_eq!(sol.metrics.work.rips, 2, "history: one failed M1 rip must precede the successful M2 rip");
    // Where the work went: each expansion is of one search kind, and each one after the first
    // pass belongs to one attempt, kept or rolled back.
    let work = &sol.metrics.work;
    let s = work.by_search;
    assert_eq!(s.widening + s.blockers + s.priced_blockers + s.what_if + s.escape, work.expansions, "work: the search kinds must sum to the expansions: {s:?}");
    assert_eq!(work.open_after_first_pass, 1, "work: the first pass must leave N open");
    let a = work.by_attempt;
    assert!(a.kept > 0 && a.rolled_back > 0, "work: the kept and the rolled-back attempt must each count work: {a:?}");
    assert_eq!(a.kept + a.rolled_back, work.expansions - first.metrics.work.expansions, "work: the attempts' fates must sum to the work after the first pass: {a:?}");
    assert!(a.after_unkeepable <= a.rolled_back, "work: work after an attempt could no longer be kept is part of its rollback: {a:?}");

    // At fast effort the job ends after the first pass: N stays open, blocked by M1, nothing is
    // ripped, and the work is the first pass's.
    let mut fast = p.clone();
    fast.effort = Effort::Fast;
    let quick = GridRouter::default().route(&fast, &Progress::new());
    assert!(blocked_by(&quick, N, &[M1]) && quick.open.len() == 1, "fast: the first pass's open net and its blocker are named: {:?}", quick.open);
    assert_eq!((quick.metrics.work.rips, quick.metrics.work.expansions), (0, first.metrics.work.expansions),
        "fast: no rip-up, only the first pass's work");

    assert!(
        partials.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "stream: offered transaction steps must strictly increase"
    );
    let (last_step, last) = partials.last().expect("stream: the completed route must have an offered partial");
    assert_eq!(*last_step, sol.metrics.work.transactions, "stream: final step must equal counted transactions");
    assert_eq!(last.as_ref(), &sol, "stream: the last partial must equal the returned solution");
    // The rip is offered on its own, before N routes: M1 is gone while N is still open.
    let m1_ripped = partials.iter().position(|(step, partial)| {
        *step > 3 && blocked_by(partial, M1, &[N]) && !has_copper(partial, M1)
            && partial.open.iter().any(|(net, _)| *net == N)
    }).expect("stream: ripping M1 must remove its copper and report it blocked by N before N routes");
    assert!(
        partials[m1_ripped + 1..].iter().any(|(_, partial)| {
            !partial.open.iter().any(|(net, _)| *net == M1) && has_copper(partial, M1)
                && partial.add == first.add && partial.open == first.open
        }),
        "stream: the failed M1 attempt must roll back to the first pass's copper and open nets"
    );
    assert!(
        partials.iter().any(|(step, partial)| {
            *step > 3 && blocked_by(partial, M2, &[N]) && !has_copper(partial, M2)
        }),
        "stream: ripping M2 must remove its copper and report it blocked by N"
    );
    for (step, partial) in &partials {
        // First-pass offers count only the planned nets already tried; the phase counts all.
        let nets = (*step as usize).min(p.nets.len());
        assert_eq!(
            partial.metrics.nets_routed as usize + partial.metrics.nets_open as usize,
            nets + partial.repaired.len(),
            "stream accounting: step {step} must account for every tried net and repair"
        );
        assert_eq!(
            partial.metrics.nets_open as usize, partial.open.len(),
            "stream accounting: step {step} must count exactly its listed open nets"
        );
    }
    assert!(
        statuses.iter().any(|status| status.current == Some(N) && status.retry == Some(RetryWhy::RipUp)),
        "status: N must announce that its blockers were ripped up"
    );
    assert_eq!(
        statuses.iter().filter(|status| status.retry == Some(RetryWhy::Ripped))
            .map(|status| status.current).collect::<Vec<_>>(),
        vec![Some(M1), Some(M2)],
        "status: the ripped nets must announce their reroutes in order, M1 then M2"
    );
    let repeated = GridRouter::default().route(&p, &Progress::new());
    assert_eq!(sol, repeated, "determinism: the same gaps problem must produce exactly the same solution");
}

/// Why: drum16's VBUS, cut off at two places, stayed open. An attempt ripped only the blockers of
/// the first blockade; the net then met the next one, the attempt rolled back, and the next found
/// the same first blockers and stopped.
#[test]
fn rip_up_clears_every_blockade_of_a_net_cut_off_twice() {
    let p = setup(walls());
    assert_eq!(
        p.nets.iter().map(|net| net.net).collect::<Vec<_>>(), vec![M1, M2, N],
        "fixture: short M1 and M2 must precede long N in plan order"
    );
    let first = Arc::new(Mutex::new(None));
    let output = first.clone();
    let progress = Progress::at_steps(vec![3], move |_, partial| *output.lock().unwrap() = Some(partial));
    let sol = GridRouter::default().route(&p, &progress);
    let first = first.lock().unwrap().take().expect("premise: step 3 offers the completed first pass");
    assert!(
        blocked_by(&first, N, &[M1]) || blocked_by(&first, N, &[M2]),
        "premise: the first pass leaves N blocked by one of the walls' narrow nets: {:?}", first.open
    );
    assert!(has_copper(&first, M1) && has_copper(&first, M2), "premise: M1 and M2 hold the wide gaps");

    assert!(sol.open.is_empty(), "result: N routes through both walls: {:?}", sol.open);
    let (_, conn) = apply_clean(&p, &sol);
    assert!(conn.unrouted.is_empty(), "connectivity: the solution connects every net: {:?}", conn.unrouted);
    for x in [mm(6.0), mm(12.0)] {
        assert_crosses_gap(&sol, N, x, mm(5.4), mm(6.6));
    }
    assert_crosses_gap(&sol, M1, mm(6.0), mm(1.0), mm(1.85));
    assert_crosses_gap(&sol, M2, mm(12.0), mm(1.0), mm(1.85));
    assert_eq!(sol.metrics.work.rips, 2, "one attempt rips M1 and M2, each once");
}

/// Why: on a dense LED controller board an LED net stayed open. Ripping its blocker let it
/// through, but the ripped net's way back was held by a third net that had room elsewhere; a
/// ripped net could not rip in turn, so every attempt rolled back.
#[test]
fn rip_up_passes_a_displacement_on() {
    let p = setup(corridor());
    assert_eq!(
        p.nets.iter().map(|net| net.net).collect::<Vec<_>>(), vec![M2, M1, N],
        "fixture: the short M2 precedes M1, and the long bus N comes last in plan order"
    );
    let first = Arc::new(Mutex::new(None));
    let output = first.clone();
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let status_output = statuses.clone();
    let progress = Progress::at_steps(vec![3], move |_, partial| *output.lock().unwrap() = Some(partial))
        .with_status(move |status| status_output.lock().unwrap().push(status));
    let sol = GridRouter::default().route(&p, &progress);
    let first = first.lock().unwrap().take().expect("premise: step 3 offers the completed first pass");
    assert!(blocked_by(&first, N, &[M1]), "premise: the first pass leaves N blocked by M1 alone: {:?}", first.open);
    assert_crosses_gap(&first, M2, mm(6.0), mm(5.0), mm(6.62));
    assert_crosses_gap(&first, M1, mm(6.0), mm(10.0), mm(12.22));

    assert!(sol.open.is_empty(), "result: N takes g1, M1 moves to g2 and M2 to g3: {:?}", sol.open);
    let (_, conn) = apply_clean(&p, &sol);
    assert!(conn.unrouted.is_empty(), "connectivity: the solution connects every net: {:?}", conn.unrouted);
    assert_crosses_gap(&sol, N, mm(6.0), mm(10.0), mm(12.22));
    assert_crosses_gap(&sol, M1, mm(6.0), mm(5.0), mm(6.62));
    assert_crosses_gap(&sol, M2, mm(6.0), mm(1.0), mm(2.05));
    assert_eq!(sol.metrics.work.rips, 2, "one attempt rips M1 for N, then M2 for M1, each once");
    let statuses = std::mem::take(&mut *statuses.lock().unwrap());
    assert!(
        statuses.iter().any(|status| status.current == Some(M1) && status.retry == Some(RetryWhy::RipUp)),
        "status: the ripped M1 announces that it ripped M2 in turn"
    );
}

#[test]
fn rip_up_never_touches_user_copper_and_reports_blockers() {
    for m2_track in [Some((Origin::Authored, false)), Some((Origin::Solved { by: solvers::GRID }, true))] {
        let p = setup(gaps(m2_track));
        let original = p.board.tracks.iter().find(|track| track.net == M2)
            .expect("fixture: M2 must start with a track through g2").clone();
        assert_eq!(
            p.nets.iter().map(|net| net.net).collect::<Vec<_>>(), vec![M1, N],
            "fixture: connected M2 must be outside the unrouted-net scope"
        );
        let sol = GridRouter::default().route(&p, &Progress::new());
        assert_eq!(sol.open.len(), 1, "fixed copper: only N must stay open");
        assert!(blocked_by(&sol, N, &[M1]), "fixed copper: N must report M1 as its blocker: {:?}", sol.open);
        assert!(has_copper(&sol, M1), "fixed copper: M1 must retain its routed copper after rollback");
        assert_eq!(sol.metrics.nets_routed, 1, "fixed copper: M1 must be counted as routed");
        assert!(sol.remove.is_empty(), "fixed copper: no original board item may be removed");
        // N's first rip of M1 fails and records the pair (N, M1); its second, with M1 priced and
        // still the only way, fails again and records nothing new, which ends N's tries.
        assert_eq!(sol.metrics.work.rips, 2, "termination: a failed attempt that records no new pair must end N's tries");
        let (after, conn) = apply_clean(&p, &sol);
        assert_eq!(conn.unrouted, vec![N], "connectivity: M1 and fixed M2 must stay connected");
        assert_eq!(
            after.tracks.iter().find(|track| track.id == original.id), Some(&original),
            "fixed copper: the authored or locked M2 track must remain exactly unchanged"
        );
    }
}

/// Why: on a handheld board 3V3 stayed open at a capacitor pad. Another net's route had cut the
/// ground pour, and the job's repair of it boxed the pad in; copper of a net outside the problem
/// counted as fixed, the job's own repairs too, so no rip could move it.
#[test]
fn rip_up_moves_the_jobs_own_pour_repair() {
    let p = setup(repair_box());
    assert_eq!(
        p.nets.iter().map(|net| net.net).collect::<Vec<_>>(), vec![A, B, N],
        "fixture: the pour net G is connected and outside the problem, and the shorts A and B precede N"
    );
    let first = Arc::new(Mutex::new(None));
    let output = first.clone();
    let progress = Progress::at_steps(vec![3], move |_, partial| *output.lock().unwrap() = Some(partial));
    let sol = GridRouter::default().route(&p, &progress);
    let first = first.lock().unwrap().take().expect("premise: step 3 offers the completed first pass");
    assert_eq!(first.repaired, vec![G], "premise: A's and B's routes cut the pour and the job repairs G");
    assert!(blocked_by(&first, N, &[G]), "premise: G's repair boxes N's pad in, and N names it: {:?}", first.open);
    // The repair of B's cut: G's job copper reaching below y = 11, far from N's box.
    let far = |item: &Item| match item {
        Item::Track(track) => track.net == G && track.path.steps().any(|(_, seg)| seg.end().y > mm(11.0)),
        Item::Via(via) => via.net == G && via.at.y > mm(11.0),
        _ => false,
    };
    let far_repair: Vec<&Item> = first.add.iter().filter(|item| far(item)).collect();
    assert!(!far_repair.is_empty(), "premise: the job repairs B's cut of the pour too: {:?}", first.add);

    assert!(sol.open.is_empty(), "result: N routes once G's repair moves: {:?}", sol.open);
    assert_eq!(sol.repaired, vec![G], "result: the job repairs G again");
    let (_, conn) = apply_clean(&p, &sol);
    assert!(conn.unrouted.is_empty(), "connectivity: every net, the pour net G too, is connected: {:?}", conn.unrouted);
    let left = |at: P| at.x < mm(6.0) && at.y < mm(10.0);
    let g_left = sol.add.iter().any(|item| match item {
        Item::Track(track) => track.net == G
            && track.path.steps().any(|(a, seg)| left(a) || matches!(seg, Seg::Line(b) if left(b))),
        Item::Via(via) => via.net == G && left(via.at),
        _ => false,
    });
    assert!(!g_left, "result: N holds the short way round, so G's repair takes the gap in the wall");
    assert_eq!(sol.metrics.work.rips, 1, "one attempt rips G's repair once");
    let kept: Vec<&Item> = far_repair.iter().copied().filter(|item| sol.add.contains(item)).collect();
    assert_eq!(kept, far_repair, "partial rip: the repair far from the blockade keeps its items and ids");
    let copper = |add: &[Item], net: NetId| (
        add.iter().map(|item| match item { Item::Track(track) if track.net == net => track.path.length().round() as i64, _ => 0 }).sum::<i64>(),
        add.iter().filter(|item| matches!(item, Item::Via(via) if via.net == net)).count() as u32,
    );
    let [attempt] = sol.metrics.kept.as_slice() else { panic!("metric: one kept attempt: {:?}", sol.metrics.kept) };
    assert_eq!(attempt.own.net, N, "metric: the kept attempt is N's");
    assert_eq!(attempt.ripped.iter().map(|net| net.net).collect::<Vec<_>>(), vec![G], "metric: the attempt ripped G");
    assert!(attempt.others.is_empty(), "metric: no other net changed: {:?}", attempt.others);
    for net in std::iter::once(&attempt.own).chain(&attempt.ripped) {
        assert_eq!((net.length_before, net.vias_before), copper(&first.add, net.net), "metric: {:?}'s before is its first-pass copper", net.net);
        assert_eq!((net.length_after, net.vias_after), copper(&sol.add, net.net), "metric: {:?}'s after is a recount of its copper", net.net);
    }
}

/// Why: on a handheld board a pin added to routed nets (GND, VDD, the I2C pair) made their whole
/// solved trees rippable, and the job laid one of them again elsewhere, cutting other pads off.
/// The gaps board with M2's solved, unlocked track through g2 and a new M2 pin beside M2L: by
/// default the job keeps that track, joins the new pin to it, and N stays open behind M1 as
/// when M2's track is fixed. Asked for a re-route, the job may rip M2's track: M2 moves to g1,
/// pin and all, and N takes g2.
#[test]
fn a_pin_added_to_a_routed_net_joins_its_kept_tree_unless_a_reroute_is_asked() {
    let mut board = gaps(Some((Origin::Solved { by: solvers::GRID }, false)));
    place(&mut board, vec![pin("M2X", P::mm(1.4, 3.6), M2)]);
    board.reindex();
    let mut p = setup(board);
    let original = p.board.tracks.iter().find(|track| track.net == M2).expect("fixture: M2 starts with a track through g2").clone();
    assert!(p.nets.iter().any(|net| net.net == M2) && !p.reroute, "fixture: M2, with its new pin, is in the problem, and a job keeps the board's copper by default");

    let sol = GridRouter::default().route(&p, &Progress::new());
    assert!(sol.remove.is_empty(), "kept: no copper the board had is removed: {:?}", sol.remove);
    let (after, conn) = apply_clean(&p, &sol);
    assert_eq!(after.tracks.iter().find(|track| track.id == original.id), Some(&original), "kept: M2's solved track stays exactly as it was");
    assert!(!conn.unrouted.contains(&M2) && !sol.open.iter().any(|(net, _)| *net == M2), "kept: the new pin joins M2's tree: {:?}", sol.open);
    assert!(blocked_by(&sol, N, &[M1]), "kept: N stays open behind M1, as when M2's track is fixed: {:?}", sol.open);

    p.reroute = true;
    let sol = GridRouter::default().route(&p, &Progress::new());
    assert!(sol.open.is_empty(), "re-route: every net routes: {:?}", sol.open);
    assert!(sol.remove.contains(&original.id), "re-route: M2's solved track is ripped: {:?}", sol.remove);
    let (_, conn) = apply_clean(&p, &sol);
    assert!(conn.unrouted.is_empty(), "re-route: the solution connects every net: {:?}", conn.unrouted);
    assert_crosses_gap(&sol, N, mm(6.0), mm(4.4), mm(5.6));
    assert_crosses_gap(&sol, M2, mm(6.0), mm(1.0), mm(1.85));
}

/// Why: on a handheld board one GND pin boxed in between fine-pitch pins failed the whole net: the
/// router named the pour's centre and left the islands the earlier routes had cut off the pour.
/// On `islands`, A and B cut G's pour strip into three before G routes; G joins every island and
/// GX as the board stands then, and the pin nothing reaches (GB, its first terminal) stays the
/// one open terminal, named with why. Everything added is DRC-clean.
#[test]
fn one_unreachable_pin_leaves_the_rest_of_its_net_routed_and_is_named() {
    let p = setup(islands());
    assert_eq!(p.nets.iter().map(|net| net.net).collect::<Vec<_>>(), vec![A, NetId(2), G],
        "fixture: G, with its pad off the strip and its boxed pad, is in the problem after the shorts A and B");
    let pad = |name: &str| ConnItem::Pad { placed: p.board.placed.iter().find(|placed| placed.refdes == name).expect("fixture: the pad's part").id, pad: PadIx(0) };
    let boxed = sel_of(&p.board, pad("GB"));
    let sol = GridRouter::default().route(&p, &Progress::new());
    let [(open, reason)] = &sol.open[..] else { panic!("only G stays open: {:?}", sol.open) };
    assert_eq!(*open, G, "only G stays open: {:?}", sol.open);
    assert!(matches!(reason, Reason::Terminal { with: Some(sel), .. } | Reason::NoRoom { between: [sel, _], .. } if *sel == boxed),
        "the reason names the pin nothing reaches: {reason:?}");
    let (after, conn) = apply_clean(&p, &sol);
    let groups: Vec<Vec<ConnItem>> = conn.clusters.iter().filter(|c| c.nets.contains(&G))
        .map(|c| c.items.iter().copied().filter(|item| matches!(item, ConnItem::Pad { .. })).collect()).collect();
    let rest: Vec<ConnItem> = ["G1", "G2", "G3", "GX"].map(pad).to_vec();
    assert!(groups.len() == 2 && groups.contains(&vec![pad("GB")]) && groups.iter().any(|g| rest.iter().all(|r| g.contains(r))),
        "every island and GX are joined, only the boxed pin is apart: {groups:?} on {} tracks", after.tracks.len());
}

/// Why: on a handheld board a pin walled in by kept copper (another net's via in front of it, a
/// track along its pad row) came back as NoRoom at the far pad, naming neither that copper nor
/// its layer; only listing the board's copper by hand found it. Re-routing it (the route line's
/// Re-route) may move the copper in its way, and joins every net it moves again.
#[test]
fn a_net_walled_in_by_kept_copper_names_that_copper_and_its_layer() {
    // M2's solved track holds one gap the wide N fits; an earlier job lays M1 through the other.
    let only = |board: Board, net: NetId| {
        let mut p = setup(board);
        p.nets.retain(|n| n.net == net);
        p
    };
    let p = only(gaps(Some((Origin::Solved { by: solvers::GRID }, false))), M1);
    let sol = GridRouter::default().route(&p, &Progress::new());
    assert!(sol.open.is_empty(), "fixture setup: the earlier job routes M1: {:?}", sol.open);
    let (board, _) = apply_clean(&p, &sol);
    let p = only(board, N);
    let sol = GridRouter::default().route(&p, &Progress::new());
    let kept: Vec<Sel> = p.board.tracks.iter().map(|t| Sel::Item(p.board.unit, t.id)).collect();
    match sol.open.as_slice() {
        [(net, Reason::NoRoom { at, layer, by, .. })] if *net == N => {
            assert_eq!(*layer, LayerId::F_CU, "the layer of the copper in the way is named");
            assert!(!by.is_empty() && by.iter().all(|sel| kept.contains(sel)),
                "the kept tracks in N's way are named, and nothing else: {by:?} of {kept:?}");
            assert!((at.x - mm(6.0)).abs() <= mm(1.0), "where: at the wall that copper closes, not at a pad: {at:?}");
        }
        open => panic!("N is walled in by copper the job may not move: NoRoom, got {open:?}"),
    }

    let mut p = p;
    p.reroute = true;
    let sol = GridRouter::default().route(&p, &Progress::new());
    assert!(sol.open.is_empty(), "re-route: N routes once the kept copper may move: {:?}", sol.open);
    assert!(!sol.remove.is_empty() && sol.remove.iter().all(|id| p.board.tracks.iter().any(|t| t.id == *id && t.net != N)),
        "re-route: other nets' kept tracks move: {:?}", sol.remove);
    let (_, conn) = apply_clean(&p, &sol);
    assert!(conn.unrouted.is_empty(), "re-route: every net it moved is joined again: {:?}", conn.unrouted);
}
