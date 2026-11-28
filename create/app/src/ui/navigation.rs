use makepad_widgets::*;
use crate::lists::{Page, ViewData};

impl ViewData {
    pub fn show_caption(&self, cx: &mut Cx, ui: &WidgetRef) {
        let tree = self.tree.snapshot();
        let title = match self.page {
            Page::Project | Page::Tasks | Page::Issues | Page::Board | Page::Docs => {
                self.workspace.current.and_then(|id| tree.project(id)).map(|p| p.title.as_str())
            }
            // Joining is about another town: the caption names none.
            Page::Join => None,
            _ => tree.town.map(|_| tree.label.as_str()),
        };
        let label = ui.label(cx, ids!(caption_title));
        match title.filter(|s| !s.is_empty()) {
            Some(title) => label.set_text(cx, &format!("Makepad Town · {title}")),
            None => label.set_text(cx, "Makepad Town"),
        }
    }
}

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.RailText = Label{
        padding: 0 draw_text +: {color: palette.text text_style: theme.font_bold{font_size: 9.75}}
    }
    mod.widgets.WorkCaptionTitle = View{
        width: Fill height: Fit flow: Right align: Align{y: 0.5}
        caption_window_gap := View{width: 72 height: 1}
        caption_title := Label{
            width: Fill padding: 0 max_lines: 1 text_overflow: Ellipsis
            text: "Makepad Town" draw_text +: {color: palette.muted text_style: theme.font_regular{font_size: 9}}
        }
        recording := View{
            visible: false width: Fit height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
            PresenceDot{draw_bg +: {color: theme.color_error border_radius: 3}}
            line := RailText{width: Fit{max: "50vw"} max_lines: 1 text_overflow: Ellipsis}
            stop_recording := PanelButton{text: "Stop" a11y: {label: "Stop recording"}}
        }
    }
    mod.widgets.Kicker = Label{
        padding: 0 draw_text +: {color: palette.faint text_style: theme.font_code{font_size: 7.875}}
    }
    let RailButton = PanelButton{
        width: Fill height: 30 align: Align{x: 0 y: 0.5}
        padding: Inset{left: 10 right: 10}
        draw_bg +: {border_size: 0 border_radius: 3.5 color: palette.panel}
        draw_text +: {color: palette.muted text_style: theme.font_bold{font_size: 9.75}}
        icon_walk: Walk{width: 14 height: 14}
        draw_icon +: {color: palette.muted color_hover: palette.text}
    }
    let ActiveMark = SolidView{width: 2 height: Fill draw_bg.color: palette.accent}
    // One tree row: the whole row opens its page, and only the twisty above it folds.
    let TreeBody = View{
        width: Fill height: Fill flow: Right align: Align{y: 0.5} spacing: 2 padding: Inset{left: 2 right: 6}
        indent := View{width: 0 height: 1}
        twisty := RailButton{
            width: 24 height: 24 padding: 0 align: Center text: "▾" a11y: {label: "Fold"}
            draw_bg +: {color: #0000 color_hover: palette.card color_down: palette.card}
            draw_text +: {color: palette.faint text_style: theme.font_regular{font_size: 9}}
        }
        twisty_gap := View{visible: false width: 24 height: 1}
        glyph := RailText{width: 14 draw_text +: {color: palette.faint text_style: theme.font_regular{font_size: 9}}}
        titles := View{
            width: Fill height: Fit flow: Down
            name := RailText{
                width: Fill max_lines: 1 text_overflow: Ellipsis a11y: {hidden: true}
                draw_text.text_style: theme.font_regular{font_size: 9.75}
            }
        }
    }
    let TreeRow = View{
        width: Fill height: 30 flow: Overlay
        open := RailButton{text: ""}
        mark := ActiveMark{visible: false}
        body := TreeBody{}
    }
    mod.widgets.TownTreeRail = mod.widgets.TownTreeListBase{
        width: Fill height: Fill
        badge_bg: palette.sel badge_urgent_bg: palette.accent badge_text: palette.text badge_urgent_text: palette.onaccent
        list := PortalList{
            width: Fill height: Fill
            Join := View{
                width: Fill height: Fit flow: Right{wrap: true} distribute: Distribute.SpaceBetween
                align: Align{y: 0.5} padding: Inset{left: 8}
                connect_join := RailButton{text: "Join a town"}
            }
            Row := TreeRow{}
            TownRow := TreeRow{
                height: 40 open +: {height: Fill}
                body +: {
                    titles +: {
                        name +: {draw_text.text_style: theme.font_bold{font_size: 9.75}}
                        note := Kicker{width: Fill max_lines: 1 text_overflow: Ellipsis}
                    }
                    // Shown only while the town has unread chat.
                    unread_badge := RoundedView{
                        visible: false width: Fit height: 16 padding: Inset{left: 5 right: 5} align: Center
                        draw_bg +: {color: palette.sel border_radius: 8}
                        count := Kicker{draw_text.color: palette.text}
                    }
                }
            }
        }
    }
    mod.widgets.TownRail = View{
        width: Fill height: Fill flow: Down
        tree := mod.widgets.TownTreeRail{}
        rail_publish := PanelButton{visible: false text: "Publish…"}
        publish_config_error := Label{
            visible: false width: Fill height: Fit flow: Right{wrap: true} padding: Inset{left: 4 right: 4 top: 6 bottom: 0}
            draw_text +: {color: palette.warn text_style: theme.font_regular{font_size: 9}}
        }
    }
    // Keep one focus and press target while its caption gives way to the icon.
    let NarrowTab = View{
        width: Fill height: 30 flow: Overlay align: Center
        press := PanelButton{width: Fill height: Fill text: "" draw_bg +: {color: palette.panel border_size: 0}}
        caption := Label{
            width: Fill padding: Inset{left: 6 right: 6} align: Center
            max_lines: 1 text_overflow: Ellipsis a11y: {hidden: true}
            draw_text +: {color: palette.muted text_style: theme.font_bold{font_size: 9}}
        }
        glyph := Icon{visible: false width: 16 height: 16 size: 16 draw_icon.color: palette.muted a11y: {hidden: true}}
    }
    // A shared app adds its editor to the narrow form's tabs.
    mod.widgets.NarrowBar = SolidView{
        visible: false width: Fill height: Fit flow: Right spacing: 4
        padding: Inset{left: 10 right: 10 top: 4 bottom: 4} draw_bg.color: palette.panel
        View{
            width: Fill height: Fit flow: Overlay align: Align{x: 0.92 y: 0.2}
            chat_tab := NarrowTab{
                press +: {a11y: {label: "Chat"}} caption +: {text: "Chat"}
                glyph +: {draw_icon.svg: crate_resource("self://resources/icons/chat.svg")}
            }
            chat_dot := PresenceDot{visible: false width: 8 height: 8 draw_bg.color: palette.accent}
        }
        places_tab := NarrowTab{
            press +: {a11y: {label: "Places"}} caption +: {text: "Places"}
            glyph +: {draw_icon.svg: crate_resource("self://resources/icons/projects.svg")}
        }
        page_tab := NarrowTab{
            press +: {a11y: {label: "Page"}} caption +: {text: "Home"}
            glyph +: {draw_icon.svg: crate_resource("self://resources/icons/docs.svg")}
        }
        edit_tab := NarrowTab{
            visible: false press +: {a11y: {label: "Edit the shared app"}} caption +: {text: "Edit"}
            glyph +: {draw_icon.svg: crate_resource("self://resources/icons/edit.svg")}
        }
    }
}
