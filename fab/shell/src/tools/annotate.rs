//! The marker: a left drag on the view draws a mark for the AI while the chat's Annotate is on.
//!
//! A drag is drawn at once as a raw line (the overlay, in the frame of the mouse); when the
//! button comes up the stroke and the camera it was drawn with go to the model worker
//! (`MarkAction::Add`), which resolves it with the rays `fab.pick` uses and answers what it
//! covers and where it is on the model. Nothing here waits for that answer, touches the
//! program, or counts as an edit: marks are session state like a half-placed measurement.
//!
//! A resolved mark keeps its outline as model-frame points of the solid it was drawn on, so
//! it turns with the camera and moves with its part ([`Mark::plate_points`]).
//!
//! Navigation stays as it is: only an unmodified primary drag draws; Shift+drag orbits,
//! the middle button and the secondary button pan, the wheel zooms.

use crate::api::*;
use crate::api::PaneId;
use crate::tools::session;
use makepad_widgets_core::*;

/// The marker's colour: one bright hue that reads on every render mode.
pub const COLOR: Vec4f = Vec4f { x: 1.0, y: 0.16, z: 0.55, w: 1.0 };
/// Line width on screen, points.
pub const WIDTH: f64 = 3.5;
/// A point nearer than this (points) to the last one adds nothing to a drag.
const MIN_STEP: f64 = 1.5;

/// The marker is on: the chat's Annotate button.
pub fn active(state: &AppState) -> bool {
    state.ui.marker
}

/// What the marker asks of the app.
#[derive(Clone, Debug, PartialEq)]
pub enum MarkAction {
    /// A stroke ended: resolve it on the model worker. `line` is in the view's own points
    /// (from its top left corner), `rect` the view's size, `camera` what it was drawn with.
    Add { id: u64, camera: Camera, rect: [f64; 2], line: Vec<[f64; 2]> },
    /// The last stroke was taken back.
    Pop,
    /// All marks went.
    Clear,
    /// Esc: the marker is off, the marks stay.
    Leave,
}

/// What the worker said of a stroke.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub number: u32,
    pub kind: String,
    /// What the mark covers: solid, node, share of its hits.
    pub covers: Vec<(String, Option<String>, f64)>,
    pub outline: Vec<Anchor>,
}

/// A point of a mark: on a solid (its model frame) or, for a mark on nothing, on the plate.
#[derive(Clone, Debug, PartialEq)]
pub struct Anchor {
    pub solid: Option<String>,
    /// The solid's model frame, or plate mm.
    pub p: [f64; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub enum MarkState {
    /// The worker has not answered: the raw line shows.
    Resolving,
    Ready(Resolved),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Mark {
    pub id: u64,
    /// The raw drag, in the view's points.
    pub line: Vec<DVec2>,
    pub camera: Camera,
    pub rect: [f64; 2],
    pub state: MarkState,
}

/// The marks on the view and the stroke being drawn.
#[derive(Clone, Debug, Default)]
pub struct Marker {
    pub marks: Vec<Mark>,
    pub drawing: Option<Mark>,
    pub next_id: u64,
    /// The marks were sent with a line: drawing again starts afresh.
    pub sent: bool,
}

impl Marker {
    pub fn resolving(&self) -> usize {
        self.marks.iter().filter(|m| m.state == MarkState::Resolving).count()
    }
}

/// The plate-frame (printer mm) point of an anchor now, given each part's `model_from_printer` now.
pub fn plate_now(anchor: &Anchor, state: &AppState) -> Option<[f64; 3]> {
    match &anchor.solid {
        None => Some(anchor.p),
        Some(solid) => {
            let part = state.plate.parts.iter().find(|p| &p.object == solid)?;
            Some(apply(&invert(&part.model_from_printer), anchor.p))
        }
    }
}

pub fn apply(m: &[[f64; 4]; 3], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3])
}

/// The inverse of an affine map (its linear part invertible).
pub fn invert(m: &[[f64; 4]; 3]) -> [[f64; 4]; 3] {
    let a = [[m[0][0], m[0][1], m[0][2]], [m[1][0], m[1][1], m[1][2]], [m[2][0], m[2][1], m[2][2]]];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0]) + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    let cof = |r: usize, c: usize| {
        let (r1, r2, c1, c2) = ((r + 1) % 3, (r + 2) % 3, (c + 1) % 3, (c + 2) % 3);
        a[r1][c1] * a[r2][c2] - a[r1][c2] * a[r2][c1]
    };
    let mut out = [[0.0; 4]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = cof(c, r) / det;
        }
    }
    for r in 0..3 {
        out[r][3] = -(out[r][0] * m[0][3] + out[r][1] * m[1][3] + out[r][2] * m[2][3]);
    }
    out
}

