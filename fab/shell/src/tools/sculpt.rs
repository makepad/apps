//! Surface brush points, kept in the sculpted part's model frame as they are sampled: the views
//! draw the stroke live in the part's field (`viewport::sdf`, a sculpt over the part's strokes
//! log), and release hands the model the same points as one sculpt stroke (one undo step), so
//! the committed field is the one already drawn.

use crate::api::*;
use crate::tools::{paint, session};
use fab_paint_field::StrokeOp;
use makepad_widgets_core::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Counts changes of the live strokes (a point, a new or released stroke, one landed or
/// dropped): what follows them (the live slice) looks for a new count.
static REVISION: AtomicU64 = AtomicU64::new(0);

fn changed() {
    REVISION.fetch_add(1, Ordering::Relaxed);
}

#[derive(Clone, Debug)]
pub struct Stroke {
    /// Unique per press: the views tell a stroke growing from a new one by it.
    pub id: u64,
    part: PlatePart,
    pub op: StrokeOp,
    /// The brush in the model frame (mm) and its strength, as the press began.
    pub radius: f64,
    pub strength: f64,
    /// Printer mm: the sampling's spacing.
    points: Vec<[f64; 3]>,
    /// The same points in the part's model frame: what the views draw and release records.
    pub model: Vec<[f64; 3]>,
}

impl Stroke {
    pub fn object(&self) -> &str {
        &self.part.object
    }
}

#[derive(Clone, Debug, Default)]
pub struct SculptDraft {
    pub hover: Option<RayHit>,
    /// The released strokes not in the plate yet (oldest first: the model takes them in that
    /// order), then the one being drawn when `drawing`.
    pub strokes: Vec<Stroke>,
    pub drawing: bool,
    pub pressed: bool,
    pub alt: bool,
    pub size_at: f64,
    /// When the newest point was sampled (a frame showing it measures the brush's latency).
    pub sampled: Option<Instant>,
}

impl SculptDraft {
    fn drawn(&mut self) -> Option<&mut Stroke> {
        if self.drawing { self.strokes.last_mut() } else { None }
    }

    /// The stroke being drawn, taken off the list.
    fn take_drawn(&mut self) -> Option<Stroke> {
        if !std::mem::take(&mut self.drawing) {
            return None;
        }
        changed();
        self.strokes.pop()
    }

    /// A new scene: the stroke being drawn goes; the released ones stay until their edits land.
    pub fn reset_for_scene(&mut self) {
        self.take_drawn();
        let strokes = std::mem::take(&mut self.strokes);
        *self = SculptDraft { strokes, ..Default::default() };
    }
}

pub fn active(state: &AppState) -> bool {
    state.ui.gizmo == GizmoMode::Sculpt && state.tool == Tool::Select && state.ui.in_mode(PlateMode::Design)
}

fn with_alt(op: StrokeOp, alt: bool) -> StrokeOp {
    match (op, alt) {
        (StrokeOp::Raise, true) => StrokeOp::Lower,
        (StrokeOp::Lower, true) => StrokeOp::Raise,
        _ => op,
    }
}

pub fn operation(state: &AppState) -> StrokeOp {
    with_alt(state.ui.sculpt_op, session::with(|s| s.sculpt.alt))
}

/// Drops the stroke being drawn (the released ones still show until their edits land).
pub fn cancel() {
    session::with(|s| {
        s.sculpt.take_drawn();
        s.sculpt.pressed = false;
        s.sculpt.hover = None;
    });
}

/// The oldest released stroke's edit is in the plate now, or was refused.
pub fn landed() {
    session::with(|s| {
        let drawn = s.sculpt.take_drawn();
        if !s.sculpt.strokes.is_empty() {
            s.sculpt.strokes.remove(0);
            changed();
        }
        if let Some(d) = drawn {
            s.sculpt.strokes.push(d);
            s.sculpt.drawing = true;
        }
    });
}

/// The strokes the views draw live, oldest first, and when the newest point was sampled.
pub fn with_live<R>(f: impl FnOnce(&[Stroke], Option<Instant>) -> R) -> R {
    session::with(|s| f(&s.sculpt.strokes, s.sculpt.sampled))
}

/// The live strokes' change count (see [`REVISION`]).
pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

/// The brush while strokes are live: the height (printer mm) of the newest point, and whether
/// a stroke is being drawn (else released ones wait to land).
pub fn brush() -> Option<(f64, bool)> {
    session::with(|s| s.sculpt.strokes.last().and_then(|t| t.points.last()).map(|p| (p[2], s.sculpt.drawing)))
}

pub fn owns_input(state: &AppState, input: &ViewportInput) -> bool {
    active(state) && paint::owns_brush_input(input, session::with(|s| s.sculpt.pressed))
}

pub fn owns_scroll(state: &AppState, mods: &KeyModifiers) -> bool {
    active(state) && mods.shift
}

fn hover(input: &ViewportInput, state: &AppState) -> Option<(usize, RayHit)> {
    let hit = paint::part_hit(input, state);
    session::with(|s| s.sculpt.hover = hit.map(|(_, h)| h));
    hit
}

/// A part's model frame at a printer point (mm).
fn to_model(part: &PlatePart, p: [f64; 3]) -> [f64; 3] {
    let m = &part.model_from_printer;
    std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3])
}

