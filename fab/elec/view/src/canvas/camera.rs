//! The 2D canvas seam every electronics 2D view builds on (the explorer, the board, the Part and
//! proof views): the camera, the level-of-detail ladder, and what a view's items answer for
//! picking and for remote driving.
//!
//! Frames: the world is Y up (C1: board and sheet coordinates alike), in whatever unit the view
//! draws in (grid steps on the sheet, millimetres on the board); the screen is the pane's logical
//! points, Y down, as Makepad lays out. `dpi` is the pass's pixel ratio, so `px_per_unit * dpi` is
//! physical pixels per unit, the measure the ladder and the line weights use.
//!
//! Cost: every function here is O(1) except `Ladder::at`, O(rungs) (at most `MAX_RUNGS`). Nothing
//! allocates; a frame calls `uniforms` once per draw list and `Ladder::at` once per pane.
use makepad_widgets_core::{dvec2, Rect, Vec2d};

/// The camera of one canvas pane: the world point at the pane's centre and the zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera2d {
    /// The world point shown at the centre of `view`.
    pub center: [f64; 2],
    /// Logical points per world unit (the zoom).
    pub px_per_unit: f64,
    /// The pane on screen, logical points.
    pub view: Rect,
    /// Physical pixels per logical point.
    pub dpi: f64,
}

impl Default for Camera2d {
    fn default() -> Self {
        Self { center: [0.0, 0.0], px_per_unit: 1.0, view: Rect::default(), dpi: 1.0 }
    }
}

impl Camera2d {
    /// The camera that shows the world box `bounds` (`[min_x, min_y, max_x, max_y]`) whole in
    /// `view`, with `margin` logical points on every side; an empty or degenerate box is shown
    /// at one point per unit round its centre.
    pub fn fit(view: Rect, dpi: f64, bounds: [f64; 4], margin: f64) -> Self {
        Self::fit_with_inset(view, dpi, bounds, margin, [0.0; 4])
    }

    /// Fits and centres the box in the clear part of the pane, keeping `view` for drawing and
    /// picking. The inset is `[left, top, right, bottom]` in logical points. If either clear span
    /// is under a quarter of the pane's span, the whole pane takes the fit. O(1).
    pub fn fit_with_inset(view: Rect, dpi: f64, bounds: [f64; 4], margin: f64, inset: [f64; 4]) -> Self {
        let [left, top, right, bottom] = inset;
        let clear = view.size - dvec2(left + right, top + bottom);
        let (size, offset) = if clear.x < view.size.x * 0.25 || clear.y < view.size.y * 0.25 {
            (view.size, dvec2(0.0, 0.0))
        } else {
            (clear, dvec2((left - right) * 0.5, (top - bottom) * 0.5))
        };
        let w = bounds[2] - bounds[0];
        let h = bounds[3] - bounds[1];
        let center = [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5];
        let room = dvec2((size.x - 2.0 * margin).max(1.0), (size.y - 2.0 * margin).max(1.0));
        let px_per_unit = if w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite() {
            (room.x / w).min(room.y / h)
        } else if w > 0.0 && w.is_finite() {
            room.x / w
        } else if h > 0.0 && h.is_finite() {
            room.y / h
        } else {
            1.0
        };
        let mut cam = Self { center, px_per_unit, view, dpi: dpi.max(1e-6) };
        cam.pan_px(offset);
        cam
    }

    /// The screen point (logical) of world point `w`.
    pub fn to_screen(&self, w: [f64; 2]) -> Vec2d {
        let c = self.view.center();
        dvec2(c.x + (w[0] - self.center[0]) * self.px_per_unit, c.y - (w[1] - self.center[1]) * self.px_per_unit)
    }

    /// The world point under screen point (logical) `s`.
    pub fn to_world(&self, s: Vec2d) -> [f64; 2] {
        let c = self.view.center();
        let k = 1.0 / self.px_per_unit;
        [self.center[0] + (s.x - c.x) * k, self.center[1] - (s.y - c.y) * k]
    }