/// The camera as `fab.render`'s answers tell one: eye and axes in plate mm, `fov` degrees (vertical)
/// or an orthographic `across` (mm), `w` by `h` pixels. `right` is as long as the aspect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FabCamera {
    pub eye: [f64; 3],
    pub forward: [f64; 3],
    pub right: [f64; 3],
    pub up: [f64; 3],
    pub fov: Option<f64>,
    pub across: Option<f64>,
}

/// `camera` seen through a view of `rect`'s shape, read off the rays the view picks with, so a
/// pixel of the picture is the ray the viewport itself casts there.
pub fn fab_camera(camera: Camera, rect: Rect) -> FabCamera {
    let proj = ViewProjector::new(camera, rect);
    let at = |nx: f64, ny: f64| proj.ray(dvec2(rect.pos.x + (nx * 0.5 + 0.5) * rect.size.x, rect.pos.y + (0.5 - ny * 0.5) * rect.size.y));
    let mm = |v: Vec3f| [v.x as f64 * 1000.0, v.y as f64 * 1000.0, v.z as f64 * 1000.0];
    let unit = |v: Vec3f| {
        let v = v.normalize();
        [v.x as f64, v.y as f64, v.z as f64]
    };
    let centre = at(0.0, 0.0);
    let forward = unit(centre.dir);
    if camera.ortho {
        let (o0, ox, oy) = (centre.origin, at(1.0, 0.0).origin, at(0.0, 1.0).origin);
        let (rx, ry) = (ox - o0, oy - o0);
        let across = ry.length() as f64 * 2000.0;
        let span = across * 0.5 / 1000.0;
        let scale = |v: Vec3f, by: f64| [v.x as f64 / by, v.y as f64 / by, v.z as f64 / by];
        return FabCamera { eye: mm(o0), forward, right: scale(rx, span), up: scale(ry, span), fov: None, across: Some(across) };
    }
    let t = (camera.fov_y_deg as f64).to_radians().mul_add(0.5, 0.0).tan();
    let dir = |nx: f64, ny: f64| {
        let d = at(nx, ny).dir.normalize();
        let along = (d.x * centre.dir.normalize().x + d.y * centre.dir.normalize().y + d.z * centre.dir.normalize().z) as f64;
        [d.x as f64 / along, d.y as f64 / along, d.z as f64 / along]
    };
    let (dx, dy) = (dir(1.0, 0.0), dir(0.0, 1.0));
    let side = |d: [f64; 3]| std::array::from_fn(|k| (d[k] - forward[k]) / t);
    FabCamera { eye: mm(camera.eye), forward, right: side(dx), up: side(dy), fov: Some(camera.fov_y_deg as f64), across: None }
}

/// Chaikin's corner cutting twice: a hand-drawn line without the jitter of a mouse.
pub fn smooth(line: &[DVec2]) -> Vec<DVec2> {
    let mut points = line.to_vec();
    for _ in 0..2 {
        if points.len() < 3 {
            break;
        }
        let mut next = Vec::with_capacity(points.len() * 2);
        next.push(points[0]);
        for w in points.windows(2) {
            next.push(w[0] * 0.75 + w[1] * 0.25);
            next.push(w[0] * 0.25 + w[1] * 0.75);
        }
        next.push(points[points.len() - 1]);
        points = next;
    }
    points
}

