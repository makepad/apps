//! The view pie: Q held over a 3D view (a Plate view, the Product view) opens the views round
//! the pointer, each where the view looks from (Top up, Right right, …), Frame selected low
//! right, Frame all low left; the pointer aims, releasing Q takes the slot aimed at (none:
//! nothing) or a press on a slot takes it, Escape closes it. What it takes acts on its view: opening it makes its
//! view the one the camera commands act on (`ViewportState::camera_pane`).
//!
//! Its owner passes it the events first (`FabViewPie::take_event`): a press while it is open
//! is its alone. Each pill is a button by its name in the semantic tree while the pie is open.

use crate::api::{AppState, PaneId, PresetView, ShellAction};
use crate::ui::stage::StageAction;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // A pill of the view pie: its name on the floating cards' ground, with their drop shadow,
    // lit (the accent, black ink) while the pointer points at it (`hot`).
    let PiePill = View{
        width: #(PIE_PILL.x) height: #(PIE_PILL.y)
        align: Align{x: 0.5 y: 0.5}
        show_bg: true
        draw_bg: mod.draw.FabFloatBg{
            hot: instance(0.0)
            radius: uniform(#(PIE_PILL.y * 0.5 - 0.5))
            pixel: fn() {
                let lo = min(self.shadow_offset, vec2(0.0, 0.0)) - vec2(self.shadow_radius, self.shadow_radius)
                let hi = max(self.shadow_offset, vec2(0.0, 0.0)) + vec2(self.shadow_radius, self.shadow_radius)
                let p = self.pos * (self.rect_size + hi - lo) + lo
                let shade = GaussShadow.rounded_box_shadow(self.shadow_offset, self.rect_size + self.shadow_offset, p, self.shadow_radius * 0.5, self.radius)
                let sdf = Sdf2d.viewport(p)
                sdf.clear(self.shadow_color * shade)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, self.radius)
                sdf.fill_keep(mix(self.color, fab.color_accent, self.hot))
                sdf.stroke(self.border_color, 1.0)
                return sdf.result
            }
        }
        label := mod.widgets.FabLabel{
            text: ""
            draw_text +: {
                hot: instance(0.0)
                get_color: fn() { return mix(fab.color_text, fab.color_text_on_accent, self.hot) }
            }
        }
    }
    mod.widgets.FabViewPieBase = #(FabViewPie::register_widget(vm))
    mod.widgets.FabViewPie = set_type_default() do mod.widgets.FabViewPieBase{
        // over the whole view (its rect is where Q opens it); nothing of it draws while closed
        width: Fill height: Fill flow: Overlay
        pie_hub := View{
            visible: false
            width: #(PIE_HUB) height: #(PIE_HUB)
            show_bg: true
            // (the pills' ground and shadow, round, a dot at the centre)
            draw_bg: mod.draw.FabFloatBg{
                radius: uniform(#(PIE_HUB * 0.5 - 0.5))
                pixel: fn() {
                    let lo = min(self.shadow_offset, vec2(0.0, 0.0)) - vec2(self.shadow_radius, self.shadow_radius)
                    let hi = max(self.shadow_offset, vec2(0.0, 0.0)) + vec2(self.shadow_radius, self.shadow_radius)
                    let p = self.pos * (self.rect_size + hi - lo) + lo
                    let shade = GaussShadow.rounded_box_shadow(self.shadow_offset, self.rect_size + self.shadow_offset, p, self.shadow_radius * 0.5, self.radius)
                    let sdf = Sdf2d.viewport(p)
                    sdf.clear(self.shadow_color * shade)
                    let r = self.rect_size.x * 0.5
                    sdf.circle(r, r, r - 1.0)
                    sdf.fill_keep(self.color)
                    sdf.stroke(self.border_color, 1.0)
                    sdf.circle(r, r, 2.5)
                    sdf.fill(fab.color_text)
                    return sdf.result
                }
            }
        }
        pie0 := PiePill{ visible: false a11y: {role: "button" label: "Top"} label +: { text: "Top" } }
        pie1 := PiePill{ visible: false a11y: {role: "button" label: "Back"} label +: { text: "Back" } }
        pie2 := PiePill{ visible: false a11y: {role: "button" label: "Right"} label +: { text: "Right" } }
        pie3 := PiePill{ visible: false a11y: {role: "button" label: "Frame selected"} label +: { text: "Frame selected" } }
        pie4 := PiePill{ visible: false a11y: {role: "button" label: "Bottom"} label +: { text: "Bottom" } }
        pie5 := PiePill{ visible: false a11y: {role: "button" label: "Frame all"} label +: { text: "Frame all" } }
        pie6 := PiePill{ visible: false a11y: {role: "button" label: "Left"} label +: { text: "Left" } }
        pie7 := PiePill{ visible: false a11y: {role: "button" label: "Front"} label +: { text: "Front" } }
    }
}

/// The view pie's pills (points), the hub's side, how far the pills stand from the centre, and
/// how far the pointer goes from it before a direction counts.
const PIE_PILL: DVec2 = DVec2 { x: 112.0, y: 26.0 };
const PIE_HUB: f64 = 26.0;
const PIE_RADIUS: f64 = 124.0;
const PIE_DEAD: f64 = 16.0;

/// The view pie's slot the pointer points at from `centre`: none in the dead zone, else the
/// nearest of the eight compass directions (0 north, clockwise).
fn pie_hot(centre: DVec2, pointer: DVec2) -> Option<u8> {
    let d = pointer - centre;
    if d.length() < PIE_DEAD {
        return None;
    }
    Some(((d.x.atan2(-d.y) / std::f64::consts::FRAC_PI_4).round() as i32).rem_euclid(8) as u8)
}