    /// The world box the pane shows: `[min_x, min_y, max_x, max_y]`.
    pub fn visible(&self) -> [f64; 4] {
        let hw = self.view.size.x * 0.5 / self.px_per_unit;
        let hh = self.view.size.y * 0.5 / self.px_per_unit;
        [self.center[0] - hw, self.center[1] - hh, self.center[0] + hw, self.center[1] + hh]
    }

    /// The world rectangle a screen rectangle covers, as `[min_x, min_y, max_x, max_y]`.
    pub fn rect_to_world(&self, r: Rect) -> [f64; 4] {
        let a = self.to_world(r.pos);
        let b = self.to_world(r.pos + r.size);
        [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]
    }

    /// The screen rectangle (logical) of a world box `[min_x, min_y, max_x, max_y]`.
    pub fn rect_to_screen(&self, b: [f64; 4]) -> Rect {
        let lo = self.to_screen([b[0], b[3]]);
        let hi = self.to_screen([b[2], b[1]]);
        Rect { pos: lo, size: hi - lo }
    }

    /// The world-to-screen map as the canvas shaders take it, one `vec4` uniform:
    /// `(off_x, off_y, px_per_unit, dpi)` with `screen = (off_x + w.x * ppu, off_y - w.y * ppu)` in
    /// logical points. The offsets are formed in f64, so only the product is f32.
    pub fn uniforms(&self) -> [f32; 4] {
        let c = self.view.center();
        [
            (c.x - self.center[0] * self.px_per_unit) as f32,
            (c.y + self.center[1] * self.px_per_unit) as f32,
            self.px_per_unit as f32,
            self.dpi as f32,
        ]
    }

    /// Physical pixels per world unit: what the ladder is read at.
    pub fn device_px_per_unit(&self) -> f64 {
        self.px_per_unit * self.dpi
    }

    /// World units one physical pixel covers.
    pub fn unit_per_device_px(&self) -> f64 {
        1.0 / self.device_px_per_unit()
    }

    /// Zooms by `factor` keeping the world point under screen point `s` where it is.
    pub fn zoom_about(&mut self, s: Vec2d, factor: f64) {
        if !(factor.is_finite() && factor > 0.0) {
            return;
        }
        let before = self.to_world(s);
        self.px_per_unit *= factor;
        let after = self.to_world(s);
        self.center[0] += before[0] - after[0];
        self.center[1] += before[1] - after[1];
    }

    /// Moves the picture by `delta` logical points (the world follows the pointer).
    pub fn pan_px(&mut self, delta: Vec2d) {
        self.center[0] -= delta.x / self.px_per_unit;
        self.center[1] += delta.y / self.px_per_unit;
    }
}

/// At most this many rungs on one ladder.
pub const MAX_RUNGS: usize = 8;

/// One representation on a ladder. Thresholds are physical pixels per world unit: a rung is
/// requested (`admit`) a little before it shows, kept down to `keep` so a zoom that dithers at the
/// edge does not thrash the worker, and fades in over `fade` (start, end).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rung {
    pub name: &'static str,
    pub admit: f64,
    pub keep: f64,
    pub fade: (f64, f64),
}

/// The ladder's state under one camera: the presentation weight of each rung (they sum to one),
/// what the worker should build (`wants`) and what it may drop (`!keeps`), and the rung with the
/// largest weight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LadderAt {
    pub weights: [f32; MAX_RUNGS],
    pub wants: [bool; MAX_RUNGS],
    pub keeps: [bool; MAX_RUNGS],
    pub dominant: usize,
    pub len: usize,
}

/// A physical-pixel level-of-detail ladder, coarse to fine (Scope's code ladder, generalised).
/// Rung 0 is the coarsest and always present; each finer rung replaces the ones below it as it
/// fades in, so the weights cross-fade and always sum to one.
#[derive(Clone, Debug, PartialEq)]
pub struct Ladder {
    rungs: Vec<Rung>,
}

