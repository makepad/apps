use crate::export::export;
use crate::image::Geom;
use crate::profile::ResolvedProfile;
use crate::read_gerber;
use crate::tables::{fill_generic_tables, PlacementRules};
use fab_elec_board::{Anchor, Board, BoardNet, Origin, Track};
use fab_elec_circuit::{Circuit, Net, NetKind};
use fab_elec_core::geom::ArcGeom;
use fab_elec_core::{mm, BoardId, ClassId, Contour, LayerId, NetId, P, Path, Seg, Shape};

#[test]
fn restyled_tracks_export_as_true_arcs_and_read_back_equal() {
    let mut board = Board::new(BoardId(0), "arcs");
    board.outline.push(Contour::rect(P::ZERO, P::mm(12.0, 10.0), 0));
    board.nets.push(BoardNet { name: "signal".into(), class: ClassId(0) });
    let mut circuit = Circuit::default();
    circuit.nets.push(Net { id: NetId(0), name: "signal".into(), class: ClassId(0), volts: None, kind: NetKind::Signal, scope: None });
    for (layer, widths, path) in [
        // Tangent line, CCW arc, line, CW arc, CCW arc; mids need not bisect arcs.
        (LayerId::F_CU, None, Path::new(P::mm(1.0, 1.0)).line(P::mm(3.0, 1.0))
            .arc(P::mm(3.6, 1.2), P::mm(4.0, 2.0)).line(P::mm(4.0, 4.0))
            .arc(P::mm(4.2, 4.6), P::mm(5.0, 5.0)).arc(P::mm(5.6, 5.2), P::mm(6.0, 6.0))),
        (LayerId::F_CU, Some(vec![mm(0.5), mm(0.35), mm(0.2)]), Path::new(P::mm(1.0, 3.0))
            .arc(P::mm(1.2, 3.6), P::mm(2.0, 4.0)).line(P::mm(3.0, 4.0)).arc(P::mm(3.6, 4.2), P::mm(4.0, 5.0))),
        // A 10 um sagitta across 10 mm gives a radius of about 1.25 m.
        (LayerId::F_CU, None, Path::new(P::mm(1.0, 8.0)).arc(P::mm(6.0, 8.01), P::mm(11.0, 8.0))),
        (LayerId::B_CU, None, Path::new(P::mm(2.0, 6.0)).arc(P::mm(2.2, 6.6), P::mm(3.0, 7.0)).line(P::mm(5.0, 7.0))),
    ] {
        let id = board.alloc_id();
        board.tracks.push(Track { id, net: NetId(0), layer, w: mm(0.2), widths, path,
            ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored });
    }
    let mut profile = ResolvedProfile::standard_2_layer("arcs", "A");
    fill_generic_tables(&mut profile, &board, &circuit, &PlacementRules::default());
    let e = export(&board, &circuit, &[], &profile).expect("arc board exports");
    assert!(e.validation.ok(), "arc export validates: {:?}", e.validation.failures().collect::<Vec<_>>());
    for (layer, name) in [(LayerId::F_CU, "arcs-F_Cu.gbr"), (LayerId::B_CU, "arcs-B_Cu.gbr")] {
        let expected: Vec<_> = board.tracks.iter().filter(|t| t.layer == layer).flat_map(|t|
            t.path.steps().enumerate().map(move |(i, (a, seg))| (a, seg, t.seg_width(i)))).collect();
        let arcs = expected.iter().filter(|(_, seg, _)| matches!(seg, Seg::Arc { .. })).count();
        let file = e.files.iter().find(|f| f.name == name).expect("copper file");
        let text = std::str::from_utf8(&file.bytes).unwrap();
        assert!(text.contains("G75"), "{name}: multi-quadrant arcs");
        if layer == LayerId::F_CU {
            assert!(text.contains("G02") && text.contains("G03"), "front has both arc directions");
        }
        assert_eq!(text.lines().filter(|l| l.contains("D01") && l.contains('I') && l.contains('J')).count(), arcs, "{name}: one arc command per arc");
        let image = read_gerber(&file.bytes).expect("arc Gerber reads back");
        assert_eq!(image.items.len(), expected.len(), "{name}: one item per segment, no chords");
        assert_eq!(image.items.iter().filter(|i| matches!(i.geom, Geom::Shape(Shape::Arc { .. }))).count(), arcs, "{name}: every arc stays an arc");
        for (item, &(a, seg, w)) in image.items.iter().zip(&expected) {
            let (got_a, got_b, got_w) = match &item.geom {
                Geom::Shape(Shape::Segment { a, b, w } | Shape::Arc { a, b, w, .. }) => (*a, *b, *w),
                other => panic!("{name}: unexpected track geometry {other:?}"),
            };
            assert!(a.dist(got_a) <= 2.0 && seg.end().dist(got_b) <= 2.0, "{name}: endpoints within 2 nm");
            assert_eq!(got_w, w, "{name}: exact segment width");
            match seg {
                Seg::Arc { mid, end } => {
                    let Geom::Shape(Shape::Arc { mid: got_mid, .. }) = &item.geom else { panic!("{name}: arc became a chord") };
                    let want = ArcGeom::new(a, mid, end).expect("model arc");
                    let got = ArcGeom::new(got_a, *got_mid, got_b).expect("read-back arc");
                    assert!((got.radius - want.radius).abs() <= 1000.0, "{name}: radius within 1 um");
                    assert!((got.center.0 - want.center.0).hypot(got.center.1 - want.center.1) <= 1000.0, "{name}: centre within 1 um");
                    assert!(want.at(0.5).dist(*got_mid) <= 1000.0, "{name}: midpoint on the model arc within 1 um");
                    assert_eq!(got.sweep.signum(), want.sweep.signum(), "{name}: arc direction preserved");
                }
                Seg::Line(_) => assert!(matches!(item.geom, Geom::Shape(Shape::Segment { .. })), "{name}: line stays a line"),
            }
        }
    }
}
