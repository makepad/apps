use makepad_widgets::*;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    let Selectable = PanelInput{
        is_read_only: true is_multiline: true height: Fit
        align: Align{x: 0 y: 0}
    }
    let ConnectionPanel = SolidView{
        width: "min(460px, 100vw - 24px)" height: Fit
        flow: Down draw_bg.color: palette.panel
        ProjectHeading{width: Fill padding: 12 text: "Connections"}
        PanelLine{}
        ScrollYView{
            width: Fill height: Fit max_height: "min(590px, 100vh - 104px)"
            padding: 14 spacing: 7 flow: Down
            ProjectHeading{text: "Link a device"}
            connect_lan := CheckBox{text: "Link directly on the same network"}
            connect_intro := View{
                width: Fill height: Fit spacing: 7 flow: Down
                BodyNote{text: "You'll get words to read to the other person, and they'll read theirs to you."}
                connect_start := PanelButton{text: "Start"}
                connect_devices := View{
                    visible: false width: Fill height: Fit flow: Down spacing: 7
                    connect_devices_warning := BodyNote{draw_text.color: palette.warn}
                    connect_devices_actions := View{
                        width: Fill height: Fit flow: Right{wrap: true} spacing: 7
                        connect_devices_retry := PanelButton{text: "Try again"}
                        connect_devices_forget := PanelButton{text: "Forget known devices…"}
                    }
                    connect_devices_confirmation := View{
                        visible: false width: Fill height: Fit flow: Down spacing: 7
                        BodyNote{text: "This moves the unreadable list aside, keeps it, and starts an empty one. Devices you linked with before need fresh words."}
                        View{
                            width: Fill height: Fit flow: Right{wrap: true} spacing: 7
                            connect_devices_confirm := PanelButton{text: "Forget them"}
                            connect_devices_cancel := PanelButton{text: "Cancel"}
                        }
                    }
                }
            }
            connect_form := View{
                visible: false width: Fill height: Fit spacing: 7 flow: Down
                Kicker{text: "YOUR WORDS"}
                connect_own_words := Selectable{a11y: {label: "Your words"}}
                connect_privacy := BodyNote{}
                connect_note := BodyNote{visible: false}
                Kicker{text: "YOUR NAME"}
                connect_name := PanelInput{empty_text: "How they'll see you" a11y: {label: "Your name"}}
                Kicker{text: "THEIR WORDS"}
                connect_peer_words := PanelInput{
                    is_multiline: true height: 54 align: Align{x: 0 y: 0}
                    empty_text: "Type the words they read to you" a11y: {label: "Their words"}
                }
                connect_go := PanelButton{text: "Connect"}
                connect_same := BodyNote{visible: false}
            }
            connect_linked := View{
                visible: false width: Fill height: Fit spacing: 7 flow: Down
                connect_linked_title := ProjectHeading{width: Fill}
                BodyNote{text: "Read these out together. If either differs, disconnect: someone may be between you."}
                Kicker{text: "YOUR DEVICE"}
                connect_own_device := Selectable{a11y: {label: "Your device"}}
                Kicker{text: "THEIR DEVICE"}
                connect_peer_device := Selectable{a11y: {label: "Their device"}}
                connect_checked := PanelButton{text: "I've checked the words"}
                share_note := BodyNote{visible: false}
                Kicker{text: "SHARE A SPACE"}
                share_status := BodyNote{}
                share_idle := View{
                    width: Fill height: Fit flow: Down spacing: 7
                    share_this := PanelButton{}
                    share_reason := BodyNote{visible: false draw_text.color: palette.muted}
                    share_history := BodyNote{}
                    share_new := PanelButton{}
                    BodyNote{text: "It starts empty. Nothing from this space goes along."}
                }
                share_cancel := PanelButton{visible: false text: "Cancel"}
                share_incoming := View{
                    visible: false width: Fill height: Fit flow: Down spacing: 7
                    share_counts := BodyNote{}
                    BodyNote{text: "Joining opens it here. Your own space stays as it is, and the space menu takes you back to it."}
                    share_private := BodyNote{}
                    View{
                        width: Fill height: Fit flow: Right{wrap: true} spacing: 7
                        share_join := PanelButton{text: "Join"}
                        share_decline := PanelButton{text: "Not now"}
                    }
                }
                share_open := PanelButton{visible: false text: "Open it"}
            }
            share_offline_note := BodyNote{visible: false}
            shared_space := View{
                visible: false width: Fill height: Fit flow: Down spacing: 7
                Kicker{text: "SHARED SPACE"}
                shared_line := BodyNote{}
                shared_stop := PanelButton{}
                shared_confirmation := View{
                    visible: false width: Fill height: Fit flow: Down spacing: 7
                    shared_question := ProjectHeading{width: Fill}
                    shared_consequences := BodyNote{}
                    View{
                        width: Fill height: Fit flow: Right{wrap: true} spacing: 7
                        shared_confirm := PanelButton{}
                        shared_cancel := PanelButton{text: "Cancel"}
                    }
                }
            }
            connect_status := BodyNote{visible: false}
            connect_error := BodyNote{visible: false draw_text.color: palette.warn}
            connect_stop := PanelButton{visible: false}
            PanelLine{}
            ai_commands := View{
                width: Fill height: Fit flow: Down spacing: 5
                ProjectHeading{text: "Connect your AI"}
                Kicker{text: "Terminal agent"}
                ai_terminal := Selectable{a11y: {label: "Terminal command"}}
                Kicker{text: "MCP server"}
                ai_mcp := Selectable{a11y: {label: "MCP command"}}
            }
            ai_another := PanelButton{
                visible: false text: "Connect another AI" padding: 0
                draw_bg +: {border_size: 0 color: palette.panel}
                draw_text.color: palette.accent
            }
        }
    }

    let SeatName = RailText{
        width: Fill max_lines: 1 text_overflow: Ellipsis
        draw_text.text_style: theme.font_bold{font_size: 8}
    }
    let SeatStatus = Kicker{
        width: Fill max_lines: 1 text_overflow: Ellipsis draw_text.color: palette.muted
    }
    let CaptionSeat = View{
        width: Fit height: Fit flow: Right spacing: 3 align: Align{y: 0.5}
    }
    mod.widgets.CaptionConnections = PopoverToggle{
        trigger: Manual placement: BottomEnd panel_padding: 1 offset: 5
        draw_panel +: {color: palette.panel border_color: palette.line border_color_2: palette.line}
        width: 510 height: 34 flow: Overlay align: Center
        connect_open := PanelButton{
            width: Fill height: Fill text: "" padding: 0 a11y: {label: "Connections"}
            draw_bg +: {color: palette.chrome border_color: palette.line border_size: 1 border_radius: 5}
        }
        caption_seats := View{
            width: Fit height: Fit flow: Right spacing: 9 padding: Inset{left: 6 right: 6} align: Align{y: 0.5}
            caption_you := CaptionSeat{
                a11y: {label: "You · online"}
                badge := SeatBadge{}
                PresenceDot{draw_bg.color: palette.good}
                words := View{width: 42 height: Fit flow: Down name := SeatName{} status := SeatStatus{}}
            }
            caption_ai := CaptionSeat{
                a11y: {label: "Your AI · not connected"}
                badge := AiBadge{}
                ai_offline := PresenceDot{}
                ai_online := PresenceDot{visible: false draw_bg.color: palette.good}
                words := View{width: 82 height: Fit flow: Down name := SeatName{} status := SeatStatus{}}
            }
            caption_peer := CaptionSeat{
                a11y: {label: "Peer · not connected"}
                badge := PeerBadge{}
                peer_offline := PresenceDot{}
                peer_reaching := PresenceDot{visible: false draw_bg.color: palette.warn}
                peer_online := PresenceDot{visible: false draw_bg.color: palette.good}
                words := View{width: 118 height: Fit flow: Down name := SeatName{} status := SeatStatus{}}
            }
            caption_peer_ai := CaptionSeat{
                a11y: {label: "Peer’s AI · not connected"}
                badge := PeerAiBadge{}
                PresenceDot{}
                words := View{width: 82 height: Fit flow: Down name := SeatName{} status := SeatStatus{}}
            }
            caption_link := Kicker{visible: false draw_text.color: palette.warn}
            Kicker{text: "▾" draw_text.color: palette.muted}
        }
        content := ConnectionPanel{}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::cx_draw::CxDraw;

    /// The caption's connection control sits wholly inside the caption bar, centred, with room
    /// above and below. On macOS the bar once took the system's shorter title bar height, and the
    /// control ran into the window's top edge.
    #[test]
    fn the_caption_control_sits_inside_the_caption_bar() {
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            crate::script_mod(vm);
        });
        let cx = &mut *pool;
        // The caption row as the window lays it out: the pinned height, centred on y.
        let bar = cx.with_vm(|vm| {
            let value = script_eval!(vm, {
                use mod.prelude.workspace.*
                use mod.widgets.*
                View{
                    width: Fill height: #(crate::CAPTION_HEIGHT) flow: Right align: Align{y: 0.5}
                    padding: Inset{left: 12 right: 12}
                    connections := CaptionConnections{}
                }
            });
            WidgetRef::script_from_value(vm, value)
        });
        let size = dvec2(1120., 200.);
        let pass = DrawPass::new(cx);
        pass.set_size(cx, size);
        let mut list = DrawList2d::new(cx);
        // The control's panel is a popover, drawn in the window's overlay.
        let overlay = cx.with_vm(|vm| Overlay::script_new(vm));
        {
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, None);
            list.begin_always(&mut frame);
            overlay.begin(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            bar.draw_all(&mut frame, &mut Scope::empty());
            frame.end_pass_sized_turtle();
            overlay.end(&mut frame);
            list.end(&mut frame);
            frame.end_pass(&pass);
        }
        let outer = bar.area().rect(cx);
        let control = bar.widget(cx, ids!(connections)).area().rect(cx);
        assert_eq!(outer.size.y, crate::CAPTION_HEIGHT, "the caption takes the pinned height");
        assert!(control.size.y > 0., "the control is laid out");
        let above = control.pos.y - outer.pos.y;
        let below = outer.pos.y + outer.size.y - (control.pos.y + control.size.y);
        assert!(above >= 3. && below >= 3., "the control keeps room inside the caption: {above} above, {below} below");
        assert!((above - below).abs() <= 1., "the control is centred in the caption: {above} above, {below} below");
    }
}
