use fab_elec_bench::source::TriggerKind;
use fab_elec_bench::trigger::{events_qualified, validate_qualifier, validate_runt, Edge, EdgeDetector, EdgeTrigger, Qualifier, Slope};

const STEP: f64 = 1e-5;

// A sample inside the hysteresis band on each transition exercises pending acquisition.
fn train(short_period: bool) -> (Vec<f64>, Vec<f32>) {
    let rises: Vec<_> = (0..20).map(|pulse| 10 + pulse * 100 - if short_period && pulse >= 10 { 30 } else { 0 }).collect();
    (0..=2010).map(|sample| {
        let value = rises.iter().enumerate().find_map(|(pulse, &rise)| {
            let fall = rise + if !short_period && pulse == 10 { 20 } else { 50 };
            if sample == rise { Some(0.55) }
            else if sample > rise && sample < fall { Some(1.0) }
            else if sample == fall { Some(0.45) }
            else { None }
        }).unwrap_or(0.0);
        (sample as f64 * STEP, value)
    }).unzip()
}

fn trigger(slope: Slope) -> EdgeTrigger { EdgeTrigger { level: 0.5, hyst: 0.2, slope, holdoff: 0.0 } }

#[test]
fn pulse_widths_periods_and_pending_crossings_qualify_before_holdoff() {
    let (times, values) = train(false);
    let short = Qualifier::Width { min: 0.0, max: 0.3e-3 };
    let long = Qualifier::Width { min: 0.3e-3, max: f64::INFINITY };
    let edge = trigger(Slope::Falling);
    let runt = events_qualified(&times, &values, &edge, short);
    assert_eq!(runt.len(), 1, "only the short high pulse qualifies");
    assert!((runt[0] - 10.3e-3).abs() <= STEP, "the event is timed at that pulse's falling edge");
    let normal = events_qualified(&times, &values, &edge, long);
    assert_eq!(normal.len(), 19, "every normal pulse has a rising reference, including the first");
    assert!(!normal.contains(&runt[0]));
    let start = 11;
    let starts_high = events_qualified(&times[start..], &values[start..], &edge, long);
    assert_eq!(starts_high.len(), 18, "starting high loses the first falling edge's rising reference");
    assert!(starts_high[0] > 1e-3);

    let (period_times, period_values) = train(true);
    let period = Qualifier::Period { min: 0.0, max: 0.9e-3 };
    let short_period = events_qualified(&period_times, &period_values, &trigger(Slope::Rising), period);
    assert_eq!(short_period.len(), 1, "only the short rising-to-rising period qualifies");
    assert!((short_period[0] - 9.8e-3).abs() <= STEP);

    for (times, values, edge, qualifier, count) in [
        (&times, &values, edge, short, 1), (&times, &values, edge, long, 19),
        (&period_times, &period_values, trigger(Slope::Rising), period, 1),
    ] {
        let mut detector = EdgeDetector::with_qualifier(edge, qualifier);
        let mut pending = None;
        let mut confirmed = 0;
        for (&t, &y) in times.iter().zip(values) {
            match detector.push(t, y) {
                Edge::Accepted(at) => {
                    assert_eq!(pending.take(), Some(at), "every qualifying ramp exposes its crossing before confirmation");
                    confirmed += 1;
                }
                Edge::Dropped => panic!("a pending crossing on this train must qualify at confirmation"),
                _ => {}
            }
            if let Some(at) = detector.pending().into_iter().flatten().next() { pending = Some(at); }
        }
        assert_eq!(confirmed, count);
        assert!(pending.is_none());
        assert_eq!(detector.unqualified(), 20 - count, "the slope filter excludes reference edges from rejection counts");
        assert_eq!(detector.held_off(), 0);
    }

    let mut held = EdgeDetector::with_qualifier(EdgeTrigger { holdoff: 1.0, ..edge }, short);
    for (&t, &y) in times.iter().zip(&values) { held.push(t, y); }
    assert_eq!(held.events(), 1, "earlier nonqualifying pulses do not start holdoff");
    assert_eq!(held.held_off(), 0, "nonqualifying pulses are never counted as held off");
    assert_eq!(held.unqualified(), 19);
    held.set_qualifier(long);
    held.set(edge);
    assert_eq!(held.events(), 1, "edits preserve counters");
    assert_eq!(held.unqualified(), 19);
}

