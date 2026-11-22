use fab_elec_bench::measure;
use fab_elec_instr::synth::Synth;
use std::f64::consts::{FRAC_1_SQRT_2, PI, TAU};

#[test]
fn measurements_match_closed_forms() {
    let s = Synth { noise: 0.0, ..Synth::pwm() };
    let (t, y) = s.record(200_000);
    let period = s.period();
    let fine = 1.0 / (24.0 * s.ring_freq());
    let settling = 6.0 / (s.zeta * s.wn);
    assert_eq!(t.len(), 200_000, "record sample count");
    assert_eq!(t.len(), y.len(), "record time and value counts");
    assert_eq!(t[0], 0.0, "record begins at zero");
    assert!(t[t.len() - 1] >= 6.0 * period, "record spans at least six periods");

    let mut cycle = 0usize;
    let mut last_edge = 0.0;
    let mut next_rising = period;
    let mut next_falling = s.duty * period;
    let mut complete_end = 0usize;
    for (i, times) in t.windows(2).enumerate() {
        let dt = times[1] - times[0];
        assert!(dt > 0.0, "strictly increasing record time at sample {i}");
        assert!(dt <= period / 64.0, "coarse step bound at sample {i}");
        if times[0] - last_edge <= settling {
            assert!(dt <= fine, "fine step bound after an edge at sample {i}");
        }
        assert!(times[1] <= next_rising, "rising breakpoint was skipped at cycle {}", cycle + 1);
        assert!(times[1] <= next_falling, "falling breakpoint was skipped at cycle {cycle}");
        if times[1] == next_falling {
            last_edge = times[1];
            next_falling = (cycle + 1) as f64 * period + s.duty * period;
        }
        if times[1] == next_rising {
            last_edge = times[1];
            cycle += 1;
            complete_end = i + 1;
            next_rising = (cycle + 1) as f64 * period;
        }
    }
    assert!(cycle >= 6, "at least six exact rising breakpoints");

    let frequency = measure::frequency(&t, &y).expect("PWM frequency is defined");
    assert!((frequency / s.freq - 1.0).abs() < 1e-6, "PWM frequency: {frequency}");
    let duty = measure::duty(&t, &y).expect("PWM duty is defined");
    assert!((duty - s.duty).abs() < 1e-4, "PWM duty: {duty}");
    let (top, base) = measure::top_base(&t, &y).expect("PWM top and base are defined");
    assert!((top - s.hi).abs() < 0.002, "PWM histogram top: {top}");
    assert!((base - s.lo).abs() < 0.002, "PWM histogram base: {base}");
    let overshoot = measure::overshoot(&t, &y).expect("PWM overshoot is defined");
    assert!((overshoot - s.overshoot()).abs() < 0.005, "PWM overshoot: {overshoot}");
    let preshoot = measure::preshoot(&t, &y).expect("PWM preshoot is defined");
    assert!((preshoot - s.overshoot()).abs() < 0.005, "PWM preshoot: {preshoot}");

    let first_step_crossing = |level: f64| {
        let mut lo = 0.0;
        // The first peak bounds the initial monotone part of the response.
        let mut hi = PI / (s.wn * (1.0 - s.zeta * s.zeta).sqrt());
        for _ in 0..60 {
            let mid = (lo + hi) * 0.5;
            if s.step(mid) < level {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) * 0.5
    };
    let expected_rise = first_step_crossing(0.9) - first_step_crossing(0.1);
    let rise = measure::rise_time(&t, &y).expect("PWM rise time is defined");
    assert!((rise / expected_rise - 1.0).abs() < 0.02, "PWM rise time: {rise}, reference: {expected_rise}");
    let fall = measure::fall_time(&t, &y).expect("PWM fall time is defined");
    assert!((fall / expected_rise - 1.0).abs() < 0.02, "PWM fall time: {fall}, reference: {expected_rise}");
    // Use complete source periods so the final partial pulse cannot bias the mean.
    let mean = measure::mean(&t[..=complete_end], &y[..=complete_end]).expect("PWM mean is defined");
    let expected_mean = s.lo + (s.hi - s.lo) * s.duty;
    assert!((mean - expected_mean).abs() < 1e-3, "PWM time-weighted mean: {mean}");

    let sine_frequency = 1_000.0;
    let mut sine_t = Vec::with_capacity(10_000);
    let mut sine_y = Vec::with_capacity(10_000);
    for i in 0..10_000 {
        let time = i as f64 / 9_999.0 * (10.0 / sine_frequency);
        sine_t.push(time);
        sine_y.push((TAU * sine_frequency * time).sin() as f32);
    }
    let sine_rms = measure::rms(&sine_t, &sine_y).expect("sine RMS is defined");
    assert!((sine_rms - FRAC_1_SQRT_2).abs() < 1e-4, "sine RMS: {sine_rms}");
    let sine_mean = measure::mean(&sine_t, &sine_y).expect("sine mean is defined");
    assert!(sine_mean.abs() < 1e-4, "sine mean: {sine_mean}");
    let sine_vpp = measure::vpp(&sine_y).expect("sine peak-to-peak range is defined");
    assert!((sine_vpp - 2.0).abs() < 1e-3, "sine peak-to-peak range: {sine_vpp}");
    let sine_measured_frequency = measure::frequency(&sine_t, &sine_y).expect("sine frequency is defined");
    assert!((sine_measured_frequency / sine_frequency - 1.0).abs() < 1e-6, "sine frequency: {sine_measured_frequency}");

    let ramp_t = [0.0, 2.0];
    let ramp_y = [0.0, 2.0];
    assert_eq!(measure::mean(&ramp_t, &ramp_y), Some(1.0), "linear segment mean");
    let ramp_rms = measure::rms(&ramp_t, &ramp_y).expect("linear segment RMS is defined");
    assert!((ramp_rms - (4.0_f64 / 3.0).sqrt()).abs() < 1e-12, "exact linear segment RMS");
    assert_eq!(measure::crossings(&ramp_t, &ramp_y, 1.0, 0.2), vec![(1.0, measure::Slope::Rising)], "interpolated hysteresis crossing");

    let degenerate: [(&str, &[f64], &[f32]); 3] = [
        ("empty", &[], &[]),
        ("single", &[0.0], &[2.0]),
        ("constant", &[0.0, 0.25, 1.0], &[2.0, 2.0, 2.0]),
    ];
    for (name, t, y) in degenerate {
        if name == "constant" {
            // a DC rail: its level and ripple are measurements, it has no edges
            assert_eq!(measure::min_max(y), Some((2.0, 2.0)), "{name}: min/max");
            assert_eq!(measure::vpp(y), Some(0.0), "{name}: peak-to-peak range");
            assert_eq!(measure::mean(t, y), Some(2.0), "{name}: mean");
            assert!((measure::rms(t, y).unwrap() - 2.0).abs() < 1e-12, "{name}: RMS");
        } else {
            assert_eq!(measure::min_max(y), None, "{name}: min/max");
            assert_eq!(measure::vpp(y), None, "{name}: peak-to-peak range");
            assert_eq!(measure::mean(t, y), None, "{name}: mean");
            assert_eq!(measure::rms(t, y), None, "{name}: RMS");
        }
        assert_eq!(measure::top_base(t, y), None, "{name}: top/base");
        assert_eq!(measure::amplitude(t, y), None, "{name}: amplitude");
        assert!(measure::crossings(t, y, 2.0, 0.2).is_empty(), "{name}: crossings");
        assert_eq!(measure::period(t, y), None, "{name}: period");
        assert_eq!(measure::frequency(t, y), None, "{name}: frequency");
        assert_eq!(measure::duty(t, y), None, "{name}: duty");
        assert_eq!(measure::rise_time_between(t, y, 0.1, 0.9), None, "{name}: custom rise time");
        assert_eq!(measure::rise_time(t, y), None, "{name}: rise time");
        assert_eq!(measure::fall_time(t, y), None, "{name}: fall time");
        assert_eq!(measure::overshoot(t, y), None, "{name}: overshoot");
        assert_eq!(measure::preshoot(t, y), None, "{name}: preshoot");
    }
}
