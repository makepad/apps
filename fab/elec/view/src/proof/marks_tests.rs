use super::*;
use crate::canvas::looks::DARK;
use std::f32::consts::FRAC_PI_2;

#[test]
fn marks_preserve_geometry_style_order_and_identity() {
    let hits = [
        Hit { id: 11, at: [2.0, -3.0], to: None, dia: 0.8, plated: Some(true) },
        Hit { id: 12, at: [-2.0, 1.0], to: Some([1.0, 5.0]), dia: 1.0, plated: Some(false) },
        Hit { id: 13, at: [6.0, 7.0], to: None, dia: 1.2, plated: None },
    ];
    let drills = drill_insts(&hits, &DARK);
    assert_eq!(drills.len(), 3, "drill count: one instance per hit");
    let [Inst::Arc(round), Inst::Box(slot), Inst::Arc(unknown)] = drills.as_slice() else {
        panic!("drill order: round disc, slot box, unknown-plating disc");
    };
    for (disc, hit) in [(round, &hits[0]), (unknown, &hits[2])] {
        assert_eq!(disc.circle, [hit.at[0], hit.at[1], hit.dia / 2.0, 0.0], "round geometry: centre and radius in millimetres");
        assert_eq!(disc.sweep, [0.0, TAU], "round sweep: full circle");
        assert_eq!(disc.fill, DARK.canvas, "round fill: canvas token");
        assert_eq!(disc.stroke, [1.0, 0.0], "round stroke: one screen point");
        assert_eq!(disc.c.ink, DARK.ink, "round ink: plated and unspecified use ink token");
        assert_eq!(disc.c.pose_a, [0.0, 0.0, 0.0, 1.0], "round pose: identity start");
        assert_eq!(disc.c.pose_b, disc.c.pose_a, "round pose: identity end");
    }
    assert_eq!(&slot.c.pose_a[..2], &[-0.5, 3.0], "slot pose: endpoint midpoint");
    assert!((slot.c.pose_a[2] - 0.9272952).abs() < 1e-6, "slot pose: angle from first endpoint to second");
    assert_eq!(slot.c.pose_a[3], 1.0, "slot pose: full opacity");
    assert_eq!(slot.c.pose_b, slot.c.pose_a, "slot pose: fixed start and end");
    assert_eq!(slot.rect, [0.0, 0.0, 3.0, 0.5], "slot rectangle: half length plus radius, half diameter");
    assert_eq!(slot.corner, 0.5, "slot corner: radius");
    assert_eq!(slot.fill, DARK.canvas, "slot fill: canvas token");
    assert_eq!(slot.stroke, [1.0, 0.0], "slot stroke: one screen point");
    assert_eq!(slot.c.ink, DARK.ink_dim, "slot ink: unplated uses dim ink token");
    for (inst, hit) in drills.iter().zip(&hits) {
        assert_eq!(inst.common().item, hit.id as f32, "drill identity: each hit retains its item id");
        assert_eq!(inst.common().look, Look::Committed.code(), "drill look: committed");
    }

    let dots = [
        Dot { id: 21, refdes: "U1".into(), at: [2.0, -3.0], side: Side::Top, rot: FRAC_PI_2 },
        Dot { id: 22, refdes: "U2".into(), at: [-4.0, 5.0], side: Side::Bottom, rot: FRAC_PI_2 },
    ];
    let placements = dot_insts(&dots, &DARK);
    assert_eq!(placements.len(), 4, "placement count: two instances per dot");
    let top_fill = [DARK.selection[0], DARK.selection[1], DARK.selection[2], 0.9];
    for ((pair, dot), fill) in placements.chunks_exact(2).zip(&dots).zip([top_fill, DARK.hover]) {
        let [Inst::Arc(disc), Inst::Line(tick)] = pair else {
            panic!("placement order: disc before orientation tick");
        };
        assert_eq!(disc.circle, [dot.at[0], dot.at[1], 0.2, 0.0], "placement disc: centre and 0.2 mm radius");
        assert_eq!(disc.sweep, [0.0, TAU], "placement disc: full circle");
        assert_eq!(disc.fill, fill, "placement fill: top selection at alpha 0.9, bottom hover");
        assert_eq!(disc.c.ink, fill, "placement ink: side colour");
        assert_eq!(disc.stroke, [0.0; 2], "placement disc: stroke disabled");
        assert_eq!(&tick.seg[..2], &dot.at, "placement tick: starts at centre");
        assert!((tick.seg[2] - dot.at[0]).abs() < 1e-6, "placement rotation: 90 degrees has zero horizontal offset");
        assert!((tick.seg[3] - dot.at[1] - 0.8).abs() < 1e-6, "placement rotation: 90 degrees points 0.8 mm up");
        assert_eq!(tick.width, [1.0, 0.08], "placement tick width: screen point and world floor");
        assert_eq!(tick.c.ink, DARK.ink, "placement tick ink: ink token");
        for inst in pair {
            assert_eq!(inst.common().item, dot.id as f32, "placement identity: disc and tick retain item id");
            assert_eq!(inst.common().look, Look::Committed.code(), "placement look: committed");
            assert_eq!(inst.common().pose_a, [0.0, 0.0, 0.0, 1.0], "placement pose: identity start");
            assert_eq!(inst.common().pose_b, inst.common().pose_a, "placement pose: identity end");
        }
    }

    let marker = Marker { id: 31, at: [4.0, -5.0], file: "drill.drl".into(), cause: "Missing hole".into(), hint: "Check the drill export".into() };
    let failures = marker_insts(std::slice::from_ref(&marker), &DARK);
    assert_eq!(failures.len(), 3, "marker count: three instances per failure");
    let [Inst::Arc(ring), Inst::Line(horizontal), Inst::Line(vertical)] = failures.as_slice() else {
        panic!("marker order: ring, horizontal crosshair, vertical crosshair");
    };
    assert_eq!(ring.circle, [4.0, -5.0, 0.8, 0.0], "marker ring: centre and 0.8 mm radius");
    assert_eq!(ring.sweep, [0.0, TAU], "marker ring: full circle");
    assert_eq!(ring.stroke, [2.0, 0.0], "marker ring: two screen point stroke");
    assert_eq!(ring.fill, [0.0; 4], "marker ring: no fill");
    for (line, expected) in [(horizontal, [2.8, -5.0, 5.2, -5.0]), (vertical, [4.0, -6.2, 4.0, -3.8])] {
        for (actual, expected) in line.seg.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6, "marker crosshair: axis aligned through centre with 1.2 mm half length");
        }
        assert_eq!(line.width, [1.0, 0.0], "marker crosshair: one screen point width");
    }
    for inst in &failures {
        assert_eq!(inst.common().item, marker.id as f32, "marker identity: all three instances retain item id");
        assert_eq!(inst.common().look, Look::Violation.code(), "marker look: violation on ring and crosshairs");
        assert_eq!(inst.common().ink, DARK.violation, "marker ink: violation token on ring and crosshairs");
        assert_eq!(inst.common().pose_a, [0.0, 0.0, 0.0, 1.0], "marker pose: identity start");
        assert_eq!(inst.common().pose_b, inst.common().pose_a, "marker pose: identity end");
    }

    assert!(drill_insts(&[], &DARK).is_empty(), "empty drills: no instances");
    assert!(dot_insts(&[], &DARK).is_empty(), "empty dots: no instances");
    assert!(marker_insts(&[], &DARK).is_empty(), "empty markers: no instances");
}