#[test]
fn breaks_forget_references_and_pending_recrossings_are_rechecked() {
    let (times, values) = train(true);
    let period = Qualifier::Period { min: 0.0, max: f64::INFINITY };
    let mut detector = EdgeDetector::with_qualifier(trigger(Slope::Rising), period);
    for (&t, &y) in times[..100].iter().zip(&values[..100]) { detector.push(t, y); }
    detector.break_stream();
    for (&t, &y) in times[100..200].iter().zip(&values[100..200]) { detector.push(t, y); }
    assert_eq!(detector.events(), 0, "the first rising edge after a break has no same-direction reference");
    for (&t, &y) in times[200..300].iter().zip(&values[200..300]) { detector.push(t, y); }
    assert_eq!(detector.events(), 1, "the next period has a fresh reference");

    let mut width = EdgeDetector::with_qualifier(trigger(Slope::Falling), Qualifier::Width { min: 0.0, max: 1.0 });
    width.push(0.0, 0.0); width.push(0.2, 1.0);
    assert!(matches!(width.push(0.4, 0.45), Edge::Pending(_)));
    assert!(width.pending()[1].is_some());
    assert_eq!(width.push(0.6, 0.55), Edge::Dropped, "a recrossing cancels the old candidate");
    assert_eq!(width.push(1.8, 0.45), Edge::None, "the later crossing exceeds the width bound");
    assert_eq!(width.pending(), [None, None]);
    assert_eq!(width.push(2.0, 0.0), Edge::None);
    assert_eq!(width.unqualified(), 1);
    width.push(2.2, 1.0);
    width.break_stream();
    width.push(2.3, 1.0); width.push(2.4, 0.0);
    assert_eq!(width.events(), 0, "a break also forgets the opposite-direction reference");

    let times: Vec<_> = (0..=7).map(|i| i as f64).collect();
    let values: Vec<_> = (0..=7).map(|i| (i % 2) as f32).collect();
    let mut held = EdgeDetector::with_qualifier(EdgeTrigger { holdoff: 3.0, ..trigger(Slope::Rising) }, Qualifier::Period { min: 2.0, max: 2.5 });
    for (&t, &y) in times.iter().zip(&values) { held.push(t, y); }
    assert_eq!(held.events(), 2, "held-off edges still provide the next period's reference; min is inclusive");
    assert_eq!(held.held_off(), 1);
    assert_eq!(held.unqualified(), 1, "only the first rising edge lacks a reference");
    assert!(events_qualified(&times, &values, &trigger(Slope::Rising), Qualifier::Period { min: 0.0, max: 2.0 }).is_empty(), "max is exclusive");

    assert_eq!(Qualifier::None.kind(), TriggerKind::Edge);
    assert_eq!(period.kind(), TriggerKind::Width);
    assert!(validate_qualifier(Qualifier::Width { min: 0.0, max: f64::INFINITY }));
    for (min, max) in [(-1.0, 1.0), (1.0, 1.0), (f64::INFINITY, f64::INFINITY), (0.0, f64::NAN)] {
        assert!(!validate_qualifier(Qualifier::Width { min, max }));
    }
}

#[test]
fn runts_accept_only_return_edges_and_keep_full_pulses_out_of_holdoff() {
    let values: Vec<f32> = [3.0, 1.0, 2.0, 1.0, 1.0, 1.0].into_iter()
        .flat_map(|peak| [-1.0, 0.125, 0.5, peak, 0.125, -0.125, -1.0, -1.0]).collect();
    let times: Vec<_> = (0..values.len()).map(|i| i as f64 * STEP).collect();
    let expected: Vec<_> = [12.5, 28.5, 36.5, 44.5].into_iter().map(|t| t * STEP).collect();
    for (slope, sign) in [(Slope::Rising, 1.0), (Slope::Falling, -1.0), (Slope::Either, 1.0), (Slope::Either, -1.0)] {
        let values: Vec<_> = values.iter().map(|y| y * sign).collect();
        let edge = EdgeTrigger { level: 0.0, hyst: 0.5, slope, holdoff: 0.0 };
        let qualifier = Qualifier::Runt { other: 2.0 * sign as f64 };
        let mut detector = EdgeDetector::with_qualifier(edge, qualifier);
        let mut accepted = Vec::new();
        let mut pending = None;
        let side = usize::from(sign > 0.0);
        for (&t, &y) in times.iter().zip(&values) {
            if let Edge::Accepted(at) = detector.push(t, y) {
                assert_eq!(pending.take(), Some(at), "{slope:?}/{sign}: only a qualifying return crossing is pending before confirmation");
                accepted.push(at);
            }
            assert_eq!(detector.pending()[1 - side], None, "{slope:?}/{sign}: outgoing crossings never become runt candidates");
            if let Some(at) = detector.pending()[side] { pending = Some(at); }
        }
        assert_eq!(accepted.len(), expected.len(), "{slope:?}/{sign}: only the four sub-threshold pulses are runts");
        for (got, want) in accepted.iter().zip(&expected) { assert!((got - want).abs() < 1e-12, "{slope:?}/{sign}: runt times are the interpolated return crossings"); }
        assert_eq!(detector.unqualified(), 2, "{slope:?}/{sign}: both overshooting and exactly touching the second level reject a full pulse");
        assert_eq!(detector.held_off(), 0, "{slope:?}/{sign}: the unbounded detector has no held-off runts");
        assert_eq!(events_qualified(&times, &values, &edge, qualifier), accepted, "{slope:?}/{sign}: batch events use the same incremental runt semantics");
        let mut held = EdgeDetector::with_qualifier(EdgeTrigger { holdoff: 12.0 * STEP, ..edge }, qualifier);
        for (&t, &y) in times.iter().zip(&values) { held.push(t, y); }
        assert_eq!(held.events(), 3, "{slope:?}/{sign}: holdoff begins only at an accepted runt return");
        assert_eq!(held.held_off(), 1, "{slope:?}/{sign}: the close runt is held off without extending holdoff");
        assert_eq!(held.unqualified(), 2, "{slope:?}/{sign}: full pulses remain unqualified rather than held off");
        let mut broken = values.clone();
        broken[35] = f32::NAN;
        let after_gap = events_qualified(&times, &broken, &edge, qualifier);
        assert_eq!(after_gap, vec![accepted[0], accepted[1], accepted[3]], "{slope:?}/{sign}: a nonfinite sample drops its active excursion and the next pulse re-arms normally");
    }
}

