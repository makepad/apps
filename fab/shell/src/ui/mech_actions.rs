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
    let Rule = View{ width: 1 height: 16 margin: Inset{left: 4 right: 4} show_bg: true draw_bg +: { color: fab.color_border_light } }
    // A run of the bar's controls that stays together: a narrow view wraps the bar between runs
    // (`FabMechActions::arrange`), and inside one only when the run alone is wider than the view.
    // Each run after the first leads with a rule, shown unless the run starts a row.
    let Run = View{ width: Fit height: Fit flow: Right align: Align{y: 0.5} spacing: 1 }

    mod.widgets.FabMechActionsBase = #(FabMechActions::register_widget(vm))
    mod.widgets.FabMechActions = set_type_default() do mod.widgets.FabMechActionsBase{
        width: Fill
        height: Fill
        flow: Down
        align: Align{x: 0.5 y: 0.0}
        // The header strip: a tab bar's ground and rule across the area's top, its left end the
        // area's nub (`STRIP_LEFT`), each row as tall as a tab bar.
        strip := View{
            width: Fill height: Fit flow: Right
            cursor: MouseCursor.Default
            padding: Inset{left: 44 right: 8}
            show_bg: true
            draw_bg +: {
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                    sdf.fill(fab.color_header)
                    sdf.rect(0.0, self.rect_size.y - 1.0, self.rect_size.x, 1.0)
                    sdf.fill(fab.color_border)
                    return sdf.result
                }
            }
            bar := View{
                width: Fit height: Fit flow: Right align: Align{y: 0.5}
                padding: Inset{top: 2 bottom: 2 left: 0 right: 0} spacing: 1
                a11y: {role: "toolbar" label: "Mechanics actions"}
                run_parts := Run{
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
                    sdf.fill_keep(vec4(fab.color_float.xyz, 0.94))
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
    /// Each run's width without its leading rule, as last drawn (0: not drawn yet).
    #[rust]
    runs: [f64; 3],
    /// The header strip's height as last drawn (0: not drawn yet).
    #[rust]
    strip_h: f64,
}

/// The bar's runs, in order.
fn runs() -> [&'static [LiveId]; 3] {
    [ids!(bar.run_parts), ids!(bar.run_scene), ids!(bar.run_play)]
}

/// What a run's leading rule takes: the rule (1) with its margins (4 + 4) and the run's spacing (1).
const RULE_W: f64 = 10.0;
/// The gap between the bar's runs.
const RUN_GAP: f64 = 1.0;
/// What the strip keeps clear: the area's nub at its left end, and a margin at its right.
const STRIP_LEFT: f64 = 44.0;
const STRIP_RIGHT: f64 = 8.0;

/// How runs of `widths` (rules not counted) fill rows of `room` in order, each run after a row's
/// first adding its rule: which runs start a row, the width a run wider than the room is squeezed
/// to (it then wraps its own controls, on rows of its own), and the widest row's inner width.
fn arrange(widths: &[f64; 3], room: f64) -> ([bool; 3], [Option<f64>; 3], f64) {
    let room = room.max(1.0);
    let mut starts = [false; 3];
    let mut squeeze = [None; 3];
    let (mut row, mut widest) = (0.0f64, 0.0f64);
    for (i, &natural) in widths.iter().enumerate() {
        let w = if natural > room {
            squeeze[i] = Some(room);
            room
        } else {
            natural
        };
        let joined = row + RUN_GAP + RULE_W + w;
        if i == 0 || joined > room {
            starts[i] = true;
            row = w;
        } else {
            row = joined;
        }
        widest = widest.max(row);
    }
    (starts, squeeze, widest)
}

impl FabMechActions {
    /// The header strip's height as last drawn (0 before its first draw): one row of a tab bar's
    /// height, more when the bar wraps. Read before the strip draws in a pass, its area is not
    /// there yet, so [`Self::measure`] keeps it. O(1).
    pub fn strip_height(&self) -> f64 {
        self.strip_h
    }

    /// The bar in one row while it fits the strip (less the nub at its left end), else wrapped
    /// between its runs, each row's first run without its rule; a run wider than the room on its
    /// own wraps its controls as well. The arrangement is set before the
    /// bar draws, from the run widths [`Self::measure`] took after the last draw; the very first
    /// draw is one row. It is read back from the widgets, never kept apart from them (a design
    /// reload puts their defaults back). Per draw: six lookups and the bar's walk. O(1).
    fn arrange(&mut self, cx: &mut Cx2d, walk: Walk) {
        if self.runs.contains(&0.0) {
            return;
        }
        let room = cx.peek_walk_turtle(walk).size.x - STRIP_LEFT - STRIP_RIGHT;
        let (starts, squeeze, widest) = arrange(&self.runs, room);
        for (i, run) in runs().into_iter().enumerate() {
            if i > 0 {
                let rule = self.view.view(cx, run).widget(cx, ids!(rule));
                if rule.visible() == starts[i] {
                    rule.set_visible(cx, !starts[i]);
                }
            }
            if let Some(mut run) = self.view.widget(cx, run).borrow_mut::<View>() {
                (run.walk.width, run.layout.flow) = match squeeze[i] {
                    Some(w) => (Size::Fixed(w), Flow::right_wrap()),
                    None => (Size::fit(), Flow::right()),
                };
            }
        }
        if let Some(mut bar) = self.view.widget(cx, ids!(bar)).borrow_mut::<View>() {
            if starts.iter().skip(1).any(|s| *s) {
                // a hair over the widest row, so the turtle breaks where the rows above did
                bar.walk.width = Size::Fixed(widest + 0.5);
                bar.layout.flow = Flow::right_wrap();
            } else {
                bar.walk.width = Size::fit();
                bar.layout.flow = Flow::right();
            }
        }
    }

    /// The strip's height and each run's width as just drawn, less its rule where one showed; a squeezed run keeps the
    /// natural width it had, which is what decides when it stops being squeezed. A width that changed (a
    /// label, Listen showing, the first draw) asks for one more draw, so the arrangement follows on
    /// the next frame; a settled bar asks for none. The rects are only good after the draw: one read
    /// before it sees the area this redraw is about to replace. Per draw: six lookups, three rect
    /// reads.
    fn measure(&mut self, cx: &mut Cx2d) {
        let mut changed = false;
        let strip_h = self.view.view(cx, ids!(strip)).area().rect(cx).size.y;
        if (strip_h - self.strip_h).abs() > 0.25 {
            self.strip_h = strip_h;
            changed = true;
        }
        for (i, run) in runs().into_iter().enumerate() {
            let squeezed = self.view.widget(cx, run).borrow::<View>().is_some_and(|v| matches!(v.walk.width, Size::Fixed(_)));
            let run = self.view.view(cx, run);
            let drawn = run.area().rect(cx).size.x;
            if squeezed || drawn <= 0.0 {
                continue;
            }
            let ruled = i > 0 && run.widget(cx, ids!(rule)).visible();
            let w = drawn - if ruled { RULE_W } else { 0.0 };
            if (w - self.runs[i]).abs() > 0.25 {
                self.runs[i] = w;
                changed = true;
            }
        }
        if changed {
            self.view.redraw(cx);
        }
    }

    /// Select the part under a right-click (unless it already is) and open the menu on it.
    pub(super) fn open_context(&self, cx: &mut Cx, state: &AppState, pane: PaneId, at: DVec2) {
        if !state.mech.ready || self.view.view(cx, ids!(strip)).area().rect(cx).contains(at) {
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
        self.arrange(cx, walk);
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
