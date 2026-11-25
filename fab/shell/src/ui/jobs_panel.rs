//! The Jobs editor shows saved prints and the printer's progress. Choosing between the list
//! and its empty message costs O(1) per draw. The list scans the jobs and draws its visible rows.

use crate::api::AppState;
use crate::ui::editors::Editor;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabJobsPanelBase = #(FabJobsPanel::register_widget(vm))
    mod.widgets.FabJobsPanel = set_type_default() do mod.widgets.FabJobsPanelBase{
        width: Fill height: Fill flow: Down
        padding: Inset{top: 0 bottom: 4}
        show_bg: true draw_bg +: { color: fab.color_editor }
        // the toolbar: the area's editor selector (and this editor's tools, when it has any)
        editor_bar := mod.widgets.FabEditorBar{}
        list := mod.widgets.FabJobList{ fill: true height: Fill }
        empty := mod.widgets.FabLabelDim{
            visible: false width: Fill margin: Inset{left: 10 top: 4}
            text: "No jobs yet: each print you send is kept here, with the printer's progress"
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabJobsPanel {
    #[deref]
    view: View,
}

impl Widget for FabJobsPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Jobs");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if scope.data.get::<AppState>().is_some_and(|state| state.ui.on_screen(Editor::Jobs)) {
            self.view.handle_event(cx, event, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            let empty = state.print.follow.jobs.is_empty();
            self.view.widget(cx, ids!(list)).set_visible(cx, !empty);
            self.view.widget(cx, ids!(empty)).set_visible(cx, empty);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}
