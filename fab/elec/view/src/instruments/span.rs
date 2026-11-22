//! Time bracket statistics from the worker; value brackets read only their two levels.

use super::simfeed::{Feature, FeatureKind};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End { Start, Finish }

/// Nearest end in plot points, including the slop boundary. Ties select the start.
pub fn bracket_end_at(x: f64, x0: f64, x1: f64, slop: f64) -> Option<End> {
    let (d0, d1) = ((x - x0).abs(), (x - x1).abs());
    if d0 <= slop && d0 <= d1 { Some(End::Start) }
    else if d1 <= slop { Some(End::Finish) }
    else { None }
}

/// Nearest horizontal level in plot points, within the four-point drag slop.
/// Endpoint order follows the stored values, even though screen y runs downwards.
pub fn value_bracket_end_at(y: f64, y0: f64, y1: f64) -> Option<End> {
    if !y.is_finite() || !y0.is_finite() || !y1.is_finite() { return None; }
    bracket_end_at(y, y0, y1, 4.0)
}

/// Absolute difference between two levels, in the lane's own units and answer SI format.
pub fn value_bracket_label(v0: f64, v1: f64, unit: &str) -> String {
    let mut label = ValueBracketLabel::default();
    label.update(v0, v1, unit);
    label.text
}

pub(super) struct ValueBracketLabel {
    delta: Option<f64>,
    unit: String,
    pub text: String,
}

impl Default for ValueBracketLabel {
    fn default() -> Self { Self { delta: None, unit: String::with_capacity(8), text: String::with_capacity(96) } }
}

impl ValueBracketLabel {
    pub fn update(&mut self, v0: f64, v1: f64, unit: &str) {
        let delta = (v1 - v0).abs();
        if self.delta == Some(delta) && self.unit == unit { return; }
        self.delta = Some(delta);
        if self.unit != unit { self.unit.clear(); self.unit.push_str(unit); }
        self.text.clear();
        self.text.push_str("dV ");
        super::axis::write_si_label(&mut self.text, delta, unit);
    }
}

/// Duration and reciprocal, followed by this lane's answer (or its pending state).
pub fn bracket_label(dt: f64, stats: Option<SpanStats>, answered: bool, unit: &str) -> String {
    let mut label = BracketLabel::default();
    label.update(dt, stats, answered, unit);
    label.text
}

pub(super) struct BracketLabel {
    key: Option<(f64, Option<SpanStats>, bool)>,
    unit: String,
    pub text: String,
}

impl Default for BracketLabel {
    fn default() -> Self { Self { key: None, unit: String::with_capacity(8), text: String::with_capacity(256) } }
}

