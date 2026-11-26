//! The marker's marks between the view and the model worker: a stroke goes out as an `annotate`
//! call with the camera it was drawn with, and the answer (what it covers, its outline on the
//! model) comes back to the view. The worker keeps the marks the AI reads (`fab.annotations`);
//! the view keeps what it draws.

use super::*;
use fab::tools::annotate::{self, Anchor, MarkAction, Resolved};
use makepad_model::json::{self, Value};

/// The camera `fab.render` answers cameras as (JSON): a view `w` by `h` pixels.
pub fn camera_json(camera: fab::api::Camera, w: f64, h: f64) -> Value {
    let c = annotate::fab_camera(camera, Rect { pos: dvec2(0.0, 0.0), size: dvec2(w, h) });
    let v3 = |v: [f64; 3]| Value::Arr(v.iter().map(|x| Value::F64(*x)).collect());
    let mut fields = vec![("px", Value::Int(w.round() as i64)), ("py", Value::Int(h.round() as i64)), ("eye", v3(c.eye)), ("forward", v3(c.forward)), ("right", v3(c.right)), ("up", v3(c.up))];
    match (c.fov, c.across) {
        (Some(fov), _) => fields.push(("fov", Value::F64(fov))),
        (_, Some(across)) => fields.push(("across", Value::F64(across))),
        _ => {}
    }
    json::obj(fields)
}

/// The worker's answer to an `annotate add`, as the view keeps it: each anchor in its solid's
/// model frame as the view's parts have it now (the mark then moves with the part).
pub fn resolved(state: &fab::api::AppState, ok: bool, text: &str) -> Result<Resolved, String> {
    if !ok {
        return Err(text.to_string());
    }
    let answer = json::parse(text.as_bytes()).map_err(|e| format!("the answer does not parse ({e})"))?;
    let number = answer.get("number").and_then(Value::as_i64).unwrap_or(0) as u32;
    let kind = answer.get("kind").and_then(Value::as_str).unwrap_or("open stroke").to_string();
    let covers = answer
        .get("covers")
        .and_then(Value::as_arr)
        .unwrap_or(&[])
        .iter()
        .filter_map(|c| {
            let share = match c.get("share")? {
                Value::F64(x) => *x,
                Value::Int(i) => *i as f64,
                _ => return None,
            };
            Some((c.get("solid")?.as_str()?.to_string(), c.get("node").and_then(Value::as_str).map(str::to_string), share))
        })
        .collect();
    let mut outline = Vec::new();
    for a in answer.get("outline").and_then(Value::as_arr).unwrap_or(&[]) {
        let Some(p) = a.get("plate").and_then(Value::as_arr).filter(|p| p.len() == 3) else { continue };
        let num = |v: &Value| match v {
            Value::F64(x) => *x,
            Value::Int(i) => *i as f64,
            _ => 0.0,
        };
        let plate = [num(&p[0]), num(&p[1]), num(&p[2])];
        let solid = a.get("solid").and_then(Value::as_str).map(str::to_string);
        match solid.as_ref().and_then(|s| state.plate.parts.iter().find(|part| &part.object == s)) {
            // the part as it stands: the mark keeps its place on the part
            Some(part) => outline.push(Anchor { solid, p: annotate::apply(&part.model_from_printer, plate) }),
            None => outline.push(Anchor { solid: None, p: plate }),
        }
    }
    Ok(Resolved { number, kind, covers, outline })
}

impl App {
    /// The marks leave the view (the chip's clear, a new document): the worker's unsent ones go too.
    pub(super) fn clear_marks(&mut self, cx: &mut Cx) {
        annotate::clear();
        self.forget_marks(true);
        fab::ui::redraw(cx, &[ids!(tool_overlay)]);
    }

    /// The marker's Cmd/Ctrl+Z: the last stroke goes.
    pub(super) fn pop_mark(&mut self, cx: &mut Cx) {
        if annotate::pop() {
            if let Some(host) = &mut self.host {
                host.call_as_person("annotate", json::obj(vec![("action", json::s("pop"))]));
            }
            fab::ui::redraw(cx, &[ids!(tool_overlay)]);
        }
    }

    /// The worker's marks and the text that went with them go (the view's own are the caller's).
    pub(super) fn forget_marks(&mut self, person: bool) {
        self.mark_text = None;
        self.mark_read = None;
        self.mark_calls.clear();
        if let Some(host) = &mut self.host {
            let args = json::obj(vec![("action", json::s("clear"))]);
            if person { host.call_as_person("annotate", args); } else { host.call("annotate", args); }
        }
    }

    /// The texture of the 3D view, to read back for the marks' picture.
    pub(super) fn mark_capture(&mut self, cx: &mut Cx) -> Option<Texture> {
        let viewport = fab::ui::viewport_of(cx, &self.ui, self.state.view.lead_plate());
        let viewport = viewport.borrow::<fab::viewport::FabViewport>()?;
        let (texture, size) = viewport.view_texture(cx);
        self.mark_view = [size.x, size.y];
        Some(texture)
    }

    /// The view's picture is back: the marks are painted in it as the view draws them, and the
    /// worker is asked what the marks are in that picture's pixels (its own camera).
    pub(super) fn mark_picture(&mut self, _cx: &mut Cx, pixels: Option<&mut shell_chat::picture::Pixels>) {
        let [vw, vh] = self.mark_view;
        let (w, h) = match &pixels {
            Some(p) => (p.width as f64, p.height as f64),
            None => (vw, vh),
        };
        if let Some(p) = pixels {
            let proj = fab::api::ViewProjector::new(*self.state.view.camera(self.state.view.lead_plate()), Rect { pos: dvec2(0.0, 0.0), size: dvec2(vw, vh) });
            let lines = annotate::lines(&self.state, &proj, self.state.view.lead_plate());
            annotate::paint(&mut p.rgba, p.width, p.height, &lines, w / vw.max(1.0));
        }
        let Some(host) = &mut self.host else { return };
        let args = json::obj(vec![("camera", camera_json(*self.state.view.camera(self.state.view.lead_plate()), w, h)), ("compact", Value::Bool(true))]);
        self.mark_read = Some((host.call("annotations", args), None));
    }

    /// The marks went with the person's line `line`: the worker keeps them readable; drawing again
    /// starts afresh.
    pub(super) fn marks_sent(&mut self, _cx: &mut Cx, line: usize) {
        fab::tools::session::with(|s| s.marker.sent = true);
        if let Some(host) = &mut self.host {
            host.call("annotate", json::obj(vec![("action", json::s("sent")), ("line", Value::Int(line as i64))]));
        }
    }

    pub(super) fn mark_action(&mut self, cx: &mut Cx, action: &MarkAction) {
        match action {
            MarkAction::Add { id, camera, rect, line } => {
                let Some(host) = &mut self.host else { return };
                let args = json::obj(vec![
                    ("action", json::s("add")),
                    ("camera", camera_json(*camera, rect[0], rect[1])),
                    ("line", Value::Arr(line.iter().map(|p| Value::Arr(vec![Value::F64(p[0]), Value::F64(p[1])])).collect())),
                ]);
                let call = host.call_as_person("annotate", args);
                self.mark_calls.push((call, *id));
            }
            MarkAction::Pop => self.pop_mark(cx),
            MarkAction::Clear => self.forget_marks(true),
            MarkAction::Leave => {
                self.state.ui.marker = false;
            }
        }
        fab::ui::redraw(cx, &[ids!(tool_overlay)]);
    }
}
