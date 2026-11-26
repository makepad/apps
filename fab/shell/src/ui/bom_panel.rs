//! The Electronics bill of materials editor.

use crate::api::AppState;
use crate::ui::editors::Editor;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabBomPanelBase = #(FabBomPanel::register_widget(vm))
    mod.widgets.FabBomPanel = set_type_default() do mod.widgets.FabBomPanelBase{
        width: Fill height: Fill flow: Down
        show_bg: true draw_bg +: {color: fab.color_editor}
        list := mod.widgets.BomPane{}
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabBomPanel {
    #[deref] view: View,
}

impl Widget for FabBomPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if scope.data.get::<AppState>().is_some_and(|state| state.ui.on_screen(Editor::Bom)) {
            self.view.handle_event(cx, event, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}
