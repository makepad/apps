//! The print panels, each a dock tab:
//!
//! * [`FabSettingsPanel`] — the job: profile source, printer, process and the
//!   print's colours on the printer's trays (the process's values are the slicer
//!   panel's, `ui::print_settings`).
//! * [`FabPrinterPanel`] — the linked printer: the confirm card the top bar's Print opens, the
//!   printer's card (model, status, nozzles, temperatures), the job in progress, every tray with
//!   its real colour and the print's colours mapped onto the trays. The confirm card is the only
//!   way to start a print: Start print shows only while nothing stands in the way, and the card
//!   says what does.
//!
//! The panels only edit through [`PrintAction`]s.

use super::*;
use makepad_fab_settings::virtual_printer::Kind as VirtualKind;
use crate::api::AppState;
use std::collections::HashMap;
use crate::ui::popover::{menu_picked, open_menu, ui_actions, FabUiAction, MenuItem, MenuPlace};

/// A menu longer than this splits into submenus.
const MENU_CHUNK: usize = 24;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // A titled group inside a panel: a small upper-case caption over its rows.
    mod.widgets.FabGroup = View{
        width: Fill
        height: Fit
        flow: Down
        spacing: 4
        padding: Inset{left: 12 right: 12 top: 10 bottom: 12}
        show_bg: true
        draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.rect(0.0, self.rect_size.y - 1.0, self.rect_size.x, 1.0)
                sdf.fill(fab.color_border)
                return sdf.result
            }
        }
    }
    mod.widgets.FabGroupTitle = mod.widgets.FabLabel{
        margin: Inset{bottom: 4}
        draw_text +: {
            color: fab.color_text_dim
            text_style: theme.font_bold{ font_size: fab.font_size_small }
        }
    }
    // A raised card (the printer, the job).
    mod.widgets.FabCard = View{
        width: Fill
        height: Fit
        flow: Down
        spacing: 6
        padding: Inset{left: 12 right: 12 top: 10 bottom: 10}
        show_bg: true
        draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                sdf.fill_keep(fab.color_panel_sub)
                sdf.stroke(fab.color_border, 1.0)
                return sdf.result
            }
        }
    }
    // A status: its words, then a dot coloured by `state` (0 off, 1 ok, 2 busy, 3 error) at the
    // right, no backdrop.
    mod.widgets.FabStatusPill = View{
        width: Fit
        height: 20
        flow: Right
        align: Align{x: 0.0 y: 0.5}
        padding: Inset{left: 0 right: 14 top: 0 bottom: 0}
        show_bg: true
        draw_bg +: {
            state: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                var c = fab.color_text_muted
                if self.state > 0.5 { c = fab.color_ok }
                if self.state > 1.5 { c = fab.color_accent }
                if self.state > 2.5 { c = fab.color_error }
                sdf.circle(self.rect_size.x - 4.0, self.rect_size.y * 0.5, 3.0)
                sdf.fill(c)
                return sdf.result
            }
        }
        text := mod.widgets.FabLabelSmall{ text: "" draw_text +: { color: fab.color_text } }
    }

    let PrintRow = mod.widgets.FabFieldRow{}
    let Pick = mod.widgets.FabDropdownButton{
        width: Fill
        height: theme.prop_row_height
        label +: { width: Fill max_lines: 1 text_overflow: TextOverflow.Ellipsis text: "—" }
    }

    // A printer's folding section in the Printers editor: its name, and a status pill in the
    // header's trail.
    let PrinterSection = PropSection{
        header +: {hdr +: {
            icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/printer.svg")}}
            title +: {text: ""}
            trail +: {
                width: Fit height: Fit flow: Right spacing: 4 align: Align{y: 0.5}
                pill := mod.widgets.FabStatusPill{}
            }
        }}
        body +: {flow: Down spacing: 2 padding: Inset{top: 4 bottom: 8}}
    }

    // A part of a printer's section: a nested section that folds, as the Part panel's parameter
    // groups and the Legend's parts (its bar a short level in), no card behind it.
    let Sub = PropSection{
        // with its body's spacing (2), the sections' gap above it
        margin: Inset{top: theme.prop_section_gap}
        header +: {hdr +: {
            // in by the indent on the left; on the right, the same edge as every header's
            margin: Inset{left: 10 right: theme.prop_pad}
            padding: Inset{left: theme.prop_label_gap right: theme.prop_pad}
        }}
        body +: {flow: Down spacing: 6 padding: Inset{left: 10 right: theme.prop_pad top: 6 bottom: 8}}
    }

    // A row of the Status table: the name in a column of its own, the value beside it; a rule
    // between the columns and under the row, and every other row shaded (`odd`).
    let TableRow = View{
        width: Fill height: Fit flow: Right align: Align{y: 0.5}
        show_bg: true
        draw_bg +: {
            odd: instance(0.0)
            last: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.rect(1.0, 0.0, self.rect_size.x - 2.0, self.rect_size.y)
                sdf.fill(vec4(1.0, 1.0, 1.0, 0.03 * self.odd))
                // the rule between the columns, and under the row (not the last)
                sdf.rect(#(TABLE_NAME_W), 0.0, 1.0, self.rect_size.y)
                sdf.fill(#xffffff14)
                sdf.rect(1.0, self.rect_size.y - 1.0, self.rect_size.x - 2.0, (1.0 - self.last))
                sdf.fill(#xffffff14)
                return sdf.result
            }
        }
        name := mod.widgets.FabLabelDim{ width: #(TABLE_NAME_W) padding: Inset{left: 8 right: 8 top: 5 bottom: 5} text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis }
        value := mod.widgets.FabLabel{ width: Fill padding: Inset{left: 8 right: 8 top: 5 bottom: 5} text: "" }
    }

    // The job of the printer in use (in the Printers editor's Jobs tab): its profile, its
    // colours on the printer's trays, its slice, each a section of the printer's.
    mod.widgets.FabSettingsPanelBase = #(FabSettingsPanel::register_widget(vm))
    mod.widgets.FabSettingsPanel = set_type_default() do mod.widgets.FabSettingsPanelBase{
        width: Fill
        height: Fit
        flow: Down
        spacing: 2
        profile := Sub{
            header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/gear.svg")}} title +: {text: "Profile"}}}
            body +: {
                source_row := PrintRow{ name +: {text: "Profiles"}
                    source := Pick{ tag: @print_source owner: @print_source }
                }
                machine_row := PrintRow{ name +: {text: "Printer"}
                    machine := Pick{ tag: @print_machine owner: @print_machine }
                }
                plate_row := PrintRow{ name +: {text: "Plate"}
                    plate := Pick{ tag: @print_plate owner: @print_plate }
                }
                process_row := PrintRow{ name +: {text: "Process"}
                    process := Pick{ tag: @print_process owner: @print_process }
                }
            }
        }
        colours := Sub{
            header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/palette.svg")}} title +: {text: "Colours"}}}
            body +: {
                map := mod.widgets.FabColorMap{}
                note := mod.widgets.FabLabelSmall{ width: Fill text: "" }
            }
        }
        slice := Sub{
            header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/layers.svg")}} title +: {text: "Slice"}}}
            body +: {
                status_row := View{
                    width: Fill height: Fit flow: Down spacing: 6
                    progress := mod.widgets.FabProgress{ width: Fill visible: false }
                    status := mod.widgets.FabLabel{ width: Fill text: "Not sliced yet." }
                }
                actions := View{
                    width: Fill height: Fit flow: Right spacing: 6
                    FabTip{ text: "Write the sliced plate as a printer package"
                        export := mod.widgets.FabButton{ text: "Export…" }
                    }
                }
                summary := mod.widgets.FabLabelSmall{ width: Fill text: "" }
            }
        }
    }

    // The printers other than the one in use, before or after it in the list (`part` 0 or 1):
    // each a section with what it last reported and the button that puts it in use.
    mod.widgets.FabPrinterSectionsBase = #(FabPrinterSections::register_widget(vm))
    mod.widgets.FabPrinterSections = set_type_default() do mod.widgets.FabPrinterSectionsBase{
        width: Fill height: Fit flow: Down
        Printer := PrinterSection{
            body +: {
                summary := mod.widgets.FabLabelDim{ width: Fill text: "" }
                use_row := View{
                    width: Fill height: Fit flow: Right
                    FabTip{ text: "Use this printer: its trays, its job and its status show here, and Print sends to it"
                        use_btn := mod.widgets.FabButton{ text: "Use this printer" }
                    }
                }
            }
        }
    }

    // The Jobs tab: each printer a section with the jobs sent to it (and "Other printers" for
    // jobs of printers no longer set up), each job a row as the Jobs list's.
    mod.widgets.FabPrinterJobsBase = #(FabPrinterJobs::register_widget(vm))
    mod.widgets.FabPrinterJobs = set_type_default() do mod.widgets.FabPrinterJobsBase{
        width: Fill height: Fit flow: Down
        Printer := PrinterSection{
            body +: {
                jobs := mod.widgets.FabJobRows{}
                none := mod.widgets.FabLabelDim{ width: Fill visible: false margin: Inset{left: 10} text: "No jobs sent to this printer yet." }
                use_row := View{
                    width: Fill height: Fit flow: Right margin: Inset{left: 10 top: 4}
                    FabTip{ text: "Use this printer: its job (profile, colours, slice) shows here, and Print sends to it"
                        use_btn := mod.widgets.FabButton{ text: "Use this printer" }
                    }
                }
            }
        }
    }

    mod.widgets.FabPrinterPanelBase = #(FabPrinterPanel::register_widget(vm))
    mod.widgets.FabPrinterPanel = set_type_default() do mod.widgets.FabPrinterPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        // the toolbar: the area's editor selector, Add printer, and the filter
        editor_bar := mod.widgets.FabEditorBar{
            // the printers, or the jobs sent to each
            view_tabs := View{
                width: Fit height: Fit flow: Right align: Align{y: 0.5}
                printers_tab := mod.widgets.FabUnderlineTab{ text: "Printers" a11y: {label: "Printers"} }
                jobs_tab := mod.widgets.FabUnderlineTab{ text: "Jobs" a11y: {label: "Jobs"} }
            }
            mod.widgets.FabVRule{}
            FabTip{ text: "Add a printer: a virtual twin of a printer model, or your network printer"
                add_printer := mod.widgets.FabIconButton{
                    width: 28 height: 24 padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
                    a11y: {label: "Add printer"}
                    draw_icon +: {svg: crate_resource("self://resources/icons/plus.svg")}
                }
            }
            // narrows the list to the printers whose names hold every word typed
            search := mod.widgets.FabSearch{width: Fill{min: 80} row +: {input +: {empty_text: "Filter"}}}
        }
        body := mod.widgets.FabScroll{
            padding: Inset{left: theme.prop_pad right: theme.prop_pad top: #(crate::ui::widgets::PANEL_TOP)}
            // no printer in use (or none at all): what to do
            none := mod.widgets.FabCard{
                visible: true
                margin: Inset{bottom: 10}
                title := mod.widgets.FabTitleLabel{ text: "" draw_text +: { color: fab.color_text_well } }
                text := mod.widgets.FabLabelDim{ width: Fill text: "" draw_text +: { color: fab.color_text_well_dim } }
            }
            before := mod.widgets.FabPrinterSections{part: 0}
            // the printer in use: everything it reports, its trays, the print's question and its
            // camera, each a section of its own that folds (no cards)
            active := PrinterSection{
                visible: false
                body +: {
                    link_note := mod.widgets.FabLabelSmall{width: Fill height: Fit text: "" visible: false}
                    // the print's question, from the top bar's Print until Start print or Cancel
                    confirm := Sub{
                        visible: false
                        header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/play.svg")}} title +: {text: "Print"}}}
                        body +: {
                            question := mod.widgets.FabLabel{ width: Fill text: "" }
                            stick_row := PrintRow{ visible: false name +: {text: "USB stick"}
                                stick := Pick{ tag: @resin_stick owner: @resin_stick }
                            }
                            progress := mod.widgets.FabProgress{ width: Fill height: 8 visible: false }
                            check := mod.widgets.FabLabelDim{ width: Fill text: "" }
                            blocker := mod.widgets.FabLabel{ width: Fill text: "" visible: false draw_text +: { color: fab.color_error } }
                            FabTip{ text: "Show this printability error in the slice view; again for the next one"
                                show_issue := mod.widgets.FabButton{ text: "" visible: false }
                            }
                            accept := mod.widgets.FabCheckBox{ text: "" visible: false }
                            flow_cali := mod.widgets.FabCheckBox{ text: "Flow calibration" }
                            timelapse := mod.widgets.FabCheckBox{ text: "Timelapse" }
                            plate_clear := mod.widgets.FabCheckBox{ text: "The plate is clear: the last print is off the bed" }
                            speed_row := mod.widgets.FabVirtualSpeed{}
                            buttons := View{
                                width: Fill height: Fit flow: Right spacing: 6
                                go := mod.widgets.FabButtonAccent{ text: "Start print" }
                                cancel := mod.widgets.FabButton{ text: "Cancel" }
                            }
                        }
                    }
                    // the printer's own state: its live view, its model, heads, bed, storage and
                    // errors; the camera pops out to float over the window by its button in the
                    // header (and comes back by the floating card's ×, or here)
                    card := Sub{
                        visible: false
                        header +: {hdr +: {
                            icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/printer.svg")}}
                            title +: {text: "Status"}
                            trail +: {
                                width: Fit height: Fit flow: Right align: Align{y: 0.5}
                                pop_tip := FabTip{ text: "Pop the camera out: it floats over the window, wherever you work"
                                    pop_out := mod.widgets.FabIconButton{
                                        width: 24 height: 22 padding: Inset{left: 4 right: 4 top: 3 bottom: 3}
                                        a11y: {label: "Pop out camera"}
                                        draw_icon +: {svg: crate_resource("self://resources/icons/maximize.svg")}
                                    }
                                }
                            }
                        }}
                        body +: {
                            cam_box := View{ width: Fill height: 200 camera := mod.widgets.FabPrinterCamera{} }
                            floating := View{
                                visible: false
                                width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                                mod.widgets.FabLabelDim{ width: Fill text: "The camera floats over the window." }
                                FabTip{ text: "Put the camera back here"
                                    back := mod.widgets.FabButton{ text: "Back here" }
                                }
                            }
                            // what the printer reports, as a table: a row each for its model, each head,
                            // the bed, the chamber, storage and errors (`printer_rows`)
                            status_table := View{
                                width: Fill height: Fit flow: Down spacing: 0
                                show_bg: true
                                draw_bg +: {
                                    pixel: fn() {
                                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                                        sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius)
                                        sdf.stroke(#xffffff14, 1.0)
                                        return sdf.result
                                    }
                                }
                                row0 := TableRow{ visible: false }
                                row1 := TableRow{ visible: false }
                                row2 := TableRow{ visible: false }
                                row3 := TableRow{ visible: false }
                                row4 := TableRow{ visible: false }
                                row5 := TableRow{ visible: false }
                                row6 := TableRow{ visible: false }
                                row7 := TableRow{ visible: false }
                                row8 := TableRow{ visible: false }
                                row9 := TableRow{ visible: false }
                            }
                            note := mod.widgets.FabLabel{ width: Fill text: "" }
                            controls := View{
                                width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                                clear := mod.widgets.FabButton{ text: "Clear printer error" visible: false }
                                Filler{}
                                FabTip{ text: "List the files on the printer's USB drive"
                                    storage := mod.widgets.FabButton{ text: "Storage" }
                                }
                            }
                        }
                    }
                    orient := Sub{
                        visible: false
                        header +: {hdr +: {title +: {text: "Orientation test"}}}
                        body +: {
                            text := mod.widgets.FabLabelDim{ width: Fill text: "" }
                            status := mod.widgets.FabLabel{ width: Fill text: "" visible: false }
                            FabTip{ text: "Write the orientation test's file to the printer's USB stick"
                                save := mod.widgets.FabButton{ text: "Save the test to the stick" }
                            }
                        }
                    }
                    stats_card := Sub{
                        visible: false
                        header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/layers.svg")}} title +: {text: "The printer reports"}}}
                        body +: {
                            heads := mod.widgets.FabColourRows{}
                            printer_text := mod.widgets.FabLabelDim{ width: Fill text: "" }
                            job_title := mod.widgets.FabGroupTitle{ text: "THE JOB THAT WAS SENT" }
                            job_text := mod.widgets.FabLabelDim{ width: Fill text: "" }
                            filaments := mod.widgets.FabColourRows{ job: true }
                            FabTip{ text: "Everything else the printer's reports carry, without who or where it is"
                                raw_button := mod.widgets.FabButton{ text: "Raw report" }
                            }
                            raw := mod.widgets.FabLabelDim{ width: Fill visible: false text: "" }
                        }
                    }
                    job := mod.widgets.FabJobCard{ visible: false }
                    trays_card := Sub{
                        visible: false
                        header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/nozzle.svg")}} title +: {text: "Filament on the printer"}}}
                        body +: {
                            trays := mod.widgets.FabTrayGrid{}
                        }
                    }
                    map_card := Sub{
                        header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/palette.svg")}} title +: {text: "Colours → trays"}}}
                        body +: {
                            map := mod.widgets.FabColorMap{}
                        }
                    }
                    // its settings, last: how Fab links to it, a virtual printer's feed paths, and calibration
                    settings_box := Sub{
                        header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/gear.svg")}} title +: {text: "Settings"}}}
                        body +: {
                            mode_row := PrintRow{ name +: { text: "Link" }
                                modes := View{width: Fit height: Fit flow: Right
                                    connected := mod.widgets.FabSegmentTab{text: "Connected" a11y: {label: "Connected mode"}}
                                    disconnected := mod.widgets.FabSegmentTab{text: "Virtual" a11y: {label: "Virtual mode"}}
                                }
                            }
                            // a virtual printer's feed paths
                            virtual_card := View{
                                visible: false
                                width: Fill height: Fit flow: Down spacing: 6
                                kind_row := PrintRow{ name +: {text: "Kind"}
                                    FabTip{width: Fill text: "Choose trays feeding nozzles or one spool per tool"
                                        kind := Pick{tag: @virtual_kind owner: @virtual_kind}
                                    }
                                }
                                heads_row := PrintRow{ name +: {text: "Heads"}
                                    FabTip{text: "Remove one head" minus := mod.widgets.FabButton{text: "−"}}
                                    count := mod.widgets.FabLabel{text: "" width: Fit}
                                    FabTip{text: "Add one head" plus := mod.widgets.FabButton{text: "+"}}
                                }
                                trays_row := PrintRow{ name +: {text: "Trays per nozzle"}
                                    FabTip{text: "Remove one tray per nozzle" minus := mod.widgets.FabButton{text: "−"}}
                                    count := mod.widgets.FabLabel{text: "" width: Fit}
                                    FabTip{text: "Add one tray per nozzle" plus := mod.widgets.FabButton{text: "+"}}
                                }
                                tpu_row := FabTip{text: "Give TPU its own external feed path"
                                    tpu := mod.widgets.FabCheckBox{text: "Dedicated TPU path"}
                                }
                                reset_row := PrintRow{ name +: {text: "Preset"}
                                    FabTip{width: Fill text: "Replace the layout and tray contents with a preset"
                                        reset := Pick{tag: @virtual_reset owner: @virtual_reset label +: {text: "Reset to preset"}}
                                    }
                                }
                                mod.widgets.FabLabelSmall{width: Fill height: Fit flow: Right{wrap: true} text: "Set slot contents in the trays below or in the colour list."}
                            }
                            no_settings := mod.widgets.FabLabelDim{ width: Fill text: "This printer has no link or feed settings in Fab." visible: false }
                            // tuning Fab to this printer and its filament: each step a section
                            calibrate_part := View{
                                width: Fill height: Fit flow: Down spacing: 4 margin: Inset{top: 4}
                                cal_title := mod.widgets.FabGroupTitle{ text: "CALIBRATION" }
                                cal_context := mod.widgets.FabLabelDim{ width: Fill text: "" }
                                steps := mod.widgets.FabCalibrateSteps{}
                                cal_status := mod.widgets.FabLabelDim{ width: Fill text: "" }
                            }
                        }
                    }
                }
            }
            after := mod.widgets.FabPrinterSections{part: 1}
        }
        // the Jobs tab: every printer with the jobs sent to it
        jobs_body := mod.widgets.FabScroll{
            visible: false
            padding: Inset{left: theme.prop_pad right: theme.prop_pad top: #(crate::ui::widgets::PANEL_TOP)}
            jobs_none := mod.widgets.FabCard{
                visible: false
                margin: Inset{bottom: 10}
                title := mod.widgets.FabTitleLabel{ text: "No printer in use." draw_text +: { color: fab.color_text_well } }
                text := mod.widgets.FabLabelDim{ width: Fill text: "Use one of the printers below: its job, its profile, colours and slice, shows in its section." draw_text +: { color: fab.color_text_well_dim } }
            }
            jobs_before := mod.widgets.FabPrinterJobs{part: 0}
            // the printer in use: its job (profile, colours, slice), then the jobs sent to it
            jobs_active := PrinterSection{
                visible: false
                body +: {
                    job_settings := mod.widgets.FabSettingsPanel{}
                    sent := Sub{
                        header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/clock.svg")}} title +: {text: "Sent jobs"}}}
                        body +: {
                            sent_rows := mod.widgets.FabJobRows{}
                            sent_none := mod.widgets.FabLabelDim{ width: Fill visible: false text: "No jobs sent to this printer yet." }
                        }
                    }
                }
            }
            jobs_after := mod.widgets.FabPrinterJobs{part: 1}
        }
    }
}

/// The card when no printer is in use: with printers to pick from, and with none at all.
const NO_PRINTER: (&str, &str) = ("No printer in use.", "Use one of the printers below to assign materials, slice and print on it.");
const NO_PRINTERS: (&str, &str) = ("No printers yet.", "Add a virtual twin with + to assign materials, slice and try printing without a printer, or add your printer's address and access code in ~/.makepad/fab/printers.");

/// Shows the Printers editor's Jobs tab (where the job of the printer in use is).
#[derive(Clone, Debug)]
pub struct ShowJobTab;

/// The printers the editor lists: every one but "No printer".
fn listed(p: &PrintState) -> Vec<(PrinterChoice, String)> {
    p.links.choices.iter().filter(|(c, _)| *c != PrinterChoice::None).cloned().collect()
}

/// Whether `title` holds every word of the filter (lowercased words; none: all).
fn shows(words: &[String], title: &str) -> bool {
    let title = title.to_lowercase();
    words.iter().all(|w| title.contains(w.as_str()))
}

// ===========================================================================
// The Jobs tab
// ===========================================================================

/// The Jobs tab's sections: a printer each (as the Printers tab lists them, the filter's), its
/// header saying how many jobs it has and whether one prints, its body the jobs sent to it; then
/// "Other printers" for jobs of printers no longer set up, when there are any.
#[derive(Script, Widget)]
pub struct FabPrinterJobs {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[layout] layout: Layout,
    /// The printers before the one in use (0) or after it (1, with "Other printers").
    #[live] part: u32,
    /// The editor's filter, its words lowercased.
    #[rust] pub query: Vec<String>,
    #[rust] templates: HashMap<LiveId, ScriptObjectRef>,
    #[rust] items: ComponentMap<LiveId, WidgetRef>,
    /// The sections drawn last, and their printers (none: "Other printers").
    #[rust] shown: Vec<LiveId>,
    #[rust] choices: Vec<(LiveId, PrinterChoice)>,
    /// The target when the sections were drawn: a Use press links from it.
    #[rust] owner: EditOwner,
    #[redraw] #[rust] area: Area,
}

impl ScriptHook for FabPrinterJobs {
    fn on_before_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.templates.clear();
        }
    }

    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if let (Some(id), Some(t)) = (kv.key.as_id(), kv.value.as_object()) {
                            self.templates.insert(id, vm.new_object_ref(t));
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            if let Some(template) = self.templates.get(&live_id!(Printer)) {
                let v: ScriptValue = template.as_object().into();
                for (_, w) in self.items.iter_mut() {
                    w.script_apply(vm, apply, scope, v);
                }
            }
        }
    }
}

