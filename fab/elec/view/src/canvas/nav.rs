//! Canvas navigation: wheel zoom glides, pinch, two-finger and drag pans with a fling on release,
//! and hop flights (van Wijk and Nuij) to a point or a box. Pure: no `Cx`, no frames. The pane feeds
//! it input in logical points with event times and calls `step` once per delivered frame; `step`
//! says whether anything still moves, so frames are asked only while the camera moves.
//!
//! The pieces are Scope's, shared through `makepad_explore_kit`, so both explorers feel the same.
//! Direct input (a wheel tick, a pinch, a pan) ends a running flight or fling at once: motion
//! never blocks input. Cost: O(1) per input and per step (the glide keeps the ticks of the last
//! 80 ms).
use super::camera::Camera2d;
use makepad_explore_kit::hop::{hop_duration, HopPath};
use makepad_explore_kit::pan_fling::{Fling, VelocityTracker};
use makepad_explore_kit::wheel_zoom::{self, ZoomStep, WHEEL_ZOOM_GAIN};
use makepad_widgets_core::{dvec2, Vec2d};
use std::collections::VecDeque;

/// A release that would coast less than this many points is a stop, not a flick.
const FLING_FLOOR_POINTS: f64 = 4.0;

#[derive(Clone, Copy, Debug)]
struct Flight {
    path: HopPath,
    start: f64,
    duration: f64,
}

/// The navigation state of one pane.
#[derive(Clone, Debug)]
pub struct Nav {
    /// The zoom range, logical points per world unit.
    pub min_ppu: f64,
    pub max_ppu: f64,
    glide: VecDeque<ZoomStep>,
    glide_last: f64,
    glide_at: Vec2d,
    tracker: VelocityTracker,
    drag_offset: Vec2d,
    fling: Option<Fling>,
    flight: Option<Flight>,
}

impl Default for Nav {
    fn default() -> Self {
        Self {
            min_ppu: 1e-6,
            max_ppu: 1e6,
            glide: VecDeque::new(),
            glide_last: 0.0,
            glide_at: dvec2(0.0, 0.0),
            tracker: VelocityTracker::default(),
            drag_offset: dvec2(0.0, 0.0),
            fling: None,
            flight: None,
        }
    }
}

fn smootherstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

impl Nav {
    pub fn with_range(min_ppu: f64, max_ppu: f64) -> Self {
        Self { min_ppu, max_ppu, ..Self::default() }
    }

    /// Anything in motion (a glide, a fling or a flight).
    pub fn moving(&self) -> bool {
        !self.glide.is_empty() || self.fling.is_some() || self.flight.is_some()
    }

    /// A flight is running.
    pub fn flying(&self) -> bool {
        self.flight.is_some()
    }

    /// Ends every motion where it is.
    pub fn stop(&mut self) {
        self.glide.clear();
        self.fling = None;
        self.flight = None;
    }

    fn clamp_zoom(&self, cam: &mut Camera2d, anchor: Vec2d) {
        let ppu = cam.px_per_unit.clamp(self.min_ppu, self.max_ppu);
        if ppu != cam.px_per_unit {
            cam.zoom_about(anchor, ppu / cam.px_per_unit);
        }
    }

    /// A wheel tick at `at` (logical points), `delta_y` as the platform gives it: a short glide of
    /// constant log-zoom velocity. Negative `delta_y` zooms in, as in Scope.
    pub fn wheel(&mut self, at: Vec2d, delta_y: f64, now: f64) {
        if !delta_y.is_finite() || delta_y == 0.0 {
            return;
        }
        self.fling = None;
        self.flight = None;
        if self.glide.is_empty() {
            self.glide_last = now;
        }
        self.glide_at = at;
        self.glide.push_back(ZoomStep { delta: -delta_y * WHEEL_ZOOM_GAIN, since: now });
    }

    /// A pinch step about `at`: `scale` is the change since the previous step.
    pub fn pinch(&mut self, cam: &mut Camera2d, at: Vec2d, scale: f64) {
        if !(scale.is_finite() && scale > 0.0) {
            return;
        }
        self.stop();
        cam.zoom_about(at, scale);
        self.clamp_zoom(cam, at);
    }

    /// A pan by `delta` logical points with no momentum of its own (a two-finger scroll; the
    /// platform's own momentum arrives as further scrolls).
    pub fn scroll_pan(&mut self, cam: &mut Camera2d, delta: Vec2d) {
        if !delta.x.is_finite() || !delta.y.is_finite() {
            return;
        }
        self.stop();
        cam.pan_px(delta);
    }

    /// A pan drag starts (the pan key or the middle button held over the canvas).
    pub fn drag_begin(&mut self, now: f64) {
        self.stop();
        self.tracker.clear();
        self.drag_offset = dvec2(0.0, 0.0);
        self.tracker.push(now, self.drag_offset);
    }

    /// The pan drag moved by `delta` logical points.
    pub fn drag_move(&mut self, cam: &mut Camera2d, delta: Vec2d, now: f64) {
        cam.pan_px(delta);
        self.drag_offset = self.drag_offset + delta;
        self.tracker.push(now, self.drag_offset);
    }

    /// The pan drag ends: a flick coasts on.
    pub fn drag_end(&mut self, now: f64) {
        let velocity = self.tracker.velocity(now);
        self.tracker.clear();
        self.fling = Fling::start(velocity, now, FLING_FLOOR_POINTS);
    }

