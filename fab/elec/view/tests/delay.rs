//! Delay labels, live edge identity, and the bench's cross-lane hit test share one fixture.
use fab_elec_view::instruments::{
    axis::si_label,
    delay::{delay_label, follow_edge, lane_at},
    simfeed::{Feature, FeatureKind},
};
use makepad_widgets_core::{dvec2, Rect};

#[test]
fn delay_labels_following_and_lane_gaps() {
    use FeatureKind::{Fall, Rise, Top};
    const PERIOD: f64 = 100e-6;
    let features: Vec<_> = [(-1.0, Rise), (-0.7, Fall), (0.0, Rise), (0.1, Top), (0.3, Fall), (1.0, Rise)]
        .into_iter().map(|(cycle, kind)| {
            let t = cycle * PERIOD;
            Feature { kind, t, v: if kind == Top { 3.3 } else { 1.65 },
                crossings: (kind != Top).then_some([t - 1e-9, t, t + 1e-9]) }
        }).collect();
    let lanes = [
        (7, Rect { pos: dvec2(10.0, 20.0), size: dvec2(200.0, 60.0) }),
        (3, Rect { pos: dvec2(10.0, 90.0), size: dvec2(200.0, 60.0) }),
        (9, Rect { pos: dvec2(10.0, 160.0), size: dvec2(200.0, 60.0) }),
    ];

    assert_eq!(delay_label("PA0", Rise, "D1.2", Rise, 3.2e-9), "PA0 rise -> D1.2 rise 3.2 ns", "nanosecond delay names both nets and edge kinds");
    assert_eq!(delay_label("PA0", Fall, "D1.2", Rise, 12.5e-6), "PA0 fall -> D1.2 rise 12.5 µs", "microsecond delay uses the answer's micro prefix");
    assert_eq!(delay_label("D1.2", Rise, "PA0", Fall, -3.2e-9), "D1.2 rise -> PA0 fall −3.2 ns", "negative delays retain their direction and minus sign");
    assert_eq!(delay_label("A", Rise, "B", Fall, -12.5e-6), format!("A rise -> B fall {}", si_label(-12.5e-6, "s")), "delay precision and sign agree with si_label");
    assert_eq!(delay_label("A", Rise, "B", Rise, 0.0), "A rise -> B rise 0 s", "coincident edges have a zero delay");

    let mut kept = 0.0;
    for shift in [0.2, 0.6, 1.0, 0.7] {
        let shifted: Vec<_> = features.iter().map(|feature| Feature {
            t: feature.t + shift * PERIOD,
            crossings: feature.crossings.map(|crossings| crossings.map(|t| t + shift * PERIOD)), ..*feature
        }).collect();
        let next = follow_edge(&shifted, Rise, kept, PERIOD).expect("a sub-half-period capture shift keeps an edge");
        assert!((next - shift * PERIOD).abs() < 1e-15, "successive shifts smaller than half a period follow the original rise, not its neighbours");
        kept = next;
    }
    assert_eq!(follow_edge(&features, Rise, PERIOD * 0.29, PERIOD), Some(0.0), "following a rise ignores the nearer fall and top");
    assert_eq!(follow_edge(&features, Fall, PERIOD * 0.29, PERIOD), Some(PERIOD * 0.3), "following a fall preserves the edge kind");
    assert_eq!(follow_edge(&features[5..], Rise, 0.0, PERIOD), Some(PERIOD), "an edge exactly one period away is still followed");
    assert_eq!(follow_edge(&features[5..], Rise, -PERIOD * 0.01, PERIOD), None, "an edge beyond one period to the right is missing");
    assert_eq!(follow_edge(&features[..1], Rise, PERIOD * 0.01, PERIOD), None, "an edge beyond one period to the left is missing");
    assert_eq!(follow_edge(&features, Rise, 0.0, 0.0), Some(0.0), "an exact edge survives before a period is measured");
    assert_eq!(follow_edge(&[], Rise, 0.0, PERIOD), None, "an empty capture has no edge");
    assert_eq!(follow_edge(&features, Top, 0.0, PERIOD), None, "levels cannot become delay ends");
    assert_eq!(follow_edge(&features, Rise, 0.0, -PERIOD), None, "a negative period is invalid");
    assert_eq!(follow_edge(&features, Rise, f64::NAN, PERIOD), None, "a nonfinite kept time has no edge");

    assert_eq!(lane_at(lanes, dvec2(25.0, 30.0)), Some(7), "the upper card returns its stable channel, not its display index");
    assert_eq!(lane_at(lanes, dvec2(25.0, 100.0)), Some(3), "a cross-lane drag finds the middle card");
    assert_eq!(lane_at(lanes, dvec2(25.0, 200.0)), Some(9), "a cross-lane drag finds the lower card");
    assert_eq!(lane_at(lanes, dvec2(25.0, 85.0)), None, "the first lane gap is not a drop target");
    assert_eq!(lane_at(lanes, dvec2(25.0, 155.0)), None, "the second lane gap is not a drop target");
    assert_eq!(lane_at(lanes, dvec2(5.0, 100.0)), None, "a pointer beside the stack is outside every lane");
    assert_eq!(lane_at(lanes, dvec2(25.0, 230.0)), None, "a pointer below the stack is outside every lane");
    assert_eq!(lane_at(lanes.into_iter().rev(), dvec2(25.0, 100.0)), Some(3), "display reordering preserves the channel returned by its rectangle");
    assert_eq!(lane_at([], dvec2(25.0, 100.0)), None, "an empty lane stack has no target");
}
