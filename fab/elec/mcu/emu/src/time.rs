//! The shared conversion between machine ticks and solver seconds.

/// Simulated time of a tick: `t_reset + tick / tick_hz`, retaining whole seconds exactly.
pub fn time(t_reset: f64, tick: u64, tick_hz: u64) -> f64 {
    assert!(tick_hz != 0, "tick frequency must be nonzero");
    t_reset + (tick / tick_hz) as f64 + (tick % tick_hz) as f64 / tick_hz as f64
}

/// The smallest tick whose `time(..)` is at least `t`, where `t >= t_reset`.
/// Within the exact time domain, `tick_at(t_reset, time(t_reset, c, hz), hz) == c`.
/// Panics for invalid times or a tick grid too fine for this floating-point domain.
pub fn tick_at(t_reset: f64, t: f64, tick_hz: u64) -> u64 {
    assert!(tick_hz != 0 && t_reset.is_finite() && t.is_finite() && t >= t_reset,
        "invalid tick/time conversion");
    let estimate = (t - t_reset) * tick_hz as f64;
    assert!(estimate < 18446744073709551616.0, "tick exceeds u64");
    let mut tick = estimate as u64;
    // In the exact domain, subtraction/multiplication rounding shifts the
    // estimate by at most a few ticks. Compare using the forward map itself.
    for _ in 0..4 {
        if time(t_reset, tick, tick_hz) < t {
            tick = tick.checked_add(1).expect("tick exceeds u64");
        } else if tick != 0 && time(t_reset, tick - 1, tick_hz) >= t {
            tick -= 1;
        } else { return tick; }
    }
    assert!(time(t_reset, tick, tick_hz) >= t
        && (tick == 0 || time(t_reset, tick - 1, tick_hz) < t),
        "time lies outside the exact tick domain");
    tick
}

/// Exact-time horizon in seconds: 2^23 s. Below it, one tick is more than
/// twice `ulp(t)` for the supported grids up to 480 MHz. Callers keep absolute
/// simulated time below this limit, including a nonnegative reset offset.
pub const TIME_EXACT_LIMIT: f64 = 8388608.0;
