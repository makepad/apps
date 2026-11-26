use crate::lists::ViewData;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.PublishMediaList = #(PublishMediaList::register_widget(vm))
    mod.widgets.PublishPage = SolidView{
        width: Fill height: Fill flow: Down draw_bg.color: palette.bg
        SolidView{
            width: Fill height: Fit padding: 12 flow: Right align: Align{y: 0.5}
            draw_bg.color: palette.panel
            ProjectHeading{width: Fill text: "Publish draft"}
            publish_back := PanelButton{text: "Back to Share"}
        }
        PanelLine{}
        View{
            width: Fill height: Fill padding: 14 spacing: 7 flow: Down
            Kicker{text: "TARGET"}
            publish_target := DropDown{width: Fill height: 30}
            Kicker{text: "TITLE"}
            publish_title := PanelInput{empty_text: "Post title" a11y: {label: "Post title"}}
            Kicker{text: "TEXT"}
            publish_text := PanelInput{
                height: 74 align: Align{x: 0 y: 0}
                is_multiline: true empty_text: "Write in Markdown" a11y: {label: "Post text"}
            }
            View{
                width: Fill height: Fit flow: Right align: Align{y: 0.5}
                publish_count := Kicker{width: Fill text: "MEDIA"}
                publish_add := PanelButton{text: "Add files…"}
            }
            publish_empty := BodyNote{text: "Choose files or drop media onto this window."}
            publish_media := PublishMediaList{
                width: Fill height: Fill
                list := PortalList{
                    width: Fill height: Fill
                    Row := View{
                        width: Fill height: Fit flow: Down padding: Inset{bottom: 3}
                        select_file := PanelButton{width: Fill align: Align{x: 0 y: 0.5}}
                    }
                }
            }
            publish_status := BodyNote{visible: false}
            publish_url := PanelInput{visible: false is_read_only: true a11y: {label: "Published link"}}
            PanelLine{}
            View{
                width: Fill height: Fit flow: Right{wrap: true} spacing: 7 align: Align{x: 1 y: 0.5}
                publish_clear := PanelButton{text: "Discard draft"}
                publish_confirm := PanelButton{text: "Confirm" enabled: false}
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct PublishMediaList {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
}

impl Widget for PublishMediaList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            let Some(state) = scope.data.get::<ViewData>().and_then(|v| v.publish.as_ref()) else { continue };
            list.set_item_range(cx, 0, state.entries.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(e) = state.entries.get(i) else { continue };
                let row = list.item(cx, i, id!(Row));
                let button = row.button(cx, ids!(select_file));
                let selected = state.draft.ids.contains(&e.id);
                button.set_text(cx, &format!("{}  {}  ·  {} bytes", if selected { "☑" } else { "☐" }, e.media.file, e.media.bytes));
                button.set_enabled(cx, !state.sending());
                row.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}
