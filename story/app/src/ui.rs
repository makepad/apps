//! Makepad Story's window: the caption (the File and Window menus, the CUT and DECK tabs,
//! the present window, the master level, volume and mute, the theme picker and the chat
//! toggles), the two pages and the window's one chat
//! beside them. The app owns actions and widget values. The caller registers
//! the widgets, the theme, the pages' desks and the chat panel first.

use makepad_widgets::*;

/// The window's template (`mod.story.Ui`), in its own namespace.
pub fn script_mod(vm: &mut ScriptVm) {
    script_eval!(vm, { mod.story = {} true });
    window::script_mod(vm);
}

mod window {
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let CaptionMode = Button{
        width: Fit height: 22 margin: 0 padding: Inset{left: 6 right: 6 top: 3 bottom: 3}
        grab_key_focus: false
        draw_bg +: {
            color: stage.raised
            color_hover: stage.control_top
            color_down: stage.row
            border_color: stage.border
            // ONE RADIUS: match the dropdown chrome (theme 2.5).
            border_radius: 2.5
            border_size: 1.0
        }
        draw_text +: {
            color: stage.text_soft
            color_hover: stage.accent
            text_style: theme.font_bold{font_size: 10}
        }
    }

    // A caption value as a dropdown slider: the chip takes only a click (a
    // drag in the caption would fight dragging the window) and its slider
    // lives in a popover below it.
    let CaptionSlider = DropSlider{
        draw_bg +: {color: stage.control color_hover: stage.control_hover border_color: stage.border}
        draw_icon +: {color: stage.label}
        draw_text +: {color: stage.text_bright text_style: theme.font_bold{font_size: 9}}
        draw_panel +: {color: stage.panel border_color: stage.border_hi}
        draw_track +: {color: stage.groove}
        draw_fill +: {color: stage.accent}
        draw_knob +: {color: stage.text_bright}
    }

