use fab_elec_instr::synth::Synth;
use fab_elec_bench::trigger::*;
use fab_elec_view::instruments::{
    phosphor::{decay_keep, Persistence as Picture},
    scope::{Persistence, ScopeSettings},
    scrub::measurement_rows,
    trigger_ui::{afterglow_at, afterglow_position, centred_level, ghost_level, holdoff_drag, StopAtTrigger},
};

#[test]
fn afterglow_track_and_decay() {
    let dt = 1.0 / 60.0;
    let mut settings = ScopeSettings::default();
    assert_eq!(afterglow_at(0.0), Persistence::Off, "left detent is off");
    assert_eq!(afterglow_at(1.0), Persistence::Infinite, "right detent is forever");
    assert_eq!(afterglow_position(Persistence::Off), 0.0, "off maps to the left end");
    assert_eq!(afterglow_position(Persistence::Infinite), 1.0, "forever maps to the right end");
    for position in [0.04, 0.13, 0.27, 0.5, 0.73, 0.87, 0.96] {
        let value = afterglow_at(position);
        assert!((afterglow_position(value) - position).abs() < 1e-12, "log position round-trips at {position}");
        let expected = 0.01 * 1000.0_f64.powf((position - 0.04) / 0.92);
        assert!((value.tau() - expected).abs() < 1e-12, "track follows a continuous log scale at {position}");
    }
    for position in [0.0, 0.04, 0.2, 0.5, 0.8, 0.96, 1.0] {
        let value = afterglow_at(position);
        settings.persistence = value;
        assert_eq!(settings.look(false).tau, value.tau(), "continuous afterglow reaches the screen at {position}");
        let keep = decay_keep(settings.look(false).tau, dt);
        let (mut picture, mut blends) = (Picture::default(), Vec::new());
        let _ = picture.frame(1, 1, &[], 0.0, value.tau(), &mut blends);
        assert!(picture.frame(1, 1, &[], dt, value.tau(), &mut blends));
        let blend = blends[0];
        assert!((f64::from(blend.keep) - keep / (keep + 1.0)).abs() < 1e-7, "afterglow uses phosphor's decay factor at {position}");
        assert!((f64::from(blend.scale) - 1.0 / (keep + 1.0)).abs() < 1e-7, "afterglow preserves waveform normalization at {position}");
    }
}

#[test]
fn level_uses_published_swing_and_destination_view() {
    let t = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7];
    let edge = measurement_rows(&t, &[-2.0, -2.0, 6.0, 6.0, -2.0, -2.0, 6.0, 6.0], 0.1);
    let dc = measurement_rows(&t, &[3.0; 8], 0.1);
    assert!((centred_level(&edge, 0).unwrap() - 2.0).abs() < 1e-12, "edge rows centre top and base");
    assert!((centred_level(&dc, 0).unwrap() - 3.0).abs() < 1e-12, "DC rows use the mean without edge levels");
    assert_eq!(centred_level(&edge, 1), None, "another channel's measurements cannot set the level");
    assert_eq!(centred_level(&[], 0), None, "absent measurements leave the level unchanged");
    assert_eq!(ghost_level(2.0, -1.0, 4.0), 2.0, "ghost preserves volts within the destination view");
    assert_eq!(ghost_level(6.0, -1.0, 4.0), 4.0, "ghost clips to the top of another lane");
    assert_eq!(ghost_level(-2.0, -1.0, 4.0), -1.0, "ghost clips to the bottom of another lane");
}

#[test]
fn holdoff_edge_maps_the_post_trigger_span() {
    let (width, span, post) = (800.0, 100e-6, 60e-6);
    let drag = |distance| holdoff_drag(distance, width, span, post);
    assert_eq!(drag(0.0), 0.0, "notch is zero holdoff");
    assert_eq!(drag(-10.0), 0.0, "left of zero cannot produce negative holdoff");
    assert_eq!(drag(900.0), post, "holdoff is clamped to the post-trigger span");
    assert!((drag(100.0) - 12.5e-6).abs() < 1e-15, "edge displacement maps continuously to seconds");
    let mut previous = 0.0;
    for distance in 0..=900 {
        let value = drag(distance as f64);
        assert!(value >= previous, "holdoff is monotonic at pixel {distance}");
        assert!(value <= post, "holdoff stays in the window at pixel {distance}");
        previous = value;
    }
}