impl FabPrinterJobs {
    /// The sections shown, in order: their headers for the editor's swipe-fold.
    pub fn headers(&self) -> Vec<WidgetRef> {
        self.shown.iter().filter_map(|id| self.items.get(id).cloned()).collect()
    }

    fn item(&mut self, cx: &mut Cx, id: LiveId, open: bool) -> Option<WidgetRef> {
        if let Some(w) = self.items.get(&id) {
            return Some(w.clone());
        }
        let value: ScriptValue = self.templates.get(&live_id!(Printer))?.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        cx.widget_tree_insert_child(self.uid, id, w.clone());
        // open at first: the printer in use, and one with a job on its way or printing
        w.as_prop_section().set_is_open(cx, open, Animate::No);
        self.items.insert(id, w.clone());
        Some(w)
    }
}

impl Widget for FabPrinterJobs {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Draw(_)) { return; }
        for id in &self.shown {
            if let Some(w) = self.items.get(id) {
                w.handle_event(cx, event, scope);
            }
        }
        let Event::Actions(actions) = event else { return };
        for (id, choice) in &self.choices {
            if self.items.get(id).is_some_and(|w| w.button(cx, ids!(use_btn)).clicked(actions)) {
                cx.action(PrintAction::LinkPrinter(self.owner.clone(), choice.clone()));
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        use crate::print::job_list::{jobs_count, FabJobRows, JobsOf};
        cx.begin_turtle(walk, self.layout);
        self.shown.clear();
        // the sections first (what each holds), then drawn: a section's rows read the app's state
        let mut plan: Vec<(LiveId, String, JobsOf, usize, bool, bool)> = Vec::new();
        if let Some(state) = scope.data.get::<AppState>() {
            let p = &state.print;
            self.owner.capture(PrinterScope::Target, p);
            let printers = listed(p);
            let ids: Vec<String> = printers.iter().filter_map(|(c, _)| c.id()).collect();
            // the printers before the one in use, or after it (all of them before when none is)
            let at = printers.iter().position(|(c, _)| *c == p.links.picked);
            let range = match (self.part, at) {
                (0, Some(i)) => 0..i,
                (0, None) => 0..printers.len(),
                (_, Some(i)) => i + 1..printers.len(),
                (_, None) => 0..0,
            };
            self.choices.clear();
            let mut sections: Vec<(LiveId, String, JobsOf, bool)> = Vec::new();
            for (c, title) in &printers[range] {
                let Some(pid) = c.id().filter(|_| shows(&self.query, title)) else { continue };
                self.choices.push((choice_id(c), c.clone()));
                sections.push((choice_id(c), title.clone(), JobsOf::Printer(pid), true));
            }
            // jobs of printers no longer set up: last, after every printer
            let others = JobsOf::Others(ids);
            if (self.part == 1 || at.is_none()) && jobs_count(p, &others).0 > 0 && shows(&self.query, "Other printers") {
                sections.push((live_id!(other_printers), "Other printers".to_string(), others, false));
            }
            for (id, title, of, usable) in sections {
                let (count, active) = jobs_count(p, &of);
                plan.push((id, title, of, count, active, usable));
            }
        }
        for (id, title, of, count, active, usable) in plan {
            let Some(w) = self.item(cx, id, active) else { continue };
            w.view(cx, ids!(use_row)).set_visible(cx, usable);
            w.label(cx, ids!(hdr.title)).set_text(cx, &title);
            let (dot, text) = match count {
                _ if active => (2.0, format!("Printing · {count} jobs")),
                0 => (0.0, "No jobs".to_string()),
                1 => (1.0, "1 job".to_string()),
                n => (1.0, format!("{n} jobs")),
            };
            let pill = w.view(cx, ids!(pill));
            if let Some(mut pill) = pill.borrow_mut() {
                pill.draw_bg.set_dyn_instance(cx, live_id!(state), &[dot as f32]);
            }
            pill.label(cx, ids!(text)).set_text(cx, &text);
            w.label(cx, ids!(none)).set_visible(cx, count == 0);
            if let Some(mut rows) = w.widget(cx, ids!(jobs)).borrow_mut::<FabJobRows>() {
                rows.of = of;
            }
            w.draw_all(cx, scope);
            self.shown.push(id);
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

// ===========================================================================
// The printers not in use
// ===========================================================================

/// The printers before or after the one in use (`part` 0 or 1), each a folding section with the
/// status it last reported and "Use this printer". Sections are made from
/// the `Printer` template, one per printer, and kept for its next draw.
#[derive(Script, Widget)]
pub struct FabPrinterSections {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[layout] layout: Layout,
    #[live] part: u32,
    /// The editor's filter, its words lowercased.
    #[rust] pub query: Vec<String>,
    #[rust] templates: HashMap<LiveId, ScriptObjectRef>,
    #[rust] items: ComponentMap<LiveId, WidgetRef>,
    /// The sections drawn last, and their printers.
    #[rust] shown: Vec<(LiveId, PrinterChoice)>,
    /// The target when the sections were drawn: a Use press links from it.
    #[rust] owner: EditOwner,
    #[redraw] #[rust] area: Area,
}

impl ScriptHook for FabPrinterSections {
    fn on_before_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.templates.clear();
        }
    }

    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if let (Some(id), Some(t)) = (kv.key.as_id(), kv.value.as_object()) {
                            self.templates.insert(id, vm.new_object_ref(t));
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            if let Some(template) = self.templates.get(&live_id!(Printer)) {
                let v: ScriptValue = template.as_object().into();
                for (_, w) in self.items.iter_mut() {
                    w.script_apply(vm, apply, scope, v);
                }
            }
        }
    }
}

