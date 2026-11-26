//! The Electronics Requirements editor. The list reads the same judged report as the caption.

use crate::api::AppState;
use crate::ui::editors::Editor;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabRequirementsPanelBase = #(FabRequirementsPanel::register_widget(vm))
    mod.widgets.FabRequirementsPanel = set_type_default() do mod.widgets.FabRequirementsPanelBase{
        width: Fill height: Fill flow: Down
        show_bg: true draw_bg +: {color: fab.color_editor}
        list := mod.widgets.ReqList{
            color_ok: fab.color_ok color_error: fab.color_error color_warning: fab.color_warning
            color_text_muted: fab.color_text_muted color_text_dim: fab.color_text_dim
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabRequirementsPanel {
    #[deref] view: View,
}

impl Widget for FabRequirementsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if scope.data.get::<AppState>().is_some_and(|state| state.ui.on_screen(Editor::Requirements)) {
            self.view.handle_event(cx, event, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}
