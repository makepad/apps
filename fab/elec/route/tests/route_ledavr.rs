//! ledavr's placement routed by the grid engine Fab's Route action runs, at the default style and
//! effort, over the whole board. Every net routes, no via is drilled in a pad, applying the route
//! moves none of its copper, and routing adds no DRC finding to the placement's own. (The organic
//! stage of the action's job only moves the grid's vias, never into a pad: the organic tests ask
//! ViaInPad.)

use fab_elec_board::zone::fill_all;
use fab_elec_board::{connect, drc, resolve_rules, Board, Item};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_lang::{evaluate, std_libs, Sources};
use fab_elec_route::auto::GridRouter;
use fab_elec_route::{problem, scope_nets, Autorouter, Base, Effort, Progress, RBase, RouteScope, Style};
use std::sync::Arc;

/// ledavr's board before routing: its parts placed, ledctl's LED anode tracks, two GND vias and
/// the GND pour on both layers.
fn placed() -> Arc<Board> {
    let mut sources = Sources::new();
    for (file, text) in [
        ("ledavr.ckt.splash", include_str!("../../fixtures/ledavr.ckt.splash")),
        ("ledavr.pcb.splash", include_str!("ledavr_placed.pcb.splash")),
        ("parts/avr/ATMEGA328P_TQFP32.part.splash", include_str!("../../fixtures/parts/avr/ATMEGA328P_TQFP32.part.splash")),
    ] {
        sources.add(file, text).unwrap_or_else(|e| panic!("{file}: an electronics document: {e:?}"));
    }
    sources.ensure_system().expect("ledavr has a system document");
    let evaluated = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("ledavr evaluates: {diags:?}"));
    let unit = evaluated.design.units.iter().find(|u| u.name == "ledavr").expect("ledavr has a unit");
    unit.board.clone().expect("ledavr's placement is a board")
}

/// The findings other than open connections, as (kind, message), sorted.
fn findings(violations: &[Violation]) -> Vec<(CheckKind, String)> {
    let mut out: Vec<_> = violations.iter().filter(|v| v.kind != CheckKind::Unconnected)
        .map(|v| (v.kind.clone(), v.message.clone())).collect();
    out.sort();
    out
}

/// Why: the grid router drilled seven vias into pads of their own net on this board (the MCU's,
/// the crystal's and its load capacitors'), where DRC reports each one.
#[test]
fn ledavr_routes_every_net_with_no_via_in_a_pad() {
    let board = placed();
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let placement = findings(&drc(&board, &conn, &rules, &fills, None).violations);
    let base = Base::new(board, rules.clone(), conn, fills);
    let nets = scope_nets(&base.board, &base.conn, &RouteScope::All);
    assert!(!nets.is_empty(), "the placement leaves nets to route");
    let world = Arc::new(RBase::build(&base.board));
    let p = problem(&base, world, &nets, Style::default(), Effort::default());
    let sol = GridRouter::default().route(&p, &Progress::new());
    assert!(sol.open.is_empty(), "every net routes: {:?}", sol.open);

    let mut routed = (*base.board).clone();
    assert!(routed.apply_all(&sol.ops()).is_ok(), "the routed solution applies");
    // Why: a junction attached at a cell centre inside its host track's copper, and applying the
    // route put that end on the host's centreline, which moved verified copper into a pad's
    // clearance. The board must get the copper the grid verified: no anchor moves any of it.
    for item in &sol.add {
        if let Item::Track(track) = item {
            let placed = routed.tracks.iter().find(|t| t.id == track.id).expect("a routed track is on the board");
            assert_eq!(placed.path, track.path, "track {} keeps its verified path when its anchors place its ends", track.id.0);
        }
    }
    let fills = fill_all(&routed, &rules);
    let conn = connect(&routed, &fills);
    assert!(conn.unrouted.is_empty(), "no net is left unrouted: {:?}", conn.unrouted);
    let violations = drc(&routed, &conn, &rules, &fills, None).violations;
    let in_pad: Vec<_> = violations.iter().filter(|v| v.kind == CheckKind::ViaInPad).collect();
    assert!(in_pad.is_empty(), "no via is drilled in a pad: {in_pad:#?}");
    assert_eq!(findings(&violations), placement, "routing adds no finding to the placement's own");
}
