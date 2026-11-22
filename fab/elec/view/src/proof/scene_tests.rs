use super::{ProofScene, Role, Thing};
use fab_elec_board::{resolve_rules, zone::fill_all};
use fab_elec_cam::tables::{fill_generic_tables, PlacementRules};
use fab_elec_cam::{export, ResolvedProfile};
use fab_elec_fixtures::ledctl;

#[test]
fn the_scene_is_the_files_as_read_back_and_picks_what_is_under_the_pointer() {
    let d = ledctl();
    let u = &d.units[0];
    let (board, circuit) = (u.board.as_ref().unwrap(), &u.circuit);
    let fills = fill_all(board, &resolve_rules(board, None));
    let mut profile = ResolvedProfile::standard_2_layer("ledctl", "A");
    fill_generic_tables(&mut profile, board, circuit, &PlacementRules::default());
    let e = export(board, circuit, &fills, &profile).unwrap();
    let s = ProofScene::build(&e);

    assert_eq!(s.layers.len(), 9, "every Gerber file of the profile is a layer");
    assert!(s.layers.windows(2).all(|w| w[0].role != Role::Edge || w[1].role == Role::Edge), "the edge is on top of the stack");
    assert_eq!(s.drills.len(), 13, "the drill hits are the drill files' holes");
    assert_eq!(s.dots.len(), 30, "a dot per placed part");
    assert_eq!(s.bom.len(), profile.bom.rows.len() + 1, "the BOM as written, header first");
    assert!(s.markers.is_empty() && s.validation.ok(), "a valid export has no failure markers");
    let w = s.bounds[2] - s.bounds[0];
    let h = s.bounds[3] - s.bounds[1];
    assert!((w - 50.0).abs() < 0.5 && (h - 30.0).abs() < 0.5, "bounds are the outline: {w} x {h}");
    let pour = &s.layers[s.layers.iter().position(|l| l.file == "ledctl-F_Cu.gbr").unwrap()];
    assert!(!pour.fills.is_empty(), "the GND pour is an exact polygon set for the board's polygon scene");
    let planes = super::planes(&s);
    assert!(planes.layers.iter().any(|(l, sl)| *l == fab_elec_core::LayerId::F_CU && !sl.polys.is_empty()), "the front copper plane carries the pour");
    assert!(!s.ghost.is_empty() && !super::ghost_planes(&s).layers.is_empty(), "the model's pours are planes too");
    assert!(!s.ghost.is_empty(), "the model's layers are kept as the ghost");

    // a pad of the front copper layer is found at its centre, topmost first, and named by its pin
    let li = s.layers.iter().position(|l| l.file == "ledctl-F_Cu.gbr").unwrap();
    let l = &s.layers[li];
    let pad = l.items.iter().find(|i| i.attrs.is_some_and(|a| l.attrs[a as usize].pin.is_some())).expect("a pad with a pin");
    let c = pad.prim.bbox().center();
    let at = [fab_elec_core::to_mm(c.x), fab_elec_core::to_mm(c.y)];
    let hits = s.pick(at, 0.05, &|_| true);
    assert!(hits.contains(&pad.id), "the pointer over a pad hits it");
    let Some(Thing::Item { attrs: Some(a), file, .. }) = s.thing(pad.id) else { panic!("a pad is a layer object with attributes") };
    assert!(a.pin.is_some() && file == "ledctl-F_Cu.gbr", "the pad is named by its pin from the file ({file})");
    let rank = |id: u32| match s.thing(id) {
        Some(Thing::Marker(_)) => 0,
        Some(Thing::Dot(_)) => 1,
        Some(Thing::Drill(_)) => 2,
        _ => 3,
    };
    assert!(hits.windows(2).all(|w| rank(w[0]) <= rank(w[1])), "marks, dots and drills come before layer objects");
    assert!(s.pick(at, 0.05, &|l| l != li).iter().all(|id| *id != pad.id), "a hidden layer is not picked");
    assert!(s.pick([-100.0, -100.0], 0.5, &|_| true).is_empty(), "nothing off the board");
}
