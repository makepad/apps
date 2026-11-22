use fab_elec_bench::edges::{Edge, EdgeTracker, Threshold};
use fab_elec_bench::measure::{self, Slope};
#[path = "support/pwm.rs"] mod pwm;

#[test]
fn edges_share_threshold_timing_and_reject_chatter() {
    let threshold = Threshold { level: 0.5, hysteresis: 0.2 };
    let t: Vec<_> = (0..10).map(|i| i as f64).collect();
    let y = [0.0, 0.48, 0.52, 0.49, 0.51, 1.0, 0.52, 0.48, 0.51, 0.0];
    let edges = measure::edges(&t, &y, threshold);
    assert_eq!(edges.len(), 2, "sub-band noise cannot add edges");
    assert_eq!(edges[0], Edge { t: 3.5, dir: Slope::Rising }, "last threshold crossing wins over an earlier candidate");
    assert!(edges[1].t > 8.0 && edges[1].t < 9.0, "fall uses the last crossing, not the band end");
    assert!(measure::edges(&t, &y, Threshold { hysteresis: 0.0, ..threshold }).len() > edges.len(), "zero hysteresis exposes the same noise crossings");
    let mut tracker = EdgeTracker::new(threshold);
    assert_eq!(tracker.high(), None, "initial state is unknown");
    let streamed: Vec<_> = t.chunks(3).zip(y.chunks(3)).flat_map(|(t, y)| {
        t.iter().zip(y).filter_map(|(&t, &y)| tracker.push(t, y as f64)).collect::<Vec<_>>()
    }).collect();
    assert_eq!(streamed, edges, "chunked detection is bit-identical to offline edges");
    assert_eq!(tracker.high(), Some(false), "last decisive state is low");
    assert_eq!(tracker.push(f64::NAN, 1.0), None, "nonfinite time makes no edge");
    assert_eq!(tracker.push(10.0, f64::INFINITY), None, "nonfinite voltage makes no edge");
    assert_eq!(tracker.push(8.0, 1.0), None, "older time makes no edge");
    tracker.reset();
    assert_eq!(tracker.high(), None, "reset clears state");
    assert_eq!(tracker.push(0.0, 0.5), None, "initial band sample is not an edge");
    assert_eq!(tracker.high(), None, "initial band sample remains undecided");

    let skipped = measure::edges(&[0.0, 1.0, f64::NAN, 2.0, 3.0], &[0.0, f32::NAN, 1.0, 1.0, 0.0], threshold);
    assert_eq!(skipped, vec![Edge { t: 1.0, dir: Slope::Rising }, Edge { t: 2.5, dir: Slope::Falling }], "offline helpers skip invalid samples exactly like the streaming tracker");

    let (t, y) = pwm::record(0.008, 0.0);
    let timing = measure::timing(&t, &y, threshold);
    assert!((timing.frequency.unwrap() - 1000.0).abs() < 6.0, "jittered frequency is within one step");
    assert!((timing.duty.unwrap() - 30.0).abs() < 0.6, "jittered duty is within one step");
    assert!((timing.width_high.unwrap() - 0.0003).abs() < 0.000006, "high width is within one step");
    assert!((timing.width_low.unwrap() - 0.0007).abs() < 0.000006, "low width is within one step");
    assert_eq!(timing.n_cycles, 7, "only complete rising cycles count");
    assert!(timing.sigma_period.unwrap() < 0.000006, "period jitter is bounded by sampling");
    let pulses = measure::pulses(&t, &y, threshold);
    assert_eq!(pulses.len(), 15, "only pulses bounded by two edges are returned");
    assert!(pulses.windows(2).all(|p| p[0].high != p[1].high), "complete pulses alternate");
    assert_eq!(pulses[0].period, None, "first pulse has no preceding period");
    assert!((pulses[2].period.unwrap() - 0.001).abs() < 0.000006, "pulse period refers to the preceding same-slope edge");
}
