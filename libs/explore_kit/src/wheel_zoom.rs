//! Wheel zoom timing shared by every explorer map: each wheel tick adds a
//! constant log-zoom velocity over a short window, so ticks glide.
use std::collections::VecDeque;

// Win32 wheel deltas are in 120-unit notches. Preserve fractional ticks while
// matching the same visual gain in the base map and floating maps.
pub const WHEEL_ZOOM_GAIN: f64 = if cfg!(target_os = "windows") { 0.001 } else { 0.01 };
const STEP_SECONDS: f64 = 0.080;

/// One tick contributes constant log-zoom velocity until its original deadline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomStep { pub delta: f64, pub since: f64 }

pub fn advance(steps: &mut VecDeque<ZoomStep>, last: &mut f64, now: f64) -> (f64, bool) {
    let mut delta = 0.0;
    for step in steps.iter() {
        let before = ((*last-step.since)/STEP_SECONDS).clamp(0.0,1.0);
        let after = ((now-step.since)/STEP_SECONDS).clamp(0.0,1.0);
        delta += step.delta*(after-before);
    }
    *last = now;
    while steps.front().is_some_and(|step| now-step.since >= STEP_SECONDS) { steps.pop_front(); }
    (delta,steps.is_empty())
}