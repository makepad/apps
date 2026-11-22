use fab_elec_route::organic::{
    geom::{Segment2, Vec2},
    model::{Trace, TracePoint},
    settings::OrganicSettings,
    spline::{build_curve, centripetal_cr},
};

#[test]
fn plain_spline_geometry() {
    let settings = OrganicSettings::base();
    let mut line = Trace {
        points: vec![
            TracePoint { pos: Vec2::new(1.0, 2.0), width: 0.25, ..TracePoint::default() },
            TracePoint { pos: Vec2::new(4.0, 6.0), width: 0.5, ..TracePoint::default() },
        ],
        ..Trace::default()
    };
    build_curve(&mut line, &settings, 6, None);
    assert_eq!(line.smoothed.len(), 2, "two-node export contains exactly two nodes");
    for (i, point) in line.points.iter().enumerate() {
        assert_eq!(
            (line.smoothed[i].x, line.smoothed[i].y), (point.pos.x, point.pos.y),
            "two-node export preserves node {i} exactly"
        );
    }
    assert_eq!(line.point_widths, vec![0.25, 0.5], "two-node export preserves node widths");
    assert_eq!(line.length, 5.0, "two-node export has the 3-4-5 chord length");

    let mut zigzag = Trace {
        points: [Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0), Vec2::new(2.0, 0.0), Vec2::new(3.0, 1.0)]
            .into_iter()
            .map(|pos| TracePoint { pos, ..TracePoint::default() })
            .collect(),
        ..Trace::default()
    };
    build_curve(&mut zigzag, &settings, 6, None);
    assert_eq!(zigzag.smoothed.len(), 3 * 6 + 1, "zigzag exports six samples per chord and the final node");
    assert_eq!((zigzag.smoothed[0].x, zigzag.smoothed[0].y), (0.0, 0.0), "zigzag preserves its first endpoint");
    assert_eq!((zigzag.smoothed[18].x, zigzag.smoothed[18].y), (3.0, 1.0), "zigzag preserves its last endpoint");
    for (s, point) in zigzag.points.iter().enumerate() {
        assert_eq!(
            (zigzag.smoothed[s * 6].x, zigzag.smoothed[s * 6].y), (point.pos.x, point.pos.y),
            "zigzag interpolates node {s} exactly at sample s * 6"
        );
    }
    let chord_sum: f32 = zigzag.smoothed.windows(2).map(|pair| pair[0].distance_to(pair[1])).sum();
    assert_eq!(zigzag.length, chord_sum, "zigzag length equals the sum of its smoothed chords");

    for slack in [0.0f64, 0.05] {
        build_curve(&mut zigzag, &settings, 6, Some(&[slack; 3]));
        assert_eq!(zigzag.smoothed.len(), 19, "slack {slack} preserves the sample count");
        for i in 0..3 {
            let chord = Segment2::new(zigzag.points[i].pos, zigzag.points[i + 1].pos);
            for s in 0..=6 {
                let distance = chord.distance_to_point(zigzag.smoothed[i * 6 + s]) as f64;
                assert!(
                    distance <= slack + 1e-6,
                    "slack {slack} bounds sample {s} of chord {i}: distance {distance}"
                );
            }
        }
    }

    zigzag.points[1].straight_before = true;
    build_curve(&mut zigzag, &settings, 6, None);
    let stub = Segment2::new(zigzag.points[0].pos, zigzag.points[1].pos);
    for s in 0..=6 {
        assert!(
            stub.distance_to_point(zigzag.smoothed[s]) <= 1e-6,
            "straight stub keeps segment 0 sample {s} on its chord"
        );
    }

    let p0 = Vec2::new(-1.0, 2.0);
    let p1 = Vec2::new(0.0, 0.0);
    let p2 = Vec2::new(2.0, 1.0);
    let p3 = Vec2::new(3.0, -1.0);
    assert!(
        centripetal_cr(p0, p1, p2, p3, 0.0).distance_to(p1) <= 1e-5,
        "centripetal Catmull-Rom at s = 0 returns p1"
    );
    assert!(
        centripetal_cr(p0, p1, p2, p3, 1.0).distance_to(p2) <= 1e-5,
        "centripetal Catmull-Rom at s = 1 returns p2"
    );
}
