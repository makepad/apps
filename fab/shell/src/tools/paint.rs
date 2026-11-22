//! Brush interaction only: surface picks in metres, the stroke shown live by the views
//! (`viewport::paint`), one worker edit on release (a record in the part's strokes file).

use crate::api::*;
use crate::tools::session;
use makepad_widgets_core::*;

#[derive(Clone, Debug)]
pub struct Stroke {
    object: String,
    part: PlatePart,
    radius: f64,
    rgb: [f32; 3],
    /// The print colour it paints in.
    color: Option<String>,
    /// Painted back to the part's own colour.
    erase: bool,
    points: Vec<[f64; 3]>,
    /// The colour, or a support channel.
    target: PaintTarget,
}

#[derive(Clone, Debug, Default)]
pub struct PaintDraft {
    pub hover: Option<RayHit>,
    pub stroke: Option<Stroke>,
    pub pressed: bool,
    /// The stroke just released, shown until its edit lands in the plate (or is refused).
    pub pending: Option<Stroke>,
    /// Fill: what a click fills now (a part and its feature, `None`: the whole part), and the
    /// features added with Shift on that part.
    pub fill_hover: Option<(String, Option<String>)>,
    pub fill_pick: Vec<(String, Option<String>)>,
    /// Alt is held: it flips Paint and Erase for as long as it is.
    pub alt: bool,
    /// When the brush size last changed with Shift+scroll (`Cx::time_now`), for its readout.
    pub size_at: f64,
    /// Where a Shift press began in fill mode: its click multi-picks on release unless it orbited.
    pub shift_press: Option<DVec2>,
}

/// How long the size readout stays by the brush ring after the last change (seconds).
pub const SIZE_READOUT_SECS: f64 = 1.2;

/// Painting back the part's own colour now: the Erase setting, flipped while Alt is held.
pub fn erasing(state: &AppState) -> bool {
    state.ui.paint_erase ^ session::with(|s| s.paint.alt)
}

/// The colour a press paints on `part` and the print colour it prints in: the chosen one, or
/// the part's own when erasing. A support channel shows in its tint and prints in nothing.
fn paint_rgb(state: &AppState, part: &PlatePart, erase: bool) -> ([f32; 3], Option<String>) {
    if let Some(tint) = state.ui.paint_target.tint() {
        return (tint, None);
    }
    if erase {
        return ([part.color[0], part.color[1], part.color[2]], Some(part.print_color.clone()));
    }
    // The colour as its material shows now (the mapping may have changed since the pick).
    let shown = state.ui.paint_name.as_deref().and_then(|n| state.print.colors.row(n)).map(|r| [r.shown[0], r.shown[1], r.shown[2]]);
    (shown.unwrap_or(state.ui.paint_color), state.ui.paint_name.clone())
}

/// The brush radius range (mm).
pub const MIN_RADIUS: f64 = 0.1;
pub const MAX_RADIUS: f64 = 100.0;

pub fn active(state: &AppState) -> bool {
    state.ui.gizmo == GizmoMode::Paint && state.tool == Tool::Select && state.ui.in_mode(PlateMode::Design)
}

/// The stroke being drawn: its part, points (printer mm), radius (mm), colour and whether it
/// erases, for the views to show at once.
pub fn live_stroke() -> Option<(String, Vec<[f64; 3]>, f64, [f32; 3], bool)> {
    session::with(|s| s.paint.stroke.as_ref().or(s.paint.pending.as_ref()).filter(|st| !st.points.is_empty()).map(|st| (st.object.clone(), st.points.clone(), st.radius, st.rgb, st.erase)))
}

/// The height (printer mm) of the newest point of the stroke being drawn or waiting to land.
pub fn brush_z() -> Option<f64> {
    session::with(|s| s.paint.stroke.as_ref().or(s.paint.pending.as_ref()).and_then(|st| st.points.last()).map(|p| p[2]))
}