/// Printer mm per model mm of a part: its placement's scale (of a scale by axis, their
/// geometric mean, so a stroke's tube is the brush's size on the plate on the whole).
fn scale_of(part: &PlatePart) -> f64 {
    let m = &part.model_from_printer;
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    1.0 / det.abs().cbrt().max(1e-12)
}

fn new_stroke(state: &AppState, i: usize, alt: bool) -> Stroke {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let part = state.plate.parts[i].clone();
    let radius = state.ui.sculpt_radius / scale_of(&part);
    Stroke { id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed), part, op: with_alt(state.ui.sculpt_op, alt), radius, strength: state.ui.sculpt_strength.clamp(0.0, 1.0), points: Vec::new(), model: Vec::new() }
}

fn sample(stroke: &mut Stroke, state: &AppState, i: usize, hit: RayHit) -> bool {
    if !paint::same_placement(&stroke.part, &state.plate.parts[i]) {
        return false;
    }
    let n = stroke.points.len();
    paint::sample_point(&mut stroke.points, state.ui.sculpt_radius, hit);
    if stroke.points.len() == n {
        return false;
    }
    let p = to_model(&stroke.part, stroke.points[n]);
    stroke.model.push(p);
    changed();
    true
}

/// Starts a stroke on part `i` at `hit`, as the one being drawn.
fn start(s: &mut session::ToolSession, state: &AppState, i: usize, hit: RayHit) {
    let mut stroke = new_stroke(state, i, s.sculpt.alt);
    if sample(&mut stroke, state, i, hit) {
        s.sculpt.sampled = Some(Instant::now());
    }
    s.sculpt.strokes.push(stroke);
    s.sculpt.drawing = true;
    changed();
}

/// Releases the stroke being drawn: to the model, and on the list until its edit lands.
fn commit(cx: &mut Cx) {
    let stroke = session::with(|s| {
        let stroke = s.sculpt.take_drawn()?;
        if stroke.model.is_empty() {
            return None;
        }
        s.sculpt.strokes.push(stroke.clone());
        Some(stroke)
    });
    let Some(stroke) = stroke else { return };
    log!("fab sculpt: {:?} of {} points on {}", stroke.op, stroke.model.len(), stroke.part.object);
    cx.action(PartAction::SculptStroke { object: stroke.part.object, op: stroke.op, radius: stroke.radius, strength: stroke.strength, points: stroke.model });
}

