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
use crate::api::AppState;
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
    // A status pill: dot + text, coloured by `state` (0 off, 1 ok, 2 busy, 3 error).
    mod.widgets.FabStatusPill = View{
        width: Fit
        height: 20
        flow: Right
        align: Align{x: 0.0 y: 0.5}
        spacing: 6
        padding: Inset{left: 8 right: 9 top: 0 bottom: 0}
        show_bg: true
        draw_bg +: {
            state: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                var c = fab.color_text_muted
                if self.state > 0.5 { c = fab.color_ok }
                if self.state > 1.5 { c = fab.color_accent }
                if self.state > 2.5 { c = fab.color_error }
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, self.rect_size.y * 0.25)
                sdf.fill(vec4(c.xyz, 0.16))
                sdf.circle(10.0, self.rect_size.y * 0.5, 3.0)
                sdf.fill(c)
                return sdf.result
            }
        }
        text := mod.widgets.FabLabelSmall{ margin: Inset{left: 6} text: "" draw_text +: { color: fab.color_text } }
    }

    let PrintRow = mod.widgets.FabFieldRow{}
    let Pick = mod.widgets.FabDropdownButton{
        width: Fill
        height: theme.prop_row_height
        label +: { width: Fill max_lines: 1 text_overflow: TextOverflow.Ellipsis text: "—" }
    }

    mod.widgets.FabSettingsPanelBase = #(FabSettingsPanel::register_widget(vm))
    mod.widgets.FabSettingsPanel = set_type_default() do mod.widgets.FabSettingsPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        // the toolbar: the area's editor selector (and this editor's tools, when it has any)
        editor_bar := mod.widgets.FabEditorBar{
            // filters the rows below by name (a section's title shows it whole)
            search := mod.widgets.FabSearch{width: Fill{min: 80} row +: {input +: {empty_text: "Filter"}}}
        }
        // the job's settings as the Part editor's: sections that fold, each a bar with its glyph
        body := mod.widgets.FabScroll{
            padding: Inset{left: theme.prop_pad right: theme.prop_pad top: #(crate::ui::widgets::PANEL_TOP)}
            // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
            profile_slot := View{width: Fill height: Fit
                profile := PropSection{
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
            }
            // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
            colours_slot := View{width: Fill height: Fit
                colours := PropSection{
                    header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/palette.svg")}} title +: {text: "Colours"}}}
                    body +: {
                        map := mod.widgets.FabColorMap{}
                        note := mod.widgets.FabLabelSmall{ width: Fill text: "" }
                    }
                }
            }
            // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
            slice_slot := View{width: Fill height: Fit
                slice := PropSection{
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
        }
    }

    mod.widgets.FabPrinterPanelBase = #(FabPrinterPanel::register_widget(vm))
    mod.widgets.FabPrinterPanel = set_type_default() do mod.widgets.FabPrinterPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        // the toolbar: the area's editor selector (and this editor's tools, when it has any)
        editor_bar := mod.widgets.FabEditorBar{}
        body := mod.widgets.FabScroll{
            padding: Inset{left: 10 right: 10 top: 10 bottom: 10}
            spacing: 10
            none := mod.widgets.FabCard{
                visible: true
                title := mod.widgets.FabTitleLabel{ text: "" }
                text := mod.widgets.FabLabelDim{ width: Fill text: "" }
            }
            confirm := mod.widgets.FabCard{
                visible: false
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
                buttons := View{
                    width: Fill height: Fit flow: Right spacing: 6
                    go := mod.widgets.FabButtonAccent{ text: "Start print" }
                    cancel := mod.widgets.FabButton{ text: "Cancel" }
                }
            }
            card := mod.widgets.FabCard{
                visible: false
                head := View{
                    width: Fill height: Fit flow: Right spacing: 10 align: Align{y: 0.5}
                    icon_slot := View{
                        width: 36 height: 36
                        padding: Inset{left: 4 right: 4 top: 4 bottom: 4}
                        show_bg: true
                        draw_bg +: {
                            pixel: fn() {
                                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                                sdf.fill(fab.color_input)
                                return sdf.result
                            }
                        }
                        FabIcon{ icon_walk: Walk{width: 28 height: 28} draw_icon +: { svg: crate_resource("self://resources/icons/printer.svg") color: fab.color_text } }
                    }
                    names := View{
                        width: Fill height: Fit flow: Down spacing: 2
                        name := mod.widgets.FabTitleLabel{ text: "" }
                        model := mod.widgets.FabLabelDim{ text: "" }
                    }
                    pill := mod.widgets.FabStatusPill{}
                }
                telemetry := mod.widgets.FabLabelDim{ width: Fill text: "" }
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
            orient := mod.widgets.FabCard{
                visible: false
                mod.widgets.FabGroupTitle{ text: "ORIENTATION TEST" }
                text := mod.widgets.FabLabelDim{ width: Fill text: "" }
                status := mod.widgets.FabLabel{ width: Fill text: "" visible: false }
                FabTip{ text: "Write the orientation test's file to the printer's USB stick"
                    save := mod.widgets.FabButton{ text: "Save the test to the stick" }
                }
            }
            stats_card := mod.widgets.FabCard{
                visible: false
                mod.widgets.FabGroupTitle{ text: "THE PRINTER REPORTS" }
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
            job := mod.widgets.FabJobCard{ visible: false }
            trays_card := mod.widgets.FabCard{
                visible: false
                mod.widgets.FabGroupTitle{ text: "FILAMENT ON THE PRINTER" }
                trays := mod.widgets.FabTrayGrid{}
            }
            map_card := mod.widgets.FabCard{
                mod.widgets.FabGroupTitle{ text: "COLOURS → TRAYS" }
                map := mod.widgets.FabColorMap{}
            }
        }
    }
}

/// The card in place of the printer's when none is linked.
const NO_PRINTER: (&str, &str) = ("No printer linked", "Fab talks to a printer on your network in LAN mode. Add the printer's address and access code in ~/.makepad/fab/printers, then restart Fab. Slicing and export work without one.");

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

/// What the confirm card asks or says: the print's question once its slice is ready, how far the
/// slice is before that (with its progress), and what to do when none is on its way. The card
/// follows the slice, so the question comes the moment the slice does.
fn confirm_question(p: &PrintState, printer: &str, empty_plate: bool) -> (String, Option<f32>) {
    if empty_plate {
        return ("The plate is empty: there is nothing to print yet.".into(), None);
    }
    match &p.status {
        SliceStatus::Running { progress, stage } => (format!("Preparing the print for {printer}: {stage}…"), Some(*progress)),
        SliceStatus::Failed(e) => (format!("The slice failed, so there is nothing to print: {}. Print tries again.", e.lines().next().unwrap_or("internal slicer error")), None),
        _ if p.can_send() && p.printer_busy() => (format!("{printer} is busy. Queue the sliced plate? It waits in Monitor and starts only when you press Start there."), None),
        _ if p.can_send() => (format!("Print the sliced plate on {printer}? It uploads to the printer's USB drive and starts at once."), None),
        _ => (format!("Preparing the print for {printer}: the slice is about to start…"), None),
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

/// The slice's status line and progress.
pub fn slice_status(p: &PrintState) -> (String, Option<f32>) {
    // a resin printer's slice is its layer stack
    if let Some(r) = &p.resin {
        return (r.status(), None);
    }
    match &p.status {
        SliceStatus::Idle if p.preview.is_none() => ("Not sliced yet.".to_string(), None),
        SliceStatus::Idle => (String::new(), None),
        SliceStatus::Running { progress, stage } => (format!("{stage}…"), Some(*progress)),
        SliceStatus::Done { seconds } => {
            let mut t = format!("Sliced in {seconds:.1} s.");
            if p.stale {
                t.push_str(" The plate changed since: slice again.");
            }
            if let Some(e) = &p.exported {
                t.push_str(&format!(" Wrote {}", e.display()));
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
    #[deref]
    view: View,
}

impl FabSettingsPanel {
    fn open(&self, cx: &mut Cx, tag: LiveId, anchor: Rect, state: &AppState) {
        let p = &state.print;
        let items = if tag == live_id!(print_source) {
            let mut v = vec![MenuItem::new(live_id!(installed), "Installed profiles").checked(p.settings.use_installed)];
            if !p.profiles.available {
                v[0] = v[0].clone().disabled();
            }
            v.push(MenuItem::new(live_id!(defaults), "Fab defaults").checked(!p.settings.use_installed));
            v
        } else if tag == live_id!(print_machine) {
            names_menu(&p.profiles.machines, &p.settings.machine)
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

    fn picks(&self, cx: &mut Cx, actions: &Actions, state: &AppState) {
        let p = &state.print;
        if let Some(id) = menu_picked(actions, live_id!(print_source)) {
            cx.action(PrintAction::UseInstalled(id == live_id!(installed)));
        }
        if let Some(n) = menu_picked(actions, live_id!(print_machine)).and_then(|id| picked_name(&p.profiles.machines, id)) {
            cx.action(PrintAction::SetMachine(n));
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
        set_pick(cx, ids!(body.profile.source_row.source), if s.use_installed { "Installed profiles" } else { "Fab defaults" });
        set_pick(cx, ids!(body.profile.machine_row.machine), &s.machine);
        set_pick(cx, ids!(body.profile.plate_row.plate), if s.plate.is_empty() { NO_PLATE_FITS } else { &s.plate });
        set_pick(cx, ids!(body.profile.process_row.process), &s.process);
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
        let note = [materials.join("\n"), p.profiles.note.clone()].into_iter().filter(|t| !t.trim().is_empty()).collect::<Vec<_>>().join("\n");
        v.label(cx, ids!(body.colours.note)).set_text(cx, note.trim());

        let (status, progress) = slice_status(p);
        v.label(cx, ids!(body.slice.status_row.status)).set_text(cx, &status);
        let bar = v.view(cx, ids!(body.slice.status_row.progress));
        bar.set_visible(cx, progress.is_some());
        if let Some(f) = progress {
            crate::ui::widgets::set_progress(cx, &bar, f as f32);
        }
        let summary = match &p.preview {
            Some(pv) if !p.stale => pv.summary.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        };
        v.label(cx, ids!(body.slice.summary)).set_text(cx, &summary);
        v.button(cx, ids!(body.slice.actions.export)).set_enabled(cx, !state.plate.parts.is_empty());
    }
}

impl Widget for FabSettingsPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Job");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::Actions(actions) = event {
            if self.view.text_input(cx, ids!(editor_bar.search.row.input)).changed(actions).is_some() {
                self.view.redraw(cx);
            }
        }
        // a section held by its grip moves with the pointer, the others sliding aside
        // (`section_reorder`): first, and with its animation's frames too
        if matches!(event, Event::MouseDown(_) | Event::MouseMove(_) | Event::MouseUp(_) | Event::NextFrame(_)) {
            let order: Vec<(LiveId, WidgetRef, WidgetRef)> = [(live_id!(profile_slot), live_id!(profile)), (live_id!(colours_slot), live_id!(colours)), (live_id!(slice_slot), live_id!(slice))]
                .into_iter().map(|(slot, id)| (slot, self.view.widget(cx, &[id]), self.view.widget(cx, &[slot]))).collect();
            crate::ui::widgets::section_reorder(cx, event, live_id!(job_sections), &order, self.view.area());
        }
        // the sections' headers first (their hover, and a drag across them folding them all)
        if matches!(event, Event::MouseMove(_) | Event::MouseDown(_) | Event::MouseUp(_) | Event::MouseLeave(_) | Event::ClearHover) {
            let sections = [ids!(profile), ids!(colours), ids!(slice)].map(|path| self.view.widget(cx, path));
            crate::ui::widgets::section_headers(cx, event, &sections);
        }
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
        if v.button(cx, ids!(body.slice.actions.export)).clicked(actions) {
            cx.action(PrintAction::Export);
        }
        if actions.iter().any(|a| a.downcast_ref::<PrintAction>().is_some_and(|a| !a.moves_playhead())) {
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // the sections in the order they were dragged into
        let body = self.view.widget(cx, ids!(body));
        crate::ui::widgets::apply_section_order(cx, live_id!(job_sections), &body);
        if let Some(state) = scope.data.get::<AppState>() {
            self.sync(cx, state);
        }
        // the toolbar's search filters the sections' rows (`filter_sections`)
        let query = self.view.text_input(cx, ids!(editor_bar.search.row.input)).text();
        let sections = [ids!(profile), ids!(colours), ids!(slice)].map(|path| self.view.widget(cx, path));
        crate::ui::widgets::filter_sections(cx, live_id!(job_sections), &query, &sections);
        self.view.draw_walk(cx, scope, walk)
    }
}

// ===========================================================================
// The printer
// ===========================================================================

#[derive(Script, ScriptHook, Widget)]
pub struct FabPrinterPanel {
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
}

impl FabPrinterPanel {
    fn sync(&mut self, cx: &mut Cx, state: &AppState) {
        let p = &state.print;
        let v = &self.view;
        // Every press of Print shows the top of the panel: the confirm card, or "No printer linked".
        if self.asked != p.confirm_asked {
            self.asked = p.confirm_asked;
            v.widget(cx, ids!(body)).set_scroll_pos(cx, dvec2(0.0, 0.0));
        }
        if let Some(r) = &p.resin {
            self.sync_resin(cx, state, r);
            return;
        }
        v.view(cx, ids!(body.orient)).set_visible(cx, false);
        let none = v.view(cx, ids!(body.none));
        none.set_visible(cx, p.printer.is_none());
        none.label(cx, ids!(title)).set_text(cx, NO_PRINTER.0);
        none.label(cx, ids!(text)).set_text(cx, NO_PRINTER.1);
        for path in [ids!(body.card), ids!(body.trays_card)] {
            v.view(cx, path).set_visible(cx, p.printer.is_some());
        }
        v.view(cx, ids!(body.map_card)).set_visible(cx, true);
        let Some(printer) = &p.printer else {
            v.view(cx, ids!(body.job)).set_visible(cx, false);
            v.view(cx, ids!(body.confirm)).set_visible(cx, false);
            v.view(cx, ids!(body.stats_card)).set_visible(cx, false);
            return;
        };
        let s = &printer.state;
        let card = v.view(cx, ids!(body.card));
        card.label(cx, ids!(head.names.name)).set_text(cx, &printer.name);
        let suffix = printer.serial_suffix.as_ref().map(|s| format!(" · …{s}")).unwrap_or_default();
        card.label(cx, ids!(head.names.model)).set_text(cx, &format!("{}{suffix}", printer.model));
        let (pill_state, pill_text) = printer_status(printer);
        let pill = card.view(cx, ids!(head.pill));
        if let Some(mut pill) = pill.borrow_mut() {
            pill.draw_bg.set_dyn_instance(cx, live_id!(state), &[pill_state as f32]);
        }
        pill.label(cx, ids!(text)).set_text(cx, &pill_text);
        card.label(cx, ids!(telemetry)).set_text(cx, &printer_telemetry(printer));
        let supported = fab_printer_link::printing_supported();
        card.label(cx, ids!(note)).set_text(cx, &printer_note(printer, supported, job_card::job_view(p, Cx::time_now()).is_some()));
        card.view(cx, ids!(controls)).set_visible(cx, supported);
        card.button(cx, ids!(controls.clear)).set_visible(cx, s.print_error.is_some_and(|e| e != 0));

        // Everything the printer reports and the job's own numbers: Monitor's.
        let monitor = state.ui.in_mode(crate::api::PlateMode::Monitor);
        let stats = v.view(cx, ids!(body.stats_card));
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
        let gcode = s.gcode_state.as_deref().unwrap_or("");

        // The confirm card: open from the press until Cancel or Start print, whatever the slice is
        // doing; the app's checks decide what stands in the way.
        let confirm = v.view(cx, ids!(body.confirm));
        confirm.set_visible(cx, supported && p.confirm_print);
        confirm.view(cx, ids!(stick_row)).set_visible(cx, false);
        let (question, progress) = confirm_question(p, &printer.name, state.plate.parts.is_empty());
        confirm.label(cx, ids!(question)).set_text(cx, &question);
        let bar = confirm.view(cx, ids!(progress));
        bar.set_visible(cx, progress.is_some());
        if let Some(f) = progress {
            crate::ui::widgets::set_progress(cx, &bar, f as f32);
        }
        let check = match &p.print_check {
            Some(Ok(what)) if p.can_send() => format!("Safety check {what}. The printer homes and levels the bed itself first."),
            _ => String::new(),
        };
        confirm.label(cx, ids!(check)).set_visible(cx, !check.is_empty());
        confirm.label(cx, ids!(check)).set_text(cx, &check);
        let blocker = p.send_blocked.as_deref();
        // Accepted printability errors stay named, above whatever else stops Start print.
        let accepted = p.errors_accepted().then(|| p.errors_text()).flatten();
        let said: Vec<&str> = accepted.as_deref().into_iter().chain(blocker).collect();
        confirm.label(cx, ids!(blocker)).set_text(cx, &said.join("\n"));
        confirm.label(cx, ids!(blocker)).set_visible(cx, !said.is_empty());
        confirm.button(cx, ids!(buttons.go)).set_visible(cx, p.can_send() && blocker.is_none());
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
    /// Where the printability errors are, one press each: the confirm card's button. The errors.
    fn show_issue(&self, cx: &mut Cx, confirm: &ViewRef, p: &PrintState) -> Vec<usize> {
        let errors = p.print_errors();
        let show = confirm.button(cx, ids!(show_issue));
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
            v.view(cx, path).set_visible(cx, false);
        }
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
        let card = v.view(cx, ids!(body.card));
        let shown = match &r.linked {
            _ if stick => Some(r.printer.as_str()),
            Linked::Found(name) => Some(name.as_str()),
            _ => None,
        };
        card.set_visible(cx, shown.is_some());
        if let Some(name) = shown {
            card.label(cx, ids!(head.names.name)).set_text(cx, job.map_or(name, |j| j.printer.as_str()));
            card.label(cx, ids!(head.names.model)).set_text(cx, if stick { "Prints from a USB stick" } else { "Resin printer on Fab's link" });
            let (pill_state, pill_text) = match job {
                Some(j) if matches!(j.stage, ResinStage::Saving { .. }) => (2.0, "Saving"),
                _ if stick => (1.0, "USB stick"),
                Some(j) if j.ended() => (1.0, "Ready"),
                Some(j) if j.stage == ResinStage::Paused => (2.0, "Paused"),
                Some(_) => (2.0, "Printing"),
                None => (1.0, "Linked"),
            };
            let pill = card.view(cx, ids!(head.pill));
            if let Some(mut pill) = pill.borrow_mut() {
                pill.draw_bg.set_dyn_instance(cx, live_id!(state), &[pill_state as f32]);
            }
            pill.label(cx, ids!(text)).set_text(cx, pill_text);
            card.label(cx, ids!(telemetry)).set_text(cx, "");
            card.label(cx, ids!(note)).set_text(cx, "");
            card.view(cx, ids!(controls)).set_visible(cx, false);
        }
        // the job on its way or on the printer; Monitor shows it at the top of its view
        v.widget(cx, ids!(body.job)).set_visible(cx, job.is_some() && !state.ui.in_mode(crate::api::PlateMode::Monitor));

        // the orientation test, for a printer that prints from a stick
        let orient = v.view(cx, ids!(body.orient));
        orient.set_visible(cx, stick);
        orient.label(cx, ids!(text)).set_text(cx, resin::TEST_TEXT);
        let status = resin::test_status(r);
        orient.label(cx, ids!(status)).set_visible(cx, status.is_some());
        orient.label(cx, ids!(status)).set_text(cx, status.as_deref().unwrap_or(""));
        orient.button(cx, ids!(save)).set_visible(cx, !matches!(r.test, resin::TestFile::Saving { .. }));

        // the confirm card: open from the press until Cancel or Start print
        let confirm = v.view(cx, ids!(body.confirm));
        confirm.set_visible(cx, p.confirm_print);
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
        makepad_platform::a11y::set_string(&mut node.name, "Printer");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
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
        if self.view.view(cx, ids!(body.stats_card)).button(cx, ids!(raw_button)).clicked(actions) {
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
        let confirm = v.view(cx, ids!(body.confirm));
        if confirm.button(cx, ids!(buttons.cancel)).clicked(actions) {
            cx.action(PrintAction::AskPrint(false));
        }
        if confirm.button(cx, ids!(buttons.go)).clicked(actions) {
            cx.action(PrintAction::SendToPrinter);
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
        let card = v.view(cx, ids!(body.card));
        if card.button(cx, ids!(controls.clear)).clicked(actions) {
            cx.action(PrintAction::PrinterControl(PrinterControl::ClearError));
        }
        if card.button(cx, ids!(controls.storage)).clicked(actions) {
            cx.action(PrintAction::PrinterControl(PrinterControl::CheckStorage));
        }
        if v.view(cx, ids!(body.orient)).button(cx, ids!(save)).clicked(actions) {
            cx.action(PrintAction::SaveOrientationTest);
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

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
