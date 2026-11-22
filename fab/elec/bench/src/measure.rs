//! The scope's measurements over a record (`t` strictly increasing, `y` the same length), the signal
//! piecewise linear between samples. Averages and histograms are time-weighted, so a solver's dense
//! steps at edges do not bias them. Statistical measures reject invalid records; explicit-threshold
//! helpers skip invalid samples just like their streaming trackers. Empty and single-sample
//! records have no complete timing measurements. A constant signal has no edges.

use crate::edges::{Edge, EdgeTracker, Threshold};
use crate::timing::{EdgeMeasures, PulseTracker, TransitionTracker};

/// The direction of a level crossing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slope {
    Rising,
    Falling,
    Either,
}

/// Minimum and maximum sample.
pub fn min_max(y: &[f32]) -> Option<(f64, f64)> {
    if y.len() < 2 {
        return None;
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &value in y {
        if !value.is_finite() {
            return None;
        }
        min = min.min(value as f64);
        max = max.max(value as f64);
    }
    Some((min, max))
}

/// Peak-to-peak sample range.
pub fn vpp(y: &[f32]) -> Option<f64> {
    let (min, max) = min_max(y)?;
    Some(max - min)
}

fn record_range(t: &[f64], y: &[f32]) -> Option<(f64, f64)> {
    if t.len() != y.len() || t.len() < 2 {
        return None;
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut previous = f64::NEG_INFINITY;
    for (&time, &value) in t.iter().zip(y) {
        if !time.is_finite() || time <= previous || !value.is_finite() {
            return None;
        }
        previous = time;
        min = min.min(value as f64);
        max = max.max(value as f64);
    }
    (t[t.len() - 1] - t[0]).is_finite().then_some((min, max))
}

/// Trapezoidal mean over the record duration.
pub fn mean(t: &[f64], y: &[f32]) -> Option<f64> {
    record_range(t, y)?;
    let duration = t[t.len() - 1] - t[0];
    let mut sum = 0.0;
    for (times, values) in t.windows(2).zip(y.windows(2)) {
        sum += ((times[1] - times[0]) / duration) * (values[0] as f64 + values[1] as f64) * 0.5;
    }
    Some(sum)
}

/// RMS using the exact integral of each linear segment squared.
pub fn rms(t: &[f64], y: &[f32]) -> Option<f64> {
    record_range(t, y)?;
    let duration = t[t.len() - 1] - t[0];
    let mut sum = 0.0;
    for (times, values) in t.windows(2).zip(y.windows(2)) {
        let a = values[0] as f64;
        let b = values[1] as f64;
        sum += ((times[1] - times[0]) / duration) * (a * a + a * b + b * b) / 3.0;
    }
    Some(sum.sqrt())
}

/// Histogram top and base, refined with each peak bin's neighbours.
pub fn top_base(t: &[f64], y: &[f32]) -> Option<(f64, f64)> {
    let (min, max) = record_range(t, y)?;
    if max <= min {
        return None;
    }
    let duration = t[t.len() - 1] - t[0];
    let mut bins = [(0.0, 0.0); 256];
    for (i, &value) in y.iter().enumerate() {
        let left = if i == 0 { 0.0 } else { (t[i] - t[i - 1]) / duration };
        let right = if i + 1 == t.len() { 0.0 } else { (t[i + 1] - t[i]) / duration };
        let weight = 0.5 * (left + right);
        let value = value as f64;
        let bin = (((value - min) / (max - min) * 256.0) as usize).min(255);
        bins[bin].0 += weight;
        bins[bin].1 += weight * value;
    }
    let peak = |start: usize, end: usize| {
        let mut best = start;
        for i in start + 1..end {
            if bins[i].0 > bins[best].0 {
                best = i;
            }
        }
        best
    };
    let refine = |bin: usize| {
        let mut weight = 0.0;
        let mut sum = 0.0;
        for &(w, s) in &bins[bin.saturating_sub(1)..=(bin + 1).min(255)] {
            weight += w;
            sum += s;
        }
        sum / weight
    };
    let base = refine(peak(0, 128));
    let top = refine(peak(128, 256));
    (top > base).then_some((top, base))
}

/// Histogram top minus base.
pub fn amplitude(t: &[f64], y: &[f32]) -> Option<f64> {
    let (top, base) = top_base(t, y)?;
    Some(top - base)
}

/// Crossings confirmed across the hysteresis band, timed at the level itself.
pub fn crossings(t: &[f64], y: &[f32], level: f64, hyst: f64) -> Vec<(f64, Slope)> {
    edges(t, y, Threshold { level, hysteresis: hyst }).into_iter().map(|edge| (edge.t, edge.dir)).collect()
}

/// Every confirmed threshold edge in a stored capture.
pub fn edges(t: &[f64], y: &[f32], threshold: Threshold) -> Vec<Edge> {
    if t.len() != y.len() { return Vec::new(); }
    let mut tracker = EdgeTracker::new(threshold);
    t.iter().zip(y).filter_map(|(&t, &y)| tracker.push(t, y as f64)).collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pulse {
    pub t: f64,
    pub width: f64,
    pub high: bool,
    /// Interval from the preceding same-direction edge to this pulse's start.
    pub period: Option<f64>,
}

pub fn pulses(t: &[f64], y: &[f32], threshold: Threshold) -> Vec<Pulse> {
    if t.len() != y.len() { return Vec::new(); }
    let mut line = EdgeTracker::new(threshold);
    let mut pulses = PulseTracker::default();
    t.iter().zip(y).filter_map(|(&t, &y)| line.push(t, y as f64).and_then(|edge| pulses.push(edge))).collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Timing {
    pub frequency: Option<f64>,
    /// Mean high time / mean period, in percent, over complete rising-to-rising cycles.
    pub duty: Option<f64>,
    pub width_high: Option<f64>,
    pub width_low: Option<f64>,
    pub n_cycles: usize,
    pub sigma_period: Option<f64>,
}

pub fn timing(t: &[f64], y: &[f32], threshold: Threshold) -> Timing {
    let mut tracker = EdgeMeasures::new(threshold, false);
    if t.len() == y.len() {
        for (&t, &y) in t.iter().zip(y) { tracker.push(t, y as f64, f64::NEG_INFINITY, f64::INFINITY); }
    }
    tracker.timing()
}

fn mid_threshold(top: f64, base: f64) -> Threshold {
    Threshold { level: base + (top - base) * 0.5, hysteresis: (top - base) * 0.1 }
}

fn mid_crossings(t: &[f64], y: &[f32], top: f64, base: f64) -> impl Iterator<Item = (f64, Slope)> {
    edges(t, y, mid_threshold(top, base)).into_iter().map(|edge| (edge.t, edge.dir))
}

/// Mean interval between successive rising mid-level crossings.
pub fn period(t: &[f64], y: &[f32]) -> Option<f64> {
    frequency(t, y).map(|frequency| 1.0 / frequency)
}

/// Reciprocal of the measured period, in Hz.
pub fn frequency(t: &[f64], y: &[f32]) -> Option<f64> {
    let (top, base) = top_base(t, y)?;
    timing(t, y, mid_threshold(top, base)).frequency
}

/// Mean high fraction across complete rising-to-rising periods (0..1).
pub fn duty(t: &[f64], y: &[f32]) -> Option<f64> {
    let (top, base) = top_base(t, y)?;
    timing(t, y, mid_threshold(top, base)).duty.map(|percent| percent / 100.0)
}

fn transition_time(t: &[f64], y: &[f32], lo: f64, hi: f64, slope: Slope) -> Option<f64> {
    if !lo.is_finite() || !hi.is_finite() || lo < 0.0 || lo > 0.5 || hi < 0.5 || hi > 1.0 || lo >= hi {
        return None;
    }
    let (top, base) = top_base(t, y)?;
    let mut tracker = TransitionTracker::new(base, top, lo, hi, slope, false);
    for (&t, &y) in t.iter().zip(y) { tracker.push(t, y as f64, f64::NEG_INFINITY, f64::INFINITY); }
    tracker.times.mean()
}

/// Mean rise time between fractions bracketing the mid level.
pub fn rise_time_between(t: &[f64], y: &[f32], lo: f64, hi: f64) -> Option<f64> {
    transition_time(t, y, lo, hi, Slope::Rising)
}

/// Mean 10% to 90% rise time.
pub fn rise_time(t: &[f64], y: &[f32]) -> Option<f64> {
    rise_time_between(t, y, 0.1, 0.9)
}

/// Mean 90% to 10% fall time.
pub fn fall_time(t: &[f64], y: &[f32]) -> Option<f64> {
    transition_time(t, y, 0.1, 0.9, Slope::Falling)
}

fn excursion(t: &[f64], y: &[f32], high: bool) -> Option<f64> {
    let (top, base) = top_base(t, y)?;
    let amplitude = top - base;
    let mid = base + amplitude * 0.5;
    let start_slope = if high { Slope::Rising } else { Slope::Falling };
    let mut start = None;
    let mut sample = 0usize;
    let mut sum = 0.0;
    let mut count = 0usize;
    for (time, slope) in mid_crossings(t, y, top, base) {
        if slope == start_slope {
            start = Some(time);
        } else if let Some(begin) = start.take() {
            while sample < t.len() && t[sample] <= begin {
                sample += 1;
            }
            // Both interpolated endpoints equal the mid level.
            let mut extreme = mid;
            while sample < t.len() && t[sample] < time {
                extreme = if high { extreme.max(y[sample] as f64) } else { extreme.min(y[sample] as f64) };
                sample += 1;
            }
            sum += if high { (extreme - top) / amplitude } else { (base - extreme) / amplitude };
            count += 1;
        }
    }
    average(sum, count)
}

/// Mean excess above the top between rising and falling crossings.
pub fn overshoot(t: &[f64], y: &[f32]) -> Option<f64> {
    excursion(t, y, true)
}

/// Mean deficit below the base between falling and rising crossings.
pub fn preshoot(t: &[f64], y: &[f32]) -> Option<f64> {
    excursion(t, y, false)
}

fn average(sum: f64, count: usize) -> Option<f64> {
    (count > 0).then(|| sum / count as f64)
}
