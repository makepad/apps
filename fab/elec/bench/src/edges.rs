//! Shared, sample-by-sample Schmitt edges, timed at the signal threshold.

use crate::measure::Slope;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Threshold {
    pub level: f64,
    pub hysteresis: f64,
}

impl Threshold {
    pub(crate) fn valid(self) -> bool {
        self.level.is_finite() && self.hysteresis.is_finite() && self.hysteresis >= 0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    pub t: f64,
    pub dir: Slope,
}

/// Streaming edge detector. The line becomes high at level + hysteresis / 2 and
/// low at level - hysteresis / 2. Edges retain the last interpolated crossing of
/// level before confirmation, rather than the time the band boundary was reached.
#[derive(Clone)]
pub struct EdgeTracker {
    threshold: Threshold,
    previous: Option<(f64, f64)>,
    high: Option<bool>,
    pending: Option<Edge>,
}

impl EdgeTracker {
    pub fn new(threshold: Threshold) -> Self {
        Self { threshold, previous: None, high: None, pending: None }
    }

    /// Nonfinite samples and nonincreasing times are skipped, without changing state.
    pub fn push(&mut self, t: f64, y: f64) -> Option<Edge> {
        if !self.threshold.valid() || !t.is_finite() || !y.is_finite()
            || self.previous.is_some_and(|(old, _)| t <= old) {
            return None;
        }
        let Threshold { level, hysteresis } = self.threshold;
        let lower = level - hysteresis * 0.5;
        let upper = level + hysteresis * 0.5;
        let previous = self.previous.replace((t, y));
        let Some(high) = self.high else {
            self.high = if y >= upper { Some(true) } else if y <= lower { Some(false) } else { None };
            return None;
        };
        if (high && y > level) || (!high && y < level) {
            self.pending = None;
        }
        if let Some((tp, yp)) = previous {
            let crossed = if high {
                (yp > level && y <= level) || (yp == level && y < level)
            } else {
                (yp < level && y >= level) || (yp == level && y > level)
            };
            if crossed {
                self.pending = Some(Edge {
                    t: tp + (t - tp) * ((level - yp) / (y - yp)),
                    dir: if high { Slope::Falling } else { Slope::Rising },
                });
            }
        }
        if (high && y <= lower) || (!high && y >= upper) {
            if let Some(edge) = self.pending.take() {
                self.high = Some(!high);
                return Some(edge);
            }
        }
        None
    }

    /// None until the first decisive sample.
    pub fn high(&self) -> Option<bool> {
        self.high
    }

    pub fn reset(&mut self) {
        self.previous = None;
        self.high = None;
        self.pending = None;
    }

    pub(crate) fn pending(&self) -> Option<Edge> {
        self.pending
    }
}
