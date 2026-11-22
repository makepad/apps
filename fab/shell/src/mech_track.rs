//! The recent pointer/hand motion used by Mechanics throws and hand prediction.

use makepad_math::Vec3f;
use std::collections::VecDeque;

const VELOCITY_S: f64 = 0.04;

/// Position samples in the caller's frame; velocity uses the newest 40 ms of motion.
#[derive(Clone, Debug, Default)]
pub struct VelocityTrack {
    at: VecDeque<(f64, Vec3f)>,
}

impl VelocityTrack {
    pub fn push(&mut self, t: f64, p: Vec3f) {
        if let Some(&(last, _)) = self.at.back() {
            if t < last {
                self.at.clear();
            } else if t == last {
                self.at.pop_back();
            }
        }
        self.at.push_back((t, p));
        // Keep one sample before the window to interpolate its boundary. Sampling frequency
        // does not shorten the window, and a stopped hand cannot retain an old fling.
        while self.at.len() > 2 && self.at[1].0 <= t - VELOCITY_S {
            self.at.pop_front();
        }
    }

    /// Units per second in the sampled frame (zero until there are two distinct times).
    pub fn velocity(&self) -> Vec3f {
        if self.at.len() < 2 {
            return Vec3f::default();
        }
        let &(t1, p1) = self.at.back().unwrap();
        let (mut t0, mut p0) = self.at[0];
        let cutoff = t1 - VELOCITY_S;
        if t0 < cutoff {
            let (t, p) = self.at[1];
            p0 = p0 + (p - p0) * ((cutoff - t0) / (t - t0)) as f32;
            t0 = cutoff;
        }
        let dt = (t1 - t0) as f32;
        if dt > 1e-4 { (p1 - p0) * (1.0 / dt) } else { Vec3f::default() }
    }
}