    /// Flies to show world point `center` at `ppu` (clamped to the range). A running flight is
    /// retargeted from where it is now. `duration` overrides the hop path's own rule.
    pub fn fly_to(&mut self, cam: &Camera2d, center: [f64; 2], ppu: f64, now: f64, duration: Option<f64>) {
        self.glide.clear();
        self.fling = None;
        let ppu = ppu.clamp(self.min_ppu, self.max_ppu);
        let from = dvec2(cam.center[0], cam.center[1]);
        let Some(path) = HopPath::between(cam.view.size.x, from, cam.px_per_unit, dvec2(center[0], center[1]), ppu) else {
            self.flight = None;
            return;
        };
        let duration = duration.unwrap_or_else(|| hop_duration(path.length()));
        self.flight = Some(Flight { path, start: now, duration: duration.max(1e-3) });
    }

    /// Flies to show the world box `bounds` whole with `margin` points round it.
    pub fn fly_to_box(&mut self, cam: &Camera2d, bounds: [f64; 4], margin: f64, now: f64, duration: Option<f64>) {
        self.fly_to_box_with_inset(cam, bounds, margin, [0.0; 4], now, duration);
    }

    /// Flies to the clear-area fit, keeping the box centred there at a zoom limit. O(1).
    pub fn fly_to_box_with_inset(&mut self, cam: &Camera2d, bounds: [f64; 4], margin: f64, inset: [f64; 4], now: f64, duration: Option<f64>) {
        let mut target = Camera2d::fit_with_inset(cam.view, cam.dpi, bounds, margin, inset);
        let middle = [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5];
        if target.center != middle {
            let anchor = target.to_screen(middle);
            self.clamp_zoom(&mut target, anchor);
        }
        self.fly_to(cam, target.center, target.px_per_unit, now, duration);
    }

    /// Advances every motion to `now`. Returns true while something still moves (ask for the next
    /// frame), false once all is at rest.
    pub fn step(&mut self, cam: &mut Camera2d, now: f64) -> bool {
        if let Some(flight) = self.flight {
            let t = ((now - flight.start) / flight.duration).clamp(0.0, 1.0);
            let (c, z) = if t >= 1.0 {
                flight.path.end()
            } else {
                flight.path.sample(flight.path.length() * smootherstep(t))
            };
            cam.center = [c.x, c.y];
            cam.px_per_unit = z;
            if t >= 1.0 {
                self.flight = None;
            }
        }
        if !self.glide.is_empty() {
            let (delta, settled) = wheel_zoom::advance(&mut self.glide, &mut self.glide_last, now);
            if delta != 0.0 {
                cam.zoom_about(self.glide_at, delta.exp());
                self.clamp_zoom(cam, self.glide_at);
            }
            if settled {
                self.glide.clear();
            }
        }
        if let Some(fling) = &mut self.fling {
            let delta = fling.step(now);
            cam.pan_px(delta);
            if fling.done(1.0 / cam.dpi.max(1e-6)) {
                self.fling = None;
            }
        }
        self.moving()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets_core::Rect;

    fn cam() -> Camera2d {
        Camera2d { center: [0.0, 0.0], px_per_unit: 2.0, view: Rect { pos: dvec2(0.0, 0.0), size: dvec2(1000.0, 600.0) }, dpi: 2.0 }
    }

    /// A flight lands exactly and stops, the wheel glides about its anchor and settles, a flick
    /// coasts on and comes to rest, and direct input cancels a flight at once.
    #[test]
    fn flights_land_glides_settle_flicks_coast_and_input_wins() {
        let mut c = cam();
        let mut nav = Nav::with_range(0.01, 100.0);
        nav.fly_to(&c, [500.0, -200.0], 8.0, 10.0, Some(0.3));
        assert!(nav.flying());
        let mut t = 10.0;
        let mut frames = 0;
        while nav.step(&mut c, t) {
            t += 1.0 / 120.0;
            frames += 1;
            assert!(frames < 200, "the flight ends");
        }
        assert!((c.center[0] - 500.0).abs() < 1e-9 && (c.center[1] + 200.0).abs() < 1e-9, "lands on the target: {:?}", c.center);
        assert!((c.px_per_unit - 8.0).abs() < 1e-9, "lands on the zoom");
        assert!((34..=40).contains(&frames), "0.3 s at 120 Hz: {frames} frames");

        let anchor = dvec2(700.0, 100.0);
        let under = c.to_world(anchor);
        nav.wheel(anchor, -30.0, 20.0);
        let mut t = 20.0;
        while nav.step(&mut c, t) {
            t += 1.0 / 60.0;
        }
        assert!(c.px_per_unit > 8.0, "a negative wheel delta zooms in");
        let still = c.to_world(anchor);
        assert!((still[0] - under[0]).abs() < 1e-6 && (still[1] - under[1]).abs() < 1e-6, "the glide keeps its anchor");

        let before = c.center;
        nav.drag_begin(30.0);
        for i in 1..=5 {
            nav.drag_move(&mut c, dvec2(-12.0, 0.0), 30.0 + i as f64 * 0.01);
        }
        nav.drag_end(30.05);
        let dragged = c.center[0] - before[0];
        let mut t = 30.05;
        let mut steps = 0;
        while nav.step(&mut c, t) {
            t += 1.0 / 60.0;
            steps += 1;
            assert!(steps < 1000, "the fling ends");
        }
        assert!(dragged > 0.0 && c.center[0] - before[0] > dragged, "the flick coasts on the way it went");

        nav.fly_to(&c, [0.0, 0.0], 1.0, 40.0, None);
        nav.step(&mut c, 40.1);
        nav.scroll_pan(&mut c, dvec2(3.0, 0.0));
        assert!(!nav.moving(), "direct input ends the flight");
        nav.pinch(&mut c, dvec2(10.0, 10.0), 1e9);
        assert_eq!(c.px_per_unit, 100.0, "zoom stays in range");
    }
}
