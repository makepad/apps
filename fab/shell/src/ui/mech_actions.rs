//! The Mechanics action menu: the Plate editor's header strip in Mechanics, a row of icon buttons
//! across the area's top with the area's nub as its left end (the look of a tab bar), and the
//! same actions on a right-click on a part. Every action applies to the selected parts, or to
//! all parts when nothing is selected (`MechAction::Do`); play, pause, step and speed are the sim's.
//! Hovering never changes the layout: the hints are tooltips.

use crate::api::*;
use crate::api::PaneId;
use crate::ui::popover::{dropdown_clicked, menu_picked, open_menu, speed_in_menu, speed_label, speed_menu, speed_pick, MenuItem, MenuPlace, SpeedMenuKind};
use makepad_widgets_core::*;

const SPEED_MENU: LiveId = live_id!(fab_mech_speed);
const CONTACT_MENU: LiveId = live_id!(fab_mech_contact);
const CONTEXT_MENU: LiveId = live_id!(fab_mech_context);

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // a strip's controls, as tall as the plate's view bar toggles
    let Button = mod.widgets.FabIconButton{
        width: 28 height: 24 padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
    }
    let Toggle = mod.widgets.FabIconToggle{
        independent: true
        width: 28 height: 24 padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
    }
    // a run's rule: the toolbars' (a plain View's quad draws nothing of itself)
    let Rule = mod.widgets.FabVRule{ margin: Inset{left: 5 right: 5} }
    // A group of the bar's controls; each after the first leads with a rule.
    let Run = View{ width: Fit height: Fit flow: Right align: Align{y: 0.5} spacing: 1 }

    mod.widgets.FabMechActionsBase = #(FabMechActions::register_widget(vm))
    mod.widgets.FabMechActions = set_type_default() do mod.widgets.FabMechActionsBase{
        width: Fill
        height: Fill
        flow: Down
        align: Align{x: 0.5 y: 0.0}
        // The header strip: the plate view's toolbar in Mechanics, as every editor's: one row that
        // clips where the view narrows and scrolls sideways (drag it, or the wheel), its left end
        // the area's editor selector (the notch layer draws it there) and a rule.
        strip := mod.widgets.FabEditorBar{
            cursor: MouseCursor.Default
            bar := View{
                width: Fit height: Fit flow: Right align: Align{y: 0.5}
                padding: Inset{top: 8 bottom: 8 left: 0 right: 0} spacing: 1
                a11y: {role: "toolbar" label: "Mechanics actions"}
                run_parts := Run{
                    // the plate's mode first, as on the plate's own bar in the other modes, so the
                    // sim can be left for Design or Monitor
                    FabTip{ text: "What the plate is for: Design edits it, Mechanics runs it in a physics sim, Monitor shows the printer's job. It stays as it is across workspaces."
                        plate_mode := mod.widgets.FabDropdownButton{
                            height: 24
                            a11y: {label: "Plate mode"}
                            tag: @fab_plate_mode
                            owner: @fab_plate_mode
                            label +: { text: "Mechanics" }
                        }
                    }
                    Rule{}
                    FabTip{ text: "Drop: let the selected parts fall onto the plate (all parts when none is selected)"
                        drop := Button{ a11y: {label: "Drop"} draw_icon +: { svg: crate_resource("self://resources/icons/drop.svg") } }
                    }
                    FabTip{ text: "Hang: pin the selected parts at their top and let them dangle"
                        hang := Button{ a11y: {label: "Hang"} draw_icon +: { svg: crate_resource("self://resources/icons/hang.svg") } }
                    }
                    FabTip{ text: "Pin or unpin the selected parts where they are"
                        pin := Toggle{ a11y: {label: "Pin"} draw_icon +: { svg: crate_resource("self://resources/icons/pin.svg") } }
                    }
                    FabTip{ text: "Throw the selected parts away from the camera"
                        throw := Button{ a11y: {label: "Throw"} draw_icon +: { svg: crate_resource("self://resources/icons/throw.svg") } }
                    }
                    FabTip{ text: "Reset the selected parts to their start pose"
                        reset := Button{ a11y: {label: "Reset"} draw_icon +: { svg: crate_resource("self://resources/icons/reset.svg") } }
                    }
                    FabTip{ text: #(crate::ui::keymap::tooltip(crate::ui::keymap::ShortcutId::MechReset))
                        reset_all := Button{ a11y: {label: "Reset all"} draw_icon +: { svg: crate_resource("self://resources/icons/refresh.svg") } }
                    }
                    FabTip{ text: "Freeze or unfreeze the selected parts"
                        freeze := Toggle{ a11y: {label: "Freeze"} draw_icon +: { svg: crate_resource("self://resources/icons/freeze.svg") } }
                    }
                }
                run_scene := Run{
                    rule := Rule{}
                    // the view's tool rails, on or off, as on the plate's own bar
                    FabTip{ text: "Show the tool rails at the view's left"
                        mech_tools := Toggle{ a11y: {label: "Tools"} draw_icon +: { svg: crate_resource("self://resources/icons/select.svg") } }
                    }
                    FabTip{ text: "Supports: this plate's planned supports as loose pieces, already broken free; whether each can leave the part"
                        supports := Toggle{ a11y: {label: "Supports"} draw_icon +: { svg: crate_resource("self://resources/icons/supports.svg") } }
                    }
                    FabTip{ text: "Shake what is held"
                        shake := Toggle{ a11y: {label: "Shake"} draw_icon +: { svg: crate_resource("self://resources/icons/shake.svg") } }
                    }
                    FabTip{ text: "Closed box: the board's case closed with the board in it; press its keys, and its light pipes show its LEDs"
                        closed := Toggle{ a11y: {label: "Closed box"} draw_icon +: { svg: crate_resource("self://resources/icons/closed_box.svg") } }
                    }
                    FabTip{ text: "Listen: hear the board's output on the speakers"
                        listen := Toggle{ a11y: {label: "Listen"} visible: false draw_icon +: { svg: crate_resource("self://resources/icons/listen.svg") } }
                    }
                }
                run_play := Run{
                    rule := Rule{}
                    FabTip{ text: #(format!("Play ({})", crate::ui::keymap::shortcut(crate::ui::keymap::ShortcutId::MechPlay)))
                        play := Button{ a11y: {label: "Play"} draw_icon +: { svg: crate_resource("self://resources/icons/play.svg") } }
                    }
                    FabTip{ text: #(format!("Pause ({})", crate::ui::keymap::shortcut(crate::ui::keymap::ShortcutId::MechPlay)))
                        pause := Button{ a11y: {label: "Pause"} visible: false draw_icon +: { svg: crate_resource("self://resources/icons/pause.svg") } }
                    }
                    FabTip{ text: "Step one frame (while paused)"
                        step := Button{ a11y: {label: "Step"} draw_icon +: { svg: crate_resource("self://resources/icons/skip_fwd.svg") } }
                    }
                    FabTip{ text: "Simulation speed, slow motion to fast"
                        speed := mod.widgets.FabDropdownButton{ height: 24 tag: @fab_mech_speed owner: @fab_mech_speed label +: { text: "1×" } }
                    }
                    FabTip{ text: "Contact geometry: Fast builds in a moment; Exact reads within a quarter layer of the true distance at sharp edges and in narrow gaps, and takes longer to build"
                        contact := mod.widgets.FabDropdownButton{ height: 24 tag: @fab_mech_contact owner: @fab_mech_contact label +: { text: "Fast" } }
                    }
                }
            }
        }
        // What Listen plays, under the bar while the closed box is heard: below real time a
        // stretch renders (the bar fills) and then plays round while the next one renders.
        listen_strip := View{
            width: Fit height: Fit flow: Down spacing: 4 visible: false
            margin: Inset{top: 6}
            padding: Inset{left: 10 right: 10 top: 5 bottom: 6}
            show_bg: true
            draw_bg +: {
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                    sdf.fill_keep(fab.color_float)
                    sdf.stroke(fab.color_float_border, 1.0)
                    return sdf.result
                }
            }
            line := mod.widgets.FabLabelDim{ width: Fit text: "" }
            progress := mod.widgets.FabProgress{ width: Fill height: 4 visible: false }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabMechActions {
    #[deref]
    view: View,
    /// The header strip's height as last drawn (0: not drawn yet).
    #[rust]
    strip_h: f64,
}

impl FabMechActions {
    /// The header strip's height as last drawn (0 before its first draw): a toolbar's. Read before the strip draws in a pass, its area is not
    /// there yet, so [`Self::measure`] keeps it. O(1).
    pub fn strip_height(&self) -> f64 {
        self.strip_h
    }

    /// The strip's height as just drawn (its area holds only after the draw); a height that
    /// changed asks for one more draw. One rect read per draw.
    fn measure(&mut self, cx: &mut Cx2d) {
        let strip_h = self.view.widget(cx, ids!(strip)).area().rect(cx).size.y;
        if (strip_h - self.strip_h).abs() > 0.25 {
            self.strip_h = strip_h;
            self.view.redraw(cx);
        }
    }

    /// Select the part under a right-click (unless it already is) and open the menu on it.
    pub(super) fn open_context(&self, cx: &mut Cx, state: &AppState, pane: PaneId, at: DVec2) {
        if !state.mech.ready || self.view.widget(cx, ids!(strip)).area().rect(cx).contains(at) {
            return;
        }
        let rect = self.view.area().rect(cx);
        let ray = ViewProjector::new(*state.view.camera(pane), rect).ray(at);
        let Some(hit) = crate::viewport::pick_ray(state, pane, &ray) else { return };
        if state.plate.is_context(hit.element) {
            return;
        }
        if !state.scene_state.selection.set.contains(&hit.element) {
            cx.action(ShellAction::SelectOnly(hit.element));
        }
        let items = vec![
            MenuItem::new(live_id!(mech_drop), "Drop"),
            MenuItem::new(live_id!(mech_hang), "Hang"),
            if state.mech.targets_pinned {
                MenuItem::new(live_id!(mech_unpin), "Unpin")
            } else {
                MenuItem::new(live_id!(mech_pin), "Pin")
            },
            MenuItem::new(live_id!(mech_throw), "Throw"),
            MenuItem::new(live_id!(mech_reset), "Reset to start pose"),
            MenuItem::sep(),
            if state.mech.targets_frozen {
                MenuItem::new(live_id!(mech_thaw), "Unfreeze")
            } else {
                MenuItem::new(live_id!(mech_freeze), "Freeze")
            },
        ];
        open_menu(cx, CONTEXT_MENU, items, Rect { pos: at, size: dvec2(0.0, 0.0) }, MenuPlace::At);
    }
}

fn picked_op(id: LiveId) -> Option<MechOp> {
    Some(match id {
        id if id == live_id!(mech_drop) => MechOp::Drop,
        id if id == live_id!(mech_hang) => MechOp::Hang,
        id if id == live_id!(mech_pin) => MechOp::Pin,
        id if id == live_id!(mech_unpin) => MechOp::Unpin,
        id if id == live_id!(mech_throw) => MechOp::Throw,
        id if id == live_id!(mech_reset) => MechOp::Home,
        id if id == live_id!(mech_freeze) => MechOp::Freeze(true),
        id if id == live_id!(mech_thaw) => MechOp::Freeze(false),
        _ => return None,
    })
}

impl Widget for FabMechActions {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Mechanics actions");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Some(state) = scope.data.get::<AppState>() else { return };
        if !state.ui.in_mode(PlateMode::Mechanics) {
            return;
        }
        match event {
            Event::Actions(actions) => {
                let bar = self.view.view(cx, ids!(bar));
                let m = &state.mech;
                let op = |cx: &mut Cx, op: MechOp| cx.action(MechAction::Do(op));
                if bar.button(cx, ids!(drop)).clicked(actions) {
                    op(cx, MechOp::Drop);
                }
                if bar.button(cx, ids!(hang)).clicked(actions) {
                    op(cx, MechOp::Hang);
                }
                if bar.radio_button(cx, ids!(pin)).clicked(actions) {
                    op(cx, if m.targets_pinned { MechOp::Unpin } else { MechOp::Pin });
                }
                if bar.button(cx, ids!(throw)).clicked(actions) {
                    op(cx, MechOp::Throw);
                }
                if bar.button(cx, ids!(reset)).clicked(actions) {
                    op(cx, MechOp::Home);
                }
                if bar.button(cx, ids!(reset_all)).clicked(actions) {
                    cx.action(MechAction::Reset);
                }
                if bar.radio_button(cx, ids!(freeze)).clicked(actions) {
                    op(cx, MechOp::Freeze(!m.targets_frozen));
                }
                if bar.radio_button(cx, ids!(supports)).clicked(actions) {
                    cx.action(MechAction::Scene(if m.scene == MechScene::Supports { MechScene::Drop } else { MechScene::Supports }));
                }
                if bar.radio_button(cx, ids!(shake)).clicked(actions) {
                    cx.action(MechAction::Shake(!m.shaking));
                }
                if bar.radio_button(cx, ids!(closed)).clicked(actions) {
                    cx.action(MechAction::Scene(if m.scene == MechScene::Closed { MechScene::Drop } else { MechScene::Closed }));
                }
                if bar.radio_button(cx, ids!(listen)).clicked(actions) {
                    cx.action(MechAction::Listen(!m.listening));
                }
                if bar.button(cx, ids!(play)).clicked(actions) {
                    cx.action(MechAction::Play);
                }
                if bar.button(cx, ids!(pause)).clicked(actions) {
                    cx.action(MechAction::Pause);
                }
                if bar.button(cx, ids!(step)).clicked(actions) {
                    cx.action(MechAction::Step);
                }
                if let Some(anchor) = dropdown_clicked(actions, SPEED_MENU) {
                    let items = speed_menu(m.speed).into_iter().filter(|item| speed_in_menu(item.id, SpeedMenuKind::Mechanics)).collect();
                    open_menu(cx, SPEED_MENU, items, anchor, MenuPlace::Below);
                }
                if let Some(id) = menu_picked(actions, SPEED_MENU) {
                    if let Some(speed) = speed_pick(id) {
                        cx.action(MechAction::Speed(speed));
                    }
                }
                if let Some(anchor) = dropdown_clicked(actions, CONTACT_MENU) {
                    let items = vec![
                        MenuItem::new(live_id!(contact_fast), "Contact geometry: Fast").radio(m.contact == MechContact::Fast),
                        MenuItem::new(live_id!(contact_exact), "Contact geometry: Exact").radio(m.contact == MechContact::Exact),
                    ];
                    open_menu(cx, CONTACT_MENU, items, anchor, MenuPlace::Below);
                }
                if let Some(id) = menu_picked(actions, CONTACT_MENU) {
                    if id == live_id!(contact_fast) {
                        cx.action(MechAction::Contact(MechContact::Fast));
                    } else if id == live_id!(contact_exact) {
                        cx.action(MechAction::Contact(MechContact::Exact));
                    }
                }
                if let Some(id) = menu_picked(actions, CONTEXT_MENU) {
                    if let Some(o) = picked_op(id) {
                        cx.action(MechAction::Do(o));
                    }
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(s) = scope.data.get::<AppState>() {
            let m = &s.mech;
            let bar = self.view.view(cx, ids!(bar));
            for (id, on) in [(ids!(pin), m.targets_pinned), (ids!(freeze), m.targets_frozen), (ids!(supports), m.scene == MechScene::Supports), (ids!(shake), m.shaking), (ids!(closed), m.scene == MechScene::Closed), (ids!(listen), m.listening)] {
                let b = bar.radio_button(cx, id);
                if b.active(cx) != on {
                    b.set_active(cx, on, Animate::No);
                }
            }
            bar.radio_button(cx, ids!(listen)).set_visible(cx, m.scene == MechScene::Closed);
            let strip = self.view.view(cx, ids!(listen_strip));
            strip.set_visible(cx, m.scene == MechScene::Closed && m.listening && !m.listen_line.is_empty());
            strip.label(cx, ids!(line)).set_text(cx, &m.listen_line);
            let progress = strip.view(cx, ids!(progress));
            progress.set_visible(cx, m.listen_progress.is_some());
            if let Some(f) = m.listen_progress {
                crate::ui::widgets::set_progress(cx, &progress, f);
            }
            bar.button(cx, ids!(play)).set_visible(cx, !m.running);
            bar.button(cx, ids!(pause)).set_visible(cx, m.running);
            bar.view(cx, ids!(speed)).label(cx, ids!(label)).set_text(cx, &speed_text(m));
            bar.view(cx, ids!(contact)).label(cx, ids!(label)).set_text(cx, contact_label(m.contact));
        }
        let step = self.view.draw_walk(cx, scope, walk);
        self.measure(cx);
        step
    }
}

/// The speed as the bar shows it: the setting, or while a board's bench leads the sim's clock,
/// "Bench" and the pace the pair reaches (three significant digits, as the bench's transport
/// writes it), so a slow pair reads as slow, and "· motion" while the bench waits for this sim's
/// motion (this sim is the slower side).
pub fn speed_text(m: &MechView) -> String {
    match m.lead {
        Some(MechLead { rate: Some(rate), waits_for_motion }) if rate.is_finite() && rate > 0.0 => format!("Bench {}×{}",
            fab_elec_view::live::strip::format_reached(rate), if waits_for_motion { " · motion" } else { "" }),
        Some(_) => "Bench".to_string(),
        None => speed_label(m.speed),
    }
}

/// The contact setting as its dropdown shows it.
fn contact_label(c: MechContact) -> &'static str {
    match c {
        MechContact::Fast => "Fast",
        MechContact::Exact => "Exact",
    }
}