/// Whether the marker owns this input: an unmodified primary press, the drag it began, its release,
/// and Esc / Cmd+Z while marks exist. Everything else navigates as it always does.
pub fn owns_input(state: &AppState, input: &ViewportInput) -> bool {
    if !active(state) {
        return false;
    }
    match input.kind {
        ViewportInputKind::PointerDown { button: PointerButton::Primary, mods, .. } => !mods.control && !mods.logo && !mods.shift && !mods.alt,
        ViewportInputKind::PointerMove { buttons, .. } => buttons & 1 != 0 && session::with(|s| s.marker.drawing.is_some()),
        ViewportInputKind::PointerUp { button: PointerButton::Primary, .. } => session::with(|s| s.marker.drawing.is_some()),
        _ => false,
    }
}

/// The marker's share of the tools' input. None: not the marker's.
pub fn handle(cx: &mut Cx, input: &ViewportInput, state: &mut AppState, pane: PaneId) -> Option<InputResponse> {
    if !active(state) {
        session::with(|s| s.marker.drawing = None);
        return None;
    }
    let local = |pos: DVec2| pos - input.rect.pos;
    match input.kind {
        ViewportInputKind::PointerDown { button: PointerButton::Primary, pos, .. } if owns_input(state, input) => {
            session::with(|s| {
                // drawing again after a line went: the earlier marks leave the view
                if s.marker.sent {
                    s.marker.marks.clear();
                    s.marker.sent = false;
                    cx.action(MarkAction::Clear);
                }
                s.marker.next_id += 1;
                let id = s.marker.next_id;
                s.marker.drawing = Some(Mark { id, line: vec![local(pos)], camera: *state.view.camera(pane), rect: [input.rect.size.x, input.rect.size.y], state: MarkState::Resolving });
            });
            Some(InputResponse { consumed: true, redraw: true, ..Default::default() })
        }
        ViewportInputKind::PointerMove { pos, .. } if owns_input(state, input) => {
            session::with(|s| {
                if let Some(mark) = &mut s.marker.drawing {
                    let p = local(pos);
                    if mark.line.last().is_none_or(|last| (*last - p).length() >= MIN_STEP) {
                        mark.line.push(p);
                    }
                }
            });
            Some(InputResponse { consumed: true, redraw: true, ..Default::default() })
        }
        ViewportInputKind::PointerUp { button: PointerButton::Primary, pos, .. } if owns_input(state, input) => {
            let done = session::with(|s| s.marker.drawing.take());
            if let Some(mut mark) = done {
                let p = local(pos);
                if mark.line.last().is_none_or(|last| (*last - p).length() >= MIN_STEP) {
                    mark.line.push(p);
                }
                cx.action(MarkAction::Add { id: mark.id, camera: mark.camera, rect: mark.rect, line: mark.line.iter().map(|p| [p.x, p.y]).collect() });
                session::with(|s| s.marker.marks.push(mark));
            }
            Some(InputResponse { consumed: true, redraw: true, ..Default::default() })
        }
        // the pen: a crosshair over the view while the marker is on
        ViewportInputKind::PointerMove { buttons: 0, .. } => Some(InputResponse { cursor: Some(MouseCursor::Crosshair), ..Default::default() }),
        ViewportInputKind::KeyDown { key: KeyCode::Escape, repeat: false, .. } => {
            cx.action(MarkAction::Leave);
            Some(InputResponse::consumed())
        }
        _ => None,
    }
}

