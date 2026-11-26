use makepad_widgets::*;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    let Name = RailText{draw_text.text_style: theme.font_bold{font_size: 8.625}}
    let Bubble = RoundedView{
        width: Fill height: Fit flow: Down spacing: 3
        head := View{
            width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{y: 0.5}
            name := Name{}
            when := Kicker{draw_text.text_style: theme.font_code{font_size: 7.5}}
        }
        message := BodyNote{draw_text +: {color: palette.text text_style: theme.font_regular{font_size: 10.5}}}
        sent := Kicker{visible: false text: "sent to your AI" draw_text.color: palette.yourai}
    }
    let Message = View{
        width: Fill height: Fit flow: Right spacing: 8 padding: Inset{bottom: 9}
        author := SeatIcons{}
        bubble := Bubble{show_bg: false}
    }
    mod.widgets.ChatConversation = View{
        width: Fill height: Fill flow: Down spacing: 8
        room_read_only := BodyNote{visible: false}
        chat_empty := BodyNote{text: "Start the conversation here."}
        transcript := mod.widgets.CreateChatListBase{
            mention_ink: palette.yourai
            width: Fill height: Fill
            list := PortalList{
                width: Fill height: Fill auto_tail: true
                Line := Message{}
                Addressed := Message{
                    bubble +: {
                        show_bg: true padding: Inset{top: 6 bottom: 6 left: 8 right: 8}
                        draw_bg +: {color: palette.sel border_radius: 4 border_size: 1 border_color: palette.line}
                        sent +: {visible: true}
                    }
                }
            }
        }
        chat_input := PanelInput{
            empty_text: "message · @ai your AI · Enter sends"
            a11y: {label: "Chat message"}
            draw_text.text_style: theme.font_code{font_size: 8.25}
        }
    }
    mod.widgets.ChatRail = SolidView{
        width: 279 height: Fill flow: Down padding: 9 spacing: 8 draw_bg.color: palette.panel
        space_notice := View{
            visible: false width: Fill height: Fit flow: Down spacing: 4
            space_notice_text := BodyNote{width: Fill}
            space_notice_ok := PanelButton{text: "OK"}
        }
        Kicker{text: "CHAT"}
        conversation := mod.widgets.ChatConversation{}
    }
}
