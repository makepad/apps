use makepad_widgets::*;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.BodyNote = Label{
        width: Fill height: Fit padding: 0 flow: Right{wrap: true}
        draw_text +: {color: palette.muted text_style: theme.font_regular{font_size: 9}}
    }
    mod.widgets.ProjectDot = RoundedView{width: 9 height: 9 draw_bg +: {color: palette.accent border_radius: 1.5}}
    mod.widgets.ProjectHeading = RailText{draw_text.text_style: theme.font_bold{font_size: 10.5}}
    mod.widgets.SeatIcons = View{
        width: Fit height: Fit spacing: 4
        you := SeatBadge{visible: false}
        your_ai := AiBadge{visible: false}
        peer := PeerBadge{visible: false}
        peer_ai := PeerAiBadge{visible: false}
    }
    let TaskCard = View{
        width: Fill height: Fit padding: Inset{bottom: 7}
        RoundedView{
            width: Fill height: Fit flow: Down spacing: 2 padding: 7
            draw_bg +: {color: palette.card border_color: palette.line border_size: 1 border_radius: 4}
            View{
                width: Fill height: Fit align: Align{y: 0.5}
                number := Kicker{width: Fill draw_text.text_style: theme.font_code{font_size: 7.5}}
                authors := SeatIcons{}
            }
            title := RailText{width: Fill flow: Right{wrap: true} draw_text.text_style: theme.font_bold{font_size: 9.375}}
        }
    }
    let Cards = mod.widgets.CreateTaskListBase{
        width: Fill height: Fill
        list := PortalList{width: Fill height: Fill TaskRow := TaskCard{}}
    }
    let Column = RoundedView{
        width: Fill height: Fill flow: Down padding: 8 spacing: 7
        draw_bg +: {color: palette.panel border_color: palette.line border_size: 1 border_radius: 4.5}
        heading := View{
            width: Fill height: Fit flow: Right align: Align{y: 0.5}
            padding: Inset{left: 3 right: 3 top: 2}
            state := Kicker{width: Fill}
            count := Kicker{text: "0"}
        }
        PanelLine{}
    }
    let BoardColumn = Column{
        empty := BodyNote{margin: Inset{left: 3 top: 2}}
        cards := Cards{}
    }
    let Capture = Tip{
        text: "coming"
        chip := RoundedView{
            width: Fit height: 29 flow: Right spacing: 6 align: Center padding: Inset{left: 9 right: 9}
            draw_bg +: {color: palette.panel2 border_size: 1 border_color: palette.line border_radius: 4}
            control := PanelButton{
                enabled: false height: Fill padding: 0 text: "" draw_bg.border_size: 0
                draw_bg.color: palette.panel2 draw_bg.color_disabled: palette.panel2 draw_text.color: palette.faint
            }
            key := RoundedView{
                width: Fit height: 16 padding: Inset{left: 4 right: 4} align: Center
                draw_bg +: {color: palette.panel2 border_color: palette.line border_size: 1 border_radius: 2}
                keycap := Kicker{}
            }
        }
    }
    mod.widgets.TaskBoard = View{
        width: Fill height: Fill flow: Down
        SolidView{
            width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 10
            padding: Inset{left: 12 right: 12 top: 9 bottom: 9}
            draw_bg.color: palette.panel
            View{
                width: Fill height: Fit flow: Right{wrap: true} align: Align{y: 0.5} spacing: 10
                ProjectDot{}
                project_name := ProjectHeading{}
                project_goal := BodyNote{width: Fit}
            }
            add_focus := PanelButton{text: "+ Task"}
        }
        PanelLine{}
        columns := View{
            width: Fill height: Fill flow: Right padding: 11 spacing: 9
            first_pair := View{
                width: Fill height: Fill flow: Right spacing: 9
                open := Column{
                    heading +: {state +: {text: "OPEN"}}
                    task_input := PanelInput{empty_text: "Add a task…" a11y: {label: "New task title"}}
                    empty := BodyNote{text: "No open tasks yet."}
                    cards := Cards{}
                }
                doing := BoardColumn{cards +: {column: 1} heading +: {state +: {text: "DOING"}} empty +: {text: "Nothing in progress yet."}}
            }
            second_pair := View{
                width: Fill height: Fill flow: Right spacing: 9
                review := BoardColumn{cards +: {column: 2} heading +: {state +: {text: "REVIEW"}} empty +: {text: "Nothing in review yet."}}
                done := BoardColumn{cards +: {column: 3} heading +: {state +: {text: "DONE"}} empty +: {text: "No completed tasks yet."}}
            }
        }
        PanelLine{}
        capture := SolidView{
            width: Fill height: Fit flow: Right{wrap: true} spacing: 8 align: Align{y: 0.5}
            padding: Inset{left: 12 right: 12 top: 8 bottom: 8}
            draw_bg.color: palette.panel
            snap := Capture{chip +: {control +: {text: "Snap"} key +: {keycap +: {text: "⌘⇧1"}}}}
            clip := Capture{chip +: {control +: {text: "Clip"} key +: {keycap +: {text: "⌘⇧2"}}}}
            voice := Capture{chip +: {control +: {text: "Voice note"} key +: {keycap +: {text: "hold ⌘⇧3"}}}}
            capture_task := PanelButton{text: "Task"}
        }
    }
}
