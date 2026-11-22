//! Fab's name, version and credits in a modal panel.

use crate::api::*;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabAboutBase = #(FabAbout::register_widget(vm))
    mod.widgets.FabAbout = set_type_default() do mod.widgets.FabAboutBase{
        width: Fill
        height: Fill
        modal := Modal{
            a11y: {label: "About Fab"}
            content +: {
                width: 520
                height: Fit
                View{
                    width: Fill
                    height: Fit
                    flow: Down
                    padding: 14
                    spacing: 8
                    show_bg: true
                    draw_bg +: {
                        pixel: fn() {
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                            sdf.fill_keep(fab.color_popover)
                            sdf.stroke(fab.color_popover_border, 1.0)
                            return sdf.result
                        }
                    }
                    mod.widgets.FabTitleLabel{ text: "Fab" }
                    mod.widgets.FabLabelMuted{ text: #(env!("CARGO_PKG_VERSION")) }
                    mod.widgets.FabHr{}
                    mod.widgets.FabLabel{ text: "The 3D modeler and slicer for printing." }
                    mod.widgets.FabLabel{
                        width: Fill
                        height: Fit
                        flow: Right{wrap: true}
                        text: "Exact geometry: derived from solidcore (Apache-2.0), a Rust port of NVIDIA SMLib; see CREDITS.md."
                    }
                    mod.widgets.FabLabel{
                        width: Fill
                        height: Fit
                        flow: Right{wrap: true}
                        text: "Organic routing: ported from TiNRS Psychedelic Router 0.1.0 by Stijn Kuipers (MIT); see CREDITS.md."
                    }
                    mod.widgets.FabLabel{ text: "Made with Makepad." }
                    View{
                        width: Fill
                        height: Fit
                        flow: Right
                        align: Align{x: 0.0 y: 0.5}
                        Filler{}
                        close := mod.widgets.FabButton{ text: "Close" }
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabAbout {
    #[deref]
    view: View,
    #[rust]
    shown: bool,
}

impl Widget for FabAbout {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let modal = self.view.modal(cx, ids!(modal));
            if modal.dismissed(actions) || self.view.button(cx, ids!(modal.close)).clicked(actions) {
                cx.action(ShellAction::ShowAbout(false));
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            if state.ui.about_open != self.shown {
                self.shown = state.ui.about_open;
                let modal = self.view.modal(cx, ids!(modal));
                if self.shown {
                    modal.open(cx);
                } else {
                    modal.close(cx);
                }
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}