    mod.story.Ui = Root{main_window := Window{
        window.title: "Makepad Story" window.inner_size: vec2(1680, 1040) window.caption_bar_height_override: 34.0
        pass +: {clear_color: stage.bg}
        caption_bar +: {new_batch: true draw_bg.color: stage.bg caption_label +: {flow: Right spacing: 10 padding: Inset{top: 3 right: 8} align: Align{x: 0 y: 0.5}
            label +: {visible: false} caption_icon +: {width: 0 height: 0 margin: 0}
            caption_window_gap := View{width: 72 height: 1}
            // The caption's whole height, not a 22 px row: the bold title's line box is as
            // tall as such a row, so the row clipped its foot. The 1 px bottom padding
            // sets the title's capitals level with the menus', on the caption's middle.
            caption_brand := View{width: Fit height: Fill padding: Inset{bottom: 1} flow: Right spacing: 7 align: Align{y: 0.5}
                Icon{icon_walk: Walk{width: 24 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/ui/logo_mark.svg") color: stage.accent}}
                // No padding: the label's default padding made it taller than the brand
                // row, so the row could not centre it and clipped its descenders.
                Label{width: Fit padding: 0 text: "Story" draw_text +: {color: stage.brand_word text_style: theme.font_bold{font_size: 13}}}
            }
            // File > Export > Web page… (web_page_export.rs); Window > Output
            // window (the monitor button's toggle).
            file_menu := MenuBar{width: Fit height: 22 bar_pad_x: 0
                // In the caption: the titles on the caption's own ground.
                draw_bg +: {pixel: fn(){ return vec4(0.0, 0.0, 0.0, 0.0) }}
                menus: [
                    {label: "File" items: [
                        {id: @export_heading label: "Export" enabled: false}
                        {id: @export_web_page label: "    Web page…"}
                    ]}
                    {label: "Window" items: [
                        {id: @output_window label: "Output window"}
                    ]}
                ]
            }
            tab_cut := CaptionMode{ text: "CUT"}
            tab_deck := CaptionMode{ text: "DECK"}
            View{width: Fill height: 22}
            View{width: Fit height: 22 flow: Right spacing: 10 align: Align{y: 0.5}
                Tip{text: "Open or close the separate output window: the deck's picture"
                    open_output := StageIconButton{margin: 0 draw_icon +: {svg: crate_resource("self:resources/icons/ui/monitor.svg")}}}
                // DECK's: the picture alone in Story's own window (F; Esc returns).
                Tip{text: "Full window: the deck's picture alone in this window (F; Esc returns)"
                    output_full := StageIconButton{visible: false margin: 0 draw_icon +: {svg: crate_resource("self:resources/icons/fit.svg")}}}
                // The sound's master (sound.rs: the device sink's level).
                Tip{text: "Master level"
                    utility_master_meter := LevelMeter{width: 68 height: 11
                        draw_bg +: {lamp_size: 7 hot_at: 0.72 track_color: stage.groove color_level: stage.accent color_hot: stage.hot color_mark: stage.mark color_lamp_off: stage.lamp_off color_lamp: stage.lamp}}}
                // Master volume as a dropdown slider, the same idiom.
                Tip{text: "Master volume: click for its slider"
                    utility_master_level := CaptionSlider{min: 0 max: 2 default: 1 display_scale: 100 suffix: "%"
                        draw_icon +: {svg: crate_resource("self:resources/icons/ui/volume.svg")}}}
                Tip{text: "Master mute — highlighted when the sound is muted"
                    utility_mute := StageIconButton{margin: 0 draw_icon +: {svg: crate_resource("self:resources/icons/ui/mute.svg")}}}
                Tip{text: "Theme: the house theme or a catalogue theme"
                    theme_btn := StageIconButton{margin: 0 draw_icon +: {svg: crate_resource("self:resources/icons/ui/theme.svg")}}}
                Tip{text: "Open or close generation and AI chat (F10)"
                    toggle_gen := StageIconButton{margin: 0 draw_icon +: {svg: crate_resource("self:resources/icons/gen.svg")}}}
            }
        }}
        body +: {width: Fill height: Fill flow: Down show_bg: true draw_bg.color: stage.bg
            workspace := View{width: Fill height: Fill new_batch: true
              // The one chat (the aichat panel; F10 and the chat button show and
              // hide it) docks beside whichever page is shown.
              chat_split := mod.widgets.DockSplitter{width: Fill height: Fill axis: SplitterAxis.Horizontal align: SplitterAlign.FromB(360) size: 8
                fold: SplitterCollapse.B min_vertical: 600 max_vertical: 260

              a: View{width: Fill height: Fill
                // Only the page being shown is built before the first frame:
                // the other is built after it (`ensure_page`), or when it is
                // opened before its turn.
                product_pages := PageFlip{width: Fill height: Fill lazy_init: true active_page: @deck_page flow: Overlay padding: 0 spacing: 0
                    // The video editor and product demo system.
                    cut_page := View{width: Fill height: Fill padding: 8
                        cut_surface := mod.widgets.StageCutDesk{}
                    }
                    // The deck player: slides of looping edits.
                    deck_page := View{width: Fill height: Fill padding: 8
                        deck_surface := mod.widgets.StageDeckDesk{}
                    }
                }
              }
              b: View{width: Fill height: Fill
                // The chat in the house accent: Send and the marks of a
                // running turn are ON.
                ai_chat := AiChatPanel{width: Fill height: Fill
                  header +: {status_dot +: {draw_bg +: {color_in_progress: stage.accent}}}
                  transcript +: {WorkRow +: {work_head +: {work_dot +: {draw_bg +: {color_in_progress: stage.accent}}}} ToolRow +: {tool_head +: {tool_dot +: {draw_bg +: {color_in_progress: stage.accent}}} tool_status +: {tool_bar_box +: {tool_bar +: {draw_bg +: {color_primary: stage.accent}}}}}}
                  composer +: {composer_bar +: {send_tip +: {send_button +: {draw_bg +: {accent: stage.accent ink_on: stage.on_accent}}}}}
                }
              }
              }
            }
            // File > Export > Web page…: the document, the folder, the
            // export's phase and its size report (web_page_export.rs).
            web_page_dialog := Modal{can_dismiss: false
                content +: {width: 560 height: Fit flow: Down spacing: 10 padding: 18 show_bg: true
                    draw_bg +: {
                        color: stage.panel
                        pixel: fn(){
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.box(0., 0., self.rect_size.x, self.rect_size.y, 8.)
                            sdf.fill(vec4(self.color.xyz, 1.))
                            return sdf.result
                        }
                    }
                    Label{width: Fill text: "Export web page" draw_text +: {color: stage.text_bright text_style: theme.font_bold{font_size: 14}}}
                    Label{width: Fill height: Fit text: "One page: index.html, a loader and one wasm with everything embedded. The song is the only file beside them. The document is traced at three sizes and checked by a held-out replay; a long film takes hours." draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 9}}}
                    View{width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                        Label{width: 70 text: "Document" draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 10}}}
                        wp_doc := DropDown{width: Fill height: 22 margin: 0 labels: ["—"]}
                    }
                    View{width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                        Label{width: 70 text: "Folder" draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 10}}}
                        wp_out := TextInput{width: Fill height: 22 margin: 0 padding: Inset{left: 4 right: 4 top: 3 bottom: 3} empty_text: "~/Movies/<title> page" draw_text +: {text_style: theme.font_regular{font_size: 9}}}
                    }
                    wp_status := Label{width: Fill height: Fit text: "" draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 10}}}
                    wp_report := Label{width: Fill height: Fit text: "" draw_text +: {color: stage.text_bright text_style: theme.font_code{font_size: 9}}}
                    View{width: Fill height: Fit flow: Right spacing: 8 align: Align{x: 1 y: 0.5}
                        wp_open := StageButton{visible: false width: Fit height: 24 padding: Inset{left: 8 right: 8} text: "Open folder"}
                        wp_close := StageButton{width: 72 height: 24 text: "Close"}
                        wp_start := StageButton{width: 72 height: 24 text: "Export"}
                    }
                }
            }
            // The theme button's picker (the shell's ThemePicker).
            theme_picker := mod.widgets.ThemePickerPanel{}
            // The menus a control raises, over everything else in the window.
            menus := MenuLayer{width: 0 height: 0 abs_pos: vec2(0.0, 0.0)}
            // The window's floating layers (the tooltip host): last, so a
            // control's Tip{} shows over every panel.
            layers := OverlayLayers{}
        }
    }}
}
}