/// What the view pie's slot does: the view from where it stands (Top up, Right right, …), Front
/// and Back the upper diagonals, the framings the lower ones.
fn pie_action(slot: u8) -> ShellAction {
    match slot {
        0 => ShellAction::PresetView(PresetView::Top),
        1 => ShellAction::PresetView(PresetView::Back),
        2 => ShellAction::PresetView(PresetView::Right),
        3 => ShellAction::FrameSelected,
        4 => ShellAction::PresetView(PresetView::Bottom),
        5 => ShellAction::FrameAll,
        6 => ShellAction::PresetView(PresetView::Left),
        _ => ShellAction::PresetView(PresetView::Front),
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabViewPie {
    #[deref]
    view: View,
    /// The view it acts on (its owner's pane).
    #[rust]
    pub pane: Option<PaneId>,
    /// While Q is held: its centre (window points) and the slot pointed at.
    #[rust]
    pie: Option<(DVec2, Option<u8>)>,
    /// The pointer's last place over the window, where the pie opens.
    #[rust]
    pointer: DVec2,
    /// Whether it showed at its last draw.
    #[rust]
    shown: bool,
}

impl FabViewPie {
    /// The pie's events: whether the event was the pie's alone (a press or Escape while it is
    /// open). Its owner calls this before its own handling; the generic event pass skips it.
    pub fn take_event(&mut self, cx: &mut Cx, event: &Event, state: &mut AppState) -> bool {
        match event {
            Event::MouseMove(e) => {
                self.pointer = e.abs;
                if let Some((centre, hot)) = &mut self.pie {
                    let aimed = pie_hot(*centre, e.abs);
                    if aimed != *hot {
                        *hot = aimed;
                        self.view.redraw(cx);
                    }
                }
                false
            }
            Event::Actions(actions) if actions.iter().any(|a| matches!(a.downcast_ref::<StageAction>(), Some(StageAction::ViewPie))) => {
                let view = self.view.area().rect(cx);
                if self.pie.is_none() && view.contains(self.pointer) {
                    // round the pointer, the ring kept inside the view
                    let reach = dvec2(PIE_RADIUS + PIE_PILL.x * 0.5 + 6.0, PIE_RADIUS + PIE_PILL.y * 0.5 + 6.0);
                    let lo = view.pos + reach;
                    let hi = view.pos + view.size - reach;
                    let centre = dvec2(self.pointer.x.clamp(lo.x, hi.x.max(lo.x)), self.pointer.y.clamp(lo.y, hi.y.max(lo.y)));
                    self.pie = Some((centre, pie_hot(centre, self.pointer)));
                    // what it takes acts on this view
                    if let Some(pane) = self.pane {
                        state.view.pressed_pane = Some(pane);
                    }
                    self.view.redraw(cx);
                }
                false
            }
            Event::KeyUp(ke) if ke.key_code == KeyCode::KeyQ => {
                if let Some((_, hot)) = self.pie.take() {
                    if let Some(slot) = hot {
                        cx.action(pie_action(slot));
                    }
                    self.view.redraw(cx);
                }
                false
            }
            Event::KeyDown(ke) if ke.key_code == KeyCode::Escape && self.pie.is_some() => {
                self.pie = None;
                self.view.redraw(cx);
                true
            }
            // a press takes the slot it is on (the dead zone: none), and closes the pie
            Event::MouseDown(e) if self.pie.is_some() => {
                if let Some((centre, _)) = self.pie.take() {
                    if let Some(slot) = pie_hot(centre, e.abs) {
                        cx.action(pie_action(slot));
                    }
                }
                self.view.redraw(cx);
                true
            }
            _ => false,
        }
    }

    /// Its pills round its centre as it draws, the one aimed at lit.
    fn sync(&mut self, cx: &mut Cx) {
        let open = self.pie.is_some();
        let pills = [ids!(pie0), ids!(pie1), ids!(pie2), ids!(pie3), ids!(pie4), ids!(pie5), ids!(pie6), ids!(pie7)];
        if open != self.shown {
            self.shown = open;
            self.view.widget(cx, ids!(pie_hub)).set_visible(cx, open);
            for path in pills {
                self.view.widget(cx, path).set_visible(cx, open);
            }
        }
        let Some((centre, hot)) = self.pie else { return };
        if let Some(mut hub) = self.view.widget(cx, ids!(pie_hub)).borrow_mut::<View>() {
            hub.walk.abs_pos = Some(centre - DVec2::all(PIE_HUB * 0.5));
        }
        for (k, path) in pills.into_iter().enumerate() {
            let angle = k as f64 * std::f64::consts::FRAC_PI_4;
            let at = centre + dvec2(angle.sin(), -angle.cos()) * PIE_RADIUS - PIE_PILL * 0.5;
            let lit = if hot == Some(k as u8) { 1.0 } else { 0.0 };
            let pill = self.view.widget(cx, path);
            if let Some(mut view) = pill.borrow_mut::<View>() {
                view.walk.abs_pos = Some(at);
                view.draw_bg.set_dyn_instance(cx, live_id!(hot), &[lit]);
            }
            if let Some(mut label) = pill.label(cx, ids!(label)).borrow_mut() {
                label.draw_text.set_dyn_instance(cx, live_id!(hot), &[lit]);
            }
        }
    }
}

impl Widget for FabViewPie {
    // (its owner passes it its events: `take_event`)
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.sync(cx);
        self.view.draw_walk(cx, scope, walk)
    }
}
