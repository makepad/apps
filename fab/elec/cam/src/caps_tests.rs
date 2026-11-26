use crate::caps::{apply_capabilities, board_limits, raised_rules};
use crate::profile::Capabilities;
use fab_elec_board::{Anchor, Board, BoardNet, CustomRule, Flash, Origin, RuleCode, RuleSet, Rules, Tent, Track, Via, ViaKind};
use fab_elec_core::{mm, BoardId, ClassId, ContentHash, Contour, ItemId, LayerId, NetId, Path, Severity, P};

#[test]
fn factory_minima_and_board_capabilities() {
    let caps = Capabilities {
        min_track: mm(0.15), min_clearance: mm(0.15), min_drill: mm(0.3), min_annular: mm(0.13),
        via_in_pad: false,
        layers: vec![2], board_min: (mm(5.0), mm(5.0)), board_max: (mm(20.0), mm(20.0)), thickness: vec![mm(1.6)], thickness_tol_ppm: 50_000,
    };
    let mut board = Board::new(BoardId(0), "capabilities");
    board.nets.push(BoardNet { name: "GND".into(), class: ClassId(0) });
    board.outline.push(Contour::rect(P::ZERO, P::mm(4.0, 10.0), 0));
    board.tracks.push(Track {
        id: ItemId(3), net: NetId(0), layer: LayerId::F_CU, w: mm(0.1), widths: None,
        path: Path::new(P::mm(1.0, 1.0)).line(P::mm(3.0, 1.0)), ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
    });
    board.vias.push(Via {
        id: ItemId(2), net: NetId(0), at: P::mm(3.0, 1.0), drill: mm(0.25), pad: mm(0.45),
        span: (LayerId::F_CU, LayerId::B_CU), kind: ViaKind::Through, tent: Tent::None, backdrill: None, locked: false, origin: Origin::Authored,
        flash: Flash::All,
    });
    board.next_item = 4;
    assert!(raised_rules(&board, &caps, "house").is_empty(), "a new board inherits every minimum");
    board.rules.base.track = Some(mm(0.1));
    board.rules.base.clearance = Some(mm(0.1));
    board.rules.base.drill = Some(mm(0.25));
    board.rules.base.annular = Some(mm(0.1));
    board.rules.custom.push(CustomRule {
        name: "local minima".into(), when: RuleCode { id: 0, hash: ContentHash([0; 16]), at: (0, 0) },
        layers: None, set: RuleSet { track: Some(mm(0.1)), clearance: Some(mm(0.2)), drill: Some(mm(0.2)), ..RuleSet::default() }, severity: Severity::Error,
    });
    let findings = raised_rules(&board, &caps, "house");
    assert_eq!(findings.iter().map(|v| v.rule.as_str()).collect::<Vec<_>>(),
        ["factory:house:track", "factory:house:clearance", "factory:house:drill", "factory:house:annular"]);
    assert!(findings.iter().all(|v| v.severity == Severity::Warning && !v.fix_hint.is_empty()));
    let original = board.rules.clone();
    let raised = apply_capabilities(&board.rules, &caps);
    let mut expected = original.clone();
    expected.base.track = Some(caps.min_track);
    expected.base.clearance = Some(caps.min_clearance);
    expected.base.drill = Some(caps.min_drill);
    expected.base.annular = Some(caps.min_annular);
    expected.custom[0].set.track = Some(caps.min_track);
    expected.custom[0].set.drill = Some(caps.min_drill);
    assert_eq!(raised, expected, "factory floors raise weak minima and preserve stricter or inherited custom values and unrelated rules");
    assert_eq!(board.rules, original, "applying factory limits leaves the input rules untouched");
    let mut stricter = raised.clone();
    stricter.base.track = Some(mm(0.4));
    stricter.base.clearance = Some(mm(0.4));
    stricter.base.drill = Some(mm(0.4));
    stricter.base.annular = Some(mm(0.4));
    assert_eq!(apply_capabilities(&stricter, &caps), stricter, "stricter design minima are preserved");
    let mut loose = caps.clone();
    loose.min_track = 0; loose.min_clearance = 0; loose.min_drill = 0; loose.min_annular = 0;
    let inherited = apply_capabilities(&Rules::default(), &loose);
    let standard = RuleSet::standard();
    assert_eq!((inherited.base.track, inherited.base.clearance, inherited.base.drill, inherited.base.annular),
        (standard.track, standard.clearance, standard.drill, standard.annular), "factory limits do not loosen inherited standard minima");


    // board-wide limits are Violations naming the profile; item limits are DRC's, not ours
    let unit = BoardId(0);
    let rule = |v: &fab_elec_circuit::Violation| v.rule.clone();
    board.outline[0] = Contour::rect(P::ZERO, P::mm(4.0, 21.0), 0);
    let v = board_limits(&board, &caps, "standard 2-layer", unit);
    assert_eq!(v.iter().map(rule).collect::<Vec<_>>(), ["factory:standard 2-layer:board_height", "factory:standard 2-layer:board_width"], "size limits, named after the profile, in rule order");
    assert!(v[1].message.contains("below the minimum") && v[0].message.contains("above the maximum"), "cause names the side");
    assert!(v[0].fix_hint.contains("20.000") && v[1].fix_hint.contains("5.000"), "hint names the factory's limit");
    assert_eq!(v[0].id, board_limits(&board, &caps, "standard 2-layer", unit)[0].id, "ids are stable");
    board.outline[0] = Contour::rect(P::ZERO, P::mm(10.0, 10.0), 0);
    for thickness in [mm(1.52), mm(1.68)] {
        board.stackup.thickness = thickness;
        assert!(board_limits(&board, &caps, "p", unit).is_empty(), "the inclusive 5% band is supported: {thickness}");
    }
    board.stackup.thickness = mm(1.68) + 1;
    assert_eq!(board_limits(&board, &caps, "p", unit).len(), 1, "just beyond the band is rejected");
    let mut tight = caps.clone();
    tight.thickness_tol_ppm = 1_000;
    board.stackup.thickness = mm(1.61);
    assert_eq!(board_limits(&board, &tight, "p", unit).len(), 1, "the band is the profile's field");
    board.stackup.thickness = mm(1.6);
    let mut four = caps.clone();
    four.layers = vec![4, 6];
    let v = board_limits(&board, &four, "p", unit);
    assert_eq!(v.len(), 1);
    assert!(v[0].fix_hint.contains("4 copper layers"), "the nearest offer is named: {}", v[0].fix_hint);
    assert_eq!((v[0].actual, v[0].required), (Some(fab_elec_circuit::Measure::Qty(fab_elec_circuit::Quantity { si: 2.0, unit: fab_elec_circuit::QtyUnit::Count })), Some(fab_elec_circuit::Measure::Qty(fab_elec_circuit::Quantity { si: 4.0, unit: fab_elec_circuit::QtyUnit::Count }))), "a layer count is a count, not a length");
}
