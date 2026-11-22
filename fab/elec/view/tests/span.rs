//! Time brackets use the capture's interpolated waveform and fifty percent edge crossings.
use fab_elec_bench::measure_index::RecordIndex;
use fab_elec_view::instruments::{
    scope::pulled_off,
    simfeed::{lane_features, FeatureKind},
    span::{bracket_end_at, bracket_label, span_stats, End, SpanRequest, SpanStats},
};
use makepad_widgets_core::{dvec2, Rect};
use std::sync::Arc;

const PERIOD: f64 = 100e-6;
const EDGE: f64 = 2e-9;
const STEP: f64 = 50e-9;
const DELAY: f64 = 10e-6;

#[test]
fn uneven_pwm_bracket_statistics_and_latest_request() {
    let starts: Vec<_> = (0..=8).map(|cycle| DELAY + cycle as f64 * PERIOD).collect();
    let end = starts[8] + PERIOD * 0.5;
    // Uneven plateau samples, with fine samples on each two-nanosecond transition.
    let mut t: Vec<_> = (0..=(end / STEP).ceil() as usize)
        .map(|i| (i as f64 + if i % 2 == 0 { 0.0 } else { 0.37 }) * STEP).collect();
    for &start in &starts {
        for offset in [0.0, PERIOD * 0.3] {
            for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] { t.push(start + offset + fraction * EDGE); }
        }
    }
    t.sort_by(f64::total_cmp);
    t.dedup();
    let y: Vec<_> = t.iter().map(|&time| {
        let after = starts.partition_point(|&start| start <= time);
        if after == 0 { return 0.0; }
        let phase = time - starts[after - 1];
        let level = if phase < EDGE { phase / EDGE }
            else if phase < PERIOD * 0.3 { 1.0 }
            else { (1.0 - (phase - PERIOD * 0.3) / EDGE).max(0.0) };
        (3.3 * level) as f32
    }).collect();
    let index = RecordIndex::new(Arc::from(t.as_slice()), Arc::from(y.as_slice()));
    let trigger = starts[1] + EDGE * 0.5;
    let features = lane_features(&t, &y, &index, trigger);
    let t0 = starts[1] + PERIOD * 0.5 - trigger;
    let t1 = t0 + 5.0 * PERIOD;
    let stats = span_stats(&t, &y, trigger, &features, t0, t1).expect("five-period bracket is inside the capture");
    let edge_error = 3.3 * EDGE / PERIOD;
    assert!((stats.mean - 0.99).abs() < edge_error, "time-weighted mean is 0.99 V within the finite-edge error");
    assert!((stats.rms - 0.3_f64.sqrt() * 3.3).abs() < edge_error, "time-weighted RMS is sqrt(duty) times amplitude within the finite-edge error");
    assert_eq!(stats.min, 0.0, "minimum includes the low plateau");
    assert!((stats.max - 3.3).abs() < 1e-6, "maximum includes the 3.3 V high plateau");
    assert_eq!((stats.rises, stats.falls), (5, 5), "five periods contain five rises and five falls");
    assert_eq!((stats.t0, stats.t1), (t0, t1), "reported boundaries preserve trigger-relative seconds");
    assert_eq!(stats.samples as usize, t.iter().filter(|&&time| time - trigger >= t0 && time - trigger <= t1).count(), "sample count includes original samples in the closed bracket");
    let shift = STEP * 0.25;
    let shifted = span_stats(&t, &y, trigger, &features, t0 + shift, t1 + shift).expect("shifted bracket remains inside the capture");
    assert!((shifted.mean / stats.mean - 1.0).abs() < 1e-6, "time weighting keeps the mean within one part per million after a quarter-spacing shift");

    let rise = features.iter().find(|feature| feature.kind == FeatureKind::Rise && feature.t > t0).expect("bracket has a rising feature").t;
    let fall = features.iter().find(|feature| feature.kind == FeatureKind::Fall && feature.t > rise).expect("rising feature has a following fall").t;
    let high = span_stats(&t, &y, trigger, &features, rise, fall).expect("crossing-to-crossing bracket is valid");
    assert_eq!((high.rises, high.falls), (1, 0), "edge counts include the left fifty percent crossing and exclude the right one");
    let start = starts[2];
    let ramp = span_stats(&t, &y, trigger, &features, start + EDGE * 0.1 - trigger, start + EDGE * 0.2 - trigger)
        .expect("a bracket between two samples still interpolates both endpoints");
    assert!((ramp.mean - 3.3 * 0.15).abs() < 1e-6, "a partial linear ramp has the mean of its interpolated endpoints");
    assert!((ramp.min - 3.3 * 0.1).abs() < 1e-6 && (ramp.max - 3.3 * 0.2).abs() < 1e-6, "partial-ramp extrema exclude samples outside the bracket");
    assert_eq!(ramp.samples, 0, "interpolated endpoints do not count as original samples");
    let whole = span_stats(&t, &y, trigger, &features, t[0] - trigger, t[t.len() - 1] - trigger)
        .expect("both record endpoints belong to the record");
    assert_eq!(whole.samples as usize, t.len(), "a whole-record bracket counts both endpoints exactly once");
    assert!(span_stats(&t, &y, trigger, &features, t1, t0).is_none(), "reversed bracket has no statistics");
    assert!(span_stats(&t, &y, trigger, &features, t0, t0).is_none(), "empty bracket has no statistics");
    assert!(span_stats(&t, &y, trigger, &features, t[0] - trigger - STEP, t1).is_none(), "partially outside bracket is not silently clipped");
    assert!(span_stats(&t, &y, trigger, &features, end, end + PERIOD).is_none(), "wholly outside bracket has no statistics");

    let request = SpanRequest::default();
    let (initial, span) = request.get();
    assert_eq!(span, None, "a new span mailbox starts clear");
    request.set(Some((t0, t1)));
    let first = request.get().0;
    request.set(Some((t0 + shift, t1 + shift)));
    request.set(Some((rise, fall)));
    let (latest, span) = request.get();
    assert_eq!(span, Some((rise, fall)), "multiple writes retain only the latest complete span");
    assert!(first > initial && latest > first, "request generations grow with writes");
    assert_eq!(latest - initial, 3, "public generation advances once per write");
    request.set(None);
    let (cleared, span) = request.get();
    assert_eq!(span, None, "clearing replaces the latest span");
    assert!(cleared > latest, "clearing also advances the generation");
}

