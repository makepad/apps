//! Window-free mappings for the scope's trigger and look controls.

use super::scope::{Measurement, Persistence};

const DETENT: f64 = 0.04;

/// The end pockets are off/forever; the intervening track spans 10 ms to 10 s.
pub fn afterglow_at(position: f64) -> Persistence {
    if position < DETENT { Persistence::Off }
    else if position > 1.0 - DETENT { Persistence::Infinite }
    else { Persistence::Seconds(0.01 * 1000.0_f64.powf((position - DETENT) / (1.0 - 2.0 * DETENT))) }
}

pub fn afterglow_position(value: Persistence) -> f64 {
    match value {
        Persistence::Off => 0.0,
        Persistence::Infinite => 1.0,
        Persistence::Seconds(t) => DETENT + (1.0 - 2.0 * DETENT) * (t.clamp(0.01, 10.0) / 0.01).log10() / 3.0,
    }
}

/// Reads only the published measurement rows, never the source record.
pub fn centred_level(rows: &[Measurement], channel: usize) -> Option<f64> {
    let row = |name: &str| rows.iter().find(|r| r.lane == channel && r.name == name && r.value.is_finite()).map(|r| r.value);
    match (row("top"), row("base")) {
        (Some(top), Some(base)) => Some(top * 0.5 + base * 0.5),
        _ => row("mean"),
    }
}

/// Preserve volts between lanes, clipping only to the destination's visible range.
pub fn ghost_level(level: f64, bottom: f64, top: f64) -> f64 { level.clamp(bottom, top) }

/// Logical pixels from trigger zero to the dragged edge, mapped into the post-trigger window.
pub fn holdoff_drag(distance: f64, width: f64, span: f64, post: f64) -> f64 {
    if width <= 0.0 || !distance.is_finite() { return 0.0; }
    (distance * span / width).clamp(0.0, post.max(0.0))
}

/// A counter, rather than a latched Fired state, makes re-arming wait for a new event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StopAtTrigger { armed: bool, seen: u64 }

impl StopAtTrigger {
    pub fn armed(&self) -> bool { self.armed }
    pub fn arm(&mut self, events: u64) { self.armed = true; self.seen = events; }
    pub fn cancel(&mut self) { self.armed = false; }
    /// True once per arm. A replaced feed's lower counter establishes a fresh baseline.
    pub fn observe(&mut self, events: u64) -> bool {
        let stop = self.armed && events > self.seen;
        self.seen = events;
        if stop { self.armed = false; }
        stop
    }
}
