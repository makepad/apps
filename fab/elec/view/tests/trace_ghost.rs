use fab_elec_view::trace::view::{align_by_marks, ghost_window, ArraySource, Ghost, GhostAlign, TraceSource};
use std::sync::Arc;

#[test]
fn ghost_aligns_and_maps() {
    let live = [8.0, 10.0, 12.0];
    for at in [0.0, 8.0, 9.25, 12.0, 20.0] {
        assert_eq!(align_by_marks(&live, &live, at), Some(0.0), "equal marks need no shift at {at}");
    }
    let period = 2.0;
    let earlier = live.map(|time| time - 3.0 * period);
    assert_eq!(align_by_marks(&live, &earlier, 12.0), Some(3.0 * period), "the earlier run moves forward by three periods");
    assert_eq!(align_by_marks(&[0.0, 10.0], &[4.0, 9.0], 4.9), Some(-4.0), "the nearer earlier live mark anchors the shift");
    assert_eq!(align_by_marks(&[0.0, 10.0], &[4.0, 9.0], 5.1), Some(1.0), "the ghost is nearest the chosen live mark, not the query");
    assert_eq!(align_by_marks(&[0.0, 10.0], &[2.0, 13.0], 5.0), Some(-2.0), "tied live marks choose the earlier one");
    assert_eq!(align_by_marks(&[10.0], &[8.0, 12.0], 10.0), Some(2.0), "tied ghost marks choose the earlier one");
    assert_eq!(align_by_marks(&[], &live, 10.0), None, "empty live marks have no alignment");
    assert_eq!(align_by_marks(&live, &[], 10.0), None, "empty ghost marks have no alignment");
    assert_eq!(align_by_marks(&[], &[], 10.0), None, "two empty runs have no alignment");
    assert_eq!(align_by_marks(&live, &live, f64::NAN), None, "a NaN query has no alignment");
    for (at, expected) in [(0.0, -8.0), (18.0, -8.0), (40.0, 2.0)] {
        assert_eq!(align_by_marks(&[1.0, 10.0, 20.0, 30.0], &[9.0, 28.0], at), Some(expected), "uneven lists use nearest times at {at}");
    }
    let many: Vec<f64> = (0..100_000).map(|i| f64::from(i) * 2.0).collect();
    let shifted: Vec<f64> = many.iter().map(|time| time - 0.25).collect();
    assert_eq!(align_by_marks(&many, &shifted, 150_000.6), Some(0.25), "large records find the nearest pair");

    let window = (7.0, 13.0);
    assert_eq!(ghost_window(window, GhostAlign::Time), window, "time alignment keeps the window");
    assert_eq!(ghost_window(window, GhostAlign::Shift(6.0)), (1.0, 7.0), "a positive shift reads an earlier window");
    assert_eq!(ghost_window(window, GhostAlign::Shift(-2.0)), (9.0, 15.0), "a negative shift reads a later window");

    let source: Arc<dyn TraceSource> =
        Arc::new(ArraySource { t: Arc::from([0.0, 2.0, 5.0]), ys: vec![Arc::from([100.0, 100.0, 100.0]), Arc::from([1.0, 5.0, -1.0])] });
    let ghost = Ghost { source, traces: vec![Some(1), None], align: GhostAlign::Shift(6.0) };
    for (time, expected) in [(5.0, None), (6.0, Some(1.0)), (7.0, Some(3.0)), (8.0, Some(5.0)), (9.5, Some(2.0)), (11.0, Some(-1.0))] {
        let mapped = ghost_window((time, time), ghost.align).0;
        let value = ghost.traces[0].and_then(|g| ghost.source.value_at(g, mapped));
        assert_eq!(value, expected, "the mapped signal reads the ghost at live time minus shift for {time}");
        assert_eq!(ghost.traces[1].and_then(|g| ghost.source.value_at(g, mapped)), None, "an unmapped signal has no ghost value");
    }
}