#[test]
fn bracket_labels_and_end_slop() {
    let stats = SpanStats { t0: 0.0, t1: 50e-6, mean: 0.99, rms: 1.81, min: 0.0, max: 3.3, rises: 3, falls: 2, samples: 100 };
    assert_eq!(bracket_label(50e-6, Some(stats), true, "V"), "50 µs (20 kHz)   mean 990 mV   rms 1.81 V   pp 3.3 V   5 edges", "bracket numbers use answer SI formatting and count both slopes");
    assert_eq!(bracket_label(50e-6, Some(stats), false, "V"), "50 µs (20 kHz)   measuring", "an old answer cannot appear under new geometry");
    assert_eq!(bracket_label(50e-6, None, true, "V"), "50 µs (20 kHz)", "an answered empty span shows duration and reciprocal alone");
    assert_eq!(bracket_label(0.0, None, false, "V"), "0 s   measuring", "a creation press has no infinite reciprocal");
    assert_eq!(bracket_label(50e-6, Some(stats), true, "A"), "50 µs (20 kHz)   mean 990 mA   rms 1.81 A   pp 3.3 A   5 edges", "current lanes retain their physical units");
    assert_eq!(bracket_end_at(6.0, 10.0, 30.0, 4.0), Some(End::Start), "left end includes four-point slop");
    assert_eq!(bracket_end_at(34.0, 10.0, 30.0, 4.0), Some(End::Finish), "right end includes four-point slop");
    assert_eq!(bracket_end_at(5.99, 10.0, 30.0, 4.0), None, "outside slop leaves the pan gesture alone");
    assert_eq!(bracket_end_at(34.01, 10.0, 30.0, 4.0), None, "right slop does not extend beyond four points");
    assert_eq!(bracket_end_at(20.0, 10.0, 30.0, 4.0), None, "band interior is not an end");
    assert_eq!(bracket_end_at(12.0, 10.0, 14.0, 4.0), Some(End::Start), "overlapping slop has deterministic ties");
    assert_eq!(bracket_end_at(13.0, 10.0, 14.0, 4.0), Some(End::Finish), "nearest end wins in overlapping slop");
    assert_eq!(bracket_end_at(10.0, 10.0, 10.0, 4.0), Some(End::Start), "coincident ends remain draggable");
}

