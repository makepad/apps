//! The area tools. "No trees here", in the Sliced view: a drag on the bed draws an area no tree
//! of the supports may stand or grow in, a rectangle (with Alt a disc) from the bed up to
//! [`NO_TREES_HEIGHT`]; letting go commits [`PartAction::NoTrees`]. "Print settings here", in
//! the Normal view and the Sliced one alike (it never changes the view): a drag draws a volume
//! that prints with settings of its own, a box on the rectangle (with Alt a sphere on the disc)
//! about the plane it is drawn on ([`settings_span`]): the shown layer (the layer slider's top)
//! in the Sliced view, the height of the model where the drag starts in the Normal one; letting
//! go commits [`PartAction::SettingsHere`] and goes back to the Select tool. The app writes either
//! into the script of the part it is for (a box: the picked part's) as a solid its statement
//! marks (`m.no_trees`, `m.settings`), one undo step, and picks it. Shift orbits as in every
//! tool. A press or a drag that draws nothing says why in the plate view. The area stays over the bed: a point of the
//! drag off it sits on its edge, one past the horizon on the edge the pointer looks towards.

use crate::api::*;
use crate::api::PaneId;
use crate::tools::session;
use makepad_widgets_core::*;

/// Under this size (mm) a drag draws nothing: a click.
const MIN_SIZE: f64 = 0.5;

/// How far (mm) along the bed's plane a point past the horizon lies, the way the pointer looks:
/// past any bed, so it sits on the edge.
const PAST_THE_BED: f64 = 1e6;

/// What the plate view says to a press off the bed's plane (the sky, or under the bed seen from
/// above it) and to a drag too small to draw anything.
pub const OFF_THE_BED: &str = "No trees here draws on the bed: start the drag on the bed";
pub const TOO_SMALL: &str = "No trees here: drag across the bed to draw the area (a click draws nothing)";
/// The same for print settings, and its press with no layer shown yet.
pub const OFF_THE_LAYER: &str = "Print settings here draws on the shown layer: start the drag over the bed";
pub const TOO_SMALL_HERE: &str = "Print settings here: drag across the layer to draw the volume (a click draws nothing)";
pub const NO_LAYER: &str = "Print settings here draws on the shown layer: wait for the slice's layers";
/// The same in the Normal view, for a press off the model.
pub const OFF_THE_MODEL: &str = "Print settings here: start the drag on the model, at the height the box goes";

/// The area being drawn (plate mm): where the drag started and where it is now, each over the
/// bed, on the plane it is drawn on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Draft {
    pub from: [f64; 2],
    pub to: [f64; 2],
    /// A disc round `from` through `to`, not a rectangle across them.
    pub disc: bool,
    /// The bed it is drawn over: (min, max).
    pub bed: ([f64; 2], [f64; 2]),
    /// The height of its plane (mm): the bed's for no trees, the shown layer's for settings.
    pub z: f64,
    /// The tool drawing it: [`Tool::NoTrees`] or [`Tool::SettingsHere`].
    pub tool: Tool,
}

impl Draft {
    /// What it draws, on the bed (a disc no wider than the bed round its centre); none while it
    /// is smaller than [`MIN_SIZE`].
    pub fn area(&self) -> Option<BedArea> {
        let (a, b) = (self.from, self.to);
        if self.disc {
            let (lo, hi) = self.bed;
            let room = (a[0] - lo[0]).min(hi[0] - a[0]).min(a[1] - lo[1]).min(hi[1] - a[1]);
            let radius = (b[0] - a[0]).hypot(b[1] - a[1]).min(room);
            return (2.0 * radius >= MIN_SIZE).then_some(BedArea::Disc { centre: a, radius });
        }
        let (min, max) = ([a[0].min(b[0]), a[1].min(b[1])], [a[0].max(b[0]), a[1].max(b[1])]);
        (max[0] - min[0] >= MIN_SIZE && max[1] - min[1] >= MIN_SIZE).then_some(BedArea::Rect { min, max })
    }
}

/// Whether one of the tools takes the view's pointer: chosen, in its view (no trees: the Sliced
/// one; print settings: the Normal or the Sliced one), with edits allowed.
pub fn active(state: &AppState) -> bool {
    let slice = state.render_mode() == RenderMode::Slice;
    if slice && state.slice_blocks_edits() {
        return false;
    }
    match state.tool {
        Tool::NoTrees => slice,
        Tool::SettingsHere => state.render_mode() != RenderMode::Force,
        _ => false,
    }
}

