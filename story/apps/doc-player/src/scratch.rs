//! A finger (touch, mouse or pen) on the film's picture: the record under
//! it. Down brakes the film; a horizontal drag scrubs it the way a
//! timeline under the finger moves (the picture's width is
//! [`SECONDS_PER_WIDTH`] of film, dragging left is forward); up lets it
//! go, with the finger's last velocity as a fling the same way. A press that never moved
//! past [`SLOP`] is a tap.

use crate::transport::Transport;
use std::collections::VecDeque;

/// Film seconds a drag across the whole picture moves.
pub const SECONDS_PER_WIDTH: f64 = 4.0;
/// Pixels a finger moves before a press is a drag.
const SLOP: f64 = 6.0;
/// The window the finger's velocity is measured over, and how recent its
/// last move must be for a release to fling.
const VELOCITY_WINDOW: f64 = 0.08;
const FLING_RECENT: f64 = 0.06;
/// A release slower than this (film seconds per second) is no fling; a
/// fling is at most `FLING_MAX`.
const FLING_MIN: f64 = 0.5;
const FLING_MAX: f64 = 16.0;

pub struct Scratch {
    start_x: f64,
    seconds_per_px: f64,
    dragging: bool,
    /// Recent (time, x) of the finger.
    moves: VecDeque<(f64, f64)>,
}

impl Scratch {
    /// A finger down at `x` (time `time`) on a picture `width` wide.
    pub fn press(transport: &mut Transport, x: f64, time: f64, width: f64) -> Scratch {
        transport.press(true);
        let mut moves = VecDeque::new();
        moves.push_back((time, x));
        // Negative: the film moves with the finger, so a drag to the left
        // brings later film in from the right.
        Scratch { start_x: x, seconds_per_px: -SECONDS_PER_WIDTH / width.max(1.0), dragging: false, moves }
    }

    /// Film seconds per second the finger moved over the last `window`.
    fn velocity(&self, now: f64, window: f64) -> f64 {
        let Some(&(t1, x1)) = self.moves.back() else { return 0.0 };
        let Some(&(t0, x0)) = self.moves.iter().find(|(t, _)| *t >= now - window) else { return 0.0 };
        if t1 - t0 < 0.008 {
            return 0.0;
        }
        ((x1 - x0) / (t1 - t0) * self.seconds_per_px).clamp(-FLING_MAX, FLING_MAX)
    }

    pub fn moved(&mut self, transport: &mut Transport, x: f64, time: f64) {
        self.moves.push_back((time, x));
        while self.moves.front().is_some_and(|(t, _)| *t < time - 0.25) {
            self.moves.pop_front();
        }
        if !self.dragging && (x - self.start_x).abs() > SLOP {
            self.dragging = true;
        }
        if self.dragging {
            transport.drag((x - self.start_x) * self.seconds_per_px);
        }
    }

    pub fn release(self, transport: &mut Transport, time: f64) {
        let recent = self.moves.back().is_some_and(|(t, _)| time - *t <= FLING_RECENT);
        let v = if recent { self.velocity(time, VELOCITY_WINDOW) } else { 0.0 };
        let fling = (v.abs() >= FLING_MIN).then_some(v);
        transport.release(self.dragging, fling);
    }
}