#[test]
fn bracket_removal_uses_only_the_cross_axis_margin() {
    let lane = Rect { pos: dvec2(100.0, 80.0), size: dvec2(200.0, 120.0) };
    for (distance, off) in [(23.9, false), (24.0, false), (24.1, true)] {
        for (pointer, across_y, side) in [
            (dvec2(200.0, 80.0 - distance), true, "above"),
            (dvec2(200.0, 200.0 + distance), true, "below"),
            (dvec2(100.0 - distance, 140.0), false, "left"),
            (dvec2(300.0 + distance, 140.0), false, "right"),
        ] {
            assert_eq!(pulled_off(lane, pointer, across_y), off, "{distance} pt {side}: removal starts only beyond 24 pt");
        }
    }
    for (pointer, across_y, side) in [
        (dvec2(0.0, 140.0), true, "left"),
        (dvec2(400.0, 140.0), true, "right"),
        (dvec2(200.0, -20.0), false, "above"),
        (dvec2(200.0, 300.0), false, "below"),
    ] {
        assert!(!pulled_off(lane, pointer, across_y), "{side} along the bracket's own drag axis never previews removal");
    }
}

#[test]
fn paused_capture_answers_new_brackets_and_clears_without_more_samples() {
    use fab_elec_bench::trigger::{EdgeTrigger, Slope, Window};
    use fab_elec_sim::stamp::{UnkKind, UnknownInfo};
    use fab_elec_view::instruments::{acquire::NetProbe, simfeed::{SimScopeConfig, SimScopeFeed}};
    use makepad_widgets_core::Cx;
    use std::time::Duration;

    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut feed = SimScopeFeed::show_with_capture_interval(&mut cx,
        NetProbe { pos: "ramp".into(), neg: None, scale: 1.0 },
        SimScopeConfig { plot: (32, 24), volts: (0.0, 4.0),
            trigger: EdgeTrigger { level: 1.5, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 },
            qualifier: fab_elec_bench::trigger::Qualifier::None, zone: None,
            window: Window { pre: 0.0005, post: 0.001 }, auto: None, frame_every: Duration::from_millis(1) },
        Duration::MAX).expect("capture worker starts");
    let mut sink = feed.sinks().make();
    // Unknown 0 is ground in a solver's vector: the probed net is the next unknown.
    sink.begin(&[UnknownInfo { name: "0".into(), kind: UnkKind::Voltage }, UnknownInfo { name: "ramp".into(), kind: UnkKind::Voltage }]);
    for i in 0..=40 { sink.point(i as f64 * 0.0001, None, &[0.0, i as f64 * 0.1]); }
    sink.idle();
    // The one requested capture is complete before the first bracket is set. No more points,
    // idle flushes or captures follow; only the measurement worker continues to run.
    let features = loop {
        if let Some(features) = feed.features() { break features; }
        std::thread::sleep(Duration::from_millis(1));
    };
    let answer = |expected| loop {
        if let Some((generation, stats)) = feed.spans() {
            if generation == expected { break stats; }
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(answer(0).iter().all(Option::is_none), "capture without a bracket publishes no stats");
    let generation = feed.set_span(Some((0.0, 0.0005)));
    let stats = answer(generation)[0].expect("a new bracket measures the already retained capture");
    assert!((stats.mean - 1.75).abs() < 1e-6, "retained ramp integrates the requested interval");
    assert!((stats.min - 1.5).abs() < 1e-6 && (stats.max - 2.0).abs() < 1e-6, "retained extrema belong to the bracket");
    feed.set_span(Some((0.0, 0.0002)));
    let latest = feed.set_span(Some((0.0001, 0.0004)));
    let moved = answer(latest)[0].expect("latest bracket move is answered without another capture");
    assert_eq!((moved.t0, moved.t1), (0.0001, 0.0004), "only the latest request is accepted by generation");
    let cleared = feed.set_span(None);
    assert!(answer(cleared).iter().all(Option::is_none), "clearing publishes no stale lane stats");
    let again = feed.set_span(Some((0.0, 0.0005)));
    assert_eq!(answer(again)[0], Some(stats), "clearing keeps the samples for a later bracket");
    let outside = feed.set_span(Some((1.0, 2.0)));
    assert!(answer(outside).iter().all(Option::is_none), "an out-of-capture bracket is answered with no stats");
    assert!(Arc::ptr_eq(&features, &feed.features().expect("capture is still published")), "bracket edits never replaced the captured features");
    feed.stop();
}
