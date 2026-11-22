//! Flick momentum for a map pan: the pointer's velocity over the last
//! samples of a drag, and the exponential coast that carries the pan on
//! after the release. Pure: no `Cx`, no frames. The view feeds it the drag's
//! samples in pan units (screen points on a flat map) and steps it once per
//! delivered frame.
use makepad_math::{dvec2, Vec2d};

/// The samples that decide the release velocity: the last 60 ms of the
/// drag (four frames at 60 Hz), so the flick's final motion counts, not
/// the whole drag, and a pointer that paused before lifting flings nothing.
pub const VELOCITY_WINDOW_SECONDS: f64 = 0.06;

/// The coast's time constant: the velocity falls to 1/e in this long, and
/// the travel left is `speed * tau`. 0.3 s is the macOS list feel.
pub const FLING_TAU_SECONDS: f64 = 0.3;

/// The longest span one step integrates: a stalled frame (an occluded
/// window, a swap) continues the coast from where it was, never jumps it.
const MAX_STEP_SECONDS: f64 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    time: f64,
    offset: Vec2d,
}

/// The drag's recent path: `(time, pan offset since the press)` inside the
/// velocity window.
#[derive(Clone, Debug, Default)]
pub struct VelocityTracker {
    samples: Vec<Sample>,
}

impl VelocityTracker {
    /// Record the pan offset at `time`; samples that fell out of the window
    /// behind it are dropped.
    pub fn push(&mut self, time: f64, offset: Vec2d) {
        self.samples
            .retain(|s| time - s.time <= VELOCITY_WINDOW_SECONDS);
        self.samples.push(Sample { time, offset });
    }
    pub fn clear(&mut self) {
        self.samples.clear();
    }
    /// The velocity at a release at `time`, in offset units per second: the
    /// secant over the samples inside the window; zero when fewer than two
    /// are left in it (the pointer paused before lifting) or they share a
    /// time.
    pub fn velocity(&self, time: f64) -> Vec2d {
        let mut inside = self
            .samples
            .iter()
            .filter(|s| time - s.time <= VELOCITY_WINDOW_SECONDS);
        let (Some(first), Some(last)) = (inside.next(), inside.last()) else {
            return dvec2(0.0, 0.0);
        };
        let span = last.time - first.time;
        if span <= 0.0 {
            return dvec2(0.0, 0.0);
        }
        (last.offset - first.offset) / span
    }
}

/// A coast in flight: the velocity decays exponentially, the pan follows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fling {
    velocity: Vec2d,
    last: f64,
}

