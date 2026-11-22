//! Guards one definition of legal spacing: the router verifier and full board DRC must agree
//! on each new track or via's legality and neighbours, without zone fills or accumulated edits.

use fab_elec_board::{
    connect, drc, resolve_rules, Anchor, BoardOp, Flash, Item, Origin, Tent, Track, Via, ViaKind,
};
use fab_elec_circuit::CheckKind;
use fab_elec_core::{mm, BBox, LayerId, NetId, Nm, Path, Sel, P};
use fab_elec_fixtures::ledctl;
use fab_elec_route::{new_pieces, solvers, verify, RBase, World};
use std::collections::BTreeSet;

fn xorshift64(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn random_point(state: &mut u64, area: BBox) -> P {
    P::new(
        area.min.x + (xorshift64(state) % (area.width() as u64 + 1)) as Nm,
        area.min.y + (xorshift64(state) % (area.height() as u64 + 1)) as Nm,
    )
}

#[test]
fn verifier_agrees_with_drc() {
    let design = ledctl();
    let board = design.units[0].board.as_deref().expect("spacing fixture has a board").clone();
    let rules = resolve_rules(&board, None);
    let base = RBase::build(&board);
    let world = World::new(&base);
    // Fills yield to new routes and are re-poured; neither side checks against them here.
    let conn = connect(&board, &[]);
    let base_ids: BTreeSet<_> = drc(&board, &conn, &rules, &[], None)
        .violations
        .into_iter()
        .map(|v| v.id)
        .collect();
    let area = board.outline_bbox().inflate(-mm(1.0));
    let nets: Vec<_> = board
        .nets
        .iter()
        .enumerate()
        .filter_map(|(i, net)| {
            let id = NetId(i as u32);
            (net.name != "GND" && board.placed.iter().any(|p| p.pad_nets.contains(&Some(id))))
                .then_some(id)
        })
        .collect();
    let anchors: Vec<_> = board
        .placed
        .iter()
        .flat_map(|p| p.footprint.pads.iter().map(move |pad| p.to_board(pad.at)))
        .chain(board.tracks.iter().map(|t| t.path.bbox().center()))
        .chain(board.vias.iter().map(|v| v.at))
        .filter(|p| area.contains(*p))
        .collect();
    assert!(!nets.is_empty(), "spacing fixture has non-GND nets with pads");
    assert!(!anchors.is_empty(), "spacing fixture has copper anchors inside {area:?}");
    assert!(area.width() > i64::from(mm(12.0)) && area.height() > i64::from(mm(12.0)), "spacing fixture has room: {area:?}");
    // The last candidate: a via of a pad's own net at the pad's centre, inside its mask web.
    let own_pad = board.placed.iter()
        .flat_map(|p| p.footprint.pads.iter().zip(&p.pad_nets).map(move |(pad, net)| (*net, p.to_board(pad.at))))
        .find_map(|(net, at)| net.filter(|n| nets.contains(n) && area.contains(at)).map(|n| (n, at)))
        .expect("spacing fixture has a pad of a non-GND net");

    let mut state = 0x4d59_5df4_d0f3_3173;
    let mut legal = 0;
    let mut collided = 0;
    let mut candidates = Vec::with_capacity(91);
    for i in 0..91 {
        let net = nets[(xorshift64(&mut state) % nets.len() as u64) as usize];
        let at = if i % 2 == 0 {
            let anchor = anchors[(xorshift64(&mut state) % anchors.len() as u64) as usize];
            // At most sqrt(2) * 0.25 mm from an existing pad, track or via centre.
            let offset = random_point(&mut state, BBox::new(P::mm(-0.25, -0.25), P::mm(0.25, 0.25)));
            let p = anchor.add(offset);
            P::new(p.x.clamp(area.min.x, area.max.x), p.y.clamp(area.min.y, area.max.y))
        } else {
            random_point(&mut state, area)
        };
        let (net, at) = if i == 90 { own_pad } else { (net, at) };
        let class = board.nets[net.ix()].class;
        let mut b2 = board.clone();
        let id = b2.alloc_id();
        let item = if i < 60 {
            let layer = if xorshift64(&mut state) & 1 == 0 { LayerId::F_CU } else { LayerId::B_CU };
            let end = loop {
                let p = random_point(&mut state, area);
                let delta = p.sub(at);
                let length_squared = delta.dot(delta);
                if ((mm(2.0) as i64).pow(2)..=(mm(12.0) as i64).pow(2)).contains(&length_squared) {
                    break p;
                }
            };
            Item::Track(Track {
                id,
                net,
                layer,
                w: rules.class(class).track.unwrap_or_else(|| rules.track_min(class)),
                widths: None,
                path: Path::new(at).line(end),
                ends: [Anchor::Free; 2],
                locked: false,
                origin: Origin::Solved { by: solvers::GRID },
            })
        } else {
            let size = rules.class(class).via.unwrap_or_else(|| rules.via(class));
            Item::Via(Via {
                id,
                net,
                at,
                drill: size.drill,
                pad: size.pad,
                span: (LayerId::F_CU, LayerId::B_CU),
                kind: ViaKind::Through,
                tent: Tent::Both,
                backdrill: None,
                locked: false,
                origin: Origin::Solved { by: solvers::GRID },
                flash: Flash::All,
            })
        };
        let new = new_pieces(&board, &item);
        let col = verify(&world, &rules, &board, &new);
        let _applied = b2
            .apply(&BoardOp::Add(item.clone()))
            .unwrap_or_else(|e| panic!("candidate addition property, candidate {i}: {item:?}: {e}"));
        let conn2 = connect(&b2, &[]);
        let result = drc(&b2, &conn2, &rules, &[], None);
        let selection = Sel::Item(board.unit, id);
        // CourtyardOverlap is excluded: it checks footprint placement, not routed copper.
        let spacing: Vec<_> = result
            .violations
            .iter()
            .filter(|v| {
                matches!(
                    v.kind,
                    CheckKind::Clearance
                        | CheckKind::EdgeClearance
                        | CheckKind::HoleClearance
                        | CheckKind::HoleToHole
                        | CheckKind::Keepout
                        | CheckKind::Short
                        | CheckKind::ViaInPad
                ) && v.items.contains(&selection)
                    && !base_ids.contains(&v.id)
            })
            .collect();
        let drc_neighbours: BTreeSet<_> = spacing
            .iter()
            .flat_map(|v| v.items.iter())
            .filter(|s| *s != &selection)
            .cloned()
            .collect();
        let verifier_neighbours: BTreeSet<_> = col.iter().map(|c| c.b.clone()).collect();
        assert_eq!(
            col.is_empty(),
            spacing.is_empty(),
            "legality agreement, candidate {i}: {item:?}; verifier={col:?}; DRC={spacing:?}"
        );
        assert_eq!(
            verifier_neighbours, drc_neighbours,
            "neighbour agreement, candidate {i}: {item:?}; left=verifier, right=DRC"
        );
        for c in &col {
            assert!(c.gap < c.need, "real shortfall property, candidate {i}: {item:?}; collision={c:?}");
        }
        if col.is_empty() {
            legal += 1;
        } else {
            collided += 1;
        }
        candidates.push((i, item));
    }
    assert!(legal >= 20, "legal non-vacuity: {legal}/91; indexed candidate geometries: {candidates:?}");
    assert!(collided >= 20, "collision non-vacuity: {collided}/91; indexed candidate geometries: {candidates:?}");
}