pub fn handle(cx: &mut Cx, input: &ViewportInput, state: &mut AppState) -> Option<InputResponse> {
    if !active(state) { cancel(); return None; }
    // A queued edit may finish during the next press; keep only an unchanged placement.
    session::with(|s| {
        if s.sculpt.drawing && s.sculpt.strokes.last().is_some_and(|d| !state.plate.parts.iter().any(|p| paint::same_placement(&d.part, p))) { s.sculpt.take_drawn(); }
    });
    let mut flipped = false;
    if let Some(alt) = paint::alt_of(input) {
        flipped = session::with(|s| std::mem::replace(&mut s.sculpt.alt, alt) != alt);
    }
    if flipped && matches!(state.ui.sculpt_op, StrokeOp::Raise | StrokeOp::Lower) && session::with(|s| s.sculpt.drawing) {
        // Alt starts the opposite stroke here, just as it flips Paint and Erase.
        let at = session::with(|s| s.sculpt.hover);
        let hit = at.and_then(|h| state.plate.part_of(h.element).map(|i| (i, h)));
        commit(cx);
        if let (Some((i, h)), true) = (hit, session::with(|s| s.sculpt.pressed)) {
            session::with(|s| start(s, state, i, h));
        }
    }
    if let ViewportInputKind::Scroll { delta, mods, .. } = input.kind {
        if owns_scroll(state, &mods) {
            // Shift+wheel may arrive as a sideways scroll.
            let d = if delta.y.abs() >= delta.x.abs() { delta.y } else { delta.x };
            let steps = (d / 80.0).clamp(-3.0, 3.0);
            if steps.abs() > 1e-4 {
                state.ui.sculpt_radius = (state.ui.sculpt_radius * 1.1f64.powf(-steps)).clamp(paint::MIN_RADIUS, paint::MAX_RADIUS);
                session::with(|s| s.sculpt.size_at = Cx::time_now());
            }
            return Some(InputResponse { consumed: true, redraw: true, wants_frames: true, ..Default::default() });
        }
    }
    let response = InputResponse { consumed: true, redraw: true, ..Default::default() };
    match input.kind {
        ViewportInputKind::PointerDown { button: PointerButton::Primary, .. } if owns_input(state, input) => {
            let hit = hover(input, state);
            session::with(|s| {
                s.sculpt.pressed = true;
                s.sculpt.take_drawn();
                if let Some((i, h)) = hit {
                    start(s, state, i, h);
                }
            });
            if let Some((i, _)) = hit { cx.action(PartAction::Select { index: i, toggle: false }); }
            Some(response)
        }
        ViewportInputKind::PointerMove { .. } if owns_input(state, input) => {
            let hit = hover(input, state);
            let started = session::with(|s| {
                let Some((i, h)) = hit else { return None };
                if let Some(stroke) = s.sculpt.drawn() {
                    if sample(stroke, state, i, h) {
                        s.sculpt.sampled = Some(Instant::now());
                    }
                    return None;
                }
                // A press off the parts starts where the drag first reaches one.
                if !s.sculpt.pressed {
                    return None;
                }
                start(s, state, i, h);
                Some(i)
            });
            if let Some(i) = started { cx.action(PartAction::Select { index: i, toggle: false }); }
            Some(response)
        }
        ViewportInputKind::PointerUp { button: PointerButton::Primary, .. } if owns_input(state, input) => {
            let hit = hover(input, state);
            session::with(|s| {
                s.sculpt.pressed = false;
                if let (Some(stroke), Some((i, h))) = (s.sculpt.drawn(), hit) {
                    if sample(stroke, state, i, h) {
                        s.sculpt.sampled = Some(Instant::now());
                    }
                }
            });
            commit(cx);
            Some(response)
        }
        ViewportInputKind::HoverOut => {
            session::with(|s| s.sculpt.hover = None);
            Some(response)
        }
        ViewportInputKind::KeyDown { key: KeyCode::Escape, .. } => {
            cancel();
            cx.action(crate::ui::stage::StageAction::Gizmo(GizmoMode::Off));
            Some(response)
        }
        _ => flipped.then(|| InputResponse { redraw: true, ..Default::default() }),
    }
}
