//! Axis geometry and labels without a drawing context.

use fab_elec_view::instruments::axis::{axis_label, axis_prefix, linear_ticks, log_ticks, si_label, time_label, Tick};
use std::{cell::Cell, collections::HashSet};

fn measure(label: &str) -> f64 { label.chars().count() as f64 * 7.0 }

fn labels(ticks: &[Tick], gap: f64, scale: f64, step: f64) {
    let labelled: Vec<_> = ticks.iter().filter(|tick| !tick.label.is_empty()).collect();
    let mut distinct = HashSet::new();
    for tick in &labelled {
        assert!(distinct.insert(&tick.label), "labels on one axis are distinct: {}", tick.label);
        let parsed = tick.label.split_whitespace().next().unwrap().replace('−', "-").parse::<f64>().expect("each axis label parses as a number") * scale;
        assert!((parsed - tick.value).abs() <= step / 1000.0, "labels round trip within step / 1000: {} at {}", tick.label, tick.value);
    }
    for pair in labelled.windows(2) {
        let required = (measure(&pair[0].label) + measure(&pair[1].label)) * 0.5 + gap;
        assert!(pair[1].at - pair[0].at + 1e-9 >= required, "adjacent labels never overlap: {pair:?}");
    }
}

#[test]
fn linear_range_sweep() {
    let gap = 8.0;
    let step = Cell::new(0.0);
    for exponent in -12..=4 {
        let magnitude = 10.0_f64.powi(exponent);
        for (a, b) in [(0.0, 3.3), (-8.0, -0.8), (-2.3, 5.7), (1.1, 1.9), (999.0, 1001.0)] {
            let (lo, hi) = (a * magnitude, b * magnitude);
            let (scale, _) = axis_prefix(lo, hi, "V");
            for length in [40.0, 80.0, 200.0, 640.0, 2000.0] {
                let ticks = linear_ticks(lo, hi, length, gap, &|value, s| { step.set(s); axis_label(value, s, scale) }, &mut measure);
                let s = step.get();
                assert!(!ticks.is_empty(), "a drawable linear range has ticks: {lo}..{hi}, {length}px");
                let normalized = s / 10.0_f64.powf(s.log10().floor());
                assert!([1.0, 2.0, 5.0, 10.0].iter().any(|n| (normalized - n).abs() < 1e-10), "linear step is on the 1-2-5 menu: {s}");
                let tolerance = hi.abs().max(lo.abs()) * 1e-12;
                assert!(ticks[0].value - lo <= s + tolerance, "low endpoint is covered within one step");
                assert!(hi - ticks.last().unwrap().value <= s + tolerance, "high endpoint is covered within one step");
                for tick in &ticks {
                    assert!(tick.value >= lo - tolerance && tick.value <= hi + tolerance, "ticks stay inside the window");
                    assert!((tick.value / s - (tick.value / s).round()).abs() < 1e-8, "linear ticks are step multiples");
                    assert!(tick.at >= 0.0 && tick.at <= length, "linear positions stay inside the axis");
                }
                for pair in ticks.windows(2) {
                    assert!(pair[0].value < pair[1].value && pair[0].at < pair[1].at, "linear ticks strictly increase");
                    assert!((pair[1].value - pair[0].value - s).abs() <= tolerance, "linear ticks do not skip a multiple");
                }
                labels(&ticks, gap, scale, s);
                let (time_scale, time_unit) = axis_prefix(lo, hi, "s");
                let times = linear_ticks(lo, hi, length, gap, &|value, s| { step.set(s); time_label(value, s, time_scale, &time_unit) }, &mut measure);
                assert!(!times.is_empty(), "signed time labels retain ticks across the range sweep");
                labels(&times, gap, time_scale, step.get());
            }
        }
    }
}

#[test]
fn logarithmic_range_sweep() {
    let gap = 8.0;
    for exponent in -12..=4 {
        let magnitude = 10.0_f64.powi(exponent);
        for (a, b) in [(1.0, 1000.0), (1.3, 83.0), (0.2, 2.0)] {
            let (lo, hi) = (a * magnitude, b * magnitude);
            let (scale, _) = axis_prefix(lo, hi, "Hz");
            for length in [40.0, 80.0, 200.0, 640.0, 2000.0] {
                let ticks = log_ticks(lo, hi, length, gap, &|value, step| axis_label(value, step, scale), &mut measure);
                assert!(!ticks.is_empty(), "a log window spanning decades has ticks");
                labels(&ticks, gap, scale, 10.0_f64.powf(lo.log10().floor()));
                for pair in ticks.windows(2) {
                    assert!(pair[0].value < pair[1].value && pair[0].at < pair[1].at, "log ticks strictly increase");
                }
                for decade in lo.log10().ceil() as i32..=hi.log10().floor() as i32 {
                    let value = 10.0_f64.powi(decade);
                    assert!(ticks.iter().any(|tick| tick.major && (tick.value - value).abs() <= value * 1e-12), "every decade inside a log window has a major tick: {value}");
                }
            }
        }
    }
    let ticks = log_ticks(1.0, 100.0, 90.0, gap, &|v, s| axis_label(v, s, 1.0), &mut measure);
    assert!(ticks.iter().filter(|t| t.major).all(|t| !t.label.is_empty()), "decade labels have priority over minor labels");
}

#[test]
fn engineering_and_trigger_labels() {
    for (lo, hi, unit, scale, prefixed) in [(0.0, 3.3, "V", 1.0, "V"), (0.0, 0.8, "V", 1e-3, "mV"), (0.0, 2e-5, "s", 1e-6, "µs"), (0.0, 0.0, "V", 1.0, "V"), (-0.8, 0.0, "V", 1e-3, "mV")] {
        let actual = axis_prefix(lo, hi, unit);
        assert!((actual.0 - scale).abs() <= scale * 1e-12 && actual.1 == prefixed, "one engineering prefix follows the whole window: {lo}..{hi}");
    }
    let (scale, unit) = axis_prefix(0.0, 1.0, "V");
    assert_eq!(unit, "mV", "a one-volt endpoint keeps 200 mV steps integral");
    for (value, label) in [(0.0, "0"), (0.2, "200"), (1.0, "1000")] {
        assert_eq!(axis_label(value, 0.2, scale), label, "engineering labels share the axis prefix");
    }
    for (value, label) in [(0.0, "0"), (0.5, "0.5"), (1.0, "1"), (1.5, "1.5"), (-0.5, "−0.5"), (-0.0, "0")] {
        assert_eq!(axis_label(value, 0.5, 1.0), label, "value labels have minimal decimals and a typographic minus");
    }
    for (value, label) in [(-40e-6, "−40 µs"), (0.0, "0"), (40e-6, "+40 µs")] {
        assert_eq!(time_label(value, 20e-6, 1e-6, "µs"), label, "time labels carry a sign and their unit except at zero");
    }
    for (value, label) in [(10e3, "10 kHz"), (50e3, "50 kHz"), (1e6, "1 MHz"), (2.56e6, "2.56 MHz"), (999.0, "999 Hz"), (5e-3, "5 mHz")] {
        assert_eq!(si_label(value, "Hz"), label, "a log axis label carries its own prefix with a mantissa in [1, 1000)");
    }
    assert!(linear_ticks(1.0, 1.0, 100.0, 8.0, &|v, s| axis_label(v, s, 1.0), &mut measure).is_empty(), "empty linear windows are harmless");
    assert!(log_ticks(0.0, 1.0, 100.0, 8.0, &|v, s| axis_label(v, s, 1.0), &mut measure).is_empty(), "log windows require positive endpoints");
}
