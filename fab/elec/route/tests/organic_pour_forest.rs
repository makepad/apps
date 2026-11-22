//! Pour forests span settled fill connectivity, keep every segment inside the fill's search core,
//! and pin exactly the pieces where no path of full width fits against the fixed-only fill.

use fab_elec_board::{
    connect, fill_all, resolve_rules, Area, Board, BoardKeepout, BoardNet, ConnItem, Connectivity,
    Hole, Origin, Placed, RuleMatrix, RuleSet, StackKind, Zone, ZoneFill, ZoneSettings, ARC_ERR,
};
use fab_elec_circuit::{ClassRules, NetClass};
use fab_elec_core::{
    mm, point_clearance, Angle, BBox, BoardId, ClassId, CompId, Contour, LayerId, LayerSet, NetId,
    Nm, PolyGrid, PolySet, Shape, Side, P,
};
use fab_elec_design::fixture::fp;
use fab_elec_parts::{Footprint, Forbid, FpAttrs, FpMount, PadShape, ZoneConnect};
use fab_elec_route::organic::pour::{forest, pour_test, PourForest, PourParams, MARGIN_NM};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn root(parent: &[usize], mut i: usize) -> usize {
    while parent[i] != i {
        i = parent[i];
    }
    i
}

fn properties(
    name: &str,
    board: &Board,
    rules: &RuleMatrix,
    fills: &[ZoneFill],
    conn: &Connectivity,
    params: &PourParams,
    result: &PourForest,
) {
    assert_eq!(result.dropped_edges, 0, "{name}: conservative graph edges all pass the exact test");
    // these fixtures have no copper that moves, so the fixed-only form is the start form
    assert_eq!(*result, forest(board, rules, fills, (board, fills), conn, params), "{name}: repeated forests are deterministic");
    let mut copper = board.all_copper();
    copper.extend(board.reached_copper(fills));
    let nets: BTreeMap<_, _> = copper.iter().filter_map(|c| c.net.map(|n| (c.item, n))).collect();
    let zone_nets: BTreeMap<_, _> = board.zones.iter().filter_map(|z| z.net.map(|n| (z.id, n))).collect();
    let poured: BTreeSet<_> = zone_nets.values().copied().collect();
    let mut by_item = BTreeMap::new();
    let mut parent: Vec<_> = (0..result.groups.len()).collect();
    let mut pinned_any = false;
    for (g, items) in result.groups.iter().enumerate() {
        assert!(!items.is_empty(), "{name}: every hard group has copper");
        assert!(items.windows(2).all(|w| w[0] < w[1]), "{name}: hard group {g} has sorted unique pieces");
        for &item in items {
            assert!(!matches!(item, ConnItem::Zone { .. }), "{name}: hard group {g} excludes zone polygons");
            assert!(by_item.insert(item, g).is_none(), "{name}: each copper piece belongs to exactly one group");
            assert_eq!(nets[&item], nets[&items[0]], "{name}: hard group {g} contains only one net");
        }
    }
    for (&item, net) in &nets {
        if poured.contains(net) {
            assert!(by_item.contains_key(&item), "{name}: every piece of a pour net has a hard group: {item:?}");
        }
    }
    // Independent hard-connectivity oracle: the connector without zone polygons. These fixtures
    // have no offered via rings, so this has exactly the same hard copper as their settled fills.
    let bare = connect(board, &[]);
    for cluster in &bare.clusters {
        let grouped: BTreeSet<_> = cluster.items.iter().filter_map(|i| by_item.get(i).copied()).collect();
        assert!(grouped.len() <= 1, "{name}: non-zone connectivity is united in each hard group");
    }
    for (g, items) in result.groups.iter().enumerate() {
        let clusters = bare.clusters.iter().filter(|c| c.items.iter().any(|i| items.contains(i))).count();
        assert_eq!(clusters, 1, "{name}: hard group {g} does not merge disconnected copper");
    }
    for (pi, path) in result.paths.iter().enumerate() {
        let (a, b) = (path.joins.0 as usize, path.joins.1 as usize);
        assert!(a < b && b < result.groups.len(), "{name}: path {pi} names two ordered existing groups");
        assert!(path.points.len() >= 2, "{name}: path {pi} has at least two points");
        assert_eq!(nets[&result.groups[a][0]], path.net, "{name}: path {pi} first group has the pour net");
        assert_eq!(nets[&result.groups[b][0]], path.net, "{name}: path {pi} last group has the pour net");
        let (ra, rb) = (root(&parent, a), root(&parent, b));
        assert_ne!(ra, rb, "{name}: accepted joins form an acyclic forest");
        parent[rb] = ra;
        let same_cluster = conn.clusters.iter().any(|c| {
            c.items.contains(&result.groups[a][0]) && c.items.contains(&result.groups[b][0])
        });
        assert!(same_cluster, "{name}: path {pi} only protects a connection present in the start form");
        for (group, point) in [
            (a, path.points[0]),
            (b, *path.points.last().expect("path has at least two points")),
        ] {
            assert!(copper.iter().any(|c| {
                c.layer == path.layer && result.groups[group].contains(&c.item)
                    && point_clearance(point, &c.shape) == 0
            }), "{name}: path {pi} endpoint {point:?} lies on a piece of its named group {group}");
        }
        let zone = board.zones.iter().find(|z| z.id == path.zone).expect("path names a fixture zone");
        assert_eq!(path.width, zone.settings.min_width + 2 * params.side_margin,
            "{name}: path {pi} carries minimum width plus side margins");
        let fill = fills.iter().find(|f| f.zone == path.zone).expect("path zone has a fill")
            .layer(path.layer).expect("path layer has a fill").set();
        let r = path.width / 2 + 2 * MARGIN_NM;
        let r0 = zone.settings.min_width / 2 - 2 * ARC_ERR - 2 * MARGIN_NM;
        let bounds = board.outline_bbox().union(fill.bbox()).inflate(r + ARC_ERR);
        let outside = PolyGrid::new(PolySet::rect(bounds).difference(&fill));
        let test = pour_test(board, rules, fills, path.zone, path.layer, params)
            .expect("path zone and layer have a shared test");
        assert_eq!(path.pinned.len() + 1, path.points.len(), "{name}: path {pi} classifies every segment");
        for (si, (points, &pinned)) in path.points.windows(2).zip(&path.pinned).enumerate() {
            let segment = Shape::Segment { a: points[0], b: points[1], w: 0 };
            assert!(!outside.collides(&segment, r0 - ARC_ERR),
                "{name}: path {pi} segment {si} independently lies in the fill's search core");
            assert!(test.reach(path.layer, points[0], points[1]),
                "{name}: path {pi} segment {si} passes the search test including every exclusion");
            assert_eq!(!outside.collides(&segment, r) && test.legal(path.layer, points[0], points[1]), !pinned,
                "{name}: path {pi} segment {si} is free exactly where a path of full width fits");
            pinned_any |= pinned;
        }
    }
    assert_eq!(result.pinned_nm > 0, pinned_any, "{name}: the pinned length counts the pinned segments");
    let mut missing = 0;
    for (ci, cluster) in conn.clusters.iter().enumerate() {
        let poured_here: BTreeSet<_> = cluster.items.iter().filter_map(|i| match i {
            ConnItem::Zone { item, .. } => zone_nets.get(item).copied(),
            _ => None,
        }).collect();
        for net in poured_here {
            let components: BTreeSet<_> = cluster.items.iter().filter_map(|item| {
                by_item.get(item).filter(|_| nets.get(item) == Some(&net)).map(|&g| root(&parent, g))
            }).collect();
            if result.unprotected == 0 {
                assert!(components.len() <= 1,
                    "{name}: path joins unite every hard group of start cluster {ci}, net {net:?}");
            }
            missing += components.len().saturating_sub(1) as u32;
        }
    }
    assert_eq!(missing, result.unprotected, "{name}: unprotected counts the remaining start-form group joins");
}