fn smooth(start: f64, end: f64, v: f64) -> f64 {
    if end <= start {
        return if v >= end { 1.0 } else { 0.0 };
    }
    let t = ((v - start) / (end - start)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Ladder {
    /// A ladder over `rungs`, coarse to fine; at most `MAX_RUNGS`, and each rung's fade starts
    /// at or after the previous one's.
    pub fn new(rungs: Vec<Rung>) -> Self {
        assert!(!rungs.is_empty() && rungs.len() <= MAX_RUNGS, "a ladder has 1 to {MAX_RUNGS} rungs");
        for w in rungs.windows(2) {
            assert!(w[1].fade.0 >= w[0].fade.0, "rung {} fades in before {}", w[1].name, w[0].name);
        }
        Self { rungs }
    }

    pub fn rungs(&self) -> &[Rung] {
        &self.rungs
    }

    /// The ladder at `device_px` physical pixels per world unit.
    pub fn at(&self, device_px: f64) -> LadderAt {
        let n = self.rungs.len();
        let mut weights = [0.0f32; MAX_RUNGS];
        let mut wants = [false; MAX_RUNGS];
        let mut keeps = [false; MAX_RUNGS];
        // Finest first: each rung takes its ramp of what the finer rungs left.
        let mut left = 1.0f64;
        for k in (0..n).rev() {
            let r = &self.rungs[k];
            let ramp = if k == 0 { 1.0 } else { smooth(r.fade.0, r.fade.1, device_px) };
            weights[k] = (left * ramp) as f32;
            left *= 1.0 - ramp;
            wants[k] = k == 0 || device_px >= r.admit;
            keeps[k] = k == 0 || device_px >= r.keep;
        }
        let dominant = (0..n).max_by(|&a, &b| weights[a].total_cmp(&weights[b]).then(a.cmp(&b))).unwrap_or(0);
        LadderAt { weights, wants, keeps, dominant, len: n }
    }

    /// The weights to draw with when only the rungs in `complete` are built: an incomplete
    /// rung's weight goes to the nearest complete coarser rung, so the coarser picture stays
    /// while the finer one is in flight. Rung 0 counts as complete.
    pub fn available(at: &LadderAt, complete: &[bool]) -> [f32; MAX_RUNGS] {
        let mut out = [0.0f32; MAX_RUNGS];
        for k in 0..at.len {
            let mut j = k;
            while j > 0 && !complete.get(j).copied().unwrap_or(false) {
                j -= 1;
            }
            out[j] += at.weights[k];
        }
        out
    }
}

/// What a view's items answer: picking, and the semantic nodes the remote interface and
/// accessibility see. Implemented over the view's retained geometry (an index), never by
/// walking draw calls. Every Fab view's key is `fab_elec_core::Sel`; the canvas itself never looks
/// inside a key, so a test scene keys its plain shapes by index.
pub trait CanvasItems {
    type Key: Clone + PartialEq + std::hash::Hash + std::fmt::Debug;
    /// The items within `radius` world units of world point `at`, topmost first.
    fn hit(&self, at: [f64; 2], radius: f64) -> Vec<Self::Key>;
    /// The semantic nodes of the items `cam` shows, their rectangles in logical points.
    fn nodes(&self, cam: &Camera2d) -> Vec<CanvasNode<Self::Key>>;
}

/// One item as a semantic node: a virtual child of the canvas a remote driver can find by name
/// and press (a press acts as a click at the rectangle's centre).
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasNode<K> {
    pub sel: K,
    /// Logical points, the pane's window coordinates.
    pub rect: Rect,
    /// What it is: "part", "pin", "net", "wire", "block", "pad", "track", "via", "zone".
    pub role: &'static str,
    /// What a person reads: a reference designator, a net name, a block's name.
    pub name: String,
    /// The value shown with it, if any ("10k", "3.3 V").
    pub value: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Camera2d {
        Camera2d {
            center: [100.0, -40.0],
            px_per_unit: 4.0,
            view: Rect { pos: dvec2(10.0, 20.0), size: dvec2(800.0, 600.0) },
            dpi: 2.0,
        }
    }

    /// The camera's maps agree with each other and with the shader uniform, Y is up, a zoom
    /// keeps its anchor, a pan follows the pointer, and `fit` shows the box whole.
    #[test]
    fn camera_maps_agree_and_navigation_keeps_its_anchors() {
        let c = cam();
        let s = c.to_screen([100.0, -40.0]);
        assert_eq!(s, c.view.center(), "the centre is the pane's centre");
        let up = c.to_screen([100.0, -39.0]);
        assert!(up.y < s.y, "world Y up is screen up");
        let w = c.to_world(dvec2(123.0, 456.0));
        let back = c.to_screen(w);
        assert!((back - dvec2(123.0, 456.0)).length() < 1e-9, "to_world inverts to_screen");
        let [ox, oy, k, dpi] = c.uniforms();
        let p = [137.5, -12.25];
        let gpu = dvec2(ox as f64 + p[0] * k as f64, oy as f64 - p[1] * k as f64);
        assert!((gpu - c.to_screen(p)).length() < 1e-3, "the uniform reproduces to_screen");
        assert_eq!(dpi, 2.0);
        let v = c.visible();
        assert!((v[2] - v[0] - 200.0).abs() < 1e-9 && (v[3] - v[1] - 150.0).abs() < 1e-9);
        let r = c.rect_to_screen(v);
        assert!((r.pos - c.view.pos).length() < 1e-9 && (r.size - c.view.size).length() < 1e-9);

        let mut z = c;
        let anchor = dvec2(200.0, 100.0);
        let under = z.to_world(anchor);
        z.zoom_about(anchor, 3.0);
        assert!((z.px_per_unit - 12.0).abs() < 1e-12);
        let still = z.to_world(anchor);
        assert!((still[0] - under[0]).abs() < 1e-9 && (still[1] - under[1]).abs() < 1e-9, "zoom keeps its anchor");
        let before = z.to_screen([0.0, 0.0]);
        z.pan_px(dvec2(15.0, -7.0));
        assert!((z.to_screen([0.0, 0.0]) - before - dvec2(15.0, -7.0)).length() < 1e-9, "pan follows the pointer");

        let f = Camera2d::fit(c.view, 1.0, [0.0, 0.0, 100.0, 50.0], 20.0);
        let shown = f.rect_to_screen([0.0, 0.0, 100.0, 50.0]);
        assert!(shown.size.x <= 760.0 + 1e-9 && shown.size.y <= 560.0 + 1e-9);
        assert!((shown.size.x - 760.0).abs() < 1e-9, "the wide box fills the width");
        assert_eq!(Camera2d::fit(c.view, 1.0, [5.0, 5.0, 5.0, 5.0], 0.0).px_per_unit, 1.0);
    }

    /// Weights sum to one at every zoom and cross-fade in order; wants run ahead of the fade
    /// and keeps behind it; a rung still in flight leaves its weight on the coarser one.
    #[test]
    fn ladder_cross_fades_admits_early_and_keeps_the_coarser_rung() {
        let ladder = Ladder::new(vec![
            Rung { name: "blocks", admit: 0.0, keep: 0.0, fade: (0.0, 0.0) },
            Rung { name: "parts", admit: 1.5, keep: 1.0, fade: (2.0, 3.0) },
            Rung { name: "symbols", admit: 4.0, keep: 3.0, fade: (5.0, 8.0) },
        ]);
        let mut last_fine = 0.0f32;
        for i in 0..200 {
            let px = i as f64 * 0.06;
            let at = ladder.at(px);
            let sum: f32 = at.weights[..at.len].iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "weights sum to one at {px}: {sum}");
            assert!(at.weights[2] >= last_fine, "the finest rung only grows with zoom");
            last_fine = at.weights[2];
        }
        let far = ladder.at(0.5);
        assert_eq!(far.dominant, 0);
        assert_eq!(&far.wants[..3], &[true, false, false]);
        let edge = ladder.at(1.7);
        assert!(edge.wants[1] && edge.weights[1] == 0.0, "admitted before visible");
        let dither = ladder.at(1.2);
        assert!(!dither.wants[1] && dither.keeps[1], "kept below admission");
        let near = ladder.at(9.0);
        assert_eq!(near.dominant, 2);
        assert!((near.weights[2] - 1.0).abs() < 1e-6);
        let mid = ladder.at(6.5);
        let shown = Ladder::available(&mid, &[true, true, false]);
        assert!(shown[2] == 0.0 && (shown[1] - (mid.weights[1] + mid.weights[2])).abs() < 1e-6);
        let none = Ladder::available(&mid, &[true, false, false]);
        assert!((none[0] - 1.0).abs() < 1e-5, "only the base is built: it carries everything");
    }
}