/// The marker's mark was answered: `answer` is the worker's JSON.
pub fn resolved(id: u64, answer: Result<Resolved, String>) {
    session::with(|s| {
        if let Some(mark) = s.marker.marks.iter_mut().find(|m| m.id == id) {
            mark.state = match answer {
                Ok(r) => MarkState::Ready(r),
                Err(e) => MarkState::Failed(e),
            };
        }
    });
}

/// One mark as drawn: its points on the view (points from the view's top left corner), closed for
/// a circle, and its number once the worker has answered.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkLine {
    pub points: Vec<DVec2>,
    pub closed: bool,
    pub number: Option<u32>,
}

/// The marks as the view draws them now: the raw line while the worker resolves a stroke (if the
/// view is still as it was drawn), then the stroke as it sits on the model, so it turns with the
/// camera and moves with its part. The picture the chat sends is painted from the same lines.
pub fn lines(state: &AppState, proj: &ViewProjector, pane: PaneId) -> Vec<MarkLine> {
    let rect = proj.rect;
    let (marks, drawing) = session::with(|s| (s.marker.marks.clone(), s.marker.drawing.clone()));
    let mut out = Vec::new();
    for mark in marks.iter().chain(drawing.iter()) {
        let line = match &mark.state {
            MarkState::Ready(r) => {
                let points = r
                    .outline
                    .iter()
                    .filter_map(|a| {
                        let p = plate_now(a, state)?;
                        proj.project(vec3(p[0] as f32 * 0.001, p[1] as f32 * 0.001, p[2] as f32 * 0.001)).map(|p| p - rect.pos)
                    })
                    .collect();
                MarkLine { points, closed: r.kind == "circle", number: Some(r.number) }
            }
            MarkState::Failed(_) => continue,
            MarkState::Resolving => {
                let same = mark.camera == *state.view.camera(pane) && (mark.rect[0] - rect.size.x).abs() < 0.5 && (mark.rect[1] - rect.size.y).abs() < 0.5;
                let raw = if same { mark.line.clone() } else { Vec::new() };
                MarkLine { points: if raw.len() > 2 { smooth(&raw) } else { raw }, closed: false, number: None }
            }
        };
        if !line.points.is_empty() {
            out.push(line);
        }
    }
    out
}

/// Paint `lines` (view points) into a top-down RGBA picture `w` by `h` that shows the view scaled by
/// `scale`: the halo and the marker line, a disc for a dot, and each number by its mark.
pub fn paint(rgba: &mut [u8], w: usize, h: usize, lines: &[MarkLine], scale: f64) {
    let mut disc = |rgba: &mut [u8], c: DVec2, r: f64, color: [u8; 4]| {
        let (x0, x1) = (((c.x - r).floor().max(0.0)) as usize, ((c.x + r).ceil().max(0.0) as usize).min(w.saturating_sub(1)));
        let (y0, y1) = (((c.y - r).floor().max(0.0)) as usize, ((c.y + r).ceil().max(0.0) as usize).min(h.saturating_sub(1)));
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (dx, dy) = (x as f64 + 0.5 - c.x, y as f64 + 0.5 - c.y);
                if dx * dx + dy * dy <= r * r {
                    rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&color);
                }
            }
        }
    };
    let marker = [(COLOR.x * 255.0) as u8, (COLOR.y * 255.0) as u8, (COLOR.z * 255.0) as u8, 255];
    let dark = [10, 10, 10, 255];
    let stroke = |rgba: &mut [u8], a: DVec2, b: DVec2, r: f64, color: [u8; 4], disc: &mut dyn FnMut(&mut [u8], DVec2, f64, [u8; 4])| {
        let steps = ((b - a).length() / (r * 0.5).max(0.5)).ceil().max(1.0) as usize;
        for i in 0..=steps {
            disc(rgba, a + (b - a) * (i as f64 / steps as f64), r, color);
        }
    };
    for line in lines {
        let pts: Vec<DVec2> = line.points.iter().map(|p| *p * scale).collect();
        let mut segs: Vec<(DVec2, DVec2)> = pts.windows(2).map(|w| (w[0], w[1])).collect();
        if line.closed && pts.len() > 2 {
            segs.push((pts[pts.len() - 1], pts[0]));
        }
        if segs.is_empty() {
            disc(rgba, pts[0], (WIDTH + 2.0) * scale.max(1.0) * 0.5 + 1.0, dark);
            disc(rgba, pts[0], WIDTH * scale.max(1.0) * 0.5, marker);
        }
        let r = WIDTH * scale.max(1.0) * 0.5;
        for &(a, b) in &segs {
            stroke(rgba, a, b, r + 1.2, dark, &mut disc);
        }
        for &(a, b) in &segs {
            stroke(rgba, a, b, r, marker, &mut disc);
        }
        if let (Some(number), Some(top)) = (line.number, pts.iter().copied().reduce(|a, p| if p.y < a.y { p } else { a })) {
            digits(rgba, w, h, top + dvec2(6.0, -22.0) * scale.max(1.0), number, scale.max(1.0), marker, dark);
        }
    }
}

