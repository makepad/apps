use crate::lists::{Page, ViewData};
use makepad_widgets::*;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.TalkPage = View{
        width: Fill height: Fill flow: Down
        padding: Inset{left: 18 right: 18 top: 18 bottom: 16} align: Align{x: 0.5}
        View{
            width: Fill{max: 660} height: Fill flow: Down spacing: 12
            ProjectHeading{text: "Talk" draw_text.text_style: theme.font_bold{font_size: 17}}
            PanelLine{}
            conversation := ChatConversation{
                spacing: 12
                chat_input +: {
                    padding: Inset{left: 12 right: 12 top: 10 bottom: 10}
                    draw_text.text_style: theme.font_regular{font_size: 11}
                }
            }
        }
    }
}

impl ViewData {
    pub fn conversation(&self, cx: &mut Cx, ui: &WidgetRef) -> WidgetRef {
        ui.widget(cx, if self.page == Page::Chat { ids!(talk_page) } else { ids!(chat_panel) })
    }

    /// The displayed input owns the room's draft. Transfer it only when changing forms.
    pub fn show_conversation(&self, cx: &mut Cx, ui: &WidgetRef) -> WidgetRef {
        let talk = ui.widget(cx, ids!(talk_page));
        let rail = ui.widget(cx, ids!(chat_panel));
        let opening = self.page == Page::Chat;
        let (from, to) = if opening { (&rail, &talk) } else { (&talk, &rail) };
        if talk.visible() != opening {
            let previous = from.text_input(cx, ids!(chat_input));
            to.text_input(cx, ids!(chat_input)).restore_state(cx, previous.save_state());
            previous.set_text(cx, "");
            talk.set_visible(cx, opening);
        }
        let show_rail = !self.compact && !opening;
        rail.set_visible(cx, show_rail);
        ui.widget(cx, ids!(right_rule)).set_visible(cx, show_rail);
        to.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{Message, Seat};
    use makepad_widgets::cx_draw::CxDraw;
    use makepad_widgets::makepad_script::script;

    #[test]
    fn talk_and_rail_show_the_same_rows_and_carry_the_draft() {
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            crate::script_mod(vm);
        });
        let cx = &mut *pool;
        let ui = cx.with_vm(|vm| {
            let value = vm.eval(script! {
                use mod.widgets.*
                View{talk_page := TalkPage{visible: false} right_rule := PanelLine{} chat_panel := ChatRail{}}
            });
            WidgetRef::script_from_value(vm, value)
        });
        let mut data = ViewData::default();
        for (text, addressed) in [("Hello", false), ("@ai Take a note", true)] {
            data.workspace.messages.push(Message {
                seat: Seat::You, name: "You".into(), time: "13:04".into(), text: text.into(), addressed,
            });
        }
        let size = dvec2(720., 600.);
        let pass = DrawPass::new(cx);
        pass.set_size(cx, size);
        let mut list = DrawList2d::new(cx);
        for (page, compact, expected, next) in [
            (Page::Tasks, false, "", "from the rail"),
            (Page::Chat, false, "from the rail", "from Talk"),
            (Page::Tasks, true, "from Talk", "from Talk"),
            (Page::Chat, true, "from Talk", "from compact Talk"),
            (Page::Tasks, false, "from compact Talk", ""),
        ] {
            data.page = page;
            data.compact = compact;
            let form = data.show_conversation(cx, &ui);
            let rail_visible = page != Page::Chat && !compact;
            assert_eq!(ui.widget(cx, ids!(chat_panel)).visible(), rail_visible);
            assert_eq!(ui.widget(cx, ids!(right_rule)).visible(), rail_visible);
            assert_eq!(ui.widget(cx, ids!(talk_page)).visible(), page == Page::Chat);
            let input = form.text_input(cx, ids!(chat_input));
            assert_eq!(input.text(), expected, "switching forms restores the current draft");
            if !form.visible() { continue }
            input.set_text(cx, next);

            {
                let event = DrawEvent::default();
                let mut draw = CxDraw::new(cx, &event);
                let mut frame = Cx2d::new(&mut draw);
                frame.begin_pass(&pass, None);
                list.begin_always(&mut frame);
                frame.begin_root_turtle(size, Layout::flow_down());
                form.draw_all(&mut frame, &mut Scope::with_data(&mut data));
                frame.end_pass_sized_turtle();
                list.end(&mut frame);
                frame.end_pass(&pass);
            }

            let rows = form.portal_list(cx, ids!(transcript.list));
            for (i, message) in data.workspace.messages.iter().enumerate() {
                let (_, row) = rows.get_item(i).expect("each form draws the transcript");
                assert_eq!(row.label(cx, ids!(message)).text(), message.text);
                assert_eq!(row.label(cx, ids!(when)).text(), message.time);
                assert_eq!(row.widget(cx, ids!(sent)).visible(), message.addressed);
            }
        }
    }
}
