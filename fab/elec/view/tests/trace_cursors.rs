use fab_elec_view::trace::{
    columns::Columns,
    cursor::{eng, snap, Cursors, Snap},
    view::{ArraySource, TraceSource},
};
use std::sync::Arc;

#[test]
fn cursors_snap_and_read_out() {
    for (value, unit, sig, expected) in [
        (50.0123e-6, "s", 5, "50.012 µs"),
        (-0.0034, "V", 3, "-3.40 mV"),
        (0.0, "A", 3, "0.00 A"),
        (1234.5, "Hz", 4, "1.235 kHz"),
        (999.96e-6, "s", 4, "1.000 ms"),
        (-999.96e-6, "s", 4, "-1.000 ms"),
        (-12.345, "V", 4, "-12.35 V"),
        (-0.0, "V", 4, "0.000 V"),
        (1e-13, "s", 3, "1.00e-1 ps"),
        (5e9, "Hz", 3, "5.00 GHz"),
        (9.9996, "V", 4, "10.00 V"),
        (f64::NAN, "V", 4, "—"),
        (f64::INFINITY, "Hz", 5, "—"),
    ] {
        assert_eq!(eng(value, unit, sig), expected, "engineering notation for {value} {unit} with {sig} figures");
    }

    let times = Arc::<[f64]>::from([0.0, 2.0, 5.0]);
    let values = Arc::<[f32]>::from([1.0, 5.0, -1.0]);
    let sparse = Columns::reduce(&times, &values, 0.0, 10.0, 100);
    assert_eq!(snap(&sparse, 22.0, 6.0, 0.0), (20.0, Snap::Sample), "sparse samples snap to the closest time");
    assert_eq!(snap(&sparse, 18.0, 20.0, 0.0), (20.0, Snap::Sample), "sparse snapping chooses the nearest of several samples");
    assert_eq!(snap(&sparse, 28.0, 6.0, 0.0), (28.0, Snap::Free), "samples beyond the radius do not attract the cursor");
    assert_eq!(snap(&sparse, 20.0, 6.0, 5.0), (20.0, Snap::Sample), "samples have priority over a crossing at the same time");
    let empty = Columns::reduce(&[], &[], 0.0, 1.0, 100);
    assert_eq!(snap(&empty, 30.0, 6.0, 0.0), (30.0, Snap::Free), "an empty trace leaves the cursor free");

    let dense_times: Vec<f64> = (0..1000).map(|i| i as f64 / 1000.0).collect();
    for edge in [0.500, 0.503] {
        let square: Vec<f32> = dense_times.iter().map(|&t| if t < edge { -1.0 } else { 1.0 }).collect();
        let columns = Columns::reduce(&dense_times, &square, 0.0, 1.0, 100);
        let (position, kind) = snap(&columns, 52.0, 6.0, 0.0);
        assert_eq!(kind, Snap::Crossing, "a dense square wave snaps to an edge at {edge}");
        let true_crossing = ((edge - 0.0005) * 100.0) as f32;
        assert!((position - true_crossing).abs() < 0.5, "edge interpolation must be within half a column at {edge}: {position}");
        assert_eq!(snap(&columns, 80.0, 6.0, 0.0), (80.0, Snap::Free), "a flat plateau is not a peak or trough");
    }
    let sine: Vec<f32> = dense_times.iter().map(|&t| (t * std::f64::consts::TAU).sin() as f32).collect();
    let columns = Columns::reduce(&dense_times, &sine, 0.0, 1.0, 100);
    for (pointer, expected, target) in [(23.0, Snap::Peak, 25.0), (77.0, Snap::Trough, 75.0)] {
        let (position, kind) = snap(&columns, pointer, 6.0, 0.0);
        assert_eq!(kind, expected, "a dense sine without a nearby crossing snaps to {expected:?}");
        assert!((position - target).abs() < 1.0, "the {expected:?} must be within one column of the true extreme: {position}");
        let column = columns.cols_data.iter().find(|c| c.x0 <= position && position <= c.x1).expect("an extreme belongs to a column");
        assert_eq!(position, column.x0 + (column.x1 - column.x0) * 0.5, "an extreme snaps to the column's sample midpoint");
    }
    assert_eq!(snap(&columns, 12.0, 3.0, 0.0), (12.0, Snap::Free), "a monotone slope is not a local extreme");

    let source = ArraySource { t: times, ys: vec![values] };
    for (t, expected) in [(-1.0, None), (0.0, Some(1.0)), (1.0, Some(3.0)), (2.0, Some(5.0)), (3.5, Some(2.0)), (8.0, Some(-1.0))] {
        assert_eq!(source.value_at(0, t), expected, "array interpolation or endpoint holding failed at {t}");
    }
    assert_eq!(source.value_at(1, 1.0), None, "a missing trace has no cursor value");
    assert_eq!(source.value_at(0, f64::NAN), None, "a NaN time has no cursor value");
    assert_eq!(source.name(0), "ch1", "default trace names start at one");
    assert_eq!(source.unit(0), "V", "the default trace unit is volts");

    let mut cursors = Cursors { a: Some(50.012e-6), b: None };
    assert_eq!(cursors.time_lines(), ["A  50.012 µs"], "a lone A cursor has only its own read-out");
    cursors.b = Some(51.250e-6);
    assert_eq!(
        cursors.time_lines(),
        ["A  50.012 µs", "B  51.250 µs", "Δt 1.2380 µs   1/Δt 807.75 kHz"],
        "two cursors show five significant figures for time difference and reciprocal"
    );
    std::mem::swap(&mut cursors.a, &mut cursors.b);
    assert_eq!(cursors.time_lines()[2], "Δt -1.2380 µs   1/Δt 807.75 kHz", "delta time is signed while frequency uses its magnitude");
    cursors.b = cursors.a;
    assert_eq!(cursors.time_lines()[2], "Δt 0.0000 s   1/Δt —", "coincident cursors have no finite reciprocal");
    cursors.a = None;
    assert_eq!(cursors.time_lines(), ["B  51.250 µs"], "a lone B cursor has only its own read-out");
    assert!(Cursors::default().time_lines().is_empty(), "cleared cursors have no read-out");
}