/// A number in a 3 by 5 pixel font, each pixel `k` big, with a dark outline.
fn digits(rgba: &mut [u8], w: usize, h: usize, at: DVec2, number: u32, k: f64, color: [u8; 4], outline: [u8; 4]) {
    const FONT: [[u8; 5]; 10] = [
        [0b111, 0b101, 0b101, 0b101, 0b111],
        [0b010, 0b110, 0b010, 0b010, 0b111],
        [0b111, 0b001, 0b111, 0b100, 0b111],
        [0b111, 0b001, 0b111, 0b001, 0b111],
        [0b101, 0b101, 0b111, 0b001, 0b001],
        [0b111, 0b100, 0b111, 0b001, 0b111],
        [0b111, 0b100, 0b111, 0b101, 0b111],
        [0b111, 0b001, 0b001, 0b001, 0b001],
        [0b111, 0b101, 0b111, 0b101, 0b111],
        [0b111, 0b101, 0b111, 0b001, 0b111],
    ];
    let cell = (k * 2.5).max(2.0);
    let put = |rgba: &mut [u8], x: f64, y: f64, pad: f64, c: [u8; 4]| {
        let (x0, y0) = ((x - pad).floor().max(0.0) as usize, (y - pad).floor().max(0.0) as usize);
        let (x1, y1) = (((x + cell + pad).ceil().max(0.0) as usize).min(w), ((y + cell + pad).ceil().max(0.0) as usize).min(h));
        for py in y0..y1 {
            for px in x0..x1 {
                rgba[(py * w + px) * 4..(py * w + px) * 4 + 4].copy_from_slice(&c);
            }
        }
    };
    let text = number.to_string();
    for pass in 0..2 {
        for (n, ch) in text.chars().enumerate() {
            let glyph = FONT[ch.to_digit(10).unwrap_or(0) as usize];
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..3 {
                    if bits >> (2 - col) & 1 == 1 {
                        let (x, y) = (at.x + (n as f64 * 4.0 + col as f64) * cell, at.y + row as f64 * cell);
                        if pass == 0 { put(rgba, x, y, cell * 0.5, outline) } else { put(rgba, x, y, 0.0, color) }
                    }
                }
            }
        }
    }
}

/// Take back the last stroke (marker mode's Cmd/Ctrl+Z). False when there is none.
pub fn pop() -> bool {
    session::with(|s| s.marker.marks.pop().is_some())
}

pub fn clear() {
    session::with(|s| {
        s.marker.marks.clear();
        s.marker.drawing = None;
        s.marker.sent = false;
    });
}

pub fn resolving() -> usize {
    session::with(|s| s.marker.resolving())
}

pub fn count() -> usize {
    session::with(|s| s.marker.marks.len())
}

