//! Pointer mapping and fits, independent of widget state and acquisition records.

use super::scope::{DIV_MAX, DIV_MIN};
use fab_elec_bench::trigger::Window;
use makepad_widgets_core::{dvec2, DVec2, Rect};
use super::acquire::Zone;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis { Horizontal, Vertical, Both }

/// Scale the window about a trigger-relative instant, preserving its screen fraction.
/// Bounds on divisions and trigger position belong to the settings, not this affine map.
pub fn zoom_time_about(window: Window, at: f64, factor: f64) -> Window {
    let (lo, hi) = zoom_value_about((-window.pre, window.post), at, factor);
    Window { pre: -lo, post: hi }
}

/// Scale an ascending value interval about a value, including intervals below zero.
pub fn zoom_value_about(span: (f64, f64), at: f64, factor: f64) -> (f64, f64) {
    if !at.is_finite() || !factor.is_finite() || factor <= 0.0 || factor == 1.0 || !span.0.is_finite() || !span.1.is_finite() || span.1 <= span.0 { return span; }
    let lo = at + (span.0 - at) * factor;
    let hi = at + (span.1 - at) * factor;
    if lo.is_finite() && hi.is_finite() && hi > lo { (lo, hi) } else { span }
}

/// Scale ten dB divisions about a level, preserving its screen fraction even at the limits.
/// Returns (top reference in dBV, dB per division).
pub fn zoom_db_about(reference: f64, db_per_div: f64, at: f64, factor: f64) -> (f64, f64) {
    if !reference.is_finite() || !db_per_div.is_finite() || db_per_div <= 0.0 || !at.is_finite()
        || !factor.is_finite() || factor <= 0.0 || factor == 1.0 { return (reference, db_per_div); }
    let scale = (db_per_div * factor).clamp(0.1, 50.0);
    if scale == db_per_div { return (reference, db_per_div); }
    let top = at + (reference - at) * (scale / db_per_div);
    if top.is_finite() { (top, scale) } else { (reference, db_per_div) }
}

/// Move the spectrum's picture `dy` points down from the press, across ten vertical divisions.
pub fn drag_db_reference(reference: f64, db_per_div: f64, dy: f64, plot_height: f64) -> f64 {
    if !reference.is_finite() || !db_per_div.is_finite() || db_per_div <= 0.0 || !dy.is_finite()
        || !plot_height.is_finite() || plot_height <= 0.0 { return reference; }
    let top = reference + 10.0 * dy / plot_height * db_per_div;
    if top.is_finite() { top } else { reference }
}

/// Split the cell's available picture height into spectrum and waterfall, clamping its share.
pub fn waterfall_split(available: f64, share: f64) -> (f64, f64) {
    if !available.is_finite() || available <= 0.0 { return (0.0, 0.0); }
    let share = if share.is_finite() { share.clamp(0.15, 0.75) } else { 1.0 / 3.0 };
    let waterfall = available * share;
    (available - waterfall, waterfall)
}

/// Grow the lower waterfall by `dy` points (upward pointer motion), using the press-time height.
pub fn drag_waterfall_share(start_share: f64, dy: f64, available: f64) -> f64 {
    if !start_share.is_finite() || !dy.is_finite() || !available.is_finite() || available <= 0.0 { return start_share; }
    (start_share + dy / available).clamp(0.15, 0.75)
}

/// The caller retains the first lock; Shift can release it to both axes for the rest of a drag.
pub fn lock_axis(delta: DVec2, slop: f64, free: bool) -> Option<Axis> {
    if !delta.x.is_finite() || !delta.y.is_finite() || !slop.is_finite() || delta.length() <= slop.max(0.0) { return None; }
    Some(if free { Axis::Both } else if delta.x.abs() >= delta.y.abs() { Axis::Horizontal } else { Axis::Vertical })
}

/// Eight divisions, centred on the extrema, with ten percent total headroom.
/// An exactly flat input gets a span of ten percent of its magnitude, or 1 mV at zero.
pub fn fit_values(min: f64, max: f64) -> (f64, f64) {
    if !min.is_finite() || !max.is_finite() || max < min { return (1.0, 0.0); }
    let centre = min * 0.5 + max * 0.5;
    let span = if max > min { (max - min) * 1.1 } else { (centre.abs() * 0.1).max(1e-3) };
    let scale = (span / 8.0).clamp(DIV_MIN, DIV_MAX);
    let offset = centre / scale;
    if offset.is_finite() { (scale, offset) } else { (1.0, 0.0) }
}

/// Show two periods at the existing trigger fraction; absent or invalid periods keep the view.
pub fn fit_time(period: Option<f64>, current: Window) -> Window {
    let Some(period) = period.filter(|p| p.is_finite() && *p > 0.0) else { return current };
    let span = current.pre + current.post;
    if !span.is_finite() || span <= 0.0 { return current; }
    let fitted = (period / 5.0).clamp(DIV_MIN, DIV_MAX) * 10.0;
    let pre = current.pre / span * fitted;
    Window { pre, post: fitted - pre }
}

/// Press-time axes keep a box fixed in signal coordinates while the pointer moves.
#[derive(Clone, Copy)]
pub struct ZoneDrag {
    pub press: DVec2,
    pub pointer: DVec2,
    pub original: Option<Zone>,
    pub lane: usize,
    pub plot: Rect,
    pub window: Window,
    pub values: (f64, f64),
}

impl ZoneDrag {
    pub fn rect(self) -> Rect {
        let a = self.press;
        let b = self.pointer;
        Rect { pos: dvec2(a.x.min(b.x), a.y.min(b.y)), size: dvec2((a.x - b.x).abs(), (a.y - b.y).abs()) }
    }

    pub fn preview(self) -> Option<Zone> {
        if self.plot.size.x <= 0.0 || self.plot.size.y <= 0.0 { return None; }
        let seconds = (self.window.pre + self.window.post) / self.plot.size.x;
        let units = (self.values.1 - self.values.0) / self.plot.size.y;
        if let Some(zone) = self.original {
            let delta = self.pointer - self.press;
            return Some(Zone { t0: zone.t0 + delta.x * seconds, t1: zone.t1 + delta.x * seconds,
                v0: zone.v0 - delta.y * units, v1: zone.v1 - delta.y * units, ..zone });
        }
        let rect = self.rect();
        Some(Zone { lane: self.lane,
            t0: (rect.pos.x - self.plot.pos.x) * seconds - self.window.pre,
            t1: (rect.pos.x + rect.size.x - self.plot.pos.x) * seconds - self.window.pre,
            v0: self.values.1 - (rect.pos.y + rect.size.y - self.plot.pos.y) * units,
            v1: self.values.1 - (rect.pos.y - self.plot.pos.y) * units, must: true })
    }

    /// An empty result on a new drag ignores it; an existing box dragged off-lane is removed.
    pub fn finish(self) -> Option<Zone> {
        if let Some(zone) = self.original {
            if self.pointer.y < self.plot.pos.y || self.pointer.y > self.plot.pos.y + self.plot.size.y { return None; }
            if (self.pointer - self.press).length() < 4.0 { return Some(Zone { must: !zone.must, ..zone }); }
        } else {
            let rect = self.rect();
            if rect.size.x < 4.0 || rect.size.y < 4.0 { return None; }
        }
        self.preview().filter(|zone| zone.valid())
    }
}
