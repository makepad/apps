//! The wave view's visible stretch of a take (the Scope's Take model): fit, zoom about an
//! instant, pan, follow a changed end, scrub, and the ruler's ticks. Pure, so it is tested alone.

use crate::instruments::axis::{axis_label, axis_prefix, linear_ticks, Tick};

/// The visible stretch `[t0, t1)` of a take or record, in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TakeWindow {
    pub t0: f64,
    pub t1: f64,
}

/// One femtosecond keeps the same zoom floor as the free trace view.
pub const MIN_SPAN: f64 = 1e-15;

fn positive_finite(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

impl TakeWindow {
    /// Shows the whole take. An empty take gets the minimum span.
    pub fn fit(end: f64) -> Self {
        Self {
            t0: 0.0,
            t1: if positive_finite(end) { end } else { MIN_SPAN },
        }
    }

    /// The visible duration in seconds.
    pub fn span(&self) -> f64 {
        self.t1 - self.t0
    }

    /// Maps a time to a plot coordinate, including times outside the window.
    pub fn x_of(&self, t: f64, x0: f64, w: f64) -> f64 {
        x0 + (t - self.t0) / self.span() * w
    }

    /// Maps a plot coordinate to time. A plot without width maps to the start.
    pub fn t_of(&self, x: f64, x0: f64, w: f64) -> f64 {
        if positive_finite(w) {
            self.t0 + (x - x0) / w * self.span()
        } else {
            self.t0
        }
    }

    /// Maps a scrub to a time between zero and the end of the take.
    pub fn scrub_at(&self, x: f64, x0: f64, w: f64, end: f64) -> f64 {
        if positive_finite(end) {
            self.t_of(x, x0, w).max(0.0).min(end)
        } else {
            0.0
        }
    }

    /// Zooms about a time and keeps the window inside the take. Factors above one zoom in.
    pub fn zoom(self, at: f64, factor: f64, end: f64) -> Self {
        if !positive_finite(end) || !positive_finite(factor) || !at.is_finite() {
            return self.or_fit(end);
        }
        let window = self.or_fit(end);
        let span = (window.span() / factor).max(MIN_SPAN);
        if span >= end {
            return Self::fit(end);
        }
        if span == window.span() {
            return window.clamp(end);
        }
        let fraction = (at - window.t0) / window.span();
        Self::place(at - fraction * span, span, end)
    }

    /// Moves by a duration and stops at either end of the take.
    pub fn pan(self, dt: f64, end: f64) -> Self {
        if !positive_finite(end) || !dt.is_finite() {
            return self.or_fit(end);
        }
        Self::place(self.t0 + dt, self.span(), end)
    }

    /// Keeps a full view fitted and a view at the old end following the new end.
    pub fn rebound(self, old_end: f64, end: f64) -> Self {
        if !old_end.is_finite() || old_end < 0.0 || !positive_finite(end) {
            return self.or_fit(end);
        }
        if self.t0 <= 0.0 && self.t1 >= old_end {
            return Self::fit(end);
        }
        if (old_end - self.t1).abs() <= MIN_SPAN {
            return Self::place(end - self.span(), self.span(), end);
        }
        self.clamp(end)
    }

    /// Keeps the span within the zoom limits and pushes the window inside the take.
    pub fn clamp(self, end: f64) -> Self {
        if !positive_finite(end) {
            return self.or_fit(end);
        }
        if self.t0 >= 0.0 && self.t1 <= end && self.span() >= MIN_SPAN {
            return self;
        }
        Self::place(self.t0, self.span(), end)
    }

    // Callers can construct windows directly through the public fields.
    fn or_fit(self, end: f64) -> Self {
        if self.t0.is_finite() && self.t1.is_finite() && self.t1 > self.t0 {
            self
        } else {
            Self::fit(end)
        }
    }

    fn place(start: f64, span: f64, end: f64) -> Self {
        let span = span.max(MIN_SPAN);
        if span >= end {
            return Self::fit(end);
        }
        let last_start = end - span;
        // At long record times even one representable step can exceed the zoom floor.
        if start >= last_start {
            let t0 = if end - last_start < MIN_SPAN { last_start.next_down() } else { last_start };
            Self { t0, t1: end }
        } else {
            let t0 = start.max(0.0);
            let t1 = t0 + span;
            Self { t0, t1: if t1 - t0 < MIN_SPAN { t1.next_up() } else { t1 } }
        }
    }
}

/// What a wheel or trackpad scroll over the wave view does: a sideways scroll (a trackpad's
/// horizontal swipe, a mouse's tilt) or Shift with the wheel pans by the returned points; an
/// upright scroll zooms (None).
pub fn scroll_pans(dx: f64, dy: f64, shift: bool) -> Option<f64> {
    if dx.abs() > dy.abs() {
        Some(dx)
    } else if shift && dy != 0.0 {
        Some(dy)
    } else {
        None
    }
}

impl TakeWindow {
    /// The window `from` grabbed and dragged `dx` points across a plot `width` wide: the instant
    /// under the pointer stays under it (a drag to the right shows earlier times), stopping at
    /// either end of the take.
    pub fn dragged(self, dx: f64, width: f64, end: f64) -> Self {
        if !positive_finite(width) {
            return self.or_fit(end);
        }
        self.pan(-dx * self.span() / width, end)
    }
}

/// Returns ruler ticks whose centred labels have at least `gap` points between them, labelled
/// with the time on the take (`0`, `0.5 s`, `1 s`), not as offsets.
pub fn ruler_ticks(
    window: TakeWindow,
    width: f64,
    gap: f64,
    measure: &mut dyn FnMut(&str) -> f64,
) -> Vec<Tick> {
    let (scale, unit) = axis_prefix(window.t0, window.t1, "s");
    let mut ticks = linear_ticks(window.t0, window.t1, width, gap,
        &|value, step| format!("{} {unit}", axis_label(value, step, scale)), measure);
    ticks.retain(|tick| (window.t0..window.t1).contains(&tick.value));
    ticks
}
