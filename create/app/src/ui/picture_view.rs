use crate::board::gesture::{scroll_nav, ScrollNav};
use makepad_widgets::{event::{DigitId, ScrollPhase}, *};
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle};
use std::path::PathBuf;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    let PictureBase = #(PictureView::register_widget(vm))
    mod.widgets.PictureView = PictureBase{
        width: Fill height: Fill
        image: Image{fit: ImageFit.Stretch}
        failure: BodyNote{width: Fill height: Fill align: Center selectable: false
            text: "This picture can't be shown"
        }
    }
}

/// The natural content size of a picture at one device pixel per picture pixel.
pub fn exact_size_in_points(pixels: DVec2, dpi: f64) -> DVec2 {
    pixels / dpi
}

#[derive(Clone, Copy)]
struct Geometry {
    view: Rect,
    pixels: DVec2,
    dpi: f64,
}

impl Geometry {
    fn fit_zoom(self) -> f64 {
        let exact = exact_size_in_points(self.pixels, self.dpi);
        (self.view.size.x / exact.x).min(self.view.size.y / exact.y)
    }

    fn limit_zoom(self, zoom: f64) -> f64 {
        zoom.clamp(self.fit_zoom().min(1.) * 0.25, 32.)
    }
}

#[derive(Clone, Copy)]
struct Camera {
    zoom: f64,
    // Picture pixels at the centre of the view, independent of window size and DPI.
    center: DVec2,
    fitting: bool,
}

impl Camera {
    fn exact(pixels: DVec2) -> Self {
        Self { zoom: 1., center: pixels * 0.5, fitting: false }
    }

    fn fit(&mut self, g: Geometry) {
        self.fitting = true;
        self.zoom = g.limit_zoom(g.fit_zoom());
        self.center = g.pixels * 0.5;
    }

    fn clamp(&mut self, g: Geometry) {
        self.zoom = g.limit_zoom(self.zoom);
        let half = g.view.size * (0.5 * g.dpi / self.zoom);
        let axis = |center: f64, pixels: f64, half: f64| {
            if pixels <= 2. * half { pixels * 0.5 } else { center.clamp(half, pixels - half) }
        };
        self.center = dvec2(axis(self.center.x, g.pixels.x, half.x), axis(self.center.y, g.pixels.y, half.y));
    }

    fn size(self, g: Geometry) -> DVec2 {
        exact_size_in_points(g.pixels, g.dpi) * self.zoom
    }

    fn can_pan(self, g: Geometry) -> bool {
        let size = self.size(g);
        size.x > g.view.size.x || size.y > g.view.size.y
    }

    fn drawn_rect(self, g: Geometry) -> Rect {
        let scale = self.zoom / g.dpi;
        let mut pos = g.view.center() - self.center * scale;
        let size = self.size(g);
        if self.zoom == 1. {
            let snap = |origin: f64, start: f64, view: f64, picture: f64| {
                let pixel = (origin * g.dpi).round();
                // Near a fractional viewport edge, round toward coverage if rounding out
                // would uncover a band. A smaller picture stays as close to centred as possible.
                let lo = ((start + view - picture) * g.dpi).ceil();
                let hi = (start * g.dpi).floor();
                if lo <= hi { pixel.clamp(lo, hi) / g.dpi } else { pixel / g.dpi }
            };
            pos = dvec2(snap(pos.x, g.view.pos.x, g.view.size.x, size.x),
                        snap(pos.y, g.view.pos.y, g.view.size.y, size.y));
        }
        Rect { pos, size }
    }

    fn zoom_about(&mut self, g: Geometry, at: DVec2, zoom: f64) {
        let pixel = (at - self.drawn_rect(g).pos) * (g.dpi / self.zoom);
        self.fitting = false;
        self.zoom = g.limit_zoom(zoom);
        self.center = pixel + (g.view.center() - at) * (g.dpi / self.zoom);
        self.clamp(g);
    }