/// The feature of part `i` under a printer point (mm): the smallest named feature whose surface
/// is within reach of it, else the whole part.
pub fn feature_at(part: &PlatePart, p: [f64; 3]) -> Option<String> {
    let m = &part.model_from_printer;
    let q: [f64; 3] = std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
    const REACH: f64 = 0.3;
    part.features
        .iter()
        .filter(|f| f.region.distance(q) <= REACH)
        .min_by(|a, b| a.volume.total_cmp(&b.volume))
        .map(|f| f.name.clone())
}

/// What the fill would paint now, for the views: the part, the features' regions (model frame)
/// and their names (`""`: the whole part).
pub fn live_fill(state: &AppState) -> Option<(String, Vec<fab_paint_field::Region>, Vec<String>)> {
    // a pattern shows once its script builds: a plain fill's preview would show the part all in
    // the paint colour
    if state.ui.paint_mode == PaintMode::Pattern {
        return None;
    }
    session::with(|s| {
        let (object, _) = s.paint.fill_hover.clone().or_else(|| s.paint.fill_pick.first().cloned())?;
        let part = state.plate.parts.iter().find(|p| p.object == object)?;
        let mut regions = Vec::new();
        let mut names = Vec::new();
        for (_, f) in s.paint.fill_pick.iter().chain(s.paint.fill_hover.iter()).filter(|(o, _)| *o == object) {
            let region = match f {
                Some(name) => part.features.iter().find(|x| x.name == *name).map_or(fab_paint_field::Region::All, |x| x.region.clone()),
                None => fab_paint_field::Region::All,
            };
            regions.push(region);
            names.push(f.clone().unwrap_or_default());
        }
        Some((object, regions, names))
    })
}