/// A stroke is being drawn: the drag needs no pick under the pointer.
pub fn drawing() -> bool {
    session::with(|s| s.marker.drawing.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn the_view(pane: PaneId) -> (Cx, AppState, Rect) {
        let cx = Cx::new(Box::new(|_, _| {}));
        let mut state = AppState::default();
        state.ui.marker = true;
        state.view.camera_mut(pane).target = vec3(0.08, 0.165, 0.0);
        state.view.camera_mut(pane).eye = vec3(0.08, 0.05, 0.12);
        session::with(|s| s.marker = Marker::default());
        (cx, state, Rect { pos: dvec2(30.0, 20.0), size: dvec2(800.0, 600.0) })
    }

    /// What the view does with input: the navigator first, then the tools.
    fn feed(cx: &mut Cx, state: &mut AppState, rect: Rect, kinds: &[ViewportInputKind], pane: PaneId) -> Vec<Action> {
        let mut nav = crate::nav::Navigator::default();
        let mut tools = crate::tools::ToolSet { pane, ..Default::default() };
        cx.capture_actions(|cx| {
            for &kind in kinds {
                let input = ViewportInput { rect, kind, hit: None };
                if !nav.handle(cx, &input, state).consumed {
                    tools.handle(cx, &input, state);
                }
            }
        })
    }

    fn drag(mods: KeyModifiers, points: &[DVec2]) -> Vec<ViewportInputKind> {
        let mut out = vec![ViewportInputKind::PointerDown { button: PointerButton::Primary, pos: points[0], mods, tap_count: 1 }];
        for w in points.windows(2) {
            out.push(ViewportInputKind::PointerMove { pos: w[1], delta: w[1] - w[0], mods, buttons: 1 });
        }
        out.push(ViewportInputKind::PointerUp { button: PointerButton::Primary, pos: points[points.len() - 1], mods });
        out
    }

    /// A plain drag with the marker on draws a mark and does nothing else: the camera, the tool and
    /// the model's own actions stay as they were; Shift+drag still orbits and draws nothing; a tap
    /// is a dot; taking the last stroke back touches only the marks.
    #[test]
    fn a_drag_draws_a_mark_and_navigation_stays() {
        let pane = crate::tools::ToolSet::default().pane;
        let (mut cx, mut state, rect) = the_view(pane);
        let before = (*state.view.camera(pane), state.tool, state.ui.workspaces.current);
        let around: Vec<DVec2> = (0..=16).map(|i| {
            let a = i as f64 * std::f64::consts::TAU / 16.0;
            rect.pos + dvec2(400.0 + 60.0 * a.cos(), 300.0 + 60.0 * a.sin())
        }).collect();
        let actions = feed(&mut cx, &mut state, rect, &drag(KeyModifiers::default(), &around), pane);
        let adds: Vec<&MarkAction> = actions.iter().filter_map(|a| a.downcast_ref::<MarkAction>()).collect();
        assert!(matches!(adds[..], [MarkAction::Add { id: 1, .. }]), "{adds:?}");
        let MarkAction::Add { line, rect: size, camera, .. } = adds[0] else { unreachable!() };
        assert_eq!(*size, [800.0, 600.0]);
        assert_eq!(*camera, before.0, "the camera it was drawn with");
        assert!(line.len() >= 16 && (line[0][0] - 460.0).abs() < 1.0, "the view's own points: {:?}", line[0]);
        assert_eq!(actions.iter().filter(|a| a.downcast_ref::<MarkAction>().is_none()).count(), 0, "no action but the mark's");
        assert_eq!((*state.view.camera(pane), state.tool, state.ui.workspaces.current), before, "navigation and mode stay");
        assert_eq!(count(), 1);
        // a tap is a dot
        let tap = rect.pos + dvec2(100.0, 100.0);
        let actions = feed(&mut cx, &mut state, rect, &drag(KeyModifiers::default(), &[tap, tap]), pane);
        assert!(actions.iter().any(|a| matches!(a.downcast_ref::<MarkAction>(), Some(MarkAction::Add { line, .. }) if line.len() == 1)));
        assert_eq!(count(), 2);
        // Shift+drag orbits: no mark
        let shift = KeyModifiers { shift: true, ..Default::default() };
        let spin: Vec<DVec2> = (0..8).map(|i| rect.pos + dvec2(300.0 + 20.0 * i as f64, 300.0)).collect();
        let actions = feed(&mut cx, &mut state, rect, &drag(shift, &spin), pane);
        assert!(actions.iter().all(|a| a.downcast_ref::<MarkAction>().is_none()));
        assert_eq!(count(), 2, "Shift+drag draws nothing");
        assert!(*state.view.camera(pane) != before.0, "and still turns the view");
        // the last stroke back; the model's undo is not involved
        assert!(pop());
        assert_eq!(count(), 1);
        // Esc leaves the marker, the marks stay
        let esc = ViewportInputKind::KeyDown { key: KeyCode::Escape, mods: KeyModifiers::default(), repeat: false };
        let actions = feed(&mut cx, &mut state, rect, &[esc], pane);
        assert!(actions.iter().any(|a| a.downcast_ref::<MarkAction>() == Some(&MarkAction::Leave)));
        assert_eq!(count(), 1);
    }

    /// While the stroke is drawn nothing else is asked of the model or the view: no pick under the
    /// pointer, no action, no worker call until the button is up (the line is the overlay's
    /// own, in the frame of the mouse), so a drag over a field part costs what a drag costs.
    #[test]
    fn a_drag_in_progress_asks_nothing_of_the_model() {
        let pane = crate::tools::ToolSet::default().pane;
        let (mut cx, mut state, rect) = the_view(pane);
        let points: Vec<DVec2> = (0..40).map(|i| rect.pos + dvec2(100.0 + 5.0 * i as f64, 200.0 + (i as f64).sin() * 30.0)).collect();
        let mut kinds = drag(KeyModifiers::default(), &points);
        kinds.pop();
        let actions = feed(&mut cx, &mut state, rect, &kinds, pane);
        assert!(actions.is_empty(), "{} actions before the button is up", actions.len());
        assert!(drawing(), "the stroke is in hand");
        assert_eq!(session::with(|s| s.marker.drawing.as_ref().map(|m| m.line.len())), Some(40));
    }

    /// A mark's camera, as the AI reads it, casts the rays the view casts: a pixel of the picture is
    /// the pixel of the view, perspective and orthographic.
    #[test]
    fn the_told_camera_casts_the_views_rays() {
        let pane = crate::tools::ToolSet::default().pane;
        let (_, state, rect) = the_view(pane);
        for ortho in [false, true] {
            let mut camera = *state.view.camera(pane);
            camera.ortho = ortho;
            camera.ortho_height = 0.3;
            let view = Rect { pos: dvec2(0.0, 0.0), size: rect.size };
            let told = fab_camera(camera, view);
            let proj = ViewProjector::new(camera, view);
            for (x, y) in [(10.0, 20.0), (400.0, 300.0), (790.0, 590.0), (120.0, 480.0)] {
                let ray = proj.ray(dvec2(x, y));
                let (u, v) = (2.0 * x / 800.0 - 1.0, 1.0 - 2.0 * y / 600.0);
                let aside: [f64; 3] = std::array::from_fn(|k| told.right[k] * u + told.up[k] * v);
                let (from, dir) = match (told.fov, told.across) {
                    (Some(fov), _) => {
                        let t = (fov.to_radians() * 0.5).tan();
                        (told.eye, std::array::from_fn::<f64, 3, _>(|k| told.forward[k] + aside[k] * t))
                    }
                    (_, Some(across)) => (std::array::from_fn(|k| told.eye[k] + aside[k] * across * 0.5), told.forward),
                    _ => unreachable!(),
                };
                let l = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
                let want = ray.dir.normalize();
                for k in 0..3 {
                    assert!((dir[k] / l - [want.x, want.y, want.z][k] as f64).abs() < 1e-4, "ortho {ortho} ({x},{y}) direction {k}");
                }
                // the same line: the told origin lies on the view's ray
                let o = ray.origin;
                let rel = [from[0] * 0.001 - o.x as f64, from[1] * 0.001 - o.y as f64, from[2] * 0.001 - o.z as f64];
                let along = rel[0] * want.x as f64 + rel[1] * want.y as f64 + rel[2] * want.z as f64;
                let off = [rel[0] - along * want.x as f64, rel[1] - along * want.y as f64, rel[2] - along * want.z as f64];
                assert!((off[0] * off[0] + off[1] * off[1] + off[2] * off[2]).sqrt() < 1e-6, "ortho {ortho} ({x},{y}): the told ray is not the view's");
            }
        }
    }

    /// The picture the chat sends has the marks in it: painted in the marker's colour where the
    /// line runs, scaled to the picture, and nothing else of the picture changes.
    #[test]
    fn the_sent_picture_has_the_marks_in_it() {
        let (w, h) = (200usize, 100usize);
        let plain = vec![40u8; w * h * 4];
        let mut marked = plain.clone();
        let ring: Vec<DVec2> = (0..32).map(|i| {
            let a = i as f64 * std::f64::consts::TAU / 32.0;
            dvec2(400.0 + 150.0 * a.cos(), 200.0 + 100.0 * a.sin())
        }).collect();
        // the view is 800 by 400 points, the picture a quarter of it
        paint(&mut marked, w, h, &[MarkLine { points: ring, closed: true, number: Some(2) }], 0.25);
        let colour = [(COLOR.x * 255.0) as u8, (COLOR.y * 255.0) as u8, (COLOR.z * 255.0) as u8, 255];
        let at = |img: &[u8], x: usize, y: usize| -> [u8; 4] { img[(y * w + x) * 4..(y * w + x) * 4 + 4].try_into().unwrap() };
        // the ring's right end, view (550, 200), is the picture's (137, 50)
        assert_eq!(at(&marked, 137, 50), colour, "the marker's colour on the line");
        assert_eq!(at(&plain, 137, 50), [40; 4]);
        assert_eq!(at(&marked, 100, 50), [40; 4], "the middle of the ring is the picture");
        let changed = (0..w * h).filter(|i| marked[i * 4..i * 4 + 4] != plain[i * 4..i * 4 + 4]).count();
        assert!(changed > 150, "{changed} pixels carry the marks");
        // nothing drawn when there are no marks
        let mut none = plain.clone();
        paint(&mut none, w, h, &[], 0.25);
        assert_eq!(none, plain, "the picture without marks is untouched");
    }

    /// A mark keeps its place on its part: the part moves, the mark moves with it.
    #[test]
    fn a_mark_moves_with_its_part() {
        let mut state = AppState::default();
        let at = |x: f64| [[1.0, 0.0, 0.0, -x], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        state.plate.parts.push(PlatePart { object: "a/b".into(), model_from_printer: at(100.0), ..Default::default() });
        // it was marked at plate x 130 when the part stood at 100: model x 30
        let anchor = Anchor { solid: Some("a/b".into()), p: apply(&at(100.0), [130.0, 5.0, 2.0]) };
        assert_eq!(plate_now(&anchor, &state), Some([130.0, 5.0, 2.0]));
        state.plate.parts[0].model_from_printer = at(115.0);
        let now = plate_now(&anchor, &state).unwrap();
        assert!((now[0] - 145.0).abs() < 1e-9 && (now[1] - 5.0).abs() < 1e-9, "{now:?}");
        assert_eq!(plate_now(&Anchor { solid: None, p: [1.0, 2.0, 3.0] }, &state), Some([1.0, 2.0, 3.0]), "a mark on nothing stays on the plate");
    }
}
