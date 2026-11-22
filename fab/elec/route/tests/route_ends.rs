//! Hand-routed track ends take priority when the grid router joins terminals, and the relax
//! after it keeps them continued.

use fab_elec_board::zone::fill_all;
use fab_elec_board::{
    connect, dangling_ends, drc, resolve_rules, Anchor, Board, BoardNet, BoardOp, Origin, Placed,
    RuleSet, StackKind, Track, Waiver,
};
use fab_elec_circuit::{CheckKind, ClassRules, NetClass, ViaSize, Violation};
use fab_elec_core::{
    mm, Angle, BoardId, ClassId, CompId, Contour, ItemId, LayerId, NetId, PadIx, Path, Sel, Side, P,
};
use fab_elec_design::fixture::fp;
use fab_elec_parts::{Footprint, FpAttrs, FpMount, Pad, PadShape};
use fab_elec_route::auto::GridRouter;
use fab_elec_route::check::Basis;
use fab_elec_route::organic::job::relax;
use fab_elec_route::{problem, scope_nets, Autorouter, Base, Effort, Progress, RBase, RouteScope, Style};
use std::sync::Arc;

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

fn pins(name: &str, p2: P) -> Board {
    let mut board = one_layer(
        name, P::mm(20.0, 10.0),
        [("M1", ClassId(0)), ("M2", ClassId(1)), ("N", ClassId(1))],
    );
    place(&mut board, vec![pin("P1", P::mm(3.0, 5.0), N), pin("P2", p2, N)]);
    board
}

fn with_track(mut board: Board, path: Path, ends: [Anchor; 2]) -> (Board, ItemId) {
    let id = board.alloc_id();
    board.tracks.push(Track {
        id, net: N, layer: LayerId::F_CU, w: mm(0.25), widths: None, path, ends,
        locked: false, origin: Origin::Authored,
    });
    board.reindex();
    (board, id)
}

fn stub_board(name: &str, tip: P, wall_pads: &[(P, P)]) -> (Board, ItemId) {
    let mut board = pins(name, P::mm(17.0, 7.5));
    // Ordinary track access can project a cell inside the round cap onto E. With the
    // 0.15 mm pitch and this outline origin, the columns bracketing E are 8.02 and
    // 8.17 mm. The first projects onto the body and the second is beyond the 8.155 mm cap.
    board.outline[0].path = board.outline[0].path.translated(P::mm(0.07, 0.0));
    if !wall_pads.is_empty() {
        place(&mut board, vec![("WALL", tip, wall(wall_pads), vec![None; wall_pads.len()])]);
    }
    let p1 = &board.placed[0];
    let path = Path::new(p1.at).line(tip);
    let start = Anchor::Pad { placed: p1.id, pad: PadIx(0) };
    with_track(board, path, [start, Anchor::Free])
}

fn findings(board: &Board) -> Vec<Violation> {
    let rules = resolve_rules(board, None);
    let fills = fill_all(board, &rules);
    drc(board, &connect(board, &fills), &rules, &fills, None).violations
}

fn route(board: Board) -> (Board, Vec<Violation>) {
    let board = Arc::new(board);
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let base = Base::new(board, rules, conn, fills);
    let nets = scope_nets(&base.board, &base.conn, &RouteScope::All);
    let world = Arc::new(RBase::build(&base.board));
    let p = problem(&base, world, &nets, Style::FortyFive, Effort::default());
    let sol = GridRouter::default().route(&p, &Progress::new());

    let name = &base.board.name;
    assert!(sol.open.is_empty(), "{name} must connect N, open nets were {:?}", sol.open);
    let mut routed = (*base.board).clone();
    routed.apply_all(&sol.ops())
        .unwrap_or_else(|e| panic!("{name} solution must apply to the input board, got {e:?}"));
    for track in &routed.tracks {
        for (anchor, point) in track.ends.into_iter().zip([track.path.start, track.path.end()]) {
            if anchor == Anchor::Free {
                assert_eq!(
                    routed.infer_anchor(track, point), Anchor::Free,
                    "{name} reload must leave track {:?} free at {point:?}", track.id
                );
            }
        }
    }
    let live = findings(&routed);
    (routed, live)
}

/// Route `board` by the grid, relax that solution, and apply the relaxed one.
fn relaxed(board: Board) -> Board {
    let board = Arc::new(board);
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let base = Base::new(board, rules, conn, fills);
    let nets = scope_nets(&base.board, &base.conn, &RouteScope::All);
    let p = problem(&base, Arc::new(RBase::build(&base.board)), &nets, Style::FortyFive, Effort::default());
    let grid = GridRouter::default().route(&p, &Progress::new());
    let (_, sol, _) = relax(&p, &Basis::of(&p), &grid, &Progress::new(), grid.metrics.work.transactions);
    let name = &base.board.name;
    assert!(sol.open.is_empty(), "{name} relaxed must connect N, open nets were {:?}", sol.open);
    let mut routed = (*base.board).clone();
    routed.apply_all(&sol.ops()).unwrap_or_else(|e| panic!("{name} relaxed solution must apply, got {e:?}"));
    routed
}

