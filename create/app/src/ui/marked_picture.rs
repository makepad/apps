//! The finding still and its annotations share the same fitted picture rectangle.
use crate::marks::{self, Head, Mark, DEFAULT_WIDTH, UNIT};
use makepad_canvas::{segment_bounds, DrawCanvasSegment};
use makepad_widgets::{event::DigitId, *};
use std::{path::{Path, PathBuf}, sync::Arc};

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*
    mod.widgets.MarkedPicture = #(MarkedPicture::register_widget(vm)){
        width: Fill height: Fill
        image: Image{fit: ImageFit.Stretch}
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MarkTool { #[default] Off, Pen, Arrow }

#[derive(Clone, Debug, Default)]
pub enum MarkedPictureAction {
    Add { width: i32, head: Head, pts: Vec<[i32; 2]> },
    #[default] None,
}

struct Drag {
    digit: DigitId,
    // Coordinates in the fitted picture at press time, so simplification is in screen pixels.
    size: DVec2,
    epsilon: f64,
    points: Vec<DVec2>,
}

impl Drag {
    fn sample(&mut self, rect: Rect, abs: DVec2, tool: MarkTool) {
        let local = (abs - rect.pos) / rect.size;
        let p = dvec2(local.x.clamp(0., 1.), local.y.clamp(0., 1.)) * self.size;
        if tool == MarkTool::Arrow && self.points.len() == 2 { self.points[1] = p; }
        else if self.points.last() != Some(&p) { self.points.push(p); }
    }

    fn finish(self, tool: MarkTool) -> MarkedPictureAction {
        let points = if tool == MarkTool::Arrow { self.points } else { marks::simplify(&self.points, self.epsilon) };
        let mut pts: Vec<_> = points.into_iter().map(|p| {
            let units = (p / self.size * UNIT as f64).round();
            [units.x as i32, units.y as i32]
        }).collect();
        pts.dedup();
        if pts.len() < 2 { return MarkedPictureAction::None }
        MarkedPictureAction::Add { width: DEFAULT_WIDTH,
            head: if tool == MarkTool::Arrow { Head::End } else { Head::None }, pts }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct MarkedPicture {
    #[uid] uid: WidgetUid,
    #[walk] walk: Walk,
    #[visible] #[live(true)] visible: bool,
    #[redraw] #[rust] area: Area,
    #[live] image: Image,
    #[live] segment: DrawCanvasSegment,
    #[rust] path: Option<PathBuf>,
    #[rust] fitted: bool,
    #[rust] marks: Arc<[(Mark, Vec4f)]>,
    #[rust] tool: MarkTool,
    #[rust] pub ink: Vec4f,
    #[rust] drag: Option<Drag>,
    #[cfg(test)] #[rust] drawn: usize,
}

impl MarkedPicture {
    pub fn set_picture(&mut self, cx: &mut Cx, path: Option<&Path>) {
        if self.path.as_deref() == path { return }
        self.drag = None;
        self.fitted = false;
        self.image.set_texture(None, 0);
        if let Some(path) = path { let _ = self.image.load_image_file_by_path_async(cx, path); }
        self.path = path.map(Path::to_owned);
        self.redraw(cx);
    }

    pub fn set_marks(&mut self, cx: &mut Cx, marks: Arc<[(Mark, Vec4f)]>) {
        if !Arc::ptr_eq(&self.marks, &marks) { self.marks = marks; self.redraw(cx); }
    }

    pub fn set_tool(&mut self, cx: &mut Cx, tool: MarkTool) {
        if self.tool == tool { return }
        self.tool = tool;
        self.drag = None;
        self.redraw(cx);
    }

    #[cfg(test)]
    pub fn segments_drawn(&self) -> usize { self.drawn }
}

fn draw_segment(cx: &mut Cx2d, draw: &mut DrawCanvasSegment, p0: DVec2, p1: DVec2, width: f64, arrow: bool) {
    draw.p0 = p0.into_vec2();
    draw.p1 = p1.into_vec2();
    draw.width = width as f32;
    draw.head = if arrow { 1. } else { 0. };
    draw.draw_abs(cx, segment_bounds(p0, p1, width, arrow));
}

impl Widget for MarkedPicture {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, Layout::flow_overlay());
        let view = cx.turtle().rect();
        #[cfg(test)] { self.drawn = 0; }
        if let Some((w, h)) = self.image.size_in_pixels(cx).filter(|&(w, h)| w > 0 && h > 0 && view.size.x > 0. && view.size.y > 0.) {
            self.fitted = true;
            let pixels = dvec2(w as f64, h as f64);
            let size = pixels * (view.size.x / pixels.x).min(view.size.y / pixels.y);
            let rect = Rect { pos: view.center() - size * 0.5, size };
            let _ = self.image.draw_walk(cx, scope, Walk::abs_rect(rect));
            cx.push_clip_rect(rect);
            let screen = |[x, y]: [i32; 2]| rect.pos + dvec2(x as f64, y as f64) * (size / UNIT as f64);
            for (mark, colour) in self.marks.iter() {
                self.segment.color = *colour;
                let width = (mark.width as f64 / UNIT as f64) * size.x;
                for (i, pair) in mark.pts.windows(2).enumerate() {
                    draw_segment(cx, &mut self.segment, screen(pair[0]), screen(pair[1]), width,
                        mark.head == Head::End && i + 2 == mark.pts.len());
                    #[cfg(test)] { self.drawn += 1; }
                }
            }
            if let Some(drag) = &self.drag {
                self.segment.color = self.ink;
                let scale = size / drag.size;
                let width = DEFAULT_WIDTH as f64 / UNIT as f64 * size.x;
                for pair in drag.points.windows(2) {
                    draw_segment(cx, &mut self.segment, rect.pos + pair[0] * scale, rect.pos + pair[1] * scale,
                        width, self.tool == MarkTool::Arrow);
                }
            }
            cx.pop_clip_rect();
        } else {
            self.fitted = false;
            // The image receives its async completion and invalidates this draw list.
            self.image.draw_all(cx, scope);
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.image.handle_event(cx, event, scope);
        if !self.visible || self.tool == MarkTool::Off || !self.fitted { return }
        let area = self.image.area();
        let rect = area.rect(cx);
        if rect.size.x <= 0. || rect.size.y <= 0. { return }
        match event.hits(cx, area) {
            Hit::FingerDown(e) if e.is_primary_hit() && self.drag.is_none() => {
                let mut drag = Drag { digit: e.digit_id, size: rect.size, epsilon: 0.75 / cx.get_dpi_factor_of(&area), points: Vec::new() };
                drag.sample(rect, e.abs, self.tool);
                self.drag = Some(drag);
                cx.claim_finger_drag(e.digit_id);
                cx.set_cursor(MouseCursor::Crosshair);
            }
            Hit::FingerMove(e) => {
                let Some(drag) = self.drag.as_mut().filter(|d| d.digit == e.digit_id) else { return };
                drag.sample(rect, e.abs, self.tool);
                cx.set_cursor(MouseCursor::Crosshair);
            }
            Hit::FingerUp(e) if self.drag.as_ref().is_some_and(|d| d.digit == e.digit_id) => {
                let mut drag = self.drag.take().unwrap();
                if !e.cancelled && !e.is_sweep {
                    drag.sample(rect, e.abs, self.tool);
                    if let add @ MarkedPictureAction::Add { .. } = drag.finish(self.tool) { cx.widget_action(self.uid, add); }
                }
            }
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => { cx.set_cursor(MouseCursor::Crosshair); return }
            _ => return,
        }
        self.redraw(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::{makepad_draw::cx_draw::CxDraw,
        makepad_platform::studio::{RemoteMouseDown, RemoteMouseUp, StudioToApp}};

    #[test]
    fn a_pen_drag_commits_one_mark_in_picture_units() {
        let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let widget = cx.with_vm(|vm| {
            let value = script_eval!(vm, { use mod.widgets.* MarkedPicture{} });
            WidgetRef::script_from_value(vm, value)
        });
        let size = dvec2(800., 800.);
        let pass = DrawPass::new(&mut cx);
        pass.set_size(&mut cx, size);
        let mut list = DrawList2d::new(&mut cx);
        let mut draw = |cx: &mut Cx| {
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, None);
            list.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_overlay());
            widget.draw_all(&mut frame, &mut Scope::empty());
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
        };
        let texture = Texture::new_with_format(&mut cx, TextureFormat::VecBGRAu8_32 {
            width: 400, height: 200, data: None, updated: TextureUpdated::Full });
        widget.borrow_mut::<MarkedPicture>().unwrap().image.set_texture(Some(texture), 0);
        draw(&mut cx);
        let rect = widget.borrow::<MarkedPicture>().unwrap().image.area().rect(&cx);
        assert_eq!(rect, makepad_widgets::rect(0., 200., 800., 400.), "the image is centred and letterboxed");
        let mut time = 0.;
        let mut drag = |cx: &mut Cx, tool, start: DVec2| {
            time += 1.;
            widget.borrow_mut::<MarkedPicture>().unwrap().set_tool(cx, tool);
            let end = rect.center();
            let window_id = WindowId(1, 1);
            let button = MouseButton::PRIMARY;
            cx.dispatch_studio_msg(StudioToApp::MouseDown(RemoteMouseDown { x: start.x, y: start.y, time,
                button_raw_bits: button.bits(), ..Default::default() }), window_id, DVec2::default());
            let event = Event::MouseDown(MouseDownEvent { abs: start, window_id, time, button,
                handled: Default::default(), modifiers: Default::default() });
            widget.handle_event(cx, &event, &mut Scope::empty());
            let via = dvec2(end.x, start.y);
            widget.handle_event(cx, &Event::MouseMove(MouseMoveEvent { abs: via, lock_delta: DVec2::default(),
                window_id, time: time + 0.1, modifiers: Default::default(), handled: Default::default() }), &mut Scope::empty());
            let up = Event::MouseUp(MouseUpEvent { abs: end, window_id, time: time + 0.2, button, modifiers: Default::default() });
            let actions = cx.capture_actions(|cx| widget.handle_event(cx, &up, &mut Scope::empty()));
            cx.dispatch_studio_msg(StudioToApp::MouseUp(RemoteMouseUp { x: end.x, y: end.y, time: time + 0.2,
                button_raw_bits: button.bits(), ..Default::default() }), window_id, DVec2::default());
            actions.iter().filter_map(|a| a.as_widget_action()?.action.downcast_ref::<MarkedPictureAction>().cloned()).collect::<Vec<_>>()
        };
        let pen = drag(&mut cx, MarkTool::Pen, rect.pos);
        let [MarkedPictureAction::Add { width, head, pts }] = pen.as_slice() else { panic!("one pen drag, one action: {pen:?}") };
        assert_eq!((*width, *head), (DEFAULT_WIDTH, Head::None));
        assert_eq!(pts.as_slice(), [[0, 0], [UNIT / 2, 0], [UNIT / 2, UNIT / 2]]);
        assert!(drag(&mut cx, MarkTool::Pen, dvec2(0., 100.)).is_empty(), "a press in the letterbox passes through");
        assert!(drag(&mut cx, MarkTool::Off, rect.pos).is_empty(), "tool off passes through");
        let arrow = drag(&mut cx, MarkTool::Arrow, rect.pos);
        assert!(matches!(arrow.as_slice(), [MarkedPictureAction::Add { head: Head::End, pts, .. }]
            if pts.as_slice() == [[0, 0], [UNIT / 2, UNIT / 2]]), "the arrow uses only its endpoints");
        let mark = Mark { id: [1; 32], author: [2; 65], width: *width, head: *head, pts: pts.clone().into() };
        widget.borrow_mut::<MarkedPicture>().unwrap().set_marks(&mut cx, vec![(mark, vec4(1., 0., 0., 1.))].into());
        draw(&mut cx);
        assert_eq!(widget.borrow::<MarkedPicture>().unwrap().segments_drawn(), pts.len() - 1);
    }

    #[test]
    fn a_malformed_mark_from_a_peer_is_skipped() {
        use crate::model::tests::{group_town, record_call_event};
        use makepad_town_core::{entity::{field, kind}, event::{Content, Key, Value}};
        let (_homes, mut members) = group_town("malformed-mark", 2);
        let project = Some(members[0].project().id);
        let entity = |kind, fields| Content::Entity { kind: Key::Number(kind), project, derive: None, fields, copied_from: vec![] };
        let issue = record_call_event(&mut members, 0, entity(kind::ISSUE, vec![]));
        let stroke = |pts: &str| entity(kind::STROKE, vec![(field::PARENT, Value::Id(issue)), (Key::Name("pts".into()), Value::Literal(pts.into()))]);
        record_call_event(&mut members, 1, stroke("[1 2 3]"));
        let good = record_call_event(&mut members, 1, stroke("[0 0 1 2]"));
        let marks = marks::marks_on(&members[0].state, issue);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].id, good);
        assert_eq!(marks[0].pts.as_ref(), [[0, 0], [1, 2]]);
    }
}