/// The area being drawn, for the overlay.
pub fn draft() -> Option<Draft> {
    session::with(|s| s.area)
}

/// The height (plate mm) of the layer the Sliced view shows on top.
pub fn shown_layer_z(state: &AppState) -> Option<f64> {
    let preview = state.print.preview.as_ref()?;
    let top = crate::print::timeline::shown_top_layer(&state.print, preview);
    preview.layers.iter().find(|l| l.layer == top).map(|l| l.z as f64)
}

/// The printer's bed (plate mm): (min, max). None without a printer.
fn bed(state: &AppState) -> Option<([f64; 2], [f64; 2])> {
    let (lo, hi) = (state.plate.bed_min, state.plate.bed_max);
    (hi[0] > lo[0] && hi[1] > lo[1]).then_some((lo, hi))
}

/// The point of the bed nearest `p` on its plane: on the bed, `p` itself; off it, on its edge.
fn onto(bed: ([f64; 2], [f64; 2]), p: [f64; 2]) -> [f64; 2] {
    std::array::from_fn(|k| p[k].clamp(bed.0[k], bed.1[k]))
}

/// The point (plate mm) a press at height `z` starts the area at: under the pointer, on that
/// plane over the bed (a press beside it starts over its edge). None without a bed, or off the
/// plane in front of the eye (the sky, or under it seen from above it).
fn press_at(state: &AppState, rect: Rect, pos: DVec2, z: f64, pane: PaneId) -> Option<[f64; 2]> {
    let bed = bed(state)?;
    let ray = ViewProjector::new(*state.view.camera(pane), rect).ray(pos);
    let t = (z as f32 * 0.001 - ray.origin.z) / ray.dir.z;
    let p = ray.origin + ray.dir * t;
    (ray.dir.z.abs() >= 1e-6 && t > 0.0 && p.is_finite()).then(|| onto(bed, [p.x as f64 * 1000.0, p.y as f64 * 1000.0]))
}

/// The point (plate mm) a drag at height `z` is at: under the pointer, on that plane over the
/// bed. Off the bed it sits over its edge; past the horizon, where the plane runs off to metres
/// and then is never met, over the edge the pointer looks towards. None only for a pointer
/// looking straight up or down off the plane.
fn drag_at(state: &AppState, rect: Rect, pos: DVec2, z: f64, pane: PaneId) -> Option<[f64; 2]> {
    let bed = bed(state)?;
    let ray = ViewProjector::new(*state.view.camera(pane), rect).ray(pos);
    let t = (z as f32 * 0.001 - ray.origin.z) / ray.dir.z;
    let p = ray.origin + ray.dir * t;
    if ray.dir.z.abs() >= 1e-6 && t > 0.0 && p.is_finite() {
        return Some(onto(bed, [p.x as f64 * 1000.0, p.y as f64 * 1000.0]));
    }
    let way = dvec2(ray.dir.x as f64, ray.dir.y as f64);
    (way.length() > 1e-9).then(|| {
        let far = way.normalize() * PAST_THE_BED;
        onto(bed, [ray.origin.x as f64 * 1000.0 + far.x, ray.origin.y as f64 * 1000.0 + far.y])
    })
}

