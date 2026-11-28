use makepad_widgets::*;
use crate::model::{NoticeAction, NoticeDetails, TownNotice};

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
    mod.widgets.WhiteboardPage = View{
        width: Fill height: Fill flow: Down
        View{
            width: Fill height: Fill flow: Overlay
            surface := BoardCanvas{}
            board_empty := View{
                width: Fill height: Fill align: Center padding: 18
                RoundedView{
                    width: Fill{max: 355} height: Fit padding: 16
                    draw_bg +: {color: palette.card border_size: 1 border_color: palette.line border_radius: 5}
                    BodyNote{text: "Pick a tool above to start. Scroll or pinch to zoom, right-drag to move."}
                }
            }
            View{
                width: Fill height: Fit padding: 11 align: Align{x: 0.5}
                board_toolbar := RoundedView{
                    width: Fit height: Fit flow: Right{wrap: true} spacing: 2 padding: 4
                    draw_bg +: {color: palette.panel border_size: 1 border_color: palette.line border_radius: 5}
                    tool_select := Tool{text: "Select" draw_bg.color: palette.sel}
                    tool_hand := Tool{text: "Hand"}
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
    let TitleInput = PanelInput{
        height: Fit padding: Inset{left: 4 right: 4 top: 2 bottom: 2}
        draw_text +: {text_style: theme.font_bold{font_size: 17}}
        draw_bg +: {
            color: palette.bg color_hover: palette.bg color_focus: palette.card color_down: palette.card
            color_empty: palette.bg color_2_empty: palette.bg
            color_2_hover: palette.bg color_2_focus: palette.card color_2_down: palette.card
            border_color: #0000 border_color_hover: palette.line border_color_empty: #0000
            border_color_2_hover: palette.line border_color_2_empty: #0000
        }
    }
    let CrumbButton = PanelButton{
        height: 22 padding: Inset{left: 4 right: 4 top: 2 bottom: 2}
        draw_bg +: {color: palette.bg color_hover: palette.sel border_size: 0}
        draw_text +: {color: palette.muted text_style: theme.font_code{font_size: 8.25}}
    }
    let RemovedNote = View{
        visible: false width: Fill height: Fit flow: Right{wrap: true} align: Align{y: 0.5} spacing: 6
        removed_title := RailText{
            width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}} max_lines: 1 text_overflow: Ellipsis
            draw_text.text_style: theme.font_regular{font_size: 9}
        }
        BodyNote{width: Fit text: "removed ·"}
        undo_project := PanelButton{text: "Undo" a11y: {label: "Undo removal"}}
    }
    // The title, breadcrumb and ⋯ menu above a board, a doc or the task list.
    mod.widgets.ItemHead = SolidView{
        visible: false width: Fill height: Fit flow: Down spacing: 4
        padding: Inset{left: 14 right: 14 top: 10 bottom: 8} draw_bg.color: palette.bg
        View{
            width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 4
            crumb_town := CrumbButton{}
            Kicker{text: "›"}
            crumb_project := CrumbButton{}
            View{width: Fill height: 1}
            item_dots := PanelButton{
                width: 30 padding: 0 align: Center text: "⋯" a11y: {label: "More"}
                draw_bg +: {color: palette.bg border_size: 0}
            }
        }
        item_title := TitleInput{a11y: {label: "Title"}}
        delete_menu := View{
            visible: false width: Fill height: Fit flow: Right
            delete_ask := PanelButton{}
        }
        delete_sheet := RoundedView{
            visible: false width: Fill{max: 380} height: Fit flow: Down spacing: 6 padding: 12
            draw_bg +: {color: palette.card border_size: 1 border_color: palette.line border_radius: 6}
            delete_question := RailText{width: Fill flow: Right{wrap: true}}
            BodyNote{text: "It goes from this town for everyone in it."}
            View{
                width: Fill height: Fit flow: Right{wrap: true} spacing: 8
                delete_confirm := PanelButton{draw_bg.color: #x5a2626}
                delete_keep := PanelButton{}
            }
        }
        page_removed := RemovedNote{}
        PanelLine{}
    }
    let Tile = RoundedView{
        width: 180 height: 70 flow: Overlay
        draw_bg +: {color: palette.card border_radius: 10}
        open := PanelButton{
            width: Fill height: Fill text: ""
            draw_bg +: {color: palette.card border_radius: 10}
        }
        View{
            width: Fill height: Fill flow: Down align: Align{y: 0.5} padding: Inset{left: 12 right: 12} spacing: 4
            tile_title := RailText{width: Fill max_lines: 1 text_overflow: Ellipsis a11y: {hidden: true}}
            tile_summary := BodyNote{draw_text.text_style: theme.font_regular{font_size: 8.25}}
        }
    }
    let TileGrid = mod.widgets.TileGridBase{
        width: Fill height: Fill
        list := PortalList{
            width: Fill height: Fill
            Section := View{width: Fill height: Fit padding: Inset{top: 18 bottom: 8} section_title := Kicker{}}
            Tiles := View{
                width: Fill height: Fit flow: Right{wrap: true} spacing: 10 wrap_spacing: 10 padding: Inset{bottom: 10}
                tile_a := Tile{}
                tile_b := Tile{}
                tile_c := Tile{}
            }
        }
    }
    let MemberSeat = RoundedView{
        show_bg: false width: Fit height: Fit flow: Overlay
        draw_bg +: {color: palette.sel border_color: palette.accent border_size: 1 border_radius: 4}
        View{
            width: Fit height: Fit flow: Right spacing: 3.5 align: Align{y: 0.5}
            padding: Inset{left: 3 top: 3 bottom: 3}
            View{
                width: 20 height: 20 flow: Overlay
                badge := SeatIcons{}
                View{
                    width: Fill height: Fill align: Align{x: 1.0 y: 1.0}
                    live_mark := RoundedView{
                        visible: false width: 7 height: 7
                        draw_bg +: {color: palette.accent border_radius: 3.5 border_color: palette.bg border_size: 1}
                    }
                }
            }
            dot := StatusDot{}
            name := RailText{
                width: Fit{max: FitBound.Rel{base: Base.Full factor: 0.4}} padding: Inset{right: 9}
                max_lines: 1 text_overflow: Ellipsis
            }
        }
        tip := Tip{select := ButtonOutline{width: Fill height: Fill text: "" padding: 0 margin: 0}}
    }
    mod.widgets.PersonNamePrompt = View{
        visible: false width: Fill height: Fill flow: Down spacing: 8
        padding: Inset{left: 24 right: 24 top: 22 bottom: 8}
        ProjectHeading{text: "Your name"}
        person_name := PanelInput{empty_text: "Your name" a11y: {label: "Your name"}}
        name_continue := PanelButton{text: "Continue"}
        name_error := BodyNote{visible: false}
    }
    // In place of the town while this window has no home of its own: why, in one line.
    mod.widgets.HomeProblem = View{
        visible: false width: Fill height: Fill
        padding: Inset{left: 24 right: 24 top: 22 bottom: 8}
        home_problem_text := BodyNote{width: Fill}
    }
    // A town's notice (kept or set-aside towns, sharing, a town that couldn't open)
    // sits on its home page, never above the chat.
    mod.widgets.TownNotice = View{
        visible: false width: Fill height: Fit flow: Down spacing: 4
        town_notice_text := BodyNote{width: Fill}
        town_notice_share := PanelButton{visible: false width: Fill}
        town_notice_ok := PanelButton{visible: false}
        disclosure := View{
            visible: false width: Fill height: Fit
            fold := FoldHeader{
                header: View{
                    width: Fill height: Fit flow: Overlay align: Align{y: 0.5}
                    title := Kicker{margin: Inset{left: 16}}
                    fold_button := FoldButton{width: Fill height: 24}
                }
                body: View{width: Fill height: Fit explanation := BodyNote{width: Fill}}
            }
        }
    }
    mod.widgets.HomePage = View{
        visible: false width: Fill height: Fill flow: Down spacing: 4
        padding: Inset{left: 24 right: 24 top: 22 bottom: 8}
        ProjectHeaderBase{
            width: Fill height: Fit
            content := View{width: Fill height: Fit flow: Down spacing: 4
                View{
                    width: Fill height: Fit flow: Right{wrap: true row_align: RowAlign.Center}
                    spacing: 4 distribute: Distribute.SpaceBetween
                    home_title := TitleInput{width: Fill max_width: FitBound.Rel{base: Base.Parent factor: 0.75} a11y: {label: "Town name"}}
                    View{width: Fit height: Fit flow: Right spacing: 4 align: Align{y: 0.5}
                        home_join_call := JoinCallButton{}
                        call_share := PanelButton{visible: false enabled: false height: 26 text: "Share an app"}
                    }
                }
                choices := ShareChoices{visible: false}
                share_problem := BodyNote{visible: false width: Fill}
            }
        }
        town_notice := TownNotice{}
        home_members := mod.widgets.HomeMembersBase{
            width: Fill height: Fit
            live_call_color: palette.accent live_share_color: palette.good
            list := FlatList{
                width: Fill height: Fit{max: FitBound.Rel{base: Base.Full factor: 0.35}}
                flow: Right{wrap: true row_align: RowAlign.Center} spacing: 8
                scroll_bars +: {show_scroll_y: true}
                Seat := MemberSeat{}
                Add := Tip{
                    add_member := PanelButton{text: "+" a11y: {label: "Add a member"}}
                }
            }
        }
        add_menu := View{
            visible: false width: Fill height: Fit flow: Down spacing: 4
            home_pair := PanelButton{text: "Someone else's computer"}
            home_pair_mine := PanelButton{text: "Another computer of mine"}
            BodyNote{text: "Any member can invite someone. The other members don't need to approve."}
            home_ai := PanelButton{text: "Add my AI"}
        }
        share_incoming := PairingPrompt{visible: false}
        pair_step := ScrollYView{
            visible: false width: Fill height: Fit{max: FitBound.Rel{base: Base.Unused factor: 0.6}} flow: Down
            PairStep{}
        }
        ai_section := ScrollYView{
            visible: false width: Fill height: Fit{max: FitBound.Rel{base: Base.Unused factor: 0.6}} flow: Down
            AddAiStep{}
        }
        share_note := BodyNote{visible: false}
        share_cancel := PanelButton{visible: false text: "Withdraw offer"}
        member_card := View{
            visible: false width: Fill height: Fit flow: Down spacing: 4
            member_head := View{
                width: Fill height: Fit flow: Right{wrap: true row_align: RowAlign.Center} spacing: 6
                member_badge := SeatIcons{}
                member_name := RailText{}
                computer_label := PanelInput{visible: false width: Fill{max: 160} empty_text: "Your name" a11y: {label: "Your name"}}
                ai_name_input := PanelInput{visible: false width: Fill{max: 160} empty_text: "Your AI" a11y: {label: "AI name"}}
                ai_backend := BodyNote{visible: false width: Fit}
                member_status := BodyNote{width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}}
                hub_model := BodyNote{visible: false width: Fit max_lines: 1 text_overflow: Ellipsis}
                member_watch := PanelButton{visible: false text: "Watch"}
                member_open_call := PanelButton{visible: false text: "Open call"}
                member_call := PanelButton{visible: false text: "Call"}
                device_link := PanelButton{visible: false text: "Connect"}
                hub_offer := PanelButton{visible: false text: "Offer my AI Hub to this town"}
                remove_ai := PanelButton{visible: false text: "Remove"}
                remove_member := PanelButton{visible: false text: "Remove"}
                removing_line := BodyNote{visible: false width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}}
                undo_remove := PanelButton{visible: false text: "Undo"}
            }
            seat_devices := mod.widgets.SeatDevicesBase{
                visible: false width: Fill height: Fit padding: Inset{left: 12}
                list := FlatList{
                    width: Fill height: Fit flow: Down spacing: 4
                    scroll_bars +: {show_scroll_y: false}
                    Device := View{
                        width: Fill height: Fit flow: Right{wrap: true row_align: RowAlign.Center} spacing: 6
                        device_dot := StatusDot{}
                        device_name := RailText{}
                        device_status := BodyNote{width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}}
                        device_connect := PanelButton{visible: false text: "Connect"}
                        device_remove := PanelButton{visible: false text: "Remove"}
                    }
                }
            }
            cascade_prompt := View{
                visible: false width: Fill height: Fit flow: Right{wrap: true row_align: RowAlign.Center} spacing: 6
                cascade_question := BodyNote{width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}}
                cascade_all := PanelButton{}
                cascade_only := PanelButton{}
                cascade_cancel := PanelButton{text: "Cancel"}
            }
        }
        home_removed := RemovedNote{}
        home_grid := TileGrid{}
    }
    mod.widgets.ProjectPage = View{
        visible: false width: Fill height: Fill flow: Down spacing: 4
        padding: Inset{left: 24 right: 24 top: 18 bottom: 8}
        ProjectHeaderBase{
            width: Fill height: Fit
            content := View{width: Fill height: Fit flow: Down spacing: 4
                View{
                    width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 4
                    project_crumb := CrumbButton{}
                    View{width: Fill height: Fit flow: Right{wrap: true} spacing: 4 align: Align{x: 1 y: 0.5}
                        project_call := CallButton{}
                        call_share := PanelButton{visible: false enabled: false height: 26 text: "Share"}
                        remove_project := PanelButton{visible: false text: "Remove project" a11y: {label: "Remove empty project"}}
                    }
                }
            }
        }
        project_title := TitleInput{a11y: {label: "Project name"}}
        project_meta := BodyNote{margin: Inset{left: 4}}
        project_removed := RemovedNote{}
        ProjectHeaderBase{
            width: Fill height: Fit
            content := View{width: Fill height: Fit flow: Down spacing: 4
                choices := ShareChoices{visible: false}
                share_problem := BodyNote{visible: false width: Fill}
            }
        }
        overview_grid := TileGrid{overview: true}
    }
}