impl Fling {
    /// A coast from the release velocity at `time`, or None when the travel
    /// it would add (`speed * tau`) stays under `floor` — a drag threshold's
    /// worth: a release that barely moved is a stop, not a flick.
    pub fn start(velocity: Vec2d, time: f64, floor: f64) -> Option<Self> {
        if !velocity.x.is_finite() || !velocity.y.is_finite() {
            return None;
        }
        let fling = Fling { velocity, last: time };
        (fling.remaining() >= floor).then_some(fling)
    }
    /// The pan delta for the frame at `now`: the integral of the decaying
    /// velocity since the previous step, over at most `MAX_STEP_SECONDS`.
    pub fn step(&mut self, now: f64) -> Vec2d {
        let dt = (now - self.last).clamp(0.0, MAX_STEP_SECONDS);
        self.last = self.last.max(now);
        let decay = (-dt / FLING_TAU_SECONDS).exp();
        let delta = self.velocity * (FLING_TAU_SECONDS * (1.0 - decay));
        self.velocity = self.velocity * decay;
        delta
    }
    /// The travel left in the coast: the velocity integrated to infinity.
    pub fn remaining(&self) -> f64 {
        self.velocity.length() * FLING_TAU_SECONDS
    }
    /// Done when what is left would not move `pixel` (one device pixel).
    pub fn done(&self, pixel: f64) -> bool {
        self.remaining() < pixel
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady(tracker: &mut VelocityTracker, from: f64, count: usize, spacing: f64, step: Vec2d) -> f64 {
        let mut time = from;
        let mut offset = dvec2(0.0, 0.0);
        for _ in 0..count {
            tracker.push(time, offset);
            time += spacing;
            offset = offset + step;
        }
        time - spacing
    }

    #[test]
    fn velocity_is_the_secant_over_the_window() {
        let mut tracker = VelocityTracker::default();
        let last = steady(&mut tracker, 1.0, 6, 0.008, dvec2(4.0, -2.0));
        let v = tracker.velocity(last);
        assert!((v.x - 500.0).abs() < 1e-9 && (v.y + 250.0).abs() < 1e-9, "{v:?}");
    }

    #[test]
    fn samples_older_than_the_window_do_not_count() {
        let mut tracker = VelocityTracker::default();
        // a slow start long before the window, then a fast finish inside it
        tracker.push(0.0, dvec2(0.0, 0.0));
        tracker.push(0.5, dvec2(1.0, 0.0));
        tracker.push(1.0, dvec2(100.0, 0.0));
        tracker.push(1.02, dvec2(120.0, 0.0));
        tracker.push(1.04, dvec2(140.0, 0.0));
        let v = tracker.velocity(1.04);
        assert!((v.x - 1000.0).abs() < 1e-9, "the window's own secant: {v:?}");
    }

    #[test]
    fn a_pause_before_the_release_gives_no_velocity() {
        let mut tracker = VelocityTracker::default();
        let last = steady(&mut tracker, 2.0, 8, 0.008, dvec2(6.0, 0.0));
        assert!(tracker.velocity(last).x > 0.0);
        // the pointer holds still for a quarter second, then lifts
        assert_eq!(tracker.velocity(last + 0.25), dvec2(0.0, 0.0));
    }

    #[test]
    fn one_sample_or_a_zero_span_gives_no_velocity() {
        let mut tracker = VelocityTracker::default();
        assert_eq!(tracker.velocity(1.0), dvec2(0.0, 0.0));
        tracker.push(1.0, dvec2(5.0, 5.0));
        assert_eq!(tracker.velocity(1.0), dvec2(0.0, 0.0));
        tracker.push(1.0, dvec2(9.0, 1.0));
        assert_eq!(tracker.velocity(1.0), dvec2(0.0, 0.0), "two samples at one instant");
        tracker.clear();
        assert_eq!(tracker.velocity(1.0), dvec2(0.0, 0.0));
    }

    #[test]
    fn a_coast_starts_only_above_the_floor() {
        // 4 px of travel is the floor: 10 px/s coasts 3 px, 20 px/s coasts 6 px
        assert!(Fling::start(dvec2(10.0, 0.0), 0.0, 4.0).is_none());
        let fling = Fling::start(dvec2(20.0, 0.0), 0.0, 4.0).expect("above the floor");
        assert!((fling.remaining() - 6.0).abs() < 1e-12);
        assert!(Fling::start(dvec2(f64::NAN, 0.0), 0.0, 4.0).is_none());
        assert!(Fling::start(dvec2(0.0, f64::INFINITY), 0.0, 4.0).is_none());
    }

    #[test]
    fn steps_integrate_the_closed_form() {
        let v0 = dvec2(600.0, -300.0);
        let mut fling = Fling::start(v0, 10.0, 1.0).unwrap();
        let mut total = dvec2(0.0, 0.0);
        let mut now = 10.0;
        for _ in 0..240 {
            now += 1.0 / 120.0;
            total = total + fling.step(now);
        }
        let elapsed = now - 10.0;
        let expected = v0 * (FLING_TAU_SECONDS * (1.0 - (-elapsed / FLING_TAU_SECONDS).exp()));
        assert!((total - expected).length() < 1e-9, "{total:?} vs {expected:?}");
        let left = v0 * (-elapsed / FLING_TAU_SECONDS).exp();
        assert!((fling.remaining() - left.length() * FLING_TAU_SECONDS).abs() < 1e-9);
    }

    #[test]
    fn the_whole_coast_covers_speed_times_tau_and_stops_under_a_pixel() {
        let v0 = dvec2(750.0, 0.0);
        let mut fling = Fling::start(v0, 0.0, 4.0).unwrap();
        let mut total = dvec2(0.0, 0.0);
        let mut now = 0.0;
        let mut frames = 0;
        while !fling.done(0.5) {
            now += 1.0 / 60.0;
            let delta = fling.step(now);
            assert!(delta.x > 0.0 && delta.y == 0.0, "the coast keeps its direction");
            total = total + delta;
            frames += 1;
            assert!(frames < 1000, "the coast never ended");
        }
        assert!(fling.remaining() < 0.5);
        assert!((total.x - 750.0 * FLING_TAU_SECONDS).abs() < 0.5, "{total:?}");
        // 225 px at 60 Hz: the coast outlives a second, most of it early
        assert!(frames > 60, "{frames}");
    }

    #[test]
    fn a_stalled_frame_integrates_no_more_than_the_step_cap() {
        let v0 = dvec2(400.0, 400.0);
        let mut stalled = Fling::start(v0, 0.0, 1.0).unwrap();
        let mut capped = Fling::start(v0, 0.0, 1.0).unwrap();
        assert_eq!(stalled.step(5.0), capped.step(MAX_STEP_SECONDS));
        assert_eq!(stalled, Fling { last: 5.0, ..capped });
        // a clock that runs backwards moves nothing and keeps its place
        assert_eq!(stalled.step(4.0), dvec2(0.0, 0.0));
        assert_eq!(stalled.last, 5.0);
    }
}
