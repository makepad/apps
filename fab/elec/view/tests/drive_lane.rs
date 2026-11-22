//! Nominal drive geometry and its inverse gestures, without a window or solver.

use fab_elec_bench::trigger::Window;
use fab_elec_view::instruments::drive_lane::{duty_from_drag, freq_from_drag, pwm_edges};

fn near(actual: f64, expected: f64, property: &str) {
    assert!(
        (actual - expected).abs() <= expected.abs().max(1e-15) * 1e-12,
        "{property}: got {actual}, expected {expected}"
    );
}

#[test]
fn nominal_pwm_and_inverse_drags() {
    let freq = 10e3;
    let duty = 0.3;
    let edges = pwm_edges(freq, duty, Window { pre: 25e-6, post: 225e-6 });
    assert_eq!(edges.len(), 4, "window coverage: exactly four periods touch the 250 µs window");
    for ((rise, fall), expected) in edges.into_iter().zip([-100e-6, 0.0, 100e-6, 200e-6]) {
        near(rise, expected, "trigger alignment: rising edges are whole periods from zero");
        near(fall, expected + 30e-6, "high duration: falling edges are 30 µs after their rise");
        near(duty_from_drag(freq, fall - rise), duty, "duty inverse: each falling edge recovers its duty, including before the trigger");
        near(freq_from_drag(rise, rise + 1.0 / freq).expect("period inverse: a positive period has a frequency"), freq, "frequency inverse: one period recovers the drive frequency");
    }
    let exact = pwm_edges(freq, duty, Window { pre: 0.0, post: 100e-6 });
    assert_eq!(exact.len(), 1, "window endpoints: a half-open one-period window contains exactly one period");
    near(exact[0].0, 0.0, "trigger anchor: the first rise is exactly at zero");

    for freq in [1.0, 10e3, 10e6] {
        for duty in [0.0, 0.3, 1.0] {
            let edges = pwm_edges(freq, duty, Window { pre: 0.25 / freq, post: 2.25 / freq });
            assert_eq!(edges.len(), 4, "inverse coverage: frequency limits retain the pre-trigger and final partial periods");
            for (rise, fall) in edges {
                near(duty_from_drag(freq, fall - rise), duty, "duty inverse at limits: zero and full high time round-trip without wrapping");
                near(freq_from_drag(rise, rise + 1.0 / freq).expect("frequency inverse at limits: positive periods remain editable"), freq, "frequency inverse at limits: both knob limits round-trip");
            }
        }
        assert_eq!(duty_from_drag(freq, -1.0 / freq), 0.0, "duty lower clamp: dragging before the grabbed rise stays low");
        assert_eq!(duty_from_drag(freq, 2.0 / freq), 1.0, "duty upper clamp: dragging beyond the next rise stays high");
    }
    assert_eq!(freq_from_drag(0.0, 2.0), Some(1.0), "frequency lower clamp: long periods stop at 1 Hz");
    assert_eq!(freq_from_drag(0.0, 1e-9), Some(10e6), "frequency upper clamp: short periods stop at 10 MHz");
    assert_eq!(freq_from_drag(1.0, 1.0), None, "period collapse: coincident bracket ends have no frequency");
    assert_eq!(freq_from_drag(1.0, 0.5), None, "period reversal: crossing the anchored end has no frequency");
    assert_eq!(freq_from_drag(0.0, f64::NAN), None, "invalid period: a non-finite endpoint is rejected");
    assert!(pwm_edges(0.0, duty, Window { pre: 1.0, post: 1.0 }).is_empty(), "invalid frequency: no edge enumeration at zero frequency");
    assert!(pwm_edges(freq, duty, Window { pre: 0.0, post: 0.0 }).is_empty(), "empty window: no edge enumeration without a time span");
}