#[test]
fn pour_neck_forest() {
    let (board, ground, signal, _) = fab_elec_fixtures::pour_neck();
    let rules = resolve_rules(&board, None);
    let fills = fill_all(&board, &rules);
    let conn = connect(&board, &fills);
    assert_eq!(conn.unrouted, vec![signal], "pour neck fixture: only the signal starts open");
    assert!(!conn.unrouted.contains(&ground), "pour neck fixture: ground starts connected by its fill");
    let params = PourParams::default();
    let result = forest(&board, &rules, &fills, (&board, &fills), &conn, &params);
    assert_eq!(result.unprotected, 0, "pour neck: every start-form pour connection is protected");
    assert_eq!(result.paths.len(), 1, "pour neck: two hard ground groups need one path");
    properties("pour neck", &board, &rules, &fills, &conn, &params, &result);
}

fn place(board: &mut Board, name: &str, at: P, size: P, net: NetId) {
    let mut pad = fp::smd("1", P::ZERO, size);
    pad.stack.front = PadShape::Rect { size };
    let id = board.alloc_id();
    board.placed.push(Placed {
        id,
        comp: CompId(board.placed.len() as u32),
        path: name.into(),
        refdes: name.into(),
        footprint: Arc::new(Footprint {
            name: name.into(),
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

/// Two ground pads, L and R, 6 mm apart on a 10 x 8 mm board, and the nets after GND.
fn two_pads(name: &str, nets: &[&str]) -> Board {
    let mut board = Board::new(BoardId(0), name);
    board.outline = vec![Contour::new(fp::rect_path(P::ZERO, P::mm(10.0, 8.0)))];
    board.stackup.layers.retain(|l| matches!(l.kind, StackKind::Copper(LayerId::F_CU)));
    board.rules.base = RuleSet { clearance: Some(mm(0.2)), ..RuleSet::standard() };
    board.classes.push(NetClass {
        id: ClassId(0), name: "Default".into(),
        rules: ClassRules { clearance: Some(mm(0.2)), ..Default::default() },
    });
    for &name in std::iter::once(&"GND").chain(nets) {
        board.nets.push(BoardNet { name: name.into(), class: ClassId(0) });
    }
    place(&mut board, "L", P::mm(2.0, 4.0), P::mm(1.0, 1.0), NetId(0));
    place(&mut board, "R", P::mm(8.0, 4.0), P::mm(1.0, 1.0), NetId(0));
    board
}

/// The board's ground pour, solid to its pads.
fn pour(mut board: Board) -> Board {
    let id = board.alloc_id();
    board.zones.push(Zone {
        id, net: Some(NetId(0)), name: "GND".into(),
        layers: LayerSet::EMPTY.with(LayerId::F_CU), area: Area::Board, priority: 0,
        settings: ZoneSettings { connect: ZoneConnect::Solid, ..ZoneSettings::default() },
        locked: false, origin: Origin::Authored,
    });
    board.reindex();
    board
}

fn neck_board(opening: Nm) -> Board {
    let mut board = two_pads("two blocks and a pour neck", &["BLOCK"]);
    let height = mm(4.0) - opening / 2;
    for (name, y) in [("BOTTOM", height / 2), ("TOP", mm(8.0) - height / 2)] {
        place(&mut board, name, P::new(mm(5.0), y), P::new(mm(2.0), height), NetId(1));
    }
    pour(board)
}

/// A 20 um pour wall across the board at x = 5 mm, open at each `(centre y, width)` gap.
fn wall_board(gaps: &[(Nm, Nm)]) -> Board {
    let mut board = two_pads("a pour wall with gaps", &[]);
    let mut lo = 0;
    for &(y, width) in gaps.iter().chain(std::iter::once(&(mm(8.0), 0))) {
        let id = board.alloc_id();
        board.keepouts.push(BoardKeepout {
            id,
            area: PolySet::rect(BBox::new(P::new(mm(5.0) - 10_000, lo), P::new(mm(5.0) + 10_000, y - width / 2))),
            layers: LayerSet::one(LayerId::F_CU), forbid: Forbid::ZONES,
            reason: String::new(), origin: Origin::Authored, z: None,
        });
        lo = y + width / 2;
    }
    pour(board)
}

#[test]
fn a_neck_narrower_than_a_path_is_pinned_not_lost() {
    // The 0.64 mm opening leaves about 0.23 mm of fill after both copper gaps: wider than
    // the 0.20 mm fill minimum, but narrower than 2*r = 0.24408 mm for a 0.24 mm path.
    for (name, opening, wide) in [("wide neck", mm(1.4), true), ("narrow neck", mm(0.64), false)] {
        let board = neck_board(opening);
        let rules = resolve_rules(&board, None);
        let fills = fill_all(&board, &rules);
        let conn = connect(&board, &fills);
        assert!(!conn.unrouted.contains(&NetId(0)), "{name}: the fill still joins the two ground pads");
        let params = PourParams::default();
        let zone = &board.zones[0];
        let fill = fills.iter().find(|f| f.zone == zone.id).expect("fixture zone is filled")
            .layer(LayerId::F_CU).expect("fixture front layer is filled").set();
        let centre = P::mm(5.0, 4.0);
        assert!(fill.contains(centre), "{name}: the channel centre contains fill");
        let r = (zone.settings.min_width + 2 * params.side_margin) / 2 + 2 * MARGIN_NM;
        assert_eq!(fill.contains(P::new(centre.x, centre.y + r)), wide,
            "{name}: the neck's half width is on the expected side of r");
        let result = forest(&board, &rules, &fills, (&board, &fills), &conn, &params);
        assert_eq!((result.paths.len(), result.unprotected), (1, 0),
            "{name}: the search core passes every neck the fill passes");
        let path = &result.paths[0];
        let crosses = |w: &[P]| w[0].x.min(w[1].x) <= centre.x && centre.x <= w[0].x.max(w[1].x);
        assert!(path.points.windows(2).any(crosses), "{name}: the path crosses the neck between the foreign copper blocks");
        assert_eq!(path.points.windows(2).zip(&path.pinned).any(|(w, &p)| p && crosses(w)), !wide,
            "{name}: the path is pinned in the neck exactly when no path of its width fits there");
        assert!(!path.pinned[0] && !path.pinned[path.pinned.len() - 1],
            "{name}: only the neck is pinned: the path leaves and reaches its pads free");
        properties(name, &board, &rules, &fills, &conn, &params, &result);
    }
}

/// A neck under the fill's minimum width is not a connection, even where the search core's
/// erosion `r0` would pass it: the islands on its two sides keep separate cores, so no candidate
/// crosses it and the two pads stay apart for a track to join.
#[test]
fn a_path_never_crosses_a_neck_under_the_minimum_width() {
    // The fill reaches into a 0.196 mm gap from both sides, discs of h = 0.1 mm around the tips of
    // each side's eroded copper, and leaves a waist of about 0.19 mm: under the 0.20 mm minimum
    // width, wider than the search core's 2 * r0 (about 0.176 mm).
    let board = wall_board(&[(mm(4.0), 196_000)]);
    let rules = resolve_rules(&board, None);
    let fills = fill_all(&board, &rules);
    let conn = connect(&board, &fills);
    let fill = fills[0].layer(LayerId::F_CU).expect("fixture front layer is filled").set();
    assert!(fill.contains(P::mm(5.0, 4.0)), "the neck holds fill");
    assert!(conn.unrouted.contains(&NetId(0)), "the neck does not join the two ground pads");
    let params = PourParams::default();
    let result = forest(&board, &rules, &fills, (&board, &fills), &conn, &params);
    assert_eq!((result.groups.len(), result.paths.len(), result.unprotected), (2, 0, 0),
        "two pads, no path: nothing is joined across the neck");
    properties("wall", &board, &rules, &fills, &conn, &params, &result);
}

#[test]
fn shared_test_rejects_exclusions_that_the_fill_allows() {
    let mut board = neck_board(mm(1.4));
    let id = board.alloc_id();
    board.keepouts.push(BoardKeepout {
        id, area: PolySet::rect(BBox::new(P::mm(2.8, 3.8), P::mm(3.2, 4.2))),
        layers: LayerSet::EMPTY.with(LayerId::F_CU), forbid: Forbid::TRACKS,
        reason: String::new(), origin: Origin::Authored, z: None,
    });
    let id = board.alloc_id();
    board.holes.push(Hole {
        id, name: "plated drill".into(), at: P::mm(7.0, 4.0), drill: mm(0.4), pad: None,
        net: Some(NetId(0)), plated: true, origin: Origin::Authored,
    });
    board.reindex();
    let rules = resolve_rules(&board, None);
    let fills = fill_all(&board, &rules);
    let conn = connect(&board, &fills);
    let params = PourParams::default();
    let zone = board.zones[0].id;
    let fill = fills.iter().find(|f| f.zone == zone).expect("fixture zone is filled")
        .layer(LayerId::F_CU).expect("fixture front layer is filled").set();
    let test = pour_test(&board, &rules, &fills, zone, LayerId::F_CU, &params)
        .expect("fixture zone has a shared test");
    // the relaxer lets the pour's own tracks over its own plated drills: no exclusion there
    let own = P::mm(7.0, 4.0);
    assert!(fill.contains(own) && test.legal(LayerId::F_CU, own, own)
        && test.legal(LayerId::F_CU, own.sub(P::mm(0.7, 0.0)), own.add(P::mm(0.7, 0.0))),
        "same-net plated drill: the fill covers it and the shared test allows a path across it");
    for (name, point) in [("track-only keepout", P::mm(3.0, 4.0))] {
        assert!(fill.contains(point), "{name}: the fill allows copper at the exclusion centre");
        assert!(!test.legal(LayerId::F_CU, point, point), "{name}: the shared test rejects forbidden copper");
        let (a, b) = (point.sub(P::mm(0.7, 0.0)), point.add(P::mm(0.7, 0.0)));
        assert!(test.legal(LayerId::F_CU, a, a) && test.legal(LayerId::F_CU, b, b),
            "{name}: both ends of the crossing segment are individually legal");
        assert!(!test.legal(LayerId::F_CU, a, b),
            "{name}: an attachment or taut shortcut cannot cross the excluded interior");
    }
    assert!(test.legal(LayerId::F_CU, P::mm(1.8, 4.0), P::mm(2.2, 4.0)),
        "shared test: an unobstructed segment is legal");
    assert!(!test.legal(LayerId::B_CU, P::mm(1.8, 4.0), P::mm(2.2, 4.0)),
        "shared test: a different layer requires its own test");
    let result = forest(&board, &rules, &fills, (&board, &fills), &conn, &params);
    assert_eq!(result.unprotected, 0, "exclusions: the wide board still has room around both forbidden areas");
    properties("exclusions", &board, &rules, &fills, &conn, &params, &result);
}