fn fill(cx: &mut Cx, input: &ViewportInput, state: &AppState) -> Option<InputResponse> {
    let response = InputResponse { consumed: true, redraw: true, ..Default::default() };
    let target = |input: &ViewportInput| -> Option<(String, Option<String>)> {
        let (i, h) = hover(input, state)?;
        let part = &state.plate.parts[i];
        let p = [h.point.x as f64 * 1000.0, h.point.y as f64 * 1000.0, h.point.z as f64 * 1000.0];
        Some((part.object.clone(), if state.ui.paint_mode == PaintMode::Feature { feature_at(part, p) } else { None }))
    };
    match input.kind {
        ViewportInputKind::PointerMove { buttons: 0, .. } if owns_input(state, input) => {
            // the smooth fill shows once it is resolved on the part (no hover preview)
            let t = if state.ui.paint_mode == PaintMode::Flood { None } else { target(input) };
            session::with(|s| s.paint.fill_hover = t);
            Some(response)
        }
        // A fill turns a support channel on; the brush takes it off again.
        ViewportInputKind::PointerDown { button: PointerButton::Primary, .. } if state.ui.paint_target != PaintTarget::Color && erasing(state) && owns_input(state, input) => {
            cx.action(ShellAction::Notice { view: NoticeView::Plate, text: "A fill turns support paint on: erase it with the brush".into() });
            Some(response)
        }
        ViewportInputKind::PointerDown { button: PointerButton::Primary, .. } if state.ui.paint_mode == PaintMode::Flood && owns_input(state, input) => {
            if let Some((i, h)) = hover(input, state) {
                let seed = [h.point.x as f64 * 1000.0, h.point.y as f64 * 1000.0, h.point.z as f64 * 1000.0];
                let (rgb, color) = paint_rgb(state, &state.plate.parts[i], erasing(state));
                cx.action(PartAction::PaintFlood { object: state.plate.parts[i].object.clone(), seed, angle: state.ui.paint_angle, rgb, color, target: state.ui.paint_target });
            }
            Some(response)
        }
        // A pattern fills the whole part: its own colour and the paint colour through its volume.
        ViewportInputKind::PointerDown { button: PointerButton::Primary, .. } if state.ui.paint_mode == PaintMode::Pattern && owns_input(state, input) => {
            if state.ui.paint_target != PaintTarget::Color {
                cx.action(ShellAction::Notice { view: NoticeView::Plate, text: "A pattern mixes two colours: pick a colour, not a support brush".into() });
            } else if let Some((i, _)) = hover(input, state) {
                let part = &state.plate.parts[i];
                let (own, own_color) = paint_rgb(state, part, true);
                let (rgb, color) = paint_rgb(state, part, false);
                cx.action(PartAction::PaintPattern { object: part.object.clone(), own, own_color, rgb, color, pattern: state.ui.paint_pattern });
            }
            Some(response)
        }
        ViewportInputKind::PointerDown { button: PointerButton::Primary, pos, mods, .. } if !owns_input(state, input) => {
            // Shift+drag orbits; a Shift click without travel multi-picks on release.
            session::with(|s| s.paint.shift_press = mods.shift.then_some(pos));
            None
        }
        ViewportInputKind::PointerUp { button: PointerButton::Primary, pos, mods } if session::with(|s| s.paint.shift_press.is_some()) => {
            let press = session::with(|s| s.paint.shift_press.take());
            if !mods.shift || press.is_none_or(|p| (pos - p).length() > 4.0) { return None; }
            let Some(t) = target(input) else { return Some(response) };
            session::with(|s| {
                s.paint.fill_pick.retain(|(o, _)| *o == t.0);
                match s.paint.fill_pick.iter().position(|p| *p == t) {
                    Some(k) => { s.paint.fill_pick.remove(k); }
                    None => s.paint.fill_pick.push(t),
                }
            });
            Some(response)
        }
        ViewportInputKind::PointerDown { button: PointerButton::Primary, .. } if owns_input(state, input) => {
            let Some(t) = target(input) else { return Some(response) };
            let Some(part) = state.plate.parts.iter().find(|p| p.object == t.0) else { return Some(response) };
            let (rgb, color) = paint_rgb(state, part, erasing(state));
            session::with(|s| {
                // a pick on another part starts over there
                s.paint.fill_pick.retain(|(o, _)| *o == t.0);
                let mut features: Vec<Option<String>> = s.paint.fill_pick.drain(..).map(|(_, f)| f).collect();
                if !features.contains(&t.1) {
                    features.push(t.1.clone());
                }
                cx.action(PartAction::PaintFill { object: t.0.clone(), features, rgb, color: color.clone(), target: state.ui.paint_target });
            });
            Some(response)
        }
        ViewportInputKind::HoverOut => {
            session::with(|s| s.paint.fill_hover = None);
            Some(response)
        }
        ViewportInputKind::KeyDown { key: KeyCode::Escape, .. } => {
            cancel();
            cx.action(crate::ui::stage::StageAction::Gizmo(GizmoMode::Off));
            Some(response)
        }
        _ => None,
    }
}

pub fn cancel() {
    session::with(|s| s.paint = PaintDraft::default());
}

/// Paint owns the unmodified primary drag before the navigator can orbit.
/// Modified primary, secondary and middle gestures still navigate normally.
pub fn owns_input(state: &AppState, input: &ViewportInput) -> bool {
    if !active(state) { return false; }
    owns_brush_input(input, session::with(|s| s.paint.pressed))
}

pub(crate) fn owns_brush_input(input: &ViewportInput, pressed: bool) -> bool {
    match input.kind {
        // Alt flips Paint and Erase; Ctrl and Cmd keep their camera gestures, Shift orbits.
        ViewportInputKind::PointerDown { button: PointerButton::Primary, mods, .. } => !mods.control && !mods.logo && !mods.shift,
        ViewportInputKind::PointerMove { buttons, .. } => buttons == 0 || (buttons & 1 != 0 && pressed),
        ViewportInputKind::PointerUp { button: PointerButton::Primary, .. } => pressed,
        _ => false,
    }
}

fn hover(input: &ViewportInput, state: &AppState) -> Option<(usize, RayHit)> {
    let hit = part_hit(input, state);
    session::with(|s| s.paint.hover = hit.map(|(_, h)| h));
    hit
}

pub(crate) fn part_hit(input: &ViewportInput, state: &AppState) -> Option<(usize, RayHit)> {
    input.hit.and_then(|h| state.plate.part_of(h.element).map(|i| (i, h)))
}

