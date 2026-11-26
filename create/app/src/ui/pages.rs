use crate::board::tabs::BoardTabs;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    let Tool = PanelButton{
        height: 27 padding: Inset{left: 7 right: 7}
        draw_bg +: {color: palette.panel border_size: 0}
    }
    let PageHeader = SolidView{
        width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 10
        padding: Inset{left: 12 right: 12 top: 9 bottom: 9}
        draw_bg.color: palette.panel
        title := ProjectHeading{width: Fill}
    }
    let BoardTab = View{
        width: Fit height: Fill flow: Overlay align: Align{x: 1 y: 0.5} padding: Inset{right: 4}
        choose := PanelButton{
            width: Fit{max: 222} height: 28 padding: Inset{left: 30 right: 30}
            draw_text +: {max_lines: 1 text_overflow: Ellipsis}
        }
    }
    mod.widgets.WhiteboardPage = View{
        width: Fill height: Fill flow: Down
        board_tabs := #(BoardTabs::register_widget(vm)){
            width: Fill height: 40 padding: Inset{left: 10 right: 10} flow: Right
            list := PortalList{
                width: Fill height: Fill flow: Right use_vertical_finger_scroll: true
                Tab := BoardTab{}
                Current := BoardTab{
                    choose +: {
                        padding: Inset{left: 9 right: 51}
                        draw_bg +: {color: palette.sel border_color: palette.accent}
                    }
                    rename := ButtonFlatter{
                        text: "Rename" a11y_name: "Rename board"
                        width: 44 height: 28 margin: Inset{right: 3} padding: 0
                        draw_bg.pixel: fn() { return vec4(0.0) }
                        draw_text +: {
                            color: palette.muted color_hover: palette.text color_focus: palette.accent
                            color_down: palette.accent text_style: theme.font_regular{font_size: 8}
                        }
                    }
                }
                Editing := View{
                    width: Fit height: Fill align: Align{y: 0.5} padding: Inset{right: 4}
                    title := PanelInput{width: 180 height: 28 a11y: {label: "Board name"}}
                }
                Add := View{
                    width: Fit height: Fill align: Align{y: 0.5}
                    new_board := PanelButton{text: "New board" height: 28}
                }
            }
        }
        PanelLine{}
        View{
            width: Fill height: Fill flow: Overlay
            surface := BoardCanvas{}
            board_empty := View{
                width: Fill height: Fill align: Center padding: 18
                RoundedView{
                    width: Fill{max: 355} height: Fit padding: 16
                    draw_bg +: {color: palette.card border_size: 1 border_color: palette.line border_radius: 5}
                    BodyNote{text: "Pick a tool above, or drag one onto the board, to add the first item"}
                }
            }
            View{
                width: Fill height: Fit padding: 11 align: Align{x: 0.5}
                board_toolbar := RoundedView{
                    width: Fit height: Fit flow: Right{wrap: true} spacing: 2 padding: 4
                    draw_bg +: {color: palette.panel border_size: 1 border_color: palette.line border_radius: 5}
                    tool_select := Tool{text: "Select" draw_bg.color: palette.sel}
                    tool_sticky := Tool{text: "Sticky"}
                    tool_text := Tool{text: "Text"}
                    tool_shape := Tool{text: "Shape"}
                    tool_section := Tool{text: "Section"}
                    align_disabled := PanelButton{text: "Align" width: 66 height: 27 enabled: false}
                    align_menu := DropDown{
                        visible: false width: 66 height: 27 margin: 0
                        popup_menu: PopupMenu{
                            width: Fit
                            menu_item: PopupMenuItem{
                                draw_bg +: {mark_color: #0000 mark_color_active: #0000 mark_color_disabled: #0000}
                            }
                        }
                        popup_menu_position: #(makepad_widgets::drop_down::PopupMenuPosition::BelowInput)
                        labels: ["Left   ⌥A" "Horizontal centre   ⌥H" "Right   ⌥D"
                                 "Top   ⌥W" "Vertical middle   ⌥V" "Bottom   ⌥S"]
                    }
                }
            }
        }
    }
    mod.widgets.HostedAppsPage = View{
        width: Fill height: Fill flow: Down
        PageHeader{
            title +: {text: "Share"}
            open_publish := PanelButton{visible: false text: "Publish…"}
        }
        publish_config_error := BodyNote{visible: false padding: 12 draw_text.color: palette.warn}
        PanelLine{}
        View{
            width: Fill height: Fit flow: Down padding: 12 spacing: 12
            RoundedView{
                width: Fill height: Fit flow: Right spacing: 10 align: Align{y: 0.5} padding: 10
                draw_bg +: {color: palette.card border_size: 2 border_color: palette.you border_radius: 4.5}
                PresenceDot{width: 8 height: 8 draw_bg.color: palette.good}
                View{
                    width: Fill height: Fit flow: Down spacing: 2
                    RailText{text: "Create"}
                    Kicker{text: "this app · running" draw_text.color: palette.muted}
                }
                PanelButton{text: "Share" enabled: false}
            }
            BodyNote{text: "Run a Makepad app from your checkout or a lane, then share its window with the other person. Coming."}
        }
    }
    let ProjectCard = RoundedView{
        width: Fill height: Fit flow: Down padding: 10 spacing: 7
        draw_bg +: {color: palette.card border_size: 1 border_color: palette.line border_radius: 5}
        SolidView{width: Fill height: 3 draw_bg.color: palette.accent}
        View{
            width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
            ProjectDot{}
            open_project := PanelButton{
                width: Fill padding: 0 align: Align{x: 0 y: 0.5}
                draw_bg +: {color: palette.card border_size: 0}
                draw_text.text_style: theme.font_bold{font_size: 10.125}
            }
        }
        goal := BodyNote{}
        RoundedView{
            width: Fill height: 6 draw_bg +: {color: palette.line border_radius: 1.5}
            open := SolidView{width: Fill height: Fill draw_bg.color: palette.muted}
            doing := SolidView{width: Fill height: Fill draw_bg.color: palette.accent}
            review := SolidView{width: Fill height: Fill draw_bg.color: palette.peerai}
            done := SolidView{width: Fill height: Fill draw_bg.color: palette.good}
        }
        counts := Kicker{width: Fill flow: Right{wrap: true} draw_text.color: palette.muted}
        authors := SeatIcons{}
    }
    mod.widgets.ProjectGallery = View{
        width: Fill height: Fill flow: Down
        PageHeader{
            title +: {text: "Projects"}
            PanelButton{text: "+ New project" enabled: false}
        }
        PanelLine{}
        gallery := mod.widgets.ProjectGridBase{
            width: Fill height: Fill padding: 12
            list := PortalList{
                width: Fill height: Fill
                Pair := View{
                    width: Fill height: Fit flow: Right spacing: 10 padding: Inset{bottom: 10}
                    first := ProjectCard{}
                    second := ProjectCard{}
                    spacer := View{visible: false width: Fill height: 1}
                }
            }
        }
    }
}
