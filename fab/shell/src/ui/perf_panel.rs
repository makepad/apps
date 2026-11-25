//! The Performance editor shows the frame profiler. The graph redraws every frame while the
//! editor is on screen. The shell enables the platform's frame monitor only while it shows.

use crate::api::AppState;
use crate::ui::editors::Editor;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabPerfPanelBase = #(FabPerfPanel::register_widget(vm))
    mod.widgets.FabPerfPanel = set_type_default() do mod.widgets.FabPerfPanelBase{
        width: Fill height: Fill flow: Down
        show_bg: true draw_bg +: { color: fab.color_editor }
        // the toolbar: the area's editor selector (and this editor's tools, when it has any)
        editor_bar := mod.widgets.FabEditorBar{}
        graph := PerfGraph{ panel_width: 100000.0 panel_height: 100000.0 panel_margin: 6.0 }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabPerfPanel {
    #[deref]
    view: View,
}

impl Widget for FabPerfPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Performance");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if scope.data.get::<AppState>().is_some_and(|state| state.ui.on_screen(Editor::Performance)) {
            self.view.handle_event(cx, event, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}
