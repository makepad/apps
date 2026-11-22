//! Extrusion cross-sections.
//!
//! A bead of width w and height h is modelled as a rectangle with
//! semicircular sides: area = (w − h)·h + π(h/2)². Neighbouring beads
//! overlap so their centres sit `spacing = w − h(1 − π/4)` apart, which
//! makes the deposited areas tile the layer.

use std::f64::consts::PI;

pub fn cross_section(w: f64, h: f64) -> f64 {
    if w >= h {
        (w - h) * h + PI * (h / 2.0) * (h / 2.0)
    } else {
        // Thinner than tall: an ellipse-ish bead.
        PI * (w / 2.0) * (h / 2.0)
    }
}

pub fn spacing(w: f64, h: f64) -> f64 {
    w - h * (1.0 - PI / 4.0)
}

pub fn width_for_spacing(s: f64, h: f64) -> f64 {
    s + h * (1.0 - PI / 4.0)
}

/// A bridge bead hangs as a round thread.
pub fn bridge_section(d: f64) -> f64 {
    PI * d * d / 4.0
}