/// The tool's share of the view's input: none when it is not [`active`], or for what it leaves
/// to the view (Shift's orbit, other buttons).
pub fn handle(cx: &mut Cx, input: &ViewportInput, state: &AppState, pane: PaneId) -> Option<InputResponse> {
    if !active(state) {
        if session::with(|s| s.area.take()).is_some() {
            crate::ui::redraw(cx, &[ids!(shape_handles)]);
        }
        return None;
    }
    let drawing = draft();
    let here = state.tool == Tool::SettingsHere;
    match input.kind {
        ViewportInputKind::PointerDown { button: PointerButton::Primary, pos, mods, .. } if !mods.shift => {
            let slice = state.render_mode() == RenderMode::Slice;
            let z = match (here, slice) {
                (false, _) => Some(0.0),
                (true, true) => shown_layer_z(state),
                // the Normal view: the height of the model under the press
                (true, false) => input.hit.map(|h| h.point.z as f64 * 1000.0),
            };
            let Some(z) = z else {
                log!("fab settings here: a press at {pos:?} with {}", if slice { "no layer shown" } else { "no model under it" });
                cx.action(ShellAction::Notice { view: NoticeView::Plate, text: if slice { NO_LAYER } else { OFF_THE_MODEL }.into() });
                return Some(InputResponse::consumed());
            };
            let (Some(at), Some(bed)) = (press_at(state, input.rect, pos, z, pane), bed(state)) else {
                log!("fab {}: a press at {pos:?} off the plane at z {z:.2}", if here { "settings here" } else { "no trees" });
                cx.action(ShellAction::Notice { view: NoticeView::Plate, text: if here { OFF_THE_LAYER } else { OFF_THE_BED }.into() });
                return Some(InputResponse::consumed());
            };
            session::with(|s| s.area = Some(Draft { from: at, to: at, disc: mods.alt, bed, z, tool: state.tool }));
            crate::ui::redraw(cx, &[ids!(shape_handles)]);
            Some(InputResponse::consumed())
        }
        ViewportInputKind::PointerMove { pos, mods, buttons, .. } if drawing.is_some() && buttons & 1 != 0 => {
            if let Some(at) = drawing.and_then(|d| drag_at(state, input.rect, pos, d.z, pane)) {
                session::with(|s| s.area = s.area.map(|d| Draft { to: at, disc: mods.alt, ..d }));
                crate::ui::redraw(cx, &[ids!(shape_handles)]);
            }
            Some(InputResponse { cursor: Some(MouseCursor::Crosshair), ..InputResponse::consumed() })
        }
        ViewportInputKind::PointerMove { buttons: 0, .. } => Some(InputResponse { cursor: Some(MouseCursor::Crosshair), ..Default::default() }),
        ViewportInputKind::PointerUp { button: PointerButton::Primary, pos, mods } if drawing.is_some() => {
            let at = drawing.and_then(|d| drag_at(state, input.rect, pos, d.z, pane));
            let done = session::with(|s| s.area.take()).map(|d| Draft { to: at.unwrap_or(d.to), disc: mods.alt, ..d });
            match done.and_then(|d| Some((d, d.area()?))) {
                // a box drawn is picked in the Select tool: its handles size it, a click on the
                // model picks the part
                Some((d, area)) if d.tool == Tool::SettingsHere => {
                    cx.action(PartAction::SettingsHere { area, z: d.z });
                    cx.action(ShellAction::SetTool(Tool::Select));
                }
                Some((_, area)) => cx.action(PartAction::NoTrees { area }),
                None => {
                    let here = done.is_some_and(|d| d.tool == Tool::SettingsHere);
                    log!("fab {}: a drag of {done:?} too small to draw", if here { "settings here" } else { "no trees" });
                    cx.action(ShellAction::Notice { view: NoticeView::Plate, text: if here { TOO_SMALL_HERE } else { TOO_SMALL }.into() });
                }
            }
            crate::ui::redraw(cx, &[ids!(shape_handles)]);
            Some(InputResponse::consumed())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bed of the person's printer (plate mm).
    const BED: ([f64; 2], [f64; 2]) = ([0.0, 0.0], [350.0, 320.0]);

    /// A drag across the bed is the rectangle across its ends whichever way it went, with Alt
    /// the disc round where it started, no wider than the bed round it; a click draws nothing.
    #[test]
    fn a_drag_draws_a_rectangle_or_a_disc_and_a_click_nothing() {
        let d = Draft { from: [130.0, 120.0], to: [110.0, 125.0], disc: false, bed: BED, z: 0.0, tool: Tool::NoTrees };
        assert_eq!(d.area(), Some(BedArea::Rect { min: [110.0, 120.0], max: [130.0, 125.0] }));
        assert_eq!(Draft { disc: true, ..d }.area(), Some(BedArea::Disc { centre: [130.0, 120.0], radius: 425f64.sqrt() }));
        assert_eq!(Draft { disc: true, to: [350.0, 120.0], ..d }.area(), Some(BedArea::Disc { centre: [130.0, 120.0], radius: 120.0 }), "a disc past the bed's edge");
        assert_eq!(Draft { to: [130.1, 120.1], ..d }.area(), None, "a click");
        assert_eq!(Draft { to: [110.0, 120.2], ..d }.area(), None, "a line");
    }

    /// What the view's input does with the tool, in the order the view hands it on (the
    /// navigator first, then the tools): the areas it asked for and what the plate view said.
    fn press(cx: &mut Cx, state: &mut AppState, rect: Rect, kinds: &[ViewportInputKind], pane: PaneId) -> (Vec<BedArea>, Vec<String>) {
        let mut nav = crate::nav::Navigator::default();
        let mut tools = crate::tools::ToolSet { pane, ..Default::default() };
        let actions = cx.capture_actions(|cx| {
            for &kind in kinds {
                let input = ViewportInput { rect, kind, hit: None };
                if !nav.handle(cx, &input, state).consumed {
                    tools.handle(cx, &input, state);
                }
            }
        });
        let areas = actions.iter().filter_map(|a| match a.downcast_ref::<PartAction>() {
            Some(PartAction::NoTrees { area }) => Some(*area),
            _ => None,
        }).collect();
        let said = actions.iter().filter_map(|a| match a.downcast_ref::<ShellAction>() {
            Some(ShellAction::Notice { view: NoticeView::Plate, text: m }) => Some(m.clone()),
            _ => None,
        }).collect();
        (areas, said)
    }

    /// The person's plate: Design, the Sliced view, the cube printing and the two parts beside
    /// it not, on their printer's bed, the tool picked from the rail; and the view.
    fn the_plate() -> (Cx, AppState, Rect) {
        let cx = Cx::new(Box::new(|_, _| {}));
        let mut state = AppState::default();
        state.ui.enter(Workspace::Design);
        state.print.play.active = true;
        state.tool = Tool::NoTrees;
        (state.plate.bed_min, state.plate.bed_max) = BED;
        for (object, printed, x) in [("bracket", false, 150.0), ("vase", false, 222.0), ("lattice_cube", true, 80.0)] {
            state.plate.parts.push(PlatePart { object: format!("{object}/{object}"), printed, min_mm: [x - 21.6, 143.4, 0.0], max_mm: [x + 21.6, 186.6, 43.2], ..Default::default() });
        }
        assert!(active(&state), "the tool takes the view's pointer");
        (cx, state, Rect { pos: dvec2(0.0, 0.0), size: dvec2(800.0, 600.0) })
    }

    /// A press at `a`, the pointer dragged to `b` (and on the way), let go there.
    fn drag(a: DVec2, b: DVec2) -> [ViewportInputKind; 4] {
        let mods = KeyModifiers::default();
        let to = |pos| ViewportInputKind::PointerMove { pos, delta: (b - a) * 0.5, mods, buttons: 1 };
        [ViewportInputKind::PointerDown { button: PointerButton::Primary, pos: a, mods, tap_count: 1 }, to((a + b) * 0.5), to(b), ViewportInputKind::PointerUp { button: PointerButton::Primary, pos: b, mods }]
    }

    /// A drag across the bed asks for the area (the app writes it into the part it is for); a
    /// press off the bed and a click each say in the plate view why nothing was drawn: no press
    /// of the tool goes by without a word.
    #[test]
    fn every_press_of_the_tool_draws_or_says_why_not() {
        let pane = crate::tools::ToolSet::default().pane;
        let (mut cx, mut state, rect) = the_plate();
        state.view.camera_mut(pane).target = vec3(0.08, 0.165, 0.0);
        state.view.camera_mut(pane).eye = vec3(0.08, 0.05, 0.12);
        let projector = ViewProjector::new(*state.view.camera(pane), rect);
        let on_screen = |x: f64, y: f64| projector.project(crate::tools::transform::mm_to_world([x, y, 0.0])).expect("in view");
        let (a, b) = (on_screen(55.0, 140.0), on_screen(75.0, 150.0));
        let mods = KeyModifiers::default();
        let down = |pos| ViewportInputKind::PointerDown { button: PointerButton::Primary, pos, mods, tap_count: 1 };
        let up = |pos| ViewportInputKind::PointerUp { button: PointerButton::Primary, pos, mods };
        let (areas, said) = press(&mut cx, &mut state, rect, &drag(a, b), pane);
        let near = |p: [f64; 2], q: [f64; 2]| (p[0] - q[0]).abs() < 0.01 && (p[1] - q[1]).abs() < 0.01;
        assert!(matches!(areas[..], [BedArea::Rect { min, max }] if near(min, [55.0, 140.0]) && near(max, [75.0, 150.0])), "{areas:?}");
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(press(&mut cx, &mut state, rect, &[down(a), up(a)], pane), (vec![], vec![TOO_SMALL.to_string()]), "a click");
        // Level with the bed, the view's top half is sky: a press there finds no bed.
        state.view.camera_mut(pane).eye = vec3(0.08, -0.2, 0.01);
        state.view.camera_mut(pane).target = vec3(0.08, 0.165, 0.01);
        let sky = dvec2(400.0, 60.0);
        assert_eq!(press(&mut cx, &mut state, rect, &[down(sky), up(sky)], pane), (vec![], vec![OFF_THE_BED.to_string()]), "a press off the bed");
        assert!(draft().is_none(), "nothing left half drawn");
    }

    /// Orthographic, low over the bed and zoomed out: the bed in front of the look point reaches
    /// nearer than the eye stood. The frame's fit stands the eye back behind it, so a drag there
    /// draws under the pointer, as in perspective.
    #[test]
    fn an_orthographic_drag_draws_under_the_pointer() {
        let pane = crate::tools::ToolSet::default().pane;
        let (mut cx, mut state, rect) = the_plate();
        state.view.camera_mut(pane).target = vec3(0.08, 0.165, 0.0);
        state.view.camera_mut(pane).eye = vec3(0.08, 0.05, 0.03);
        crate::nav::orbit::set_ortho(state.view.camera_mut(pane), true);
        state.view.camera_mut(pane).ortho_height = 0.6;
        let bounds = Aabb { min: vec3(0.0, 0.0, 0.0), max: vec3(BED.1[0] as f32 * 0.001, BED.1[1] as f32 * 0.001, 0.05) };
        state.view.camera_mut(pane).fit_clip_planes(&bounds, (rect.size.x / rect.size.y) as f32);
        let projector = ViewProjector::new(*state.view.camera(pane), rect);
        let on_screen = |x: f64, y: f64| projector.project(crate::tools::transform::mm_to_world([x, y, 0.0])).expect("in view");
        let (a, b) = (on_screen(20.0, 20.0), on_screen(60.0, 40.0));
        let (areas, said) = press(&mut cx, &mut state, rect, &drag(a, b), pane);
        let near = |p: [f64; 2], q: [f64; 2]| (p[0] - q[0]).abs() < 0.05 && (p[1] - q[1]).abs() < 0.05;
        assert!(matches!(areas[..], [BedArea::Rect { min, max }] if near(min, [20.0, 20.0]) && near(max, [60.0, 40.0])), "{areas:?}, said {said:?}");
    }

    /// Print settings here draws on the shown layer: a drag over the bed asks for a volume on
    /// the plane of the layer slider's top (where the pointer meets that plane, not the bed), at
    /// its height, and goes back to the Select tool; with no layer shown a press says why. The
    /// volume stands about the layer, as tall as the rectangle's shorter side, and on the bed
    /// when it would reach under it.
    #[test]
    fn print_settings_here_draws_on_the_shown_layer() {
        let pane = crate::tools::ToolSet::default().pane;
        let (mut cx, mut state, rect) = the_plate();
        state.tool = Tool::SettingsHere;
        state.view.camera_mut(pane).target = vec3(0.08, 0.165, 0.0);
        state.view.camera_mut(pane).eye = vec3(0.08, 0.05, 0.12);
        let projector = ViewProjector::new(*state.view.camera(pane), rect);
        let on_screen = |x: f64, y: f64, z: f64| projector.project(crate::tools::transform::mm_to_world([x, y, z])).expect("in view");
        let (a, b) = (on_screen(70.0, 150.0, 10.0), on_screen(90.0, 170.0, 10.0));
        let asked = |cx: &mut Cx, state: &mut AppState| {
            let actions = cx.capture_actions(|cx| {
                let mut tools = crate::tools::ToolSet { pane, ..Default::default() };
                for kind in drag(a, b) {
                    tools.handle(cx, &ViewportInput { rect, kind, hit: None }, state);
                }
            });
            let here: Vec<(BedArea, f64)> = actions.iter().filter_map(|a| match a.downcast_ref::<PartAction>() {
                Some(PartAction::SettingsHere { area, z }) => Some((*area, *z)),
                _ => None,
            }).collect();
            let said: Vec<String> = actions.iter().filter_map(|a| match a.downcast_ref::<ShellAction>() {
                Some(ShellAction::Notice { view: NoticeView::Plate, text: m }) => Some(m.clone()),
                _ => None,
            }).collect();
            let select = actions.iter().any(|a| matches!(a.downcast_ref::<ShellAction>(), Some(ShellAction::SetTool(Tool::Select))));
            (here, said, select)
        };
        assert_eq!(asked(&mut cx, &mut state), (vec![], vec![NO_LAYER.to_string()], false), "no layer shown yet");
        state.print.preview = Some(crate::print::follow::tests::preview_of(50));
        (state.print.play.layer_hi, state.print.play.time) = (49, 1e9);
        let (here, said, select) = asked(&mut cx, &mut state);
        assert!(select, "a box drawn goes back to the Select tool");
        let near = |p: [f64; 2], q: [f64; 2]| (p[0] - q[0]).abs() < 0.01 && (p[1] - q[1]).abs() < 0.01;
        assert!(matches!(here[..], [(BedArea::Rect { min, max }, z)] if near(min, [70.0, 150.0]) && near(max, [90.0, 170.0]) && (z - 10.0).abs() < 1e-4), "{here:?}");
        assert!(said.is_empty(), "{said:?}");
        assert!(draft().is_none(), "nothing left half drawn");
        let span = |w: f64, d: f64| settings_span(BedArea::Rect { min: [0.0, 0.0], max: [w, d] }, 10.0);
        assert_eq!(span(4.0, 6.0), (8.0, 12.0), "a small box about the layer");
        assert_eq!(span(30.0, 25.0), (0.0, 25.0), "a big one stands on the bed");
        assert_eq!(settings_span(BedArea::Disc { centre: [0.0, 0.0], radius: 3.0 }, 10.0), (7.0, 13.0), "a sphere");
    }

    /// The person's slip: a drag that ran off towards the horizon drew a box of 2.6 by 18.4 m,
    /// and the supports planner stalled on it. Seen from low over the bed (the horizon across the
    /// view's middle), a drag from the bed to beside it, to just under the horizon (where the
    /// bed's plane is metres off) and past it into the sky each draw on the bed alone: the end
    /// off it sits on its edge, past the horizon on the edge the pointer looks towards.
    #[test]
    fn a_drag_off_the_bed_or_past_the_horizon_draws_on_the_bed() {
        let pane = crate::tools::ToolSet::default().pane;
        let (mut cx, mut state, rect) = the_plate();
        state.view.camera_mut(pane).eye = vec3(0.08, -0.2, 0.01);
        state.view.camera_mut(pane).target = vec3(0.08, 0.165, 0.01);
        let projector = ViewProjector::new(*state.view.camera(pane), rect);
        let on_screen = |x: f64, y: f64| projector.project(crate::tools::transform::mm_to_world([x, y, 0.0])).expect("in view");
        let from = on_screen(60.0, 100.0);
        let ahead = on_screen(80.0, 100.0).x;
        let near = |a: f64, b: f64| (a - b).abs() < 0.5;
        for (to, what, min, max) in [
            (on_screen(-30.0, 200.0), "beside the bed", [0.0, 100.0], [60.0, 200.0]),
            (dvec2(from.x + 150.0, 301.0), "just under the horizon", [60.0, 100.0], BED.1),
            (dvec2(ahead, 200.0), "past the horizon", [60.0, 100.0], [80.0, BED.1[1]]),
            (dvec2(from.x + 150.0, 200.0), "past the horizon to the right", [60.0, 100.0], BED.1),
        ] {
            let (areas, said) = press(&mut cx, &mut state, rect, &drag(from, to), pane);
            let [BedArea::Rect { min: lo, max: hi }] = areas[..] else { panic!("{what}: {areas:?}, said {said:?}") };
            assert!((0..2).all(|k| near(lo[k], min[k]) && near(hi[k], max[k])), "{what}: {lo:?}..{hi:?}, not {min:?}..{max:?}");
        }
        // Alt's disc there too: no wider than the bed round its centre.
        let alt = KeyModifiers { alt: true, ..Default::default() };
        let mut disc = drag(from, dvec2(from.x + 150.0, 301.0));
        for k in &mut disc {
            match k {
                ViewportInputKind::PointerDown { mods, .. } | ViewportInputKind::PointerMove { mods, .. } | ViewportInputKind::PointerUp { mods, .. } => *mods = alt,
                _ => {}
            }
        }
        let (areas, _) = press(&mut cx, &mut state, rect, &disc, pane);
        assert!(matches!(areas[..], [BedArea::Disc { centre, radius }] if near(centre[0], 60.0) && near(centre[1], 100.0) && near(radius, 60.0)), "{areas:?}");
    }
}
