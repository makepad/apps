//! The popup buttons: the header dropdown (label + SVG chevron,
//! `FabDropdownButton`) and the menu-bar title (`FabMenuButton`, which opens
//! on press and switches on hover). Hover/press lit; the one whose menu is up
//! carries the menu highlight.
//!
//! Pressing one is deliberately opinion-free: it reports
//! `FabUiAction::DropdownClicked` or `MenuBarOpen`. Whoever
//! owns that tag or owner builds the item list and raises the menu, and it keeps one
//! button style behind every menu in the app.
//!
//! The open look is never a per-button toggle. The button mirrors the menu
//! layer's `MenuOpened` / `MenuClosed` broadcast for its `owner`
//! ([`open_after`]), so a menu going down for *any* reason — a pick, Escape,
//! a click outside, another popup opening over it, focus loss — returns the
//! button to idle in the same pass. A `MenuClickAway` landing on this button
//! re-presses it: the press that dismissed a neighbour's menu opens this one
//! on that same click instead of needing a second. (Hover cannot linger
//! either: the layer clears hover tree-wide on every popup transition,
//! because its modal grab is what kept the hover-outs from arriving.)
//!
//! Registered with the control kit (before every panel) so panels can place it.

use crate::ui::popover::{open_after, ui_actions, FabUiAction};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabDropdownButtonBase = #(FabDropdownButton::register_widget(vm))
    mod.widgets.FabDropdownButton = set_type_default() do mod.widgets.FabDropdownButtonBase{
        width: Fit
        height: theme.prop_row_height
        flow: Right
        // The Fit-height label is centred on its ink. The DrawVector chevron
        // stays in a Fill-height, symmetrically padded slot, so y-align has no
        // deferred shift to apply to it.
        align: Align{x: 0.0 y: 0.5}
        padding: Inset{left: 6 right: 3 top: 0 bottom: 0}
        spacing: fab.chevron_gap
        cursor: MouseCursor.Hand
        show_bg: true
        tag: @editor_type
        draw_bg +: {
            hover: instance(0.0)
            down: instance(0.0)
            open: instance(0.0)
            focus: instance(0.0)
            disabled: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius)
                let fill = vec4(fab.color_button_hover.xyz, self.hover * 0.85 + self.down * 0.15)
                    .mix(vec4(fab.color_editor_alt.xyz, 0.4), self.disabled)
                    .mix(fab.color_menu_row_hover, self.open)
                sdf.fill_keep(fill)
                sdf.stroke(vec4(fab.color_focus_ring.xyz, self.focus), 1.0)
                return sdf.result
            }
        }
        animator: Animator{
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: { draw_bg: {hover: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {hover: 1.0} }
                }
            }
            down: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: { draw_bg: {down: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {down: 1.0} }
                }
            }
            open: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: { draw_bg: {open: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {open: 1.0} }
                }
            }
            focus: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: { draw_bg: {focus: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: { draw_bg: {focus: 1.0} }
                }
            }
            disabled: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: { draw_bg: {disabled: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: { draw_bg: {disabled: 1.0} }
                }
            }
        }
        label := mod.widgets.FabLabel{ height: Fit text: "Menu" }
        chevron_slot := View{
            width: fab.chevron_size
            height: Fill
            padding: Inset{top: 5 bottom: 5 left: 0 right: 0}
            chevron := mod.widgets.FabIconMuted{
                width: fab.chevron_size
                height: fab.chevron_size
                icon_walk: Walk{ width: fab.chevron_size height: fab.chevron_size }
                draw_icon +: {
                    svg: crate_resource("self://resources/icons/chevron_down.svg")
                }
            }
        }
    }

    // Menu-bar titles open on press and switch while a sibling menu is up.
    mod.widgets.FabMenuButton = mod.widgets.FabDropdownButton{
        menu_bar: true
        align: Align{x: 0.5 y: 0.5}
        padding: Inset{left: 7 right: 7 top: 0 bottom: 0}
        spacing: 0
        chevron_slot +: { visible: false }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabDropdownButton {
    #[deref]
    view: View,
    #[live(live_id!(editor_type))]
    tag: LiveId,
    /// The menu-owner id the host raises this button's menu under. The
    /// button mirrors the layer's `MenuOpened` / `MenuClosed` for it. Hosts
    /// with a fixed menu set it in the DSL (`owner: @fab_vp_mode`);
    /// `FabArea` stamps its per-slot id onto the editor-type buttons.
    #[live(LiveId(0))]
    owner: LiveId,
    #[live]
    menu_bar: bool,
    /// Mirror of "my owner's menu is up" — written only from the broadcast.
    #[rust]
    open: bool,
    /// A menu-bar menu is up, as reported by the layer.
    #[rust]
    bar_open: bool,
    /// The label as last drawn (its accessible name).
    #[rust]
    label_text: String,
}

impl FabDropdownButton {
    fn press(&mut self, cx: &mut Cx, press: Option<Vec2d>) {
        let anchor = self.view.area().rect(cx);
        if self.menu_bar {
            cx.action(FabUiAction::MenuBarOpen { owner: self.owner, anchor, press });
        } else {
            cx.action(FabUiAction::DropdownClicked { tag: self.tag, anchor });
        }
    }

    fn sync_open(&mut self, cx: &mut Cx, open: bool) {
        if self.open == open {
            return;
        }
        self.open = open;
        if open {
            self.view.animator_play(cx, ids!(open.on));
        } else {
            self.view.animator_play(cx, ids!(open.off));
        }
    }
}

impl Widget for FabDropdownButton {
    /// A button that opens a menu; its name is its label (or its tooltip's
    /// `a11y` label for an icon-only one).
    fn semantics(&self, cx: &Cx, node: &mut a11y_tree::Node) {
        let _ = cx;
        node.role = a11y_tree::Role::Button;
        // The label is what the button shows: its name, unless the instance names the button
        // itself (`a11y: {label: ..}`), and always its value, so a choice button reports its
        // choice ("Render mode" = "Sliced").
        if !self.label_text.is_empty() {
            makepad_platform::a11y::set_string(&mut node.name, &self.label_text);
            makepad_platform::a11y::set_string(&mut node.value, &self.label_text);
        }
        node.states = a11y_tree::States::FOCUSABLE | a11y_tree::States::HAS_POPUP;
        node.states.set(a11y_tree::States::EXPANDED, self.open);
        node.actions = a11y_tree::Actions::PRESS | a11y_tree::Actions::FOCUS | a11y_tree::Actions::SHOW_MENU;
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        if request.action == Actions::PRESS || request.action == Actions::SHOW_MENU {
            self.press(cx, None);
            return ActionResult::Handled;
        }
        ActionResult::Unhandled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::MouseMove(e) = event {
            if self.menu_bar && self.bar_open && !self.open && self.view.visible
                && self.view.area().clipped_rect(cx).contains(e.abs)
            {
                self.press(cx, None);
            }
        }
        let Event::Actions(actions) = event else {
            return;
        };
        if let Some(action) = actions.find_widget_action(self.view.widget_uid()) {
            match action.cast::<ViewAction>() {
                ViewAction::FingerDown(e) if self.menu_bar && e.device.is_primary_hit() => {
                    self.press(cx, e.device.is_mouse().then_some(e.abs));
                }
                ViewAction::FingerUp(e) if !self.menu_bar && e.is_over && e.device.is_primary_hit() => {
                    self.press(cx, None);
                }
                _ => {}
            }
        }
        // `was_open` is the state before this pass: a click-away's
        // `MenuClosed` rides the same pass as its `MenuClickAway`, and the
        // press that toggled this button's own menu closed must not also
        // re-open it.
        let was_open = self.open;
        for a in ui_actions(actions) {
            if let FabUiAction::MenuClickAway { at } = a {
                if !was_open
                    && self.view.visible
                    && self.view.area().clipped_rect(cx).contains(*at)
                {
                    self.press(cx, Some(*at));
                }
            }
            match a {
                FabUiAction::MenuOpened { menu_bar, .. } => self.bar_open = *menu_bar,
                FabUiAction::MenuClosed { .. } => self.bar_open = false,
                _ => {}
            }
        }
        if let Some(open) = open_after(ui_actions(actions), self.owner) {
            self.sync_open(cx, open);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let text = self.view.label(cx, ids!(label)).text();
        if text != self.label_text {
            self.label_text = text;
        }
        self.view.draw_walk(cx, scope, walk)
    }
}