#[test]
fn stop_waits_for_one_new_trigger_per_arm() {
    let mut stop = StopAtTrigger::default();
    let mut events = 7;
    assert!(!stop.observe(events), "an unarmed fire never pauses");
    stop.arm(events);
    assert!(stop.armed(), "arming is visible while playback continues");
    assert!(!stop.observe(events), "a waiting trigger never pauses");
    assert!(!stop.observe(events), "automatic sweeps without accepted edges never pause");
    events += 1;
    assert!(stop.observe(events), "the first new fire pauses");
    assert!(!stop.armed(), "the first fire clears the active arm");
    assert!(!stop.observe(events), "the same fired publication pauses only once");
    stop.arm(events);
    assert!(!stop.observe(events), "re-arming ignores the previously fired trigger");
    events += 1;
    assert!(stop.observe(events), "re-arming stops on the next fire");
    stop.arm(events);
    stop.cancel();
    assert!(!stop.observe(events + 1), "manual pause cancels the pending stop");
    stop.arm(events);
    assert!(!stop.observe(0), "a replacement feed establishes a baseline without pausing");
    assert!(stop.observe(1), "a replacement feed's first new fire pauses");
}

#[test]
fn edges_keep_their_crossing_times() {
    let mut synth = Synth::pwm();
    synth.noise = 0.0;
    let (t, y) = synth.record(300_000);
    let period = synth.period();
    let last = t[t.len() - 1];
    let trigger = EdgeTrigger { level: (synth.lo + synth.hi) * 0.5, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let tolerance = 1.0 / (24.0 * synth.ring_freq());
    let mut low = 0.0;
    let mut high = 0.25 / synth.ring_freq();
    for _ in 0..60 {
        let middle = (low + high) * 0.5;
        if synth.step(middle) < 0.5 {
            low = middle;
        } else {
            high = middle;
        }
    }
    let delay = (low + high) * 0.5;
    let rising = events(&t, &y, &trigger);
    // The sample at zero is low and arms the first rise. Its event is at the
    // step's mid crossing, not at zero. Count that edge plus covered periods.
    let expected_rising = ((last - delay) / period).floor() as usize + 1;
    assert_eq!(rising.len(), expected_rising, "a clean PWM rising edge was lost or counted twice");
    for (cycle, &time) in rising.iter().enumerate() {
        assert!((time - (cycle as f64 * period + delay)).abs() <= tolerance, "rising edge {cycle} was not timed at its own mid crossing");
    }

    let mut detector = EdgeDetector::new(trigger);
    let mut incremental = Vec::new();
    for (&time, &value) in t.iter().zip(&y) {
        if let Edge::Accepted(time) = detector.push(time, value) {
            incremental.push(time);
        }
    }
    assert_eq!(incremental, rising, "incremental detection changed event times");
    assert_eq!(detector.events(), rising.len() as u64, "the detector missed an accepted event");
    assert_eq!(detector.held_off(), 0, "zero holdoff refused an event");

    let mut held = EdgeDetector::new(EdgeTrigger { holdoff: 1.5 * period, ..trigger });
    let mut held_times = Vec::new();
    for (&time, &value) in t.iter().zip(&y) {
        if let Edge::Accepted(time) = held.push(time, value) {
            held_times.push(time);
        }
    }
    assert_eq!(held.events(), ((rising.len() + 1) / 2) as u64, "holdoff did not accept exactly every other edge");
    assert_eq!(held.held_off(), (rising.len() / 2) as u64, "holdoff did not count each refused edge");
    assert_eq!(held_times, rising.iter().step_by(2).copied().collect::<Vec<_>>(), "a refused edge changed the holdoff origin");

    let falling = events(&t, &y, &EdgeTrigger { slope: Slope::Falling, ..trigger });
    let falling_delay = synth.duty * period + delay;
    let expected_falling = ((last - falling_delay) / period).floor() as usize + 1;
    assert_eq!(falling.len(), expected_falling, "a clean PWM falling edge was lost or counted twice");
    for (cycle, &time) in falling.iter().enumerate() {
        assert!((time - (cycle as f64 * period + falling_delay)).abs() <= tolerance, "falling edge {cycle} was not timed at its own mid crossing");
    }
    let either = events(&t, &y, &EdgeTrigger { slope: Slope::Either, ..trigger });
    assert_eq!(either.len(), rising.len() + falling.len(), "Either lost or duplicated an edge");
    assert!(either.windows(2).all(|pair| pair[0] < pair[1]), "Either returned events out of order");
    for (index, &time) in either.iter().enumerate() {
        let expected = if index % 2 == 0 { rising[index / 2] } else { falling[index / 2] };
        assert_eq!(time, expected, "Either did not alternate independently armed rising and falling edges");
    }

    synth.noise = 0.008;
    let (noisy_t, noisy_y) = synth.record(300_000);
    let noisy_rising = events(&noisy_t, &noisy_y, &trigger);
    assert_eq!(noisy_rising.len(), expected_rising, "hysteresis lost an edge or accepted a noise re-crossing");
    for (cycle, &time) in noisy_rising.iter().enumerate() {
        assert!((time - (cycle as f64 * period + delay)).abs() <= tolerance, "noise moved rising edge {cycle} beyond one fine sample");
    }

    let edge = EdgeTrigger { level: 0.0, hyst: 1.0, slope: Slope::Rising, holdoff: 0.0 };
    let mut delayed = EdgeDetector::new(edge);
    assert_eq!(delayed.push(0.0, -1.0), Edge::None, "arming incorrectly emitted an edge");
    assert_eq!(delayed.push(1.0, -0.25), Edge::None, "an edge appeared before the level crossing");
    assert_eq!(delayed.push(2.0, 0.25), Edge::Pending(1.5), "a slow edge did not report its pending mid crossing");
    assert_eq!(delayed.push(3.0, 0.4), Edge::Pending(1.5), "an unconfirmed edge lost its pending time");
    assert_eq!(delayed.events(), 0, "an edge fired before reaching the far hysteresis boundary");
    assert_eq!(delayed.push(8.0, 0.45), Edge::Pending(1.5), "a delayed confirmation moved its candidate time");
    assert_eq!(delayed.push(9.0, 1.0), Edge::Accepted(1.5), "a late confirmation used the band boundary as the event time");
    assert_eq!(delayed.events(), 1, "a late confirmation was not counted exactly once");

    let mut recrossing = EdgeDetector::new(edge);
    recrossing.push(0.0, -1.0);
    assert_eq!(recrossing.push(1.0, 0.25), Edge::Pending(0.8), "a candidate was not interpolated at the level");
    assert_eq!(recrossing.push(2.0, -0.25), Edge::Dropped, "returning below the level did not drop the pending crossing");
    assert_eq!(recrossing.push(3.0, 0.25), Edge::Pending(2.5), "a re-crossing did not replace the candidate time");
    assert_eq!(recrossing.push(4.0, 1.0), Edge::Accepted(2.5), "confirmation did not use the last level crossing");

    let mut late_holdoff = EdgeDetector::new(EdgeTrigger { holdoff: 3.0, ..edge });
    late_holdoff.push(0.0, -1.0);
    assert_eq!(late_holdoff.push(1.0, 1.0), Edge::Accepted(0.5), "a confirmed single-step edge was lost");
    late_holdoff.push(2.0, -1.0);
    assert_eq!(late_holdoff.push(3.0, 0.25), Edge::Pending(2.8), "holdoff discarded a crossing before confirmation");
    assert_eq!(late_holdoff.push(10.0, 1.0), Edge::Dropped, "holdoff used confirmation time instead of crossing time");
    late_holdoff.push(11.0, -1.0);
    assert_eq!(late_holdoff.push(12.0, 1.0), Edge::Accepted(11.5), "a refused edge changed the holdoff origin");
    assert_eq!(late_holdoff.events(), 2, "holdoff changed the accepted-event count");
    assert_eq!(late_holdoff.held_off(), 1, "holdoff failed to count the delayed refusal");

    let both = EdgeTrigger { hyst: 0.0, slope: Slope::Either, ..edge };
    let exact = events(&[0.0, 1.0, 2.0, 3.0], &[-1.0, 1.0, -1.0, 1.0], &EdgeTrigger { holdoff: 1.0, ..both });
    assert_eq!(exact, [0.5, 1.5, 2.5], "zero hysteresis or exact holdoff endpoints lost alternating edges");

    let mut changed = EdgeDetector::new(edge);
    changed.push(0.0, -1.0);
    changed.push(1.0, 1.0);
    changed.push(2.0, -1.0);
    changed.set(EdgeTrigger { holdoff: 3.0, ..edge });
    assert_eq!(changed.push(3.0, 1.0), Edge::Dropped, "changing settings lost holdoff or failed to re-arm from the last sample");
    changed.push(4.0, -1.0);
    assert_eq!(changed.push(5.0, 1.0), Edge::Accepted(4.5), "changing settings prevented the next eligible edge");
    assert_eq!(changed.events(), 2, "changing settings reset the accepted-event counter");
    assert_eq!(changed.held_off(), 1, "changing settings lost the refusal counter");
}