fn sample(stroke: &mut Stroke, state: &AppState, i: usize, hit: RayHit) {
    if !same_placement(&stroke.part, &state.plate.parts[i]) { return; }
    sample_point(&mut stroke.points, stroke.radius, hit);
}

pub(crate) fn sample_point(points: &mut Vec<[f64; 3]>, radius: f64, hit: RayHit) {
    let p = [hit.point.x as f64 * 1000.0, hit.point.y as f64 * 1000.0, hit.point.z as f64 * 1000.0];
    if points.last().is_none_or(|last| (0..3).map(|j| (last[j] - p[j]).powi(2)).sum::<f64>() > (radius * 0.5).powi(2)) {
        points.push(p);
    }
}

pub(crate) fn same_placement(old: &PlatePart, part: &PlatePart) -> bool {
    old.object == part.object && old.rotate == part.rotate && old.scale == part.scale
        && old.min_mm.iter().chain(&old.max_mm).zip(part.min_mm.iter().chain(&part.max_mm)).all(|(a, b)| (a - b).abs() < 1e-5)
}

/// Hand a finished stroke to the model: one paint command.
fn commit(cx: &mut Cx, stroke: Stroke) {
    if stroke.points.is_empty() { return; }
    log!("fab paint: {} of {} points on {}", if stroke.erase { "erase stroke" } else { "stroke" }, stroke.points.len(), stroke.object);
    session::with(|d| d.paint.pending = Some(stroke.clone()));
    cx.action(PartAction::PaintStroke { object: stroke.object, rgb: stroke.rgb, color: stroke.color, erase: stroke.erase, radius: stroke.radius, points: stroke.points, target: stroke.target });
}

fn new_stroke(state: &AppState, i: usize, erase: bool) -> Stroke {
    let (rgb, color) = paint_rgb(state, &state.plate.parts[i], erase);
    Stroke { object: state.plate.parts[i].object.clone(), part: state.plate.parts[i].clone(), radius: state.ui.paint_radius, rgb, color, erase, points: Vec::new(), target: state.ui.paint_target }
}

/// Alt held down or let go, from the pointer's modifiers or the key itself.
pub(crate) fn alt_of(input: &ViewportInput) -> Option<bool> {
    match input.kind {
        ViewportInputKind::PointerDown { mods, .. } | ViewportInputKind::PointerMove { mods, .. } | ViewportInputKind::PointerUp { mods, .. } | ViewportInputKind::Scroll { mods, .. } => Some(mods.alt),
        ViewportInputKind::KeyDown { key: KeyCode::Alt, .. } => Some(true),
        ViewportInputKind::KeyUp { key: KeyCode::Alt, .. } => Some(false),
        ViewportInputKind::KeyDown { mods, .. } | ViewportInputKind::KeyUp { mods, .. } => Some(mods.alt),
        _ => None,
    }
}