impl FabPrinterSections {
    /// The sections shown, in order: their headers for the editor's swipe-fold.
    pub fn headers(&self) -> Vec<WidgetRef> {
        self.shown.iter().filter_map(|(id, _)| self.items.get(id).cloned()).collect()
    }

    fn item(&mut self, cx: &mut Cx, id: LiveId) -> Option<WidgetRef> {
        if let Some(w) = self.items.get(&id) {
            return Some(w.clone());
        }
        let value: ScriptValue = self.templates.get(&live_id!(Printer))?.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        cx.widget_tree_insert_child(self.uid, id, w.clone());
        // a printer not in use starts folded: its header says how it is
        w.as_prop_section().set_is_open(cx, false, Animate::No);
        self.items.insert(id, w.clone());
        Some(w)
    }
}

impl Widget for FabPrinterSections {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Draw(_)) { return; }
        // (their headers' hover and swipe-fold are the editor's: `FabPrinterPanel::headers`)
        for (id, _) in &self.shown {
            if let Some(w) = self.items.get(id) {
                w.handle_event(cx, event, scope);
            }
        }
        let Event::Actions(actions) = event else { return };
        for (id, choice) in &self.shown {
            let Some(w) = self.items.get(id) else { continue };
            if w.button(cx, ids!(use_btn)).clicked(actions) {
                cx.action(PrintAction::LinkPrinter(self.owner.clone(), choice.clone()));
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, self.layout);
        self.shown.clear();
        if let Some(state) = _scope.data.get::<AppState>() {
            let p = &state.print;
            self.owner.capture(PrinterScope::Target, p);
            let printers = listed(p);
            let at = printers.iter().position(|(c, _)| *c == p.links.picked).filter(|_| p.resin.is_none());
            let range = match (self.part, at) {
                (0, Some(i)) => 0..i,
                (0, None) => 0..printers.len(),
                (_, Some(i)) => i + 1..printers.len(),
                (_, None) => 0..0,
            };
            for (choice, title) in &printers[range] {
                if !shows(&self.query, title) { continue; }
                let id = choice_id(choice);
                let Some(w) = self.item(cx, id) else { continue };
                w.label(cx, ids!(hdr.title)).set_text(cx, title);
                let (state_n, status, summary) = match p.printer_of(choice) {
                    Some(snap) => {
                        let (n, text) = printer_status(snap);
                        let mode = if snap.virtual_printer { " · Virtual" } else { "" };
                        (n, text, format!("{}{mode}. Use it to see its trays, its job and its status here.", snap.model))
                    }
                    None => (0.0, "Not in use".to_string(), "Use it to see its trays, its job and its status here, and to print on it.".to_string()),
                };
                let pill = w.view(cx, ids!(pill));
                if let Some(mut pill) = pill.borrow_mut() {
                    pill.draw_bg.set_dyn_instance(cx, live_id!(state), &[state_n as f32]);
                }
                pill.label(cx, ids!(text)).set_text(cx, &status);
                w.label(cx, ids!(summary)).set_text(cx, &summary);
                w.draw_all(cx, &mut Scope::empty());
                self.shown.push((id, choice.clone()));
            }
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

/// Stage, progress, layer, time left, temperatures, storage and errors, as short lines.
/// The printer's status in a word or two, and its pill's state (0 off, 1 ready, 2 busy, 3 attention).
pub fn printer_status(printer: &fab_printer_link::Snapshot) -> (f64, String) {
    let s = &printer.state;
    if printer.status != fab_printer_link::ConnectionStatus::Connected {
        (if printer.status.label().to_lowercase().contains("error") { 3.0 } else { 0.0 }, printer.status.label().to_string())
    } else if s.print_error.is_some_and(|e| e != 0) || s.serious_hms() {
        (3.0, "Needs attention".to_string())
    } else if s.busy() {
        (2.0, s.stage_name().unwrap_or_else(|| "Printing".into()))
    } else {
        // A print someone stopped is reported as failed.
        let stopped = printer.job.as_ref().is_some_and(|j| matches!(j.stage, fab_printer_link::JobStage::Cancelled));
        match s.gcode_state.as_deref().unwrap_or("") {
            "FINISH" => (1.0, "Finished".to_string()),
            "FAILED" if stopped => (1.0, "Stopped".to_string()),
            "FAILED" => (3.0, "Failed".to_string()),
            "PAUSE" => (1.0, "Paused".to_string()),
            "" => (1.0, "Waiting for report".to_string()),
            _ => (1.0, "Ready".to_string()),
        }
    }
}

/// The job in progress: its name and, when it is Fab's, how far sending it got.
pub fn job_line(printer: &fab_printer_link::Snapshot) -> String {
    let name = printer.state.job_name.clone().or_else(|| printer.job.as_ref().map(|j| j.name.clone())).unwrap_or_else(|| "–".into());
    let stage = printer.job.as_ref().map(|j| format!(" · {}", j.stage.label())).unwrap_or_default();
    format!("{name}{stage}")
}

/// How far the job is: percent, layer, time left, what the printer reported of them.
pub fn job_detail(s: &fab_printer_link::model::PrinterState) -> Vec<String> {
    let mut detail = Vec::new();
    if let Some(pc) = s.progress {
        detail.push(format!("{pc:.0} %"));
    }
    if let (Some(l), Some(t)) = (s.layer, s.total_layers) {
        detail.push(format!("layer {l} / {t}"));
    }
    if let Some(m) = s.remaining_minutes.filter(|m| *m > 0.0) {
        detail.push(format!("{} left", format_time(m * 60.0)));
    }
    detail
}

/// A temperature against its target.
pub fn temp_text(t: fab_printer_link::model::Temperature) -> String {
    match (t.current, t.target) {
        (Some(c), Some(g)) if g > 0.0 => format!("{c:.0} → {g:.0} °C"),
        (Some(c), _) => format!("{c:.0} °C"),
        _ => "–".into(),
    }
}

pub fn printer_telemetry(p: &fab_printer_link::Snapshot) -> String {
    let s = &p.state;
    let mut lines = Vec::new();
    let temp = temp_text;
    let mut heads: Vec<_> = s.heads().into_iter().filter_map(|e| e.id.map(|id| (id, Some(e)))).collect();
    if !s.numbered_heads {
        // The first family can report nozzle details before its temperatures.
        for id in [fab_printer_link::model::CloudNozzleId(1), fab_printer_link::model::CloudNozzleId(0)] {
            if !heads.iter().any(|(head, _)| *head == id) && s.nozzles.iter().any(|n| n.id == Some(id)) {
                heads.push((id, None));
            }
        }
        heads.sort_by_key(|(id, _)| s.head_of(*id));
    }
    for (id, ext) in heads {
        let noz = s.nozzles.iter().find(|n| n.id == Some(id));
        if let Some(name) = s.head_name(id) {
            let dia = noz.and_then(|n| n.diameter).map(|d| format!(" {d:.1} mm")).unwrap_or_default();
            let kind = noz.and_then(|n| n.kind.clone()).map(|k| format!(" {k}")).unwrap_or_default();
            let t = ext.map(|e| temp(e.temperature)).unwrap_or_else(|| "–".into());
            // The filament feeding it now.
            let feed = match ext.and_then(|e| e.loaded) {
                Some(fab_printer_link::model::LoadedSlot::Tray(id)) => {
                    let material = s.tray(id).and_then(|t| t.material.clone()).filter(|m| !m.is_empty()).unwrap_or_else(|| "filament".into());
                    match id.ams {
                        _ if s.numbered_heads => format!(" · {material}"),
                        254 | 255 => format!(" · {material} from the external spool"),
                        a if a >= 128 => format!(" · {material} from AMS HT {}", a - 127),
                        _ => format!(" · {material} from tray {}", super::colours::tray_text(id)),
                    }
                }
                _ => String::new(),
            };
            lines.push(format!("{name}{dia}{kind} · {t}{feed}"));
        }
    }
    let mut rest = vec![format!("Bed {}", temp(s.bed))];
    if s.chamber.current.is_some() {
        rest.push(format!("chamber {}", temp(s.chamber)));
    }
    lines.push(rest.join(" · "));
    match s.storage {
        _ if p.virtual_printer => lines.push("Virtual printer · simulated storage".into()),
        Some(false) => lines.push("No USB drive in the printer: insert one to print.".into()),
        None if p.status == fab_printer_link::ConnectionStatus::Connected => lines.push("Storage not reported yet.".into()),
        _ => {}
    }
    if let Some(e) = s.print_error.filter(|e| *e != 0) {
        lines.push(format!("Printer error {e:08X}"));
    }
    if !s.hms.is_empty() {
        lines.push(format!("HMS: {}", s.hms.iter().map(|h| h.code()).collect::<Vec<_>>().join(", ")));
    }
    lines.join("\n")
}

/// What the Status table shows, a row each: the model, each head (its nozzle, temperature and
/// the filament feeding it), the bed, the chamber, storage, errors.
pub fn printer_rows(p: &fab_printer_link::Snapshot) -> Vec<(String, String)> {
    let s = &p.state;
    let mut rows = Vec::new();
    let suffix = p.serial_suffix.as_ref().map(|s| format!(" · …{s}")).unwrap_or_default();
    let mode = if p.virtual_printer { " · Virtual" } else { "" };
    rows.push(("Model".to_string(), format!("{}{suffix}{mode}", p.model)));
    // each head: its nozzle, its temperature and the filament feeding it (the telemetry's heads)
    let mut heads: Vec<_> = s.heads().into_iter().filter_map(|e| e.id.map(|id| (id, Some(e)))).collect();
    if !s.numbered_heads {
        for id in [fab_printer_link::model::CloudNozzleId(1), fab_printer_link::model::CloudNozzleId(0)] {
            if !heads.iter().any(|(head, _)| *head == id) && s.nozzles.iter().any(|n| n.id == Some(id)) {
                heads.push((id, None));
            }
        }
        heads.sort_by_key(|(id, _)| s.head_of(*id));
    }
    for (id, ext) in heads {
        let Some(name) = s.head_name(id) else { continue };
        let noz = s.nozzles.iter().find(|n| n.id == Some(id));
        let nozzle = [noz.and_then(|n| n.diameter).map(|d| format!("{d:.1} mm")), noz.and_then(|n| n.kind.clone())]
            .into_iter().flatten().collect::<Vec<_>>().join(" ");
        let temp = ext.map(|e| temp_text(e.temperature)).filter(|t| t != "–");
        let feed = match ext.and_then(|e| e.loaded) {
            Some(fab_printer_link::model::LoadedSlot::Tray(tray)) => {
                let material = s.tray(tray).and_then(|t| t.material.clone()).filter(|m| !m.is_empty()).unwrap_or_else(|| "filament".into());
                Some(match tray.ams {
                    _ if s.numbered_heads => material,
                    254 | 255 => format!("{material} from the external spool"),
                    a if a >= 128 => format!("{material} from AMS HT {}", a - 127),
                    _ => format!("{material} from tray {}", super::colours::tray_text(tray)),
                })
            }
            _ => None,
        };
        let value = [Some(nozzle).filter(|n| !n.is_empty()), temp, feed].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        rows.push((name, if value.is_empty() { "–".into() } else { value }));
    }
    rows.push(("Bed".to_string(), temp_text(s.bed)));
    if s.chamber.current.is_some() {
        rows.push(("Chamber".to_string(), temp_text(s.chamber)));
    }
    let storage = match s.storage {
        _ if p.virtual_printer => Some("Simulated".to_string()),
        Some(false) => Some("No USB drive: insert one to print".to_string()),
        Some(true) => Some("USB drive".to_string()),
        None if p.status == fab_printer_link::ConnectionStatus::Connected => Some("Not reported yet".to_string()),
        None => None,
    };
    if let Some(storage) = storage {
        rows.push(("Storage".to_string(), storage));
    }
    if let Some(e) = s.print_error.filter(|e| *e != 0) {
        rows.push(("Error".to_string(), format!("{e:08X}")));
    }
    if !s.hms.is_empty() {
        rows.push(("HMS".to_string(), s.hms.iter().map(|h| h.code()).collect::<Vec<_>>().join(", ")));
    }
    rows
}

/// The Status table's name column.
const TABLE_NAME_W: f64 = 84.0;

/// The Status table's rows (ten rows; a longer list puts the rest in the last).
fn set_status_rows(cx: &mut Cx, card: &WidgetRef, rows: &[(String, String)]) {
    let paths = [ids!(row0), ids!(row1), ids!(row2), ids!(row3), ids!(row4), ids!(row5), ids!(row6), ids!(row7), ids!(row8), ids!(row9)];
    // more rows than the table has: the last one holds the rest, each named
    let last = paths.len() - 1;
    let spill = (rows.len() > paths.len()).then(|| rows[last..].iter().map(|(n, v)| format!("{n}: {v}")).collect::<Vec<_>>().join(" · "));
    let shown = rows.len().min(paths.len());
    for (i, path) in paths.into_iter().enumerate() {
        let row = card.widget(cx, path);
        row.set_visible(cx, i < rows.len());
        if let Some(mut view) = row.borrow_mut::<View>() {
            view.draw_bg.set_dyn_instance(cx, live_id!(odd), &[(i % 2) as f32]);
            view.draw_bg.set_dyn_instance(cx, live_id!(last), &[u8::from(i + 1 == shown) as f32]);
        }
        match (&spill, rows.get(i)) {
            (Some(rest), Some(_)) if i == last => {
                row.label(cx, ids!(name)).set_text(cx, "More");
                row.label(cx, ids!(value)).set_text(cx, rest);
            }
            (_, Some((name, value))) => {
                row.label(cx, ids!(name)).set_text(cx, name);
                row.label(cx, ids!(value)).set_text(cx, value);
            }
            _ => {}
        }
    }
}

/// Why printing is not offered, the last command's answer (the job card says it when it shows),
/// the storage listing.
fn printer_note(printer: &fab_printer_link::Snapshot, supported: bool, job_shown: bool) -> String {
    use fab_printer_link::CommandState;
    if !supported {
        return fab_printer_link::UNSUPPORTED_PRINTING.into();
    }
    let mut out = Vec::new();
    match &printer.command {
        Some((c, CommandState::Pending)) if !job_shown => out.push(format!("{c}: waiting for the printer")),
        Some((c, CommandState::Failed(e))) if !job_shown => out.push(format!("{c}: {e}")),
        _ => {}
    }
    match &printer.storage_files {
        Some(Ok(files)) => {
            let jobs = files.iter().filter(|f| !f.dir).count();
            out.push(format!("USB drive: {jobs} files{}", if files.iter().any(|f| f.dir) { " and folders" } else { "" }));
        }
        Some(Err(e)) => out.push(format!("Storage: {e}")),
        None => {}
    }
    out.join("\n")
}

fn confirm_report(p: &PrintState) -> Option<&fab_printer_link::Snapshot> {
    p.confirm_print.as_ref()?;
    p.target_printer().map(Arc::as_ref)
}

/// What the confirm card asks or says: the print's question once its slice is ready, how far the
/// slice is before that (with its progress), and what to do when none is on its way. The card
/// follows the slice, so the question comes the moment the slice does.
pub fn confirm_question(p: &PrintState, printer: &fab_printer_link::Snapshot, empty_plate: bool) -> (String, Option<f32>) {
    let name = &printer.name;
    if empty_plate {
        return ("The plate is empty: there is nothing to print yet.".into(), None);
    }
    if let Some(stage) = p.calibration_wait() {
        return (format!("Preparing the print for {name}: {stage}…"), None);
    }
    if p.calibration_error.is_some() {
        return (format!("Cannot print on {name}: the plate failed to rebuild for this calibration."), None);
    }
    match &p.status {
        SliceStatus::Running { progress, stage } => (format!("Preparing the print for {name}: {stage}…"), Some(*progress)),
        SliceStatus::Failed(e) => (format!("The slice failed, so there is nothing to print: {}. Print tries again.", e.lines().next().unwrap_or("internal slicer error")), None),
        _ if p.can_send() && p.printer_busy() => (format!("{name} is busy. Queue the sliced plate? It waits in Monitor and starts only when you press Start there."), None),
        _ if p.can_send() && printer.virtual_printer => (format!("Start a Virtual print of the sliced plate on {name}?"), None),
        _ if p.can_send() => (format!("Print the sliced plate on {name}? It uploads to the printer's USB drive and starts at once."), None),
        _ => (format!("Preparing the print for {name}: the slice is about to start…"), None),
    }
}

/// A menu of names with the current one ticked; long lists split into
/// alphabetical submenus.
pub fn names_menu(names: &[String], current: &str) -> Vec<MenuItem> {
    let item = |n: &String| MenuItem::new(LiveId::from_str(n), n).checked(n == current);
    if names.len() <= MENU_CHUNK {
        return names.iter().map(item).collect();
    }
    let mut sorted: Vec<&String> = names.iter().collect();
    sorted.sort_by_key(|n| n.to_lowercase());
    sorted
        .chunks(MENU_CHUNK)
        .map(|chunk| {
            let first = chunk.first().map(|n| n.chars().take(12).collect::<String>()).unwrap_or_default();
            let last = chunk.last().map(|n| n.chars().take(12).collect::<String>()).unwrap_or_default();
            let mut m = MenuItem::new(LiveId::from_str(&format!("{first}..{last}")), &format!("{first} … {last}"));
            m.submenu = chunk.iter().map(|n| item(n)).collect();
            m
        })
        .collect()
}

/// The name in `names` whose id was picked.
fn picked_name(names: &[String], id: LiveId) -> Option<String> {
    names.iter().find(|n| LiveId::from_str(n) == id).cloned()
}

fn first_line<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    s.lines().next().filter(|line| !line.trim().is_empty()).unwrap_or(fallback)
}

fn file_failure(why: &str) -> String {
    format!("Export failed: {}", first_line(why, "could not write the file"))
}

/// The filament export's own status line, and whether it is a failure.
pub fn export_line(p: &PrintState) -> Option<(String, bool)> {
    match &p.export {
        ExportStatus::Idle => None,
        ExportStatus::Exporting => Some((p.calibration_wait().map_or_else(|| "Exporting…".into(), |stage| format!("Exporting · {stage}…")), false)),
        ExportStatus::Written(path) => Some((format!("Wrote {}", path.display()), false)),
        ExportStatus::Failed(why) => Some((file_failure(why), true)),
    }
}

/// An active filament or resin export failure.
pub fn export_failure(p: &PrintState) -> Option<String> {
    if let ExportStatus::Failed(why) = &p.export {
        return Some(file_failure(why));
    }
    p.resin.as_ref().filter(|r| r.export.starts_with("Export failed:"))
        .map(|r| first_line(&r.export, "Export failed").to_owned())
}

/// The failure to show over a plate outside the Sliced view.
pub fn stage_failure(p: &PrintState) -> Option<String> {
    export_failure(p).or_else(|| (p.calibration_error.is_some() || matches!(p.status, SliceStatus::Failed(_))).then(|| slice_status(p).0))
}

/// The slice's status line and progress.
pub fn slice_status(p: &PrintState) -> (String, Option<f32>) {
    // a resin printer's slice is its layer stack
    if let Some(r) = &p.resin {
        return (r.status(), None);
    }
    if let Some(stage) = p.calibration_wait() {
        let export = if matches!(p.export, ExportStatus::Exporting) { "Exporting · " } else { "" };
        let status = if let SliceStatus::Done { seconds } = p.status { format!("Sliced in {seconds:.1} s. ") } else { String::new() };
        return (format!("{status}{export}{stage}…"), None);
    }
    if let Some(err) = &p.calibration_error {
        let prefix = if matches!(p.export, ExportStatus::Failed(_)) { "Export failed — calibration rebuild failed" } else { "Calibration rebuild failed" };
        return (format!("{prefix}: {}", first_line(err, "the plate could not rebuild for this calibration")), None);
    }
    match &p.status {
        SliceStatus::Idle => {
            let line = export_line(p).map_or_else(|| if p.preview.is_none() { "Not sliced yet.".into() } else { String::new() }, |(t, _)| t);
            (line, None)
        }
        SliceStatus::Running { progress, stage } => {
            let prefix = if matches!(p.export, ExportStatus::Exporting) { "Exporting · " } else { "" };
            (format!("{prefix}{stage}…"), Some(*progress))
        }
        SliceStatus::Done { seconds } => {
            let mut t = format!("Sliced in {seconds:.1} s.");
            if p.stale {
                t.push_str(" The plate changed since: slice again.");
            }
            if let Some((line, _)) = export_line(p) {
                t.push(' ');
                t.push_str(&line);
            }
            if let Some(w) = &p.fit_warning {
                t.push_str(&format!(" {w}."));
            }
            (t, None)
        }
        SliceStatus::Failed(e) => {
            let message = e.lines().next().unwrap_or("internal slicer error");
            let retry = if p.play.active { " — retrying on the next change" } else { "" };
            (format!("Slice failed: {message}{retry}"), None)
        }
    }
}

// ===========================================================================
// Print settings
// ===========================================================================

#[derive(Script, ScriptHook, Widget)]
pub struct FabSettingsPanel {
    #[rust]
    machine_menu: Option<(EditOwner, Arc<[String]>)>,
    #[deref]
    view: View,
}

fn missing_machine(binding: &MachineBinding) -> Option<&str> {
    match binding {
        MachineBinding::Available { machine, explicit: Some(name), .. } if name != &machine.geometry.name => Some(name),
        MachineBinding::MissingPreset(name) => Some(name),
        _ => None,
    }
}

/// The target's machine menu and the preset names its picks will address.
pub fn machine_menu(p: &PrintState) -> (Vec<MenuItem>, Arc<[String]>) {
    let title = if p.links.title.is_empty() { "No printer" } else { &p.links.title };
    let mut items = vec![MenuItem::heading(&format!("Machine for {title}"))];
    if p.links.picked == PrinterChoice::None {
        items.extend(names_menu(&p.profiles.machines, &p.settings.machine));
        return (items, p.profiles.machines.clone().into());
    }
    let binding = p.printers.binding(&p.links.picked).map(Arc::as_ref);
    let editable = matches!(p.links.picked, PrinterChoice::Real(_)) && !p.links.disconnected;
    if let Some(MachineBinding::Available { machine, automatic, explicit, choices }) = binding {
        let auto = MenuItem::new(live_id!(automatic_machine), &format!("Automatic ({})", automatic.geometry.name)).checked(explicit.is_none());
        items.push(if editable { auto } else { auto.disabled() });
        if editable {
            if let Some(missing) = binding.and_then(missing_machine) {
                items.push(MenuItem::new(LiveId(0), &format!("{missing} (missing)")).checked(true).disabled());
            }
            items.extend(names_menu(choices, explicit.as_deref().unwrap_or_default()));
            return (items, choices.clone());
        }
        if p.links.layout.is_some() {
            items.extend(names_menu(&p.profiles.machines, &p.settings.machine));
            return (items, p.profiles.machines.clone().into());
        }
        items.push(MenuItem::new(LiveId(0), &machine.geometry.name).disabled());
    } else {
        let auto = MenuItem::new(live_id!(automatic_machine), "Automatic");
        items.push(if editable { auto } else { auto.disabled() });
        if let Some(missing) = binding.and_then(missing_machine) {
            items.push(MenuItem::new(LiveId(0), &format!("{missing} (missing)")).checked(true).disabled());
        }
    }
    (items, Arc::default())
}

fn machine_note(p: &PrintState) -> Option<String> {
    let binding = p.printers.binding(&p.links.picked)?;
    let missing = missing_machine(binding)?;
    Some(match binding.machine() {
        Some(machine) => format!("{missing} not found, using {}", machine.geometry.name),
        None => format!("{missing} not found"),
    })
}

impl FabSettingsPanel {
    fn open(&mut self, cx: &mut Cx, tag: LiveId, anchor: Rect, state: &AppState) {
        let p = &state.print;
        let items = if tag == live_id!(print_source) {
            let mut v = vec![MenuItem::new(live_id!(installed), "Installed profiles").checked(p.settings.use_installed)];
            if !p.profiles.available {
                v[0] = v[0].clone().disabled();
            }
            v.push(MenuItem::new(live_id!(defaults), "Fab defaults").checked(!p.settings.use_installed));
            v
        } else if tag == live_id!(print_machine) {
            let (items, names) = machine_menu(p);
            self.machine_menu = Some((EditOwner::new(PrinterScope::Target, p), names));
            items
        } else if tag == live_id!(print_plate) {
            names_menu(&p.profiles.plates, &p.settings.plate)
        } else if tag == live_id!(print_process) {
            names_menu(&p.profiles.processes, &p.settings.process)
        } else {
            return;
        };
        if !items.is_empty() {
            open_menu(cx, tag, items, anchor, MenuPlace::Below);
        }
    }

    fn picks(&mut self, cx: &mut Cx, actions: &Actions, state: &AppState) {
        let p = &state.print;
        if let Some(id) = menu_picked(actions, live_id!(print_source)) {
            cx.action(PrintAction::UseInstalled(id == live_id!(installed)));
        }
        if let Some(id) = menu_picked(actions, live_id!(print_machine)) {
            if let Some((owner, names)) = self.machine_menu.take() {
                if id == live_id!(automatic_machine) {
                    cx.action(PrintAction::SetMachine(owner, None));
                } else if let Some(name) = picked_name(&names, id) {
                    cx.action(PrintAction::SetMachine(owner, Some(name)));
                }
            }
        }
        if let Some(n) = menu_picked(actions, live_id!(print_plate)).and_then(|id| picked_name(&p.profiles.plates, id)) {
            cx.action(PrintAction::SetPlate(n));
        }
        if let Some(n) = menu_picked(actions, live_id!(print_process)).and_then(|id| picked_name(&p.profiles.processes, id)) {
            cx.action(PrintAction::SetProcess(n));
        }
    }

    fn sync(&mut self, cx: &mut Cx, state: &AppState) {
        let p = &state.print;
        let s = &p.settings;
        let v = &self.view;
        let set_pick = |cx: &mut Cx, path: &[LiveId], text: &str| {
            v.view(cx, path).label(cx, ids!(label)).set_text(cx, if text.is_empty() { "—" } else { text });
        };
        set_pick(cx, ids!(profile.source_row.source), if s.use_installed { "Installed profiles" } else { "Fab defaults" });
        set_pick(cx, ids!(profile.machine_row.machine), &s.machine);
        set_pick(cx, ids!(profile.plate_row.plate), if s.plate.is_empty() { NO_PLATE_FITS } else { &s.plate });
        set_pick(cx, ids!(profile.process_row.process), &s.process);
        // Each colour prints the material in its tray, with that material's settings. A material
        // whose colours the slice prints nothing in is not the job's.
        let prints = |i: usize| {
            let mut rows = p.colors.rows.iter().filter(|r| r.used && r.pos.is_some() && r.pos.as_ref() == p.colors.tools.get(i)).peekable();
            rows.peek().is_none() || rows.any(|r| !p.unprinted.contains(&r.name))
        };
        let materials: Vec<String> = s.materials.iter().enumerate().filter(|(i, _)| prints(*i)).map(|(_, m)| {
            let at = p.colors.tools.iter().find(|pos| pos.head == m.nozzle && Some(pos.tray) == m.tray).map_or_else(|| p.colors.layout.heads.get(m.nozzle).cloned().unwrap_or_default(), |pos| pos.label(&p.colors.layout.heads));
            format!("{at}: {}", m.preset)
        }).collect();
        let note = [materials.join("\n"), p.profiles.note.clone(), machine_note(p).unwrap_or_default()].into_iter().filter(|t| !t.trim().is_empty()).collect::<Vec<_>>().join("\n");
        v.label(cx, ids!(colours.note)).set_text(cx, note.trim());

        let (status, progress) = slice_status(p);
        v.label(cx, ids!(slice.status_row.status)).set_text(cx, &status);
        let bar = v.view(cx, ids!(slice.status_row.progress));
        bar.set_visible(cx, progress.is_some());
        if let Some(f) = progress {
            crate::ui::widgets::set_progress(cx, &bar, f as f32);
        }
        let summary = match &p.preview {
            Some(pv) if !p.stale => pv.summary.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        };
        v.label(cx, ids!(slice.summary)).set_text(cx, &summary);
        v.button(cx, ids!(slice.actions.export)).set_enabled(cx, !state.plate.parts.is_empty());
    }
}

impl Widget for FabSettingsPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Job");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // (its sections' headers are the Printers editor's: `FabPrinterPanel::headers`)
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get::<AppState>() else { return };
        for a in ui_actions(actions) {
            if let FabUiAction::DropdownClicked { tag, anchor } = a {
                self.open(cx, *tag, *anchor, state);
            }
        }
        self.picks(cx, actions, state);
        let v = &self.view;
        if v.button(cx, ids!(slice.actions.export)).clicked(actions) {
            cx.action(PrintAction::Export);
        }
        if actions.iter().any(|a| a.downcast_ref::<PrintAction>().is_some_and(|a| !a.moves_playhead())) {
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            self.sync(cx, state);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

// ===========================================================================
// The printer
// ===========================================================================

#[derive(Script, ScriptHook, Widget)]
pub struct FabPrinterPanel {
    #[rust]
    start_press: Option<PrintAction>,
    #[rust]
    control_destination: Option<ControlDestination>,
    #[rust]
    can_switch_mode: bool,
    #[rust]
    speed_destination: Option<ControlDestination>,
    #[rust]
    edit_owner: EditOwner,
    #[rust]
    layout: Option<VirtualControls>,
    #[rust]
    menu_edit: Option<(EditOwner, Option<VirtualControls>)>,
    #[deref]
    view: View,
    /// The press of Print last brought into view.
    #[rust]
    asked: u64,
    /// Which of the printability errors "Show layer" goes to next.
    #[rust]
    next_issue: usize,
    /// The raw report is open.
    #[rust]
    raw_open: bool,
    /// The stats' clock while a job runs.
    #[rust]
    tick: Timer,
    /// The filter's words, as the last draw read them.
    #[rust]
    query_words: Vec<String>,
    /// The Jobs tab shows (else the Printers tab).
    #[rust]
    jobs_view: bool,
}

fn choice_id(choice: &PrinterChoice) -> LiveId {
    choice.id().map_or(live_id!(no_printer), |id| LiveId::from_str(&id))
}

fn twin_id(preset: &str) -> LiveId { LiveId::from_str(&format!("twin:{preset}")) }

impl FabPrinterPanel {
    /// Set the link label and layout controls from the last pick or layout edit.
    fn sync_links(&mut self, cx: &mut Cx, p: &PrintState) {
        let links = &p.links;
        self.edit_owner.capture(PrinterScope::Target, p);
        self.layout = links.layout;
        let label = if links.title.is_empty() { p.target_printer().map(|p| p.name.as_str()) } else { Some(links.title.as_str()) };
        self.view.prop_section(cx, ids!(body.active)).header().label(cx, ids!(hdr.title)).set_text(cx, label.unwrap_or(""));
        self.can_switch_mode = matches!(links.picked, PrinterChoice::Real(_)) && p.resin.is_none()
            && (p.target_printer().is_some() || links.disconnected);
        let modes = self.view.view(cx, ids!(body.active.modes));
        self.view.view(cx, ids!(body.active.mode_row)).set_visible(cx, self.can_switch_mode);
        let disconnected = p.target_printer().map_or(links.disconnected, |s| s.virtual_printer);
        for (id, active) in [(ids!(connected), !disconnected), (ids!(disconnected), disconnected)] {
            let button = modes.radio_button(cx, id);
            if button.active(cx) != active { button.set_active(cx, active, Animate::No); }
        }
        let note = self.view.label(cx, ids!(body.active.link_note));
        // The shown job card can carry the target refusal only while showing its report.
        let refusal = p.print_blocked.get(&links.picked).filter(|_| (p.target_printer().is_none() || p.shown_choice() != &links.picked) && p.resin.is_none());
        let text = refusal.unwrap_or(&links.note);
        note.set_visible(cx, !text.is_empty());
        note.set_text(cx, text);
        let card = self.view.view(cx, ids!(body.active.virtual_card));
        card.set_visible(cx, links.layout.is_some() && p.resin.is_none());
        // a printer with neither a link mode nor a virtual layout: nothing to set
        self.view.label(cx, ids!(body.active.no_settings)).set_visible(cx, !self.can_switch_mode && (links.layout.is_none() || p.resin.is_some()));
        let Some(layout) = links.layout else { return };
        let nozzles = layout.kind == VirtualKind::Nozzles;
        card.label(cx, ids!(kind_row.kind.label)).set_text(cx, if nozzles { "Nozzles" } else { "Toolchanger" });
        card.label(cx, ids!(heads_row.count)).set_text(cx, &layout.heads.to_string());
        card.button(cx, ids!(heads_row.minus)).set_enabled(cx, layout.heads > 1);
        card.button(cx, ids!(heads_row.plus)).set_enabled(cx, !nozzles || layout.heads < 2);
        card.view(cx, ids!(trays_row)).set_visible(cx, nozzles);
        card.label(cx, ids!(trays_row.count)).set_text(cx, &layout.trays.to_string());
        card.button(cx, ids!(trays_row.minus)).set_enabled(cx, layout.trays > 0);
        card.widget(cx, ids!(tpu_row)).set_visible(cx, nozzles);
        card.check_box(cx, ids!(tpu_row.tpu)).set_active(cx, layout.tpu, Animate::No);
    }

    /// Open a picker or turn a control press into one complete layout change.
    fn link_actions(&mut self, cx: &mut Cx, actions: &Actions, p: &PrintState) {
        let links = &p.links;
        if self.can_switch_mode {
            let modes = self.view.view(cx, ids!(body.active.modes));
            if let Some(i) = modes.radio_button_set(cx, ids_array!(connected, disconnected)).selected(cx, actions) {
                cx.action(PrintAction::PrinterMode(self.edit_owner.clone(), i == 1));
            }
        }
        for action in ui_actions(actions) {
            let FabUiAction::DropdownClicked { tag, anchor } = action else { continue };
            let items = if *tag == live_id!(virtual_kind) {
                let Some(layout) = links.layout else { continue };
                vec![MenuItem::new(live_id!(nozzles), "Nozzles").checked(layout.kind == VirtualKind::Nozzles),
                     MenuItem::new(live_id!(toolchanger), "Toolchanger").checked(layout.kind == VirtualKind::Toolchanger)]
            } else if *tag == live_id!(virtual_reset) {
                links.presets.iter().map(|(name, title)| MenuItem::new(LiveId::from_str(name), title)).collect()
            } else { continue };
            self.menu_edit = Some((self.edit_owner.clone(), self.layout));
            open_menu(cx, *tag, items, *anchor, MenuPlace::Below);
        }
        // Add printer: a virtual twin of a model (it is put in use), or how to add a network printer
        if self.view.button(cx, ids!(editor_bar.add_printer)).clicked(actions) {
            let anchor = self.view.widget(cx, ids!(editor_bar.add_printer)).area().rect(cx);
            let twins: Vec<MenuItem> = links.twin_presets.iter().map(|preset| MenuItem::new(twin_id(preset), preset)).collect();
            let mut items = Vec::new();
            if twins.is_empty() {
                items.push(MenuItem::new(live_id!(make_twin), "Virtual twin of…").disabled());
            } else {
                items.push(MenuItem::new(live_id!(make_twin), "Virtual twin of…").flyout(twins));
            }
            items.push(MenuItem::sep());
            items.push(MenuItem::new(live_id!(network_printer), "Network printer: its address and access code in ~/.makepad/fab/printers").disabled());
            self.menu_edit = Some((self.edit_owner.clone(), self.layout));
            open_menu(cx, live_id!(add_printer), items, anchor, MenuPlace::Below);
        }
        if let Some(preset) = menu_picked(actions, live_id!(add_printer)).and_then(|id| links.twin_presets.iter().find(|preset| twin_id(preset) == id)) {
            if let Some((owner, _)) = self.menu_edit.take() { cx.action(PrintAction::MakeTwin(owner, preset.clone())); }
        }
        if let Some(id) = menu_picked(actions, live_id!(virtual_reset)) {
            if let Some((name, _)) = links.presets.iter().find(|(n, _)| LiveId::from_str(n) == id) {
                if let Some((owner, _)) = self.menu_edit.take() { cx.action(PrintAction::ResetVirtual(owner, name.clone())); }
            }
            return;
        }
        let kind_pick = menu_picked(actions, live_id!(virtual_kind));
        let (owner, layout) = if kind_pick.is_some() {
            let Some(menu) = self.menu_edit.as_ref() else { return };
            (&menu.0, menu.1)
        } else { (&self.edit_owner, self.layout) };
        let Some(mut l) = layout else { return };
        let card = self.view.view(cx, ids!(body.active.virtual_card));
        if let Some(id) = kind_pick {
            let kind = if id == live_id!(nozzles) { VirtualKind::Nozzles } else { VirtualKind::Toolchanger };
            if l.kind == kind { return; }
            l.kind = kind;
            if kind == VirtualKind::Nozzles { l.heads = l.heads.min(2); l.trays = 1; }
        } else if card.button(cx, ids!(heads_row.minus)).clicked(actions) { l.heads = l.heads.saturating_sub(1); }
        else if card.button(cx, ids!(heads_row.plus)).clicked(actions) { l.heads += 1; }
        else if card.button(cx, ids!(trays_row.minus)).clicked(actions) { l.trays = l.trays.saturating_sub(1); }
        else if card.button(cx, ids!(trays_row.plus)).clicked(actions) { l.trays += 1; }
        else if let Some(on) = card.check_box(cx, ids!(tpu_row.tpu)).changed(actions) { l.tpu = on; }
        else { return; }
        cx.action(PrintAction::VirtualLayout { owner: owner.clone(), kind: l.kind, heads: l.heads, trays: l.trays, tpu: l.tpu });
    }

    fn sync(&mut self, cx: &mut Cx, state: &AppState) {
        let p = &state.print;
        self.start_press = None;
        self.speed_destination = None;
        self.sync_links(cx, p);
        let v = &self.view;
        // Every press of Print shows the top of the panel: the confirm card, or "No printer linked".
        if self.asked != p.confirm_asked {
            self.asked = p.confirm_asked;
            v.widget(cx, ids!(body)).set_scroll_pos(cx, dvec2(0.0, 0.0));
        }
        self.control_destination = if p.resin.is_some() { None } else { ControlDestination::filament(PrinterScope::Shown, p) };
        if let Some(r) = &p.resin {
            self.sync_resin(cx, state, r);
            return;
        }
        self.sync_confirm(cx, state);
        let v = &self.view;
        v.widget(cx, ids!(body.orient)).set_visible(cx, false);
        // calibration: for the printer and filament the job uses
        let cal = &p.calibrate;
        v.widget(cx, ids!(body.calibrate_part)).set_visible(cx, true);
        v.label(cx, ids!(body.calibrate_part.cal_context)).set_text(cx, &cal.context_text());
        v.label(cx, ids!(body.calibrate_part.cal_status)).set_visible(cx, !cal.status.is_empty());
        v.label(cx, ids!(body.calibrate_part.cal_status)).set_text(cx, &cal.status);
        // the camera: in Status, or floating over the window (then a line and the way back)
        v.widget(cx, ids!(body.card.pop_tip)).set_visible(cx, true);
        v.view(cx, ids!(body.card.cam_box)).set_visible(cx, !state.ui.camera_pip);
        v.view(cx, ids!(body.card.floating)).set_visible(cx, state.ui.camera_pip);
        // no printer in use: what to do (pick one below, or add one)
        let in_use = p.links.picked != PrinterChoice::None;
        let none = v.view(cx, ids!(body.none));
        none.set_visible(cx, !in_use);
        let (title, text) = if listed(p).is_empty() { NO_PRINTERS } else { NO_PRINTER };
        none.label(cx, ids!(title)).set_text(cx, title);
        none.label(cx, ids!(text)).set_text(cx, text);
        v.widget(cx, ids!(body.active)).set_visible(cx, in_use && self.active_shows(p));
        let shown = p.shown_printer();
        let (pill_state, pill_text) = shown.map_or((0.0, "Connecting".to_string()), |printer| printer_status(printer));
        self.set_pill(cx, pill_state, &pill_text);
        for path in [ids!(body.card), ids!(body.trays_card)] {
            v.widget(cx, path).set_visible(cx, shown.is_some());
        }
        v.widget(cx, ids!(body.map_card)).set_visible(cx, true);
        let Some(printer) = shown else {
            v.view(cx, ids!(body.job)).set_visible(cx, false);
            v.widget(cx, ids!(body.stats_card)).set_visible(cx, false);
            return;
        };
        let s = &printer.state;
        let card = v.widget(cx, ids!(body.card));
        set_status_rows(cx, &card, &printer_rows(printer));
        let supported = fab_printer_link::printing_supported();
        card.label(cx, ids!(note)).set_text(cx, &printer_note(printer, supported, job_card::job_view(p, Cx::time_now()).is_some()));
        card.view(cx, ids!(controls)).set_visible(cx, supported);
        card.button(cx, ids!(controls.clear)).set_visible(cx, s.print_error.is_some_and(|e| e != 0));

        // Everything the printer reports and the job's own numbers: Monitor's.
        let monitor = state.ui.in_mode(crate::api::PlateMode::Monitor);
        let stats = v.widget(cx, ids!(body.stats_card));
        stats.set_visible(cx, monitor);
        if monitor {
            let now = Cx::time_now();
            stats.label(cx, ids!(printer_text)).set_text(cx, &job_stats::printer_lines(printer, p.follow.heard.map(|(_, at)| now - at)));
            let job = p.job_stats();
            stats.label(cx, ids!(job_title)).set_visible(cx, job.is_some());
            stats.label(cx, ids!(job_text)).set_visible(cx, job.is_some());
            if let Some((pv, numbers)) = job {
                stats.label(cx, ids!(job_text)).set_text(cx, &job_stats::job_lines(p, pv, numbers, now));
            }
            stats.button(cx, ids!(raw_button)).set_text(cx, if self.raw_open { "Hide raw report" } else { "Raw report" });
            stats.label(cx, ids!(raw)).set_visible(cx, self.raw_open);
            if self.raw_open {
                let lines: Vec<String> = s.details.iter().map(|(k, value)| format!("{k} = {value}")).collect();
                stats.label(cx, ids!(raw)).set_text(cx, &lines.join("\n"));
            }
            if job_card::job_view(p, now).is_some_and(|j| j.running) && self.tick.is_empty() {
                self.tick = cx.start_timeout(1.0);
            }
        }

        // The job on its way or on the printer; Monitor shows it at the top of its view.
        let job_shown = job_card::job_view(p, Cx::time_now()).is_some();
        v.widget(cx, ids!(body.job)).set_visible(cx, job_shown && !state.ui.in_mode(crate::api::PlateMode::Monitor));
    }

    fn sync_confirm(&mut self, cx: &mut Cx, state: &AppState) {
        let p = &state.print;
        let confirm = self.view.widget(cx, ids!(body.confirm));
        let printer = confirm_report(p).filter(|_| fab_printer_link::printing_supported());
        confirm.set_visible(cx, printer.is_some());
        let Some(printer) = printer else { return };
        if let (Some(PrintConfirm::Filament(ticket)), Some(preview)) = (&p.confirm_print, &p.preview) {
            self.start_press = Some(PrintAction::SendToPrinter { ticket: ticket.clone(), preview: preview.id });
        }
        let row = confirm.view(cx, ids!(speed_row));
        let speed = job_card::confirm_speed(p);
        job_card::sync_speed_row(cx, &row, speed.as_ref().map(|(_, speed)| *speed));
        self.speed_destination = speed.map(|(dest, _)| dest);
        let gcode = printer.state.gcode_state.as_deref().unwrap_or("");
        confirm.view(cx, ids!(stick_row)).set_visible(cx, false);
        let (question, progress) = confirm_question(p, printer, state.plate.parts.is_empty());
        confirm.label(cx, ids!(question)).set_text(cx, &question);
        let bar = confirm.view(cx, ids!(progress));
        bar.set_visible(cx, progress.is_some());
        if let Some(f) = progress {
            crate::ui::widgets::set_progress(cx, &bar, f as f32);
        }
        let check = match &p.print_check {
            Some(Ok(what)) if p.can_send() && printer.virtual_printer => format!("Safety check {what}."),
            Some(Ok(what)) if p.can_send() => format!("Safety check {what}. The printer homes and levels the bed itself first."),
            _ => String::new(),
        };
        confirm.label(cx, ids!(check)).set_visible(cx, !check.is_empty());
        confirm.label(cx, ids!(check)).set_text(cx, &check);
        let blocker = p.send_blocked.as_deref();
        // Accepted printability errors stay named, above whatever else stops Start print.
        let accepted = (p.calibration_wait().is_none() && p.calibration_error.is_none() && p.errors_accepted()).then(|| p.errors_text()).flatten();
        let said: Vec<&str> = accepted.as_deref().into_iter().chain(blocker).collect();
        confirm.label(cx, ids!(blocker)).set_text(cx, &said.join("\n"));
        confirm.label(cx, ids!(blocker)).set_visible(cx, !said.is_empty());
        confirm.button(cx, ids!(buttons.go)).set_visible(cx, self.start_press.is_some() && p.can_send() && blocker.is_none());
        confirm.button(cx, ids!(buttons.go)).set_text(cx, if p.printer_busy() { "Queue print" } else { "Start print" });
        let errors = self.show_issue(cx, &confirm, p);
        // Printability errors alone, on a settled slice: the person may print anyway.
        let accept = confirm.check_box(cx, ids!(accept));
        let overridable = !errors.is_empty() && p.can_send() && p.firm_blocker().is_none();
        accept.set_visible(cx, overridable);
        if overridable {
            accept.set_text(&if errors.len() == 1 { "Print anyway, with this problem".to_string() } else { format!("Print anyway, with these {} problems", errors.len()) });
            accept.set_active(cx, p.errors_accepted(), Animate::No);
        }
        for path in [ids!(flow_cali), ids!(timelapse)] {
            confirm.check_box(cx, path).set_visible(cx, true);
        }
        confirm.check_box(cx, ids!(flow_cali)).set_active(cx, p.print_options.flow_calibration, Animate::No);
        confirm.check_box(cx, ids!(timelapse)).set_active(cx, p.print_options.timelapse, Animate::No);
        confirm.check_box(cx, ids!(plate_clear)).set_visible(cx, matches!(gcode, "FINISH" | "FAILED"));
        confirm.check_box(cx, ids!(plate_clear)).set_active(cx, p.print_options.plate_clear, Animate::No);
    }
}

impl FabPrinterPanel {
    /// Every folding section the editor shows: the printers before the one in use, it and its
    /// parts with the calibration steps, then the printers after it. (`section_headers` sorts
    /// them down the screen.)
    fn headers(&self, cx: &Cx) -> Vec<WidgetRef> {
        let v = &self.view;
        let mut out = Vec::new();
        if let Some(list) = v.widget(cx, ids!(body.before)).borrow::<FabPrinterSections>() {
            out.extend(list.headers());
        }
        out.push(v.widget(cx, ids!(body.active)));
        for path in [ids!(body.confirm), ids!(body.card), ids!(body.orient), ids!(body.stats_card),
            ids!(body.trays_card), ids!(body.map_card), ids!(body.settings_box)] {
            out.push(v.widget(cx, path));
        }
        if let Some(steps) = v.widget(cx, ids!(body.calibrate_part.steps)).borrow::<crate::print::calibrate::FabCalibrateSteps>() {
            out.extend(steps.headers());
        }
        if let Some(list) = v.widget(cx, ids!(body.after)).borrow::<FabPrinterSections>() {
            out.extend(list.headers());
        }
        if let Some(list) = v.widget(cx, ids!(jobs_body.jobs_before)).borrow::<FabPrinterJobs>() {
            out.extend(list.headers());
        }
        for path in [ids!(jobs_body.jobs_active) as &[LiveId], ids!(jobs_body.job_settings.profile), ids!(jobs_body.job_settings.colours),
            ids!(jobs_body.job_settings.slice), ids!(jobs_body.sent)] {
            out.push(v.widget(cx, path));
        }
        if let Some(list) = v.widget(cx, ids!(jobs_body.jobs_after)).borrow::<FabPrinterJobs>() {
            out.extend(list.headers());
        }
        out
    }

    /// The Jobs tab's section of the printer in use: its name, its jobs' count, the jobs sent to
    /// it (its job, the Job panel, syncs itself); with none in use, what to do.
    fn sync_job_tab(&self, cx: &mut Cx, p: &PrintState) {
        use crate::print::job_list::{jobs_count, FabJobRows, JobsOf};
        let v = &self.view;
        let in_use = p.links.picked != PrinterChoice::None;
        v.widget(cx, ids!(jobs_body.jobs_none)).set_visible(cx, !in_use);
        let active = v.widget(cx, ids!(jobs_body.jobs_active));
        active.set_visible(cx, in_use && self.active_shows(p));
        let Some(id) = p.links.picked.id() else { return };
        let title = p.links.choices.iter().find(|(c, _)| *c == p.links.picked).map(|(_, t)| t.clone()).unwrap_or_default();
        active.as_prop_section().header().label(cx, ids!(hdr.title)).set_text(cx, &title);
        let of = JobsOf::Printer(id);
        let (count, printing) = jobs_count(p, &of);
        let (dot, text) = match count {
            _ if printing => (2.0, format!("Printing · {count} jobs")),
            0 => (1.0, "In use".to_string()),
            1 => (1.0, "In use · 1 job".to_string()),
            n => (1.0, format!("In use · {n} jobs")),
        };
        let pill = active.view(cx, ids!(pill));
        if let Some(mut pill) = pill.borrow_mut() {
            pill.draw_bg.set_dyn_instance(cx, live_id!(state), &[dot as f32]);
        }
        pill.label(cx, ids!(text)).set_text(cx, &text);
        active.label(cx, ids!(sent_none)).set_visible(cx, count == 0);
        if let Some(mut rows) = active.widget(cx, ids!(sent_rows)).borrow_mut::<FabJobRows>() {
            rows.of = of;
        }
    }

    /// The status pill in the header of the printer in use.
    fn set_pill(&self, cx: &mut Cx, state: f64, text: &str) {
        let pill = self.view.view(cx, ids!(body.active.pill));
        if let Some(mut pill) = pill.borrow_mut() {
            pill.draw_bg.set_dyn_instance(cx, live_id!(state), &[state as f32]);
        }
        pill.label(cx, ids!(text)).set_text(cx, text);
    }

    /// The editor's filter, its words lowercased.
    fn query(&self, cx: &Cx) -> Vec<String> {
        self.view.text_input(cx, ids!(editor_bar.search.row.input)).text().split_whitespace().map(|w| w.to_lowercase()).collect()
    }

    /// Whether the printer in use passes the filter.
    fn active_shows(&self, p: &PrintState) -> bool {
        let title = p.links.choices.iter().find(|(c, _)| *c == p.links.picked).map(|(_, t)| t.clone()).unwrap_or_default();
        shows(&self.query_words, &title)
    }

    /// Where the printability errors are, one press each: the confirm card's button. The errors.
    fn show_issue(&self, cx: &mut Cx, confirm: &WidgetRef, p: &PrintState) -> Vec<usize> {
        let show = confirm.button(cx, ids!(show_issue));
        if p.calibration_error.is_some() || p.calibration_wait().is_some() {
            show.set_visible(cx, false);
            return Vec::new();
        }
        let errors = p.print_errors();
        show.set_visible(cx, !errors.is_empty());
        if !errors.is_empty() {
            let next = self.next_issue % errors.len();
            let of = if errors.len() > 1 { format!(" ({} of {})", next + 1, errors.len()) } else { String::new() };
            show.set_text(cx, &match p.report().issues[errors[next]].layer {
                Some(layer) => format!("Show layer {}{of}", layer + 1),
                None => format!("Show it{of}"),
            });
        }
        errors
    }

    /// The resin printer: linked or not, its job outside Monitor, and the confirm card that asks
    /// before the stack goes to it. Nothing here is the filament printer's.
    fn sync_resin(&mut self, cx: &mut Cx, state: &AppState, r: &resin::ResinShow) {
        use resin::{Linked, ResinStage, Route, Sticks};
        let p = &state.print;
        let v = &self.view;
        for path in [ids!(body.trays_card), ids!(body.stats_card), ids!(body.map_card)] {
            v.widget(cx, path).set_visible(cx, false);
        }
        // a resin printer has no camera on Fab's link, nor filament to calibrate
        for path in [ids!(body.card.pop_tip), ids!(body.card.cam_box), ids!(body.card.floating)] {
            v.widget(cx, path).set_visible(cx, false);
        }
        v.widget(cx, ids!(body.calibrate_part)).set_visible(cx, false);
        let job = p.resin_job.as_ref();
        let stick = r.route == Route::Stick;
        let none = v.view(cx, ids!(body.none));
        let (title, text) = match (&r.linked, &r.sticks) {
            (_, Sticks::Looking) if stick => ("Looking for USB sticks", String::new()),
            (_, Sticks::Found(found)) if stick && found.is_empty() => ("No USB stick found", resin::NO_STICK.to_string()),
            (_, Sticks::Failed(why)) if stick => ("The USB sticks can't be listed", format!("{why}. {}", resin::NO_STICK)),
            _ if stick => ("", String::new()),
            (Linked::Looking, _) => ("Looking for the resin printer", String::new()),
            (Linked::Missing(why), _) if why.is_empty() => ("No resin printer linked", resin::NO_RESIN_PRINTER.to_string()),
            (Linked::Missing(why), _) => ("The resin printer can't be read", format!("{why}. {}", resin::NO_RESIN_PRINTER)),
            (Linked::Found(_), _) => ("", String::new()),
        };
        none.set_visible(cx, !title.is_empty());
        none.label(cx, ids!(title)).set_text(cx, title);
        none.label(cx, ids!(text)).set_text(cx, &text);
        // the resin printer's own section (the filament printers below it)
        let active = v.widget(cx, ids!(body.active));
        let card = v.widget(cx, ids!(body.card));
        let shown = match &r.linked {
            _ if stick => Some(r.printer.as_str()),
            Linked::Found(name) => Some(name.as_str()),
            _ => None,
        };
        card.set_visible(cx, shown.is_some());
        active.set_visible(cx, true);
        active.as_prop_section().header().label(cx, ids!(hdr.title)).set_text(cx, shown.map_or("Resin printer", |name| job.map_or(name, |j| j.printer.as_str())));
        if shown.is_none() {
            self.set_pill(cx, 0.0, "Not linked");
        }
        if let Some(_name) = shown {
            set_status_rows(cx, &card, &[("Kind".to_string(), if stick { "Prints from a USB stick" } else { "Resin printer on Fab's link" }.to_string())]);
            let (pill_state, pill_text) = match job {
                Some(j) if matches!(j.stage, ResinStage::Saving { .. }) => (2.0, "Saving"),
                _ if stick => (1.0, "USB stick"),
                Some(j) if j.ended() => (1.0, "Ready"),
                Some(j) if j.stage == ResinStage::Paused => (2.0, "Paused"),
                Some(_) => (2.0, "Printing"),
                None => (1.0, "Linked"),
            };
            self.set_pill(cx, pill_state, pill_text);
            card.label(cx, ids!(note)).set_text(cx, "");
            card.view(cx, ids!(controls)).set_visible(cx, false);
        }
        // the job on its way or on the printer; Monitor shows it at the top of its view
        v.widget(cx, ids!(body.job)).set_visible(cx, job.is_some() && !state.ui.in_mode(crate::api::PlateMode::Monitor));

        // the orientation test, for a printer that prints from a stick
        let orient = v.widget(cx, ids!(body.orient));
        orient.set_visible(cx, stick);
        orient.label(cx, ids!(text)).set_text(cx, resin::TEST_TEXT);
        let status = resin::test_status(r);
        orient.label(cx, ids!(status)).set_visible(cx, status.is_some());
        orient.label(cx, ids!(status)).set_text(cx, status.as_deref().unwrap_or(""));
        orient.button(cx, ids!(save)).set_visible(cx, !matches!(r.test, resin::TestFile::Saving { .. }));

        // the confirm card: open from the press until Cancel or Start print
        let confirm = v.widget(cx, ids!(body.confirm));
        confirm.set_visible(cx, p.confirm_print == Some(PrintConfirm::Resin));
        if p.confirm_print == Some(PrintConfirm::Resin) {
            self.start_press = Some(PrintAction::SendResin { stack: r.stack });
            confirm.view(cx, ids!(speed_row)).set_visible(cx, false);
        }
        let empty = state.plate.parts.is_empty();
        confirm.label(cx, ids!(question)).set_text(cx, &resin::question(r, empty));
        // the stick it goes to, when there is more than one to pick from
        let pick = matches!(&r.sticks, Sticks::Found(found) if stick && found.len() > 1);
        confirm.view(cx, ids!(stick_row)).set_visible(cx, pick);
        if let Some(s) = r.picked_stick().filter(|_| pick) {
            confirm.view(cx, ids!(stick_row.stick)).label(cx, ids!(label)).set_text(cx, &s.name);
        }
        confirm.view(cx, ids!(progress)).set_visible(cx, false);
        confirm.label(cx, ids!(check)).set_visible(cx, false);
        let blocker = resin::blocker(p, r);
        // accepted printability errors stay named, above whatever else stops Start print
        let accepted = resin::errors_accepted(r).then(|| p.errors_text()).flatten();
        let said: Vec<&str> = accepted.as_deref().into_iter().chain(blocker.as_deref()).collect();
        confirm.label(cx, ids!(blocker)).set_text(cx, &said.join("\n"));
        confirm.label(cx, ids!(blocker)).set_visible(cx, !said.is_empty());
        confirm.button(cx, ids!(buttons.go)).set_visible(cx, resin::can_print(p, r, empty));
        confirm.button(cx, ids!(buttons.go)).set_text(cx, if stick { "Save to the stick" } else { "Start print" });
        let errors = self.show_issue(cx, &confirm, p);
        let accept = confirm.check_box(cx, ids!(accept));
        let overridable = !errors.is_empty() && r.ready && resin::firm_blocker(p, r).is_none();
        accept.set_visible(cx, overridable);
        if overridable {
            accept.set_text(&if errors.len() == 1 { "Print anyway, with this problem".to_string() } else { format!("Print anyway, with these {} problems", errors.len()) });
            accept.set_active(cx, resin::errors_accepted(r), Animate::No);
        }
        // a filament printer's options
        for path in [ids!(flow_cali), ids!(timelapse), ids!(plate_clear)] {
            confirm.check_box(cx, path).set_visible(cx, false);
        }
    }
}

impl Widget for FabPrinterPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Printers");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // asked for the job (the Force panel's link): the Jobs tab
        if let Event::Actions(actions) = event {
            if actions.iter().any(|a| a.downcast_ref::<ShowJobTab>().is_some()) {
                self.jobs_view = true;
                self.view.redraw(cx);
            }
        }
        // every header in the editor (the printers', the in-use one's parts, its calibration
        // steps): their hover, and a swipe across them folding them all, in one pass
        if matches!(event, Event::MouseMove(_) | Event::MouseDown(_) | Event::MouseUp(_) | Event::MouseLeave(_) | Event::ClearHover) {
            let headers = self.headers(cx);
            crate::ui::widgets::section_headers(cx, event, &headers);
        }
        if self.tick.is_event(event).is_some() {
            self.tick = Timer::empty();
            self.view.redraw(cx);
        }
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if scope.data.get::<AppState>().is_some_and(|s| !s.ui.on_screen(crate::ui::editors::Editor::Printer)) {
            // off screen: none of its controls can be pressed
            return;
        }
        let Some(state) = scope.data.get::<AppState>() else { return };
        if self.view.text_input(cx, ids!(editor_bar.search.row.input)).changed(actions).is_some() {
            self.view.redraw(cx);
        }
        // the Printers tab or the Jobs tab
        for (path, jobs) in [(ids!(editor_bar.printers_tab), false), (ids!(editor_bar.jobs_tab), true)] {
            if self.view.radio_button(cx, path).clicked(actions) && self.jobs_view != jobs {
                self.jobs_view = jobs;
                self.view.redraw(cx);
            }
        }
        self.link_actions(cx, actions, &state.print);
        if self.view.widget(cx, ids!(body.stats_card)).button(cx, ids!(raw_button)).clicked(actions) {
            self.raw_open = !self.raw_open;
            self.view.redraw(cx);
        }
        // the USB stick the print is saved to
        if let Some(resin::Sticks::Found(found)) = state.print.resin.as_ref().map(|r| &r.sticks) {
            for a in ui_actions(actions) {
                if let FabUiAction::DropdownClicked { tag, anchor } = a {
                    if *tag == live_id!(resin_stick) {
                        let picked = state.print.resin.as_ref().and_then(|r| r.picked_stick()).map(|s| s.root.clone());
                        let items = found.iter().map(|s| MenuItem::new(LiveId::from_str(&s.root.to_string_lossy()), &s.name).checked(Some(&s.root) == picked.as_ref())).collect();
                        open_menu(cx, *tag, items, *anchor, MenuPlace::Below);
                    }
                }
            }
            if let Some(s) = menu_picked(actions, live_id!(resin_stick)).and_then(|id| found.iter().find(|s| LiveId::from_str(&s.root.to_string_lossy()) == id)) {
                cx.action(PrintAction::PickStick(s.root.clone()));
            }
        }
        let v = &self.view;
        let confirm = v.widget(cx, ids!(body.confirm));
        if confirm.visible() {
            let row = confirm.view(cx, ids!(speed_row));
            if let (Some(dest), Some(speed)) = (&self.speed_destination, job_card::picked_speed(cx, &row, actions)) {
                cx.action(PrintAction::SetVirtualSpeed(dest.clone(), speed));
            }
        }
        if confirm.button(cx, ids!(buttons.cancel)).clicked(actions) {
            cx.action(PrintAction::AskPrint(false));
        }
        if confirm.button(cx, ids!(buttons.go)).clicked(actions) {
            if let Some(press) = &self.start_press { cx.action(press.clone()); }
        }
        if confirm.button(cx, ids!(show_issue)).clicked(actions) {
            let errors = state.print.print_errors();
            if !errors.is_empty() {
                let next = self.next_issue % errors.len();
                self.next_issue = next + 1;
                cx.action(PrintAction::SelectIssue(errors[next]));
            }
        }
        for (path, option) in [(ids!(flow_cali), PrintOption::FlowCalibration), (ids!(timelapse), PrintOption::Timelapse), (ids!(plate_clear), PrintOption::PlateClear)] {
            if let Some(on) = confirm.check_box(cx, path).changed(actions) {
                cx.action(PrintAction::SetPrintOption(option, on));
            }
        }
        if let Some(on) = confirm.check_box(cx, ids!(accept)).changed(actions) {
            cx.action(PrintAction::AcceptErrors(on));
        }
        let card = v.widget(cx, ids!(body.card));
        if let Some(dest) = &self.control_destination {
            if card.button(cx, ids!(controls.clear)).clicked(actions) {
                cx.action(PrintAction::PrinterControl(dest.clone(), PrinterControl::ClearError));
            }
            if card.button(cx, ids!(controls.storage)).clicked(actions) {
                cx.action(PrintAction::PrinterControl(dest.clone(), PrinterControl::CheckStorage));
            }
        }
        // the camera pops out to float over the window, and back
        if v.button(cx, ids!(body.card.pop_out)).clicked(actions) {
            cx.action(crate::ui::stage::StageAction::CameraPip(!state.ui.camera_pip));
        }
        if v.button(cx, ids!(body.card.back)).clicked(actions) {
            cx.action(crate::ui::stage::StageAction::CameraPip(false));
        }
        if v.widget(cx, ids!(body.orient)).button(cx, ids!(save)).clicked(actions) {
            cx.action(PrintAction::SaveOrientationTest);
        }
        if actions.iter().any(|a| a.downcast_ref::<PrintAction>().is_some_and(|a| !a.moves_playhead())) {
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // the filter, for the printer in use and the others' sections
        self.query_words = self.query(cx);
        for path in [ids!(body.before), ids!(body.after)] {
            if let Some(mut list) = self.view.widget(cx, path).borrow_mut::<FabPrinterSections>() {
                list.query.clone_from(&self.query_words);
            }
        }
        for path in [ids!(jobs_body.jobs_before), ids!(jobs_body.jobs_after)] {
            if let Some(mut list) = self.view.widget(cx, path).borrow_mut::<FabPrinterJobs>() {
                list.query.clone_from(&self.query_words);
            }
        }
        // the tab shown, and its body
        self.view.widget(cx, ids!(body)).set_visible(cx, !self.jobs_view);
        self.view.widget(cx, ids!(jobs_body)).set_visible(cx, self.jobs_view);
        for (path, on) in [(ids!(editor_bar.printers_tab), !self.jobs_view), (ids!(editor_bar.jobs_tab), self.jobs_view)] {
            let tab = self.view.radio_button(cx, path);
            if tab.active(cx) != on { tab.set_active(cx, on, Animate::No); }
        }
        if let Some(state) = scope.data.get::<AppState>() {
            self.sync_job_tab(cx, &state.print);
        }
        if let Some(state) = scope.data.get::<AppState>() {
            self.sync(cx, state);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn export_wording_keeps_success_quiet_and_failure_actionable() {
        let mut p = PrintState::default();
        assert_eq!(export_line(&p), None);
        assert_eq!(slice_status(&p).0, "Not sliced yet.");
        p.export = ExportStatus::Exporting;
        assert_eq!(slice_status(&p).0, "Exporting…");
        p.status = SliceStatus::Running { progress: 0.5, stage: "Planning".into() };
        assert_eq!(slice_status(&p), ("Exporting · Planning…".into(), Some(0.5)));
        p.status = SliceStatus::Done { seconds: 1.25 };
        p.export = ExportStatus::Written("plate.gcode.3mf".into());
        assert_eq!(export_line(&p), Some(("Wrote plate.gcode.3mf".into(), false)));
        assert_eq!(slice_status(&p).0, "Sliced in 1.2 s. Wrote plate.gcode.3mf");
        assert_eq!(stage_failure(&p), None);
        p.export = ExportStatus::Failed("write refused\nmore detail".into());
        assert_eq!(slice_status(&p).0, "Sliced in 1.2 s. Export failed: write refused");
        assert_eq!(export_line(&p), Some(("Export failed: write refused".into(), true)));
        assert_eq!(stage_failure(&p).as_deref(), Some("Export failed: write refused"));
        p.export = ExportStatus::Failed(String::new());
        assert_eq!(export_failure(&p).as_deref(), Some("Export failed: could not write the file"));
        p.export = ExportStatus::Idle;
        p.status = SliceStatus::Failed("bad slice".into());
        assert_eq!(stage_failure(&p).as_deref(), Some("Slice failed: bad slice"));
        p.resin = Some(resin::ResinShow { export: "Export failed: layer write refused\nmore detail".into(), ..Default::default() });
        assert_eq!(stage_failure(&p).as_deref(), Some("Export failed: layer write refused"));
    }

    #[test]
    fn split_printer_scopes_follow_the_surface_that_was_drawn() {
        use fab_printer_link::{model::TrayId, Snapshot};
        use jobs::{CommittedJob, JobMeta, JobState};
        let [a, b] = ["A", "B"].map(|name| PrinterChoice::Real(name.into()));
        let mut p = PrintState::default();
        p.links.picked = a.clone();
        p.printers.set(&a, 1, Arc::new(Snapshot { name: "A".into(), ..Default::default() }));
        let report_b = Arc::new(Snapshot { name: "B".into(), virtual_printer: true, state: Arc::new(numbered_report()), ..Default::default() });
        p.printers.set(&b, 2, report_b.clone());
        let target = EditOwner::new(PrinterScope::Target, &p);
        let target_control = ControlDestination::filament(PrinterScope::Target, &p).unwrap();
        let shown_a = EditOwner::new(PrinterScope::Shown, &p);
        let late_stop = ControlDestination::filament(PrinterScope::Shown, &p).unwrap();
        p.shown = Some(b.clone());
        assert!(!late_stop.matches(&p), "a late Monitor Stop for A must be refused while B is shown, even though A is the target");
        assert!(!shown_a.matches(&p), "an edit drawn for shown A must be refused on B");
        assert!(target.matches(&p) && target_control.matches(&p), "A's target edit and control survive showing B");
        let shown_b = EditOwner::new(PrinterScope::Shown, &p);
        let b_control = ControlDestination::filament(PrinterScope::Shown, &p).unwrap();
        assert!(shown_b.matches(&p) && b_control.matches(&p), "B's shown edit and control match B");
        let mut map_owner = EditOwner::default();
        colour_map::capture_tray_owner(&mut map_owner, &p);
        assert_eq!(map_owner, target, "the colour map must capture target A when B is shown");

        let layout = trays::editable_layout(&p).expect("shown B offers a virtual tray menu");
        let slot = TrayId { ams: 3, slot: 0 };
        let tray = layout.trays.iter().find(|t| t.pos.tray == slot).expect("the telemetry menu must use B's fourth tool, absent on target A");
        assert_eq!((tray.pos.head, layout.heads[tray.pos.head].as_str()), (3, "Tool 4"), "the editable position and menu label belong to B's profile");

        apply(&mut p, &PrintAction::AskPrint(true));
        p.preview = Some(follow::tests::preview_of(1));
        p.settled = true;
        let question = |p: &PrintState| {
            let report = confirm_report(p).expect("A's confirmation must stay visible");
            confirm_question(p, report, false).0
        };
        assert_eq!(question(&p), "Print the sliced plate on A? It uploads to the printer's USB drive and starts at once.",
            "A's question uses its own name, connected mode and idle state while virtual B is busy");
        p.print_blocked.set(&a, Some("A refused".into()));
        p.print_blocked.set(&b, Some("B refused".into()));
        assert_eq!(job_card::job_view(&p, 0.0).unwrap().phase, "B refused", "the shown job card names B's refusal");
        apply(&mut p, &PrintAction::CloseJob);
        assert!(p.print_blocked.get(&b).is_none() && p.print_blocked.get(&a) == Some("A refused"), "closing B's refusal preserves A's refusal");
        for (id, printer) in [("a-job", &a), ("b-job", &b)] {
            p.follow.jobs.push(CommittedJob { meta: Arc::new(JobMeta { id: id.into(), printer: printer.id(), ..Default::default() }), ..Default::default() });
            p.sending.push(Sending { job: id.into(), printer: printer.clone() });
        }
        assert_eq!(job_card::job_view(&p, 0.0).unwrap().cancel_job.as_deref(), Some("b-job"), "the shown job card uses B's send");
        let mut sent = p.follow.job_of("b-job").unwrap().clone();
        sent.state = JobState::Sent;
        p.shown = None;
        assert!(p.unwatched_while_virtual(&sent), "B's real row sees B's held virtual link while A is both target and shown");
        assert!(!shown_b.matches(&p) && !b_control.matches(&p), "B's shown actions expire when Monitor returns to A");
        p.shown = Some(b.clone());
        p.printers.set(&b, 2, Arc::new(Snapshot { virtual_printer: false, ..(*report_b).clone() }));
        assert!(!shown_b.matches(&p) && !b_control.matches(&p), "a different reported mode refuses the old virtual edit and control");
        p.printers.install(&b, 3, true);
        assert!(p.shown_printer().is_none() && question(&p).contains("plate on A?"), "A's confirm survives B having no report");
        p.printers.set(&b, 3, report_b);
        assert!(!shown_b.matches(&p) && !b_control.matches(&p), "B's replacement refuses both old incarnations");
    }

    #[test]
    fn failed_printer_status_distinguishes_a_stopped_job() {
        use fab_printer_link::{model::PrinterState, ConnectionStatus, JobStage, JobStatus, Snapshot};

        let mut printer = Snapshot {
            status: ConnectionStatus::Connected,
            state: Arc::new(PrinterState {
                gcode_state: Some("FAILED".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(printer_status(&printer), (3.0, "Failed".into()), "no tracked job");

        printer.job = Some(JobStatus {
            name: "Simulated job".into(),
            file: "simulated.gcode.3mf".into(),
            stage: JobStage::Cancelled,
        });
        assert_eq!(printer_status(&printer), (1.0, "Stopped".into()), "cancelled job");

        printer.job.as_mut().unwrap().stage = JobStage::Failed("Print failed".into());
        assert_eq!(printer_status(&printer), (3.0, "Failed".into()), "failed job");
    }

    #[test]
    fn long_menus_split_into_submenus() {
        let names: Vec<String> = (0..60).map(|i| format!("Preset {i:02}")).collect();
        let m = names_menu(&names, "Preset 07");
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].submenu.len(), MENU_CHUNK);
        let short = names_menu(&names[..3], "Preset 01");
        assert_eq!(short.len(), 3);
        assert_eq!(picked_name(&names, LiveId::from_str("Preset 33")).as_deref(), Some("Preset 33"));
    }

    pub(crate) fn numbered_report() -> fab_printer_link::model::PrinterState {
        use fab_printer_link::model::{AmsUnit, CloudNozzleId, Extruder, LoadedSlot, PrinterState, Temperature, Tray, TrayId};
        PrinterState {
            numbered_heads: true, gcode_state: Some("RUNNING".into()), storage: Some(true),
            active_extruder: Some(CloudNozzleId(2)),
            extruders: [3, 1, 0, 2].map(|h| Extruder {
                id: Some(CloudNozzleId(h)),
                temperature: Temperature { current: Some(212.0), target: Some(220.0) },
                loaded: Some(LoadedSlot::Tray(TrayId { ams: h, slot: 0 })),
                ..Default::default()
            }).to_vec(),
            ams: (0..4).map(|h| AmsUnit {
                id: Some(h), extruder: Some(CloudNozzleId(h)),
                trays: vec![Tray {
                    id: Some(TrayId { ams: h, slot: 0 }), present: Some(true), material: Some("PLA".into()),
                    remain: Some(80.0), ..Default::default()
                }], ..Default::default()
            }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn numbered_telemetry_shows_each_tool_without_tray_labels() {
        let printer = fab_printer_link::Snapshot { state: Arc::new(numbered_report()), ..Default::default() };
        let text = printer_telemetry(&printer);
        let heads: Vec<_> = text.lines().filter(|line| line.starts_with("Tool ")).collect();
        assert_eq!(heads, [
            "Tool 1 · 212 → 220 °C · PLA", "Tool 2 · 212 → 220 °C · PLA",
            "Tool 3 · 212 → 220 °C · PLA", "Tool 4 · 212 → 220 °C · PLA",
        ]);
        assert!(!text.contains("from tray") && !text.contains("% left"));
    }
}