impl TownNotice {
    pub fn display(cx: &mut Cx, page: &WidgetRef, notice: Option<&Self>) {
        let card = page.widget(cx, ids!(town_notice));
        card.set_visible(cx, notice.is_some());
        let disclosure = card.widget(cx, ids!(disclosure));
        let fold = disclosure.fold_header(cx, ids!(fold));
        let toggle = disclosure.widget(cx, ids!(fold_button));
        let details = notice.and_then(|notice| notice.details.as_ref());
        // A disclosure belongs to the notice the person expanded, including its town and peer.
        if toggle.action_data().as_ref().and_then(|data| data.downcast_ref::<Option<NoticeDetails>>())
            .and_then(Option::as_ref) != details {
            fold.set_is_open(cx, false, Animate::No);
            toggle.set_action_data(details.cloned());
        }
        let Some(notice) = notice else { return };
        card.label(cx, ids!(town_notice_text)).set_text(cx, &notice.line);
        for (path, choice) in [(ids!(town_notice_share), &notice.offer), (ids!(town_notice_ok), &notice.hide)] {
            let button = card.widget(cx, path);
            button.set_visible(cx, choice.is_some());
            if let Some((text, action)) = choice {
                button.set_text(cx, text);
                button.set_action_data(action.clone());
            }
        }
        disclosure.set_visible(cx, details.is_some());
        if let Some(details) = details {
            disclosure.label(cx, ids!(title)).set_text(cx, details.label);
            disclosure.label(cx, ids!(explanation)).set_text(cx, &details.text);
        }
    }

    pub fn clicked<'a>(actions: &'a Actions) -> Option<&'a NoticeAction> {
        actions.iter().find_map(|action| {
            let event = action.as_widget_action()?;
            let target = event.data.as_ref()?.downcast_ref::<NoticeAction>()?;
            matches!(event.cast::<ButtonAction>(), ButtonAction::Clicked(_)).then_some(target)
        })
    }
}