pub fn handle(cx: &mut Cx, input: &ViewportInput, state: &mut AppState) -> Option<InputResponse> {
    if !active(state) { cancel(); return None; }
    let mut flipped = false;
    if let Some(alt) = alt_of(input) {
        flipped = session::with(|s| std::mem::replace(&mut s.paint.alt, alt) != alt);
    }
    if flipped {
        // The other kind of stroke starts here: the one in hand lands as its own command.
        let (done, at) = session::with(|s| (s.paint.stroke.take(), s.paint.hover));
        if let Some(stroke) = done {
            let hit = at.and_then(|h| state.plate.part_of(h.element).map(|i| (i, h)));
            commit(cx, stroke);
            if let (Some((i, h)), true) = (hit, session::with(|s| s.paint.pressed)) {
                let mut next = new_stroke(state, i, erasing(state));
                sample(&mut next, state, i, h);
                session::with(|s| s.paint.stroke = Some(next));
            }
        }
    }
    if state.ui.paint_mode != PaintMode::Brush {
        return fill(cx, input, state).or(flipped.then(|| InputResponse { redraw: true, ..Default::default() }));
    }
    // Shift+scroll sizes the brush: steps in proportion to the size, smooth for a trackpad.
    if let ViewportInputKind::Scroll { delta, mods, .. } = input.kind {
        if mods.shift {
            // The platform may turn Shift+wheel into a sideways scroll.
            let d = if delta.y.abs() >= delta.x.abs() { delta.y } else { delta.x };
            let steps = (d / 80.0).clamp(-3.0, 3.0);
            if steps.abs() > 1e-4 {
                state.ui.paint_radius = (state.ui.paint_radius * 1.1f64.powf(-steps)).clamp(MIN_RADIUS, MAX_RADIUS);
                session::with(|s| s.paint.size_at = Cx::time_now());
            }
            return Some(InputResponse { consumed: true, redraw: true, wants_frames: true, ..Default::default() });
        }
    }
    // A colour rebuild may finish during the next press. Keep that stroke if
    // its geometry/placement is unchanged; a moved or removed part invalidates it.
    session::with(|s| {
        if s.paint.stroke.as_ref().is_some_and(|d| !state.plate.parts.iter().any(|p| same_placement(&d.part, p))) { s.paint.stroke = None; }
    });
    let response = InputResponse { consumed: true, redraw: true, ..Default::default() };
    match input.kind {
        ViewportInputKind::PointerDown { button: PointerButton::Primary, .. } if owns_input(state, input) => {
            let hit = hover(input, state);
            session::with(|s| {
                s.paint.pressed = true;
                s.paint.stroke = hit.map(|(i, h)| {
                    let mut stroke = new_stroke(state, i, state.ui.paint_erase ^ s.paint.alt);
                    sample(&mut stroke, state, i, h);
                    stroke
                });
            });
            if let Some((i, _)) = hit { cx.action(PartAction::Select { index: i, toggle: false }); }
            Some(response)
        }
        ViewportInputKind::PointerMove { .. } if owns_input(state, input) => {
            let hit = hover(input, state);
            let started = session::with(|s| {
                match (&mut s.paint.stroke, hit) {
                    (Some(stroke), Some((i, h))) => sample(stroke, state, i, h),
                    // a press off the parts starts its stroke where the drag first reaches one
                    (None, Some((i, h))) if s.paint.pressed => {
                        let mut stroke = new_stroke(state, i, state.ui.paint_erase ^ s.paint.alt);
                        sample(&mut stroke, state, i, h);
                        s.paint.stroke = Some(stroke);
                        return Some(i);
                    }
                    _ => {}
                }
                None
            });
            if let Some(i) = started { cx.action(PartAction::Select { index: i, toggle: false }); }
            Some(response)
        }
        ViewportInputKind::PointerUp { button: PointerButton::Primary, .. } if owns_input(state, input) => {
            let hit = hover(input, state);
            let stroke = session::with(|s| {
                s.paint.pressed = false;
                if let (Some(stroke), Some((i, h))) = (&mut s.paint.stroke, hit) { sample(stroke, state, i, h); }
                s.paint.stroke.take()
            });
            if stroke.is_none() {
                log!("fab paint: the drag reached no part");
            }
            if let Some(s) = stroke { commit(cx, s); }
            Some(response)
        }
        ViewportInputKind::HoverOut => {
            session::with(|s| s.paint.hover = None);
            Some(response)
        }
        ViewportInputKind::KeyDown { key: KeyCode::Escape, .. } => {
            cancel();
            cx.action(crate::ui::stage::StageAction::Gizmo(GizmoMode::Off));
            Some(response)
        }
        _ => None,
    }
}

/// Shift+scroll over the viewport sizes the brush while Paint or Erase works in brush mode.
pub fn owns_scroll(state: &AppState, mods: &KeyModifiers) -> bool {
    active(state) && state.ui.paint_mode == PaintMode::Brush && mods.shift
}

/// A brush diameter for the readouts: one decimal below 10 mm, whole mm above.
pub fn diameter_text(mm: f64) -> String {
    if mm < 10.0 { format!("{mm:.1}") } else { format!("{mm:.0}") }
}