    fn pan(&mut self, g: Geometry, delta: DVec2) {
        self.center -= delta * (g.dpi / self.zoom);
        self.clamp(g);
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct PictureView {
    #[uid] uid: WidgetUid,
    #[walk] walk: Walk,
    #[redraw] #[rust] area: Area,
    #[live] image: Image,
    #[live] failure: WidgetRef,
    #[rust] loading: Option<TaskHandle<Option<ImageBuffer>>>,
    #[rust] failed: bool,
    #[rust] picture: Option<(Geometry, Camera)>,
    #[rust] drag: Option<(DigitId, DVec2)>,
    #[rust] focus_on_draw: bool,
}

impl PictureView {
    pub fn open(&mut self, cx: &mut Cx, path: PathBuf) {
        self.clear(cx);
        self.focus_on_draw = true;
        // Completion wakes the UI. Only the current file's task can install a texture.
        match cx.task_pool().submit_named(Class::Visible, "load chat picture", move || {
            let bytes = std::fs::read(path).ok()?;
            decode_image_from_data(&bytes).ok()
        }) {
            Ok(task) => self.loading = Some(task),
            Err(_) => self.failed = true,
        }
    }

    /// The open picture's size in pixels, once it has loaded.
    pub fn picture_pixels(&self) -> Option<DVec2> {
        self.picture.map(|(g, _)| g.pixels)
    }

    pub fn clear(&mut self, cx: &mut Cx) {
        if let Some(task) = self.loading.take() { task.cancel(); }
        self.image.draw_bg.draw_vars.empty_texture(0);
        if let Some(texture) = self.image.get_texture(0) { texture.release(cx); }
        self.image.set_texture(None, 0);
        self.failed = false;
        self.picture = None;
        self.drag = None;
        self.focus_on_draw = false;
        self.redraw(cx);
    }

    fn finish_load(&mut self, cx: &mut Cx) {
        let Some(result) = self.loading.as_mut().and_then(|task| task.try_take()) else { return };
        self.loading = None;
        if let Ok(Some(image)) = result {
            self.image.set_texture(Some(image.into_new_texture(cx)), 0);
            self.picture = self.image.size_in_pixels(cx).map(|(w, h)| {
                let pixels = dvec2(w as f64, h as f64);
                (Geometry { pixels, view: Rect::default(), dpi: 1. }, Camera::exact(pixels))
            });
        }
        self.failed = self.picture.is_none();
        self.redraw(cx);
    }
}

impl Widget for PictureView {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, Layout::flow_overlay());
        let view = cx.turtle().rect();
        if let Some((g, camera)) = self.picture.as_mut() {
            let dpi = cx.current_dpi_factor();
            if view.size.x > 0. && view.size.y > 0. {
                if g.view != view || g.dpi != dpi {
                    g.view = view;
                    g.dpi = dpi;
                    if camera.fitting { camera.fit(*g); } else { camera.clamp(*g); }
                }
                // At or above exact pixels, magnify each picture pixel as a single colour.
                self.image.draw_bg.sample_mode = if camera.zoom >= 1. { -1. } else { 0. };
                let _ = self.image.draw_walk(cx, scope, Walk::abs_rect(camera.drawn_rect(*g)));
            }
        } else if self.failed {
            self.failure.draw_all(cx, scope);
        }
        cx.end_turtle_with_area(&mut self.area);
        if self.focus_on_draw {
            cx.set_key_focus(self.area);
            self.focus_on_draw = false;
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        match event {
            Event::Signal => self.finish_load(cx),
            Event::WindowGotFocus(_) => {
                if self.area.is_valid(cx) { cx.set_key_focus(self.area); }
                else { self.focus_on_draw = true; }
            }
            Event::WindowLostFocus(_) => { self.drag = None; self.focus_on_draw = false; }
            // A pinch may be broadcast to every window in the root.
            Event::Pinch(e) if cx.get_window_id_of(&self.area) != Some(e.window_id) => return,
            _ => {}
        }
        let Some((g, camera)) = &mut self.picture else { return };
        let g = *g;
        if g.view.size.x <= 0. || g.view.size.y <= 0. { return }
        self.image.handle_event(cx, event, scope);
        match event.hits(cx, self.area) {
            Hit::KeyDown(k) => match k.key_code {
                KeyCode::Key0 | KeyCode::Numpad0 => camera.fit(g),
                KeyCode::Key1 | KeyCode::Numpad1 => camera.zoom_about(g, g.view.center(), 1.),
                KeyCode::Equals | KeyCode::NumpadAdd => camera.zoom_about(g, g.view.center(), camera.zoom * 2.),
                KeyCode::Minus | KeyCode::NumpadSubtract => camera.zoom_about(g, g.view.center(), camera.zoom * 0.5),
                _ => return,
            },
            Hit::FingerDown(e) if e.is_primary_hit() => {
                cx.set_key_focus_from_user(self.area);
                if camera.can_pan(g) {
                    cx.claim_finger_drag(e.digit_id);
                    self.drag = Some((e.digit_id, e.abs));
                    cx.set_cursor(MouseCursor::Grabbing);
                }
                return
            }
            Hit::FingerMove(e) => {
                let Some((_, last)) = self.drag.as_mut().filter(|(digit, _)| *digit == e.digit_id) else { return };
                camera.pan(g, e.abs - *last);
                *last = e.abs;
                cx.set_cursor(MouseCursor::Grabbing);
            }
            Hit::FingerUp(e) if e.is_primary_hit() => {
                self.drag = None;
                if e.tap_count == 2 && e.was_tap() && e.is_over {
                    if camera.fitting { camera.zoom_about(g, e.abs, 1.); } else { camera.fit(g); }
                }
                cx.set_cursor(if e.is_over && camera.can_pan(g) { MouseCursor::Grab } else { MouseCursor::Default });
            }
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                cx.set_cursor(if self.drag.is_some() { MouseCursor::Grabbing } else if camera.can_pan(g) { MouseCursor::Grab } else { MouseCursor::Default });
                return
            }
            Hit::FingerScroll(s) => {
                let Event::Scroll(e) = event else { return };
                if s.modifiers.control || s.modifiers.logo {
                    let wheel = e.is_mouse || e.phase == ScrollPhase::None;
                    match scroll_nav(s.scroll, wheel, &s.modifiers) {
                        ScrollNav::Zoom(by) => camera.zoom_about(g, s.abs, camera.zoom * by),
                        ScrollNav::Pan(by) => camera.pan(g, by),
                    }
                } else { camera.pan(g, -s.scroll); }
                e.handled_x.set(true);
                e.handled_y.set(true);
            }
            Hit::FingerPinch(p) => camera.zoom_about(g, p.abs, camera.zoom * p.scale),
            _ => return,
        }
        self.redraw(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_pixels_pan_zoom_fit_and_resize() {
        let g = Geometry { pixels: dvec2(1000., 600.), view: rect(0., 0., 400., 300.), dpi: 2. };
        let mut cam = Camera::exact(g.pixels);
        let r = cam.drawn_rect(g);
        assert_eq!(r.size, dvec2(500., 300.), "one picture pixel occupies one device pixel");
        assert_eq!(r.center(), g.view.center(), "opening shows the centre of the picture");
        assert!(g.view.is_inside_of(r), "the wide picture covers the viewport horizontally");

        cam.pan(g, dvec2(0.31, 0.));
        let origin = cam.drawn_rect(g).pos;
        assert_eq!(origin, dvec2(-49.5, 0.), "the exact-pixel origin snaps to the nearest half point");
        let pointer = dvec2(80., 110.);
        let pixel = (pointer - origin) * g.dpi;
        cam.zoom_about(g, pointer, 2.);
        assert_eq!((pointer - cam.drawn_rect(g).pos) * (g.dpi / cam.zoom), pixel, "zoom keeps the pointed-at pixel fixed");
        let center = cam.center;
        let smaller = Geometry { view: rect(0., 0., 300., 200.), ..g };
        cam.clamp(smaller);
        assert_eq!(cam.center, center, "resizing keeps the same picture point at the centre");
        for delta in [dvec2(2000., -2000.), dvec2(-2000., 2000.)] {
            cam.pan(g, delta);
            assert!(g.view.is_inside_of(cam.drawn_rect(g)), "clamping at either extreme leaves no empty band");
        }
        cam.zoom_about(g, pointer, 0.);
        assert_eq!(cam.zoom, 0.2, "the lower limit is a quarter of fit for a large picture");
        cam.zoom_about(g, pointer, 64.);
        assert_eq!(cam.zoom, 32., "magnification stops at 32 device pixels per picture pixel");
        cam.fit(g);
        assert_eq!(cam.drawn_rect(g), rect(0., 30., 400., 240.), "fit centres the entire picture and fills the limiting axis");
        cam.fit(smaller);
        assert_eq!(cam.drawn_rect(smaller), rect(0., 10., 300., 180.), "fit follows the resized viewport");
    }
}
