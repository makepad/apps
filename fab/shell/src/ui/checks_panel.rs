//! The Checks editor: the shown unit's circuit findings (ERC) over its board's (DRC), the lists the
//! Schematic and the Board used to float. A click on a row is the views' focus: the Schematic
//! glides to the finding, the Board selects it, the 3D view and the host follow. The unit is the
//! one the last Schematic or Board drew (`ElecLive::shown_unit`), else the design's first.

use crate::api::*;
use crate::ui::editors::Editor;
use fab_elec_view::board::findings::BoardChecks;
use fab_elec_view::ElecLive;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabChecksPanelBase = #(FabChecksPanel::register_widget(vm))
    mod.widgets.FabChecksPanel = set_type_default() do mod.widgets.FabChecksPanelBase{
        width: Fill height: Fill flow: Down
        show_bg: true draw_bg +: { color: fab.color_editor }
        circuit := mod.widgets.BoardChecks{ circuit: true title: "Schematic" }
        board := mod.widgets.BoardChecks{ title: "Board" }
    }
}

/// The Checks editor. Per draw: the unit read from `ElecLive`, O(1); each list rebuilds its rows
/// only when the findings or the unit change.
#[derive(Script, ScriptHook, Widget)]
pub struct FabChecksPanel {
    #[deref]
    view: View,
}

impl Widget for FabChecksPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Checks");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let on_screen = scope.data.get::<AppState>().is_some_and(|state| state.ui.on_screen(Editor::Checks));
        if on_screen {
            self.view.handle_event(cx, event, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let unit = cx.get_global_ref::<ElecLive>().and_then(|live| live.shown_unit.or_else(|| live.snap.docs.as_ref()?.units.first().map(|(id, ..)| *id)));
        for id in [ids!(circuit), ids!(board)] {
            if let Some(mut list) = self.view.widget(cx, id).borrow_mut::<BoardChecks>() {
                list.set_unit(cx, unit);
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}