#[test]
fn hand_routed_ends_are_continued_from_their_tips() {
    let tip = P::mm(8.03, 5.0);
    let (board, stub) = stub_board("stub", tip, &[]);
    let (routed, live) = route(board);
    let track_at_tip = routed.tracks.iter().any(|track| {
        let points = [track.path.start, track.path.end()];
        matches!(track.origin, Origin::Solved { .. })
            && track.ends.into_iter().zip(points).any(|(anchor, point)| {
                point == tip && matches!(anchor, Anchor::Track { id, .. } if id == stub)
            })
    });
    let via_at_tip = routed.vias.iter().any(|via| matches!(via.origin, Origin::Solved { .. }) && via.at == tip);
    assert!(
        track_at_tip || via_at_tip,
        "stub solver track must anchor to the authored stub exactly at E, or a solver via must sit at E"
    );
    assert!(
        routed.tracks.iter().any(|t| t.id == stub && t.ends[1] != Anchor::Free),
        "stub authored end must be attached after apply"
    );
    assert!(live.is_empty(), "stub live DRC must be clean, got {live:#?}");

    // The relax after the grid moves no fixed copper and leaves no fixed end free.
    let (board, stub) = stub_board("stub relaxed", tip, &[]);
    let drawn = board.tracks.iter().find(|t| t.id == stub).expect("stub relaxed setup: the authored stub").path.clone();
    let routed = relaxed(board);
    let kept = routed.tracks.iter().find(|t| t.id == stub).expect("stub relaxed: the authored stub stays on the board");
    assert_eq!(kept.path, drawn, "stub relaxed: the relax leaves the authored stub's copper as drawn");
    assert_eq!(dangling_ends(&routed, kept), [false, false], "stub relaxed: the relax leaves no end of the stub free");
    let live = findings(&routed);
    assert!(live.is_empty(), "stub relaxed live DRC must be clean, got {live:#?}");

    let board = pins("run", P::mm(17.0, 5.0));
    let path = Path::new(P::mm(7.03, 5.0)).line(P::mm(12.97, 5.0));
    let (board, run) = with_track(board, path, [Anchor::Free; 2]);
    let (routed, live) = route(board);
    assert!(
        routed.tracks.iter().any(|t| t.id == run && !t.ends.contains(&Anchor::Free)),
        "run must have both authored ends attached after apply"
    );
    assert!(live.is_empty(), "run live DRC must be clean, got {live:#?}");
    let mut cut = routed.clone();
    cut.apply(&BoardOp::Remove(run)).expect("run removing the authored track must succeed");
    let fills = fill_all(&cut, &resolve_rules(&cut, None));
    let conn = connect(&cut, &fills);
    assert_eq!(
        conn.clusters.iter().filter(|c| c.nets.contains(&N)).count(), 2,
        "run removal must split N into exactly two clusters"
    );

    // Pad centres are relative to E. The side pads end at E's x, and their inner faces
    // and the front pad's inner face are 0.35 mm from the centreline and tip respectively.
    // Each clears the copper by 0.35 - 0.125 = 0.225 mm. Each side pad stops 0.35 mm
    // short of the front pad. A new track needs a 0.25 + 2 * 0.2 = 0.65 mm gap.
    let wall_pads = [
        (P::mm(-1.20, -0.85), P::mm(2.40, 1.00)),
        (P::mm(-1.20, 0.85), P::mm(2.40, 1.00)),
        (P::mm(0.85, 0.0), P::mm(1.00, 2.70)),
    ];
    for name in ["boxed", "waived"] {
        let (mut board, stub) = stub_board(name, tip, &wall_pads);
        let unit = board.unit;
        let dangling_at_tip = |v: &Violation| {
            v.kind == CheckKind::Dangling && v.at == Some(tip) && v.items == [Sel::Item(unit, stub)]
        };
        let input = findings(&board);
        let dangling = input.iter().find(|v| dangling_at_tip(v))
            .unwrap_or_else(|| panic!("{name} input DRC must report the authored stub Dangling at E, got {input:#?}"));
        assert!(
            input.len() == 2 && input.iter().any(|v| v.kind == CheckKind::Unconnected),
            "{name} input DRC must report only Dangling at E and Unconnected on N, got {input:#?}"
        );
        if name == "waived" {
            board.rules.waivers.push(Waiver { violation: dangling.id, reason: "test stub".into() });
            assert_eq!(
                dangling_ends(&board, &board.tracks[0]), [false, false],
                "waived stub must have no routable dangling ends"
            );
        }
        let (_, live) = route(board);
        if name == "boxed" {
            assert!(
                matches!(live.as_slice(), [v] if dangling_at_tip(v)),
                "boxed live DRC must report only the authored stub Dangling at E, got {live:#?}"
            );
        } else {
            assert!(live.is_empty(), "waived live DRC must be clean, got {live:#?}");
        }
    }
}
