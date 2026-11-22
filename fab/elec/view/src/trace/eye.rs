//! Eye-diagram metrics of an NRZ record: the opening at the eye centre and the crossings' jitter,
//! as a scope's eye mode reads them out.

use fab_elec_bench::measure;
use std::f64::consts::TAU;

/// The opening of an NRZ record folded at its unit interval.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EyeMetrics {
    /// Mean one and zero levels at the eye centre.
    pub one: f64,
    pub zero: f64,
    /// The eye's inner edges at the centre: one - 3 sigma and zero + 3 sigma.
    pub top: f64,
    pub bottom: f64,
    /// Vertical opening at the eye centre, (one - 3 sigma) - (zero + 3 sigma); negative when closed.
    pub height: f64,
    /// Horizontal opening at the threshold: the unit interval minus the crossings' peak-to-peak spread;
    /// negative when closed.
    pub width: f64,
    /// Crossing-time spread about the mean crossing phase, seconds.
    pub jitter_rms: f64,
    pub jitter_pp: f64,
    /// Mean crossing phase in [0, ui): a crossing's time modulo the unit interval.
    pub crossing: f64,
}

/// Eye metrics of a record sampled at increasing times `t`, folded at unit interval `ui` seconds.
pub fn eye_metrics(t: &[f64], y: &[f32], ui: f64) -> Option<EyeMetrics> {
    if !ui.is_finite() || ui <= 0.0 || t.len() != y.len() || t.len() < 2 || t[t.len() - 1] - t[0] < 4.0 * ui {
        return None;
    }
    let (top, base) = measure::top_base(t, y)?;
    let threshold = (top + base) * 0.5;
    let crossings = measure::crossings(t, y, threshold, 0.1 * (top - base));
    if crossings.len() < 2 {
        return None;
    }

    let mut sum_sin = 0.0;
    let mut sum_cos = 0.0;
    for &(time, _) in &crossings {
        let (sin, cos) = (TAU * (time.rem_euclid(ui) / ui)).sin_cos();
        sum_sin += sin;
        sum_cos += cos;
    }
    let phase = (sum_sin.atan2(sum_cos) / TAU * ui).rem_euclid(ui);
    let deviation = |time: f64| (time.rem_euclid(ui) - phase + ui * 0.5).rem_euclid(ui) - ui * 0.5;
    let mean = crossings.iter().map(|&(time, _)| deviation(time)).sum::<f64>() / crossings.len() as f64;
    let crossing = (phase + mean).rem_euclid(ui);
    let mut squared = 0.0;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &(time, _) in &crossings {
        let delta = deviation(time) - mean;
        squared += delta * delta;
        min = min.min(delta);
        max = max.max(delta);
    }
    let jitter_rms = (squared / crossings.len() as f64).sqrt();
    let jitter_pp = max - min;

    let offset = crossing + ui * 0.5;
    let mut k = ((t[0] - offset) / ui).ceil();
    let mut cursor = 0;
    let mut counts = [0usize; 2];
    let mut means = [0.0; 2];
    let mut sums_squared = [0.0; 2];
    loop {
        let centre = offset + k * ui;
        if centre > t[t.len() - 1] {
            break;
        }
        while cursor + 1 < t.len() && t[cursor + 1] <= centre {
            cursor += 1;
        }
        let nearest = if cursor + 1 < t.len() && t[cursor + 1] - centre < (t[cursor] - centre).abs() {
            cursor + 1
        } else {
            cursor
        };
        if centre >= t[0] && (t[nearest] - centre).abs() <= ui / 8.0 {
            let value = y[nearest] as f64;
            let group = usize::from(value > threshold);
            counts[group] += 1;
            let delta = value - means[group];
            means[group] += delta / counts[group] as f64;
            sums_squared[group] += delta * (value - means[group]);
        }
        if cursor + 1 == t.len() {
            break;
        }
        // Skip centres across gaps that cannot have a sample within ui / 8.
        let next_k = (k + 1.0).max(((t[cursor + 1] - offset - ui / 8.0) / ui).floor());
        if next_k <= k {
            break;
        }
        k = next_k;
    }
    if counts.iter().any(|&count| count < 2) {
        return None;
    }
    let one = means[1];
    let zero = means[0];
    let sigma_one = (sums_squared[1] / counts[1] as f64).sqrt();
    let sigma_zero = (sums_squared[0] / counts[0] as f64).sqrt();
    let top = one - 3.0 * sigma_one;
    let bottom = zero + 3.0 * sigma_zero;
    Some(EyeMetrics {
        one,
        zero,
        top,
        bottom,
        height: top - bottom,
        width: ui - jitter_pp,
        jitter_rms,
        jitter_pp,
        crossing,
    })
}