#[test]
fn runt_pending_recrossings_and_level_validation_preserve_the_pulse_side() {
    let edge = EdgeTrigger { level: 0.0, hyst: 0.5, slope: Slope::Rising, holdoff: 0.0 };
    let qualifier = Qualifier::Runt { other: 2.0 };
    let mut detector = EdgeDetector::with_qualifier(edge, qualifier);
    detector.push(0.0, -1.0); detector.push(1.0, 1.0);
    assert!(matches!(detector.push(2.0, -0.125), Edge::Pending(_)), "a sub-threshold excursion exposes its return before band confirmation");
    assert_eq!(detector.push(3.0, 2.0), Edge::Dropped, "a recrossing that reaches the second level cancels the pending runt");
    assert_eq!(detector.push(4.0, -0.125), Edge::None, "a later return cannot forget the raw excursion's extreme");
    detector.push(5.0, -1.0);
    assert_eq!((detector.events(), detector.unqualified()), (0, 1), "the full recrossed pulse is rejected once");
    detector.push(6.0, 1.0);
    detector.break_stream();
    detector.push(7.0, 1.0); detector.push(8.0, -1.0);
    assert_eq!(detector.events(), 0, "an explicit stream break forgets the outgoing edge");
    assert_eq!(qualifier.kind(), TriggerKind::Runt, "the source capability kind is runt");
    for other in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!validate_qualifier(Qualifier::Runt { other }), "nonfinite second levels fail qualifier validation");
    }
    for (slope, low, high) in [(Slope::Rising, false, true), (Slope::Falling, true, false), (Slope::Either, true, true)] {
        let trigger = EdgeTrigger { slope, ..edge };
        assert_eq!(validate_runt(&trigger, -2.0), low, "{slope:?}: negative runts require the falling or Either side");
        assert_eq!(validate_runt(&trigger, 2.0), high, "{slope:?}: positive runts require the rising or Either side");
        for other in [-0.5, 0.0, 0.5, f64::NAN, f64::INFINITY] {
            assert!(!validate_runt(&trigger, other), "{slope:?}: the full band and nonfinite levels are excluded");
        }
    }
    // The second level's side, not the slope, picks the pulse direction; no pair ever panics.
    let mut mirrored = EdgeDetector::with_qualifier(edge, Qualifier::Runt { other: -2.0 });
    for (t, y) in [(0.0, 1.0), (1.0, -1.0), (2.0, 1.0), (3.0, 1.0)] { mirrored.push(t, y); }
    assert_eq!(mirrored.events(), 1, "a runt level below a rising trigger watches negative pulses");
    mirrored.set_qualifier(Qualifier::Runt { other: 0.0 });
    mirrored.set(EdgeTrigger { hyst: 4.0, ..edge });
    for (t, y) in [(4.0, -3.0), (5.0, 3.0), (6.0, -3.0), (7.0, 3.0)] { mirrored.push(t, y); }
    assert_eq!(mirrored.events(), 1, "a level inside the band is accepted and finds no runts");
}