impl BracketLabel {
    pub fn update(&mut self, dt: f64, stats: Option<SpanStats>, answered: bool, unit: &str) {
        use super::axis::write_si_label;
        use std::fmt::Write;
        let stats = if answered { stats } else { None };
        let key = (dt, stats, answered);
        if self.key == Some(key) && self.unit == unit { return; }
        self.key = Some(key);
        if self.unit != unit { self.unit.clear(); self.unit.push_str(unit); }
        self.text.clear();
        write_si_label(&mut self.text, dt, "s");
        if dt > 0.0 && (1.0 / dt).is_finite() {
            self.text.push_str(" ("); write_si_label(&mut self.text, 1.0 / dt, "Hz"); self.text.push(')');
        }
        if !answered { self.text.push_str("   measuring"); }
        else if let Some(stats) = stats {
            self.text.push_str("   mean "); write_si_label(&mut self.text, stats.mean, unit);
            self.text.push_str("   rms "); write_si_label(&mut self.text, stats.rms, unit);
            self.text.push_str("   pp "); write_si_label(&mut self.text, stats.max - stats.min, unit);
            let _ = write!(self.text, "   {} edges", u64::from(stats.rises) + u64::from(stats.falls));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpanStats {
    pub t0: f64, pub t1: f64,
    pub mean: f64, pub rms: f64, pub min: f64, pub max: f64,
    pub rises: u32, pub falls: u32,
    /// Original samples in [t0, t1], excluding interpolated endpoints.
    pub samples: u32,
}

/// Finite, strictly ordered source samples and the ordered features from `lane_features`.
/// Both endpoints must be inside the record. Integrate trapezoids of y and y squared,
/// clipping the first and last segment by interpolation; edge counts use [t0, t1).
/// No allocation; O(log(samples) + log(features) + samples/features inside the bracket).
pub fn span_stats(t: &[f64], y: &[f32], trigger: f64, features: &[Feature], t0: f64, t1: f64) -> Option<SpanStats> {
    if t.len() < 2 || t.len() != y.len() || !trigger.is_finite() || !t0.is_finite() || !t1.is_finite() || t0 >= t1 { return None; }
    // Subtract the trigger just as lane_features does. Adding a large source-time trigger
    // to a tiny bracket endpoint could otherwise round the two endpoints to the same time.
    let time = |i: usize| t[i] - trigger;
    let (first, last) = (time(0), time(t.len() - 1));
    if !first.is_finite() || !last.is_finite() || !(t1 - t0).is_finite() || t0 < first || t1 > last { return None; }
    let mut i = t.partition_point(|&time| time - trigger < t0);
    if i == t.len() || time(i) > t0 { i = i.saturating_sub(1); }
    i = i.min(t.len() - 2);
    let value = |i: usize, at: f64| y[i] as f64 + (y[i + 1] as f64 - y[i] as f64) * ((at - time(i)) / (time(i + 1) - time(i)));
    let mut at = t0;
    let mut a = if time(i) == at { y[i] as f64 } else { value(i, at) };
    if !a.is_finite() { return None; }
    let mut stats = SpanStats { t0, t1, mean: 0.0, rms: 0.0, min: a, max: a, rises: 0, falls: 0, samples: u32::from(time(i) == t0) };
    while i + 1 < t.len() && (at < t1 || time(i + 1) == t1) {
        let next = time(i + 1);
        let end = t1.min(next);
        // Distinct source samples can round to the same trigger-relative time. Keep their
        // extrema and sample count, but their zero-width trapezoid contributes no area.
        if !next.is_finite() || t[i + 1] <= t[i] || end < at { return None; }
        let b = if end == next { y[i + 1] as f64 } else { value(i, end) };
        if !b.is_finite() { return None; }
        let half_dt = (end - at) * 0.5;
        stats.mean += half_dt * (a + b);
        stats.rms += half_dt * (a * a + b * b);
        stats.min = stats.min.min(b);
        stats.max = stats.max.max(b);
        if end == next { stats.samples = stats.samples.saturating_add(1); }
        at = end;
        a = b;
        i += 1;
    }
    stats.mean /= t1 - t0;
    stats.rms = (stats.rms / (t1 - t0)).sqrt();
    let first = features.partition_point(|feature| feature.t < t0);
    for feature in features[first..].iter().take_while(|feature| feature.t < t1) {
        // For edges, feature.t is exactly crossings[1], the fifty percent crossing.
        match feature.kind {
            FeatureKind::Rise => stats.rises = stats.rises.saturating_add(1),
            FeatureKind::Fall => stats.falls = stats.falls.saturating_add(1),
            _ => {}
        }
    }
    Some(stats)
}

/// One UI writer publishes finite endpoints (or clears them), without waiting on the worker.
/// The odd version marks a write in progress; bumping only after the endpoints would let
/// a reader accept one new endpoint and one old endpoint under the preceding generation.
pub struct SpanRequest {
    t0: AtomicU64,
    t1: AtomicU64,
    generation: AtomicU64,
}

impl Default for SpanRequest {
    fn default() -> Self { Self::new() }
}

impl SpanRequest {
    pub fn new() -> Self {
        Self { t0: AtomicU64::new(f64::NAN.to_bits()), t1: AtomicU64::new(f64::NAN.to_bits()), generation: AtomicU64::new(0) }
    }

    /// One atomic load for publication filtering; only the worker reads the endpoints.
    pub fn generation(&self) -> u64 { self.generation.load(Ordering::SeqCst) / 2 }

    /// Return the new public generation, including a clear or a repeated span.
    pub fn set(&self, span: Option<(f64, f64)>) -> u64 {
        let (t0, t1) = span.unwrap_or((f64::NAN, f64::NAN));
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.t0.store(t0.to_bits(), Ordering::SeqCst);
        self.t1.store(t1.to_bits(), Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst) / 2 + 1
    }

    /// Worker read: retry if a write overlapped either endpoint load.
    pub fn get(&self) -> (u64, Option<(f64, f64)>) {
        loop {
            let generation = self.generation.load(Ordering::SeqCst);
            if generation % 2 != 0 { std::hint::spin_loop(); continue; }
            let t0 = f64::from_bits(self.t0.load(Ordering::SeqCst));
            let t1 = f64::from_bits(self.t1.load(Ordering::SeqCst));
            if self.generation.load(Ordering::SeqCst) == generation {
                return (generation / 2, (!t0.is_nan()).then_some((t0, t1)));
            }
        }
    }
}
