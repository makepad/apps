//! The Board controls editor: the run board's keys and knobs in an area of its own. The Transport
//! keeps the controls' header, whose press brings this editor forward.

use crate::api::*;
use crate::ui::editors::Editor;
use fab_elec_view::instruments::controls_panel::ControlsPanel;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabBoardControlsPanelBase = #(FabBoardControlsPanel::register_widget(vm))
    mod.widgets.FabBoardControlsPanel = set_type_default() do mod.widgets.FabBoardControlsPanelBase{
        width: Fill height: Fill flow: Down
        padding: Inset{left: theme.prop_pad right: theme.prop_pad top: 6 bottom: 6}
        show_bg: true draw_bg +: { color: fab.color_editor }
        body := View{
            width: Fill height: Fill flow: Down
            panel := mod.widgets.ControlsPanel{height: Fill radius: fab.radius radius_lg: fab.radius_lg}
        }
        empty := mod.widgets.FabLabelDim{
            visible: false width: Fill max_lines: 1 text_overflow: TextOverflow.Ellipsis
            text: "No keys or knobs: the running board's switches and pots show here"
        }
    }
}

/// The Board controls editor. Per draw: the panel's counts, O(controls), and its own draw.
#[derive(Script, ScriptHook, Widget)]
pub struct FabBoardControlsPanel {
    #[deref]
    view: View,
}

impl FabBoardControlsPanel {
    fn has_controls(&self, cx: &Cx) -> bool {
        self.view.widget(cx, ids!(body.panel)).borrow_mut::<ControlsPanel>()
            .and_then(|mut panel| panel.counts(cx))
            .is_some_and(|(keys, knobs)| keys > 0 || knobs > 0)
    }
}

impl Widget for FabBoardControlsPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Board controls");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !scope.data.get::<AppState>().is_some_and(|state| state.ui.on_screen(Editor::BoardControls)) {
            return;
        }
        // A hidden panel has no draw area to wake when the first controls arrive.
        if matches!(event, Event::Signal | Event::NextFrame(_))
            && self.view.widget(cx, ids!(empty)).visible() && self.has_controls(cx) {
            self.view.redraw(cx);
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let has_controls = self.has_controls(cx);
        self.view.view(cx, ids!(body)).set_visible(cx, has_controls);
        self.view.widget(cx, ids!(empty)).set_visible(cx, !has_controls);
        self.view.draw_walk(cx, scope, walk)
    }
}
