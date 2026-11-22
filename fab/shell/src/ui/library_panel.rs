//! The Library editor: the electronics library (search, pins, recents) in an area of its own. While
//! it shows, the Schematic's Library float stays hidden; a click arms an entry and a drag carries
//! it, onto whichever Schematic area it lands in (both are app-wide: `LibraryAction::Arm` and the
//! drag's item). Place part (A) in a Schematic brings this editor forward with its search taking
//! the keyboard (`ElecPanel::Library`, the shell's).
//!
//! The float and this editor are two panels over one remembered list: each restores the saved
//! pins and recents, and takes the other's changes as they are saved (`LibraryAction::Remember`).

use fab_elec_view::library::{EntryKey, LibraryAction, LibraryPanel};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabLibraryPanelBase = #(FabLibraryPanel::register_widget(vm))
    mod.widgets.FabLibraryPanel = set_type_default() do mod.widgets.FabLibraryPanelBase{
        width: Fill height: Fill flow: Down
        padding: Inset{top: 6 bottom: 4}
        show_bg: true draw_bg +: { color: fab.color_editor }
        library := mod.widgets.LibraryPanel{}
    }
}

/// The Library editor. Per draw: nothing beyond the library panel's own draw; the saved lists are
/// read once, at its first draw.
#[derive(Script, ScriptHook, Widget)]
pub struct FabLibraryPanel {
    #[deref]
    view: View,
    /// The saved pins and recents are in the panel.
    #[rust]
    restored: bool,
}

impl Widget for FabLibraryPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Library");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        for action in actions {
            // a change of pins or recents, the float's (its own come back as lists it holds,
            // which change nothing): rare, a person's click
            if let Some(LibraryAction::Remember { pinned, recent }) = action.downcast_ref::<LibraryAction>() {
                if let Some(mut library) = self.view.widget(cx, ids!(library)).borrow_mut::<LibraryPanel>() {
                    library.set_remembered(cx, pinned.clone(), recent.clone());
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.restored {
            if let Some(mut library) = self.view.widget(cx, ids!(library)).borrow_mut::<LibraryPanel>() {
                let (pinned, recent) = crate::view_prefs::load_library();
                let keys = |pairs: Vec<(String, String)>| pairs.into_iter().map(|(lib, name)| EntryKey { lib, name }).collect();
                library.set_remembered(cx, keys(pinned), keys(recent));
                self.restored = true;
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}
