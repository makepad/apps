//! A seat's view of one shared window. Picture, overlays and pointer input share this map.
use super::Rect as AppRect;
use crate::board::gesture::{scroll_nav, ScrollNav};
use makepad_widgets::{dvec2, DVec2, KeyModifiers, Rect};

pub const MIN_ZOOM: f64 = 1.0;
pub const MAX_ZOOM: f64 = 4.0;
pub const ZOOM_STEP: f64 = 0.25;

pub(crate) fn zoom_label(zoom: f64) -> String {
    let tenths = zoom * 10.;
    if (tenths.round() - tenths).abs() < 1e-6 { format!("{zoom:.1}×") } else { format!("{zoom:.2}×") }
}

pub(crate) fn wheel_zoom(scroll: DVec2, wheel: bool, mods: &KeyModifiers) -> Option<f64> {
    match scroll_nav(scroll, wheel, mods) {
        ScrollNav::Zoom(factor) => Some(factor),
        ScrollNav::Pan(_) => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewFocusRule {
    pub controls_enabled: bool,
    pub center: bool,
    pub zoom: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetPresence { Present, Pending, Missing }

#[derive(Clone, Debug, PartialEq)]
pub struct FocusState {
    pub isolated_path: Option<String>,
    pub isolated_rect: Option<AppRect>,
    pub center: bool,
    pub zoom: f64,
    pub hint: Option<String>,
}

impl Default for FocusState {
    fn default() -> Self {
        Self { isolated_path: None, isolated_rect: None, center: false, zoom: MIN_ZOOM, hint: None }
    }
}

impl FocusState {
    pub fn rule(&self) -> ViewFocusRule {
        ViewFocusRule { controls_enabled: self.isolated_path.is_some(), center: self.center, zoom: self.zoom }
    }

    pub fn toggle_isolate(&mut self, selected: Option<(&str, Option<AppRect>)>) {
        if self.isolated_path.is_some() {
            *self = Self::default();
        } else if let Some((path, rect)) = selected.filter(|(p, _)| !p.is_empty()) {
            self.isolated_path = Some(path.to_owned());
            self.hint = None;
            if let Some(rect) = rect { self.update_rect(rect); }
        }
    }

    pub fn set_center(&mut self, on: bool) { self.center = self.isolated_path.is_some() && on; }

    pub fn set_zoom(&mut self, zoom: f64) {
        if self.isolated_path.is_some() && zoom.is_finite() { self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM); }
    }

    pub fn step_zoom(&mut self, delta: f64) { self.set_zoom(self.zoom + delta); }

    pub fn scale_zoom(&mut self, factor: f64) { self.set_zoom(self.zoom * factor); }

    pub fn update_rect(&mut self, rect: AppRect) {
        if self.isolated_path.is_none() { return }
        if rect.valid() && rect.w > 0. && rect.h > 0. { self.isolated_rect = Some(rect); }
        else { self.settle(TargetPresence::Missing); }
    }

    pub fn settle(&mut self, presence: TargetPresence) -> bool {
        if presence != TargetPresence::Missing || self.isolated_path.is_none() { return false }
        *self = Self { hint: Some("Isolation cleared: widget is no longer visible.".into()), ..Self::default() };
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewportTransform {
    pub uv_min: [f64; 2],
    pub uv_max: [f64; 2],
    pub picture_rect: Rect,
    pub window_size: [f64; 2],
}

impl ViewportTransform {
    pub fn compute(picture_rect: Rect, window_size: [f64; 2], focus: &FocusState) -> Self {
        let mut tf = Self { picture_rect, window_size, uv_min: [0.; 2], uv_max: [1.; 2] };
        let rule = focus.rule();
        if !rule.controls_enabled || (!rule.center && rule.zoom == MIN_ZOOM) { return tf }
        let Some(rect) = focus.isolated_rect else { return tf };
        if !window_size.iter().all(|n| n.is_finite() && *n > 0.) { return tf }
        let span = 1. / rule.zoom;
        for (axis, center) in [rect.x + rect.w * 0.5, rect.y + rect.h * 0.5].into_iter().enumerate() {
            let origin = center / window_size[axis] - span * 0.5;
            // Life-size centering can expose the background beyond the texture.
            tf.uv_min[axis] = if rule.zoom == MIN_ZOOM { origin } else { origin.clamp(0., 1. - span) };
            tf.uv_max[axis] = tf.uv_min[axis] + span;
        }
        tf
    }

    fn origin_and_span(&self) -> (DVec2, DVec2) {
        let size = dvec2(self.window_size[0], self.window_size[1]);
        let min = dvec2(self.uv_min[0], self.uv_min[1]);
        let max = dvec2(self.uv_max[0], self.uv_max[1]);
        (min * size, (max - min) * size)
    }

    pub fn app_to_screen(&self, app_pt: DVec2) -> DVec2 {
        let (origin, span) = self.origin_and_span();
        self.picture_rect.pos + (app_pt - origin) / span * self.picture_rect.size
    }

    pub fn screen_to_app(&self, screen_pt: DVec2) -> Option<DVec2> {
        let r = self.picture_rect;
        (r.size.x > 0. && r.size.y > 0. && r.contains(screen_pt)).then(|| self.inverse(screen_pt))
    }

    /// A captured pointer can finish its drag outside the picture.
    pub(crate) fn inverse(&self, screen_pt: DVec2) -> DVec2 {
        let (origin, span) = self.origin_and_span();
        origin + (screen_pt - self.picture_rect.pos) / self.picture_rect.size * span
    }

    pub fn app_rect_to_screen(&self, r: AppRect) -> Rect {
        let pos = self.app_to_screen(dvec2(r.x, r.y));
        let end = self.app_to_screen(dvec2(r.x + r.w, r.y + r.h));
        let picture = self.picture_rect;
        Rect { pos, size: end - pos }.clip((picture.pos, picture.pos + picture.size))
    }

    /// The part of the texture inside the viewport, and its corresponding destination.
    pub(crate) fn texture_crop(&self) -> (Rect, Rect) {
        let start = dvec2(self.uv_min[0].clamp(0., 1.), self.uv_min[1].clamp(0., 1.));
        let end = dvec2(self.uv_max[0].clamp(0., 1.), self.uv_max[1].clamp(0., 1.));
        let window = dvec2(self.window_size[0], self.window_size[1]);
        let pos = self.app_to_screen(start * window);
        (Rect { pos: start, size: end - start }, Rect { pos, size: self.app_to_screen(end * window) - pos })
    }

    pub fn scrim_bands(&self, isolated: AppRect) -> [Rect; 4] {
        let hole = self.app_rect_to_screen(isolated);
        let p = self.picture_rect;
        let end = p.pos + p.size;
        let far = hole.pos + hole.size;
        [
            Rect { pos: p.pos, size: dvec2(p.size.x, hole.pos.y - p.pos.y) },
            Rect { pos: dvec2(p.pos.x, far.y), size: dvec2(p.size.x, end.y - far.y) },
            Rect { pos: dvec2(p.pos.x, hole.pos.y), size: dvec2(hole.pos.x - p.pos.x, hole.size.y) },
            Rect { pos: dvec2(far.x, hole.pos.y), size: dvec2(end.x - far.x, hole.size.y) },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: AppRect = AppRect { window: 2, x: 76., y: 41., w: 24., h: 12. };

    #[test]
    fn focus_controls_inert_without_isolation() {
        let scroll = dvec2(0., 32.);
        let mods = KeyModifiers::default();
        assert_eq!(wheel_zoom(scroll, false, &mods), None, "two-finger scrolling pans");
        let notch = wheel_zoom(scroll, true, &mods).expect("a vertical wheel notch zooms");
        assert!((notch - (-32.0_f64 * 0.005).exp()).abs() < 1e-12);
        assert_eq!(zoom_label(2. * notch * (1. / notch)), "2.0×");
        assert_eq!(zoom_label(2. - 1e-9), "2.0×");
        assert_eq!(zoom_label(1.25), "1.25×");
        let command = KeyModifiers { logo: true, ..mods };
        assert!(wheel_zoom(scroll, false, &command).is_some(), "Command-scroll zooms on a trackpad");

        let mut focus = FocusState::default();
        focus.set_center(true);
        focus.set_zoom(2.5);
        focus.scale_zoom(2.);
        assert_eq!(focus.rule(), ViewFocusRule { controls_enabled: false, center: false, zoom: 1. });

        focus.toggle_isolate(Some(("card", Some(TARGET))));
        focus.set_zoom(2.);
        focus.scale_zoom(notch);
        assert_eq!(focus.zoom, 2. * notch);
        focus.scale_zoom(4.);
        assert_eq!(focus.zoom, MAX_ZOOM);
        focus.scale_zoom(0.1);
        assert_eq!(focus.zoom, MIN_ZOOM);
    }

    #[test]
    fn focus_release_resets_center_and_zoom() {
        let mut focus = FocusState::default();
        focus.toggle_isolate(Some(("card", Some(TARGET))));
        focus.set_center(true);
        focus.set_zoom(3.);
        assert_eq!(focus.rule(), ViewFocusRule { controls_enabled: true, center: true, zoom: 3. });
        focus.toggle_isolate(None);
        assert_eq!(focus, FocusState::default());
    }

    #[test]
    fn focus_settle_missing_clears_isolation_with_hint() {
        let mut focus = FocusState::default();
        focus.toggle_isolate(Some(("card", Some(TARGET))));
        focus.set_center(true);
        focus.step_zoom(8.);
        assert_eq!(focus.zoom, MAX_ZOOM);
        assert!(!focus.settle(TargetPresence::Pending));
        assert_eq!(focus.isolated_rect, Some(TARGET));
        assert!(focus.settle(TargetPresence::Missing));
        assert_eq!(focus.hint.take().as_deref(), Some("Isolation cleared: widget is no longer visible."));
        assert_eq!(focus, FocusState::default());
    }

    #[test]
    fn viewport_transform_roundtrips_pointer_across_zoom_and_center() {
        let picture = Rect { pos: dvec2(17., 64.), size: dvec2(470., 230.) };
        let area = |r: Rect| r.size.x * r.size.y;
        let mut focus = FocusState::default();
        focus.toggle_isolate(Some(("card", Some(TARGET))));
        for zoom in [1., 1.5, 2.5, 4.] {
            for center in [false, true] {
                focus.set_zoom(zoom);
                focus.set_center(center);
                let tf = ViewportTransform::compute(picture, [235., 115.], &focus);
                for point in [dvec2(76.25, 41.5), dvec2(98.75, 52.)] {
                    let restored = tf.screen_to_app(tf.app_to_screen(point)).unwrap();
                    assert!((restored - point).length() < 1e-9, "roundtrip at {zoom}× center={center}");
                }
                if center {
                    assert!((tf.app_to_screen(dvec2(88., 47.)) - picture.center()).length() < 1e-9);
                }
                assert!((tf.uv_max[0] - tf.uv_min[0] - 1. / zoom).abs() < 1e-9);
                assert!(tf.screen_to_app(picture.pos - dvec2(0.1, 0.)).is_none());
                let (uv, dest) = tf.texture_crop();
                let far = uv.pos + uv.size;
                assert!(uv.pos.x >= 0. && uv.pos.y >= 0. && far.x <= 1. && far.y <= 1.);
                let window = dvec2(235., 115.);
                assert!((tf.app_to_screen(uv.pos * window) - dest.pos).length() < 1e-9);
                assert!((tf.app_to_screen(far * window) - (dest.pos + dest.size)).length() < 1e-9);
                if center && zoom == 1. {
                    assert!(dest.pos.x > picture.pos.x && dest.size.x < picture.size.x, "pan leaves blank space beyond the source");
                }
                let hole = tf.app_rect_to_screen(TARGET);
                let bands = tf.scrim_bands(TARGET);
                for (i, b) in bands.iter().enumerate() {
                    assert!(b.is_inside_of(picture) && !b.intersects(hole));
                    assert!(bands[..i].iter().all(|other| !other.intersects(*b)));
                }
                assert!((bands.into_iter().map(area).sum::<f64>() + area(hole) - area(picture)).abs() < 1e-9);
            }
        }
    }
}
