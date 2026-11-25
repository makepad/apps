//! Design's Force render mode: the study's results, the part's material and the infill floor
//! (`FabForcePanel`, a tab of Design's right column while the mode is on; the Force tool places
//! the fixtures and loads). The study itself runs in the app; the plate view colours the part with
//! it (`viewport::force`) and the tool overlay draws its fixtures, loads and colour key; both show
//! `AppState::force`.

use crate::api::*;
use crate::ui::busy::FabBusyLineWidgetExt;
use crate::ui::popover::{dropdown_clicked, menu_picked, open_menu, MenuItem, MenuPlace};
use crate::ui::editors::Editor;
use crate::ui::shell::ShowPanel;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*
    use mod.math.*
    use mod.shader.*
    use mod.draw

    let Seg = mod.widgets.FabSegmentTab{ height: 22 padding: Inset{left: 10 right: 10 top: 0 bottom: 0} }
    let Segs = View{ width: Fill height: Fit flow: Right{wrap: true} spacing: 2 }
    let Heading = mod.widgets.FabHeaderLabel{ width: Fill height: Fit margin: Inset{top: 4} }
    let Wrapped = mod.widgets.FabLabel{ width: Fill height: Fit flow: Right{wrap: true} text: "" }
    // A readout on the property rows' columns: its name muted and right-aligned, the value beside it.
    let StatRow = View{
        width: Fill height: Fit flow: Right spacing: theme.prop_label_gap visible: false
        stat := mod.widgets.FabLabelDim{ width: Fill{weight: 0 basis: "42%" max: theme.prop_label_width} height: Fit align: Align{x: 1.0} flow: Right text: "" }
        value := Wrapped{}
    }
    let Num = ValueInput{ width: Fill height: Fill min_height: 0 min: 0.0 max: 0.0 step: 1.0 precision: 2 }

    mod.widgets.FabForcePanelBase = #(FabForcePanel::register_widget(vm))
    mod.widgets.FabForcePanel = set_type_default() do mod.widgets.FabForcePanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        // the toolbar: the area's editor selector (and this editor's tools, when it has any)
        editor_bar := mod.widgets.FabEditorBar{}
        body := mod.widgets.FabScroll{
            flow: Down padding: 10 spacing: 6
            // the part and what it prints in, or why there is none; the link opens the Job tab's
            // COLOURS, where the part's filament is set
            head := View{
                width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                part := Wrapped{}
                mod.widgets.FabTip{ wrap_width: 300. text: "Set in Job › COLOURS: the part's filament comes from its print colour, the tray it is mapped to on the printer, else the process's filament"
                    to_job := mod.widgets.FabIconButton{ a11y: {label: "Set the filament in Job › COLOURS"} draw_icon +: { svg: crate_resource("self://resources/icons/go_to.svg") } }
                }
            }
            // what Force is busy with, or its last result: a fixed slot (its tip: how the
            // study shown was solved)
            busy := mod.widgets.FabBusyLine{}
            // what the study still needs, or how it went
            status := Wrapped{}
            // only for a filament the study has no strength data for
            study_as_row := View{
                width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5} visible: false
                mod.widgets.FabLabelDim{ width: Fit height: Fit text: "Study as" }
                study_as := mod.widgets.FabDropdownButton{ tag: @fab_force_study_as owner: @fab_force_study_as label +: { text: "Pick one" } }
            }
            r0 := StatRow{} r1 := StatRow{} r2 := StatRow{} r3 := StatRow{} r4 := StatRow{} r5 := StatRow{} r6 := StatRow{}
            Heading{ text: "Show" }
            fields := Segs{ f_vm := Seg{ text: "Von Mises" } f_p1 := Seg{ text: "Principal" } f_sf := Seg{ text: "Safety factor" } f_u := Seg{ text: "Displacement" } }
            deform_row := View{
                width: Fill height: 22 flow: Right spacing: 8 align: Align{y: 0.5}
                mod.widgets.FabLabelDim{ width: Fit height: Fit text: "Deformed" }
                deform := Num{ min: 0.0 max: 1.0 step: 0.05 precision: 2 show_fill: true }
                deform_note := mod.widgets.FabLabelDim{ width: 90 height: Fit text: "" }
            }
            floor_row := View{
                width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                Heading{ text: "Infill floor" }
                mod.widgets.FabTip{ wrap_width: 300. text: "How sparse the infill can go: the density where the safety factor falls to 1 (it breaks) and to 2, and other wall loop counts, on the study's samples"
                    find_floor := mod.widgets.FabButton{ text: "Find" height: 22 a11y: {label: "Find the infill floor"} }
                }
            }
            floor_status := Wrapped{}
            q0 := StatRow{} q1 := StatRow{} q2 := StatRow{} q3 := StatRow{} q4 := StatRow{}
            // what few studies need, out of the way
            more := mod.widgets.FabFlatButton{ text: "▸ More" }
            more_body := View{
                width: Fill height: Fit flow: Down spacing: 4 visible: false
                solid := mod.widgets.FabCheckBox{ text: "Solid plastic: no walls, infill or layers, as moulded" }
            }
        }
    }
}

/// How many result rows the panel has.
pub const STAT_ROWS: usize = 7;
/// The "Study as" menu.
const STUDY_AS_MENU: LiveId = live_id!(fab_force_study_as);

const STATS: [&[LiveId]; STAT_ROWS] = [ids!(r0), ids!(r1), ids!(r2), ids!(r3), ids!(r4), ids!(r5), ids!(r6)];
/// The busy line's tip while the study is not converged: what Refine does.
const REFINE_TIP: &str = "The safety factor away from the fixtures differs between the cell sizes, or the part is thin for them: Refine solves one size finer, about this long here";
/// The busy line's tip while no AI hub answers: what Start AI Hub does.
const START_HUB_TIP: &str = "Force's solves run on the AI hub. Start AI Hub starts this machine's AI Hub app (its icon then sits in the menu bar or tray); the study goes on once it answers";
pub const FLOOR_ROWS: usize = 5;
const FLOOR: [&[LiveId]; FLOOR_ROWS] = [ids!(q0), ids!(q1), ids!(q2), ids!(q3), ids!(q4)];

#[derive(Script, ScriptHook, Widget)]
pub struct FabForcePanel {
    #[deref]
    view: View,
    /// The foot's "More" is open.
    #[rust]
    more: bool,
}

impl FabForcePanel {
    fn sync_radios(&self, cx: &mut Cx, ids: &[&[LiveId]], on: Option<usize>) {
        for (i, id) in ids.iter().enumerate() {
            let r = self.view.radio_button(cx, id);
            if r.active(cx) != (on == Some(i)) {
                r.set_active(cx, on == Some(i), Animate::No);
            }
        }
    }
}

/// A load's size for reading: three figures, no trailing zeros (10, 0.1, 2.5, 1000).
pub fn size_text(v: f64) -> String {
    let decimals = if v >= 100.0 { 0 } else if v >= 10.0 { 1 } else if v >= 1.0 { 2 } else { 3 };
    let t = format!("{v:.decimals$}");
    if t.contains('.') { t.trim_end_matches('0').trim_end_matches('.').to_string() } else { t }
}

/// A load in a line: what it is, on what (as the script writes it), how big its region.
pub fn load_name(l: &ForceLoad) -> String {
    let on = if l.on.chars().count() > 28 { format!("{}...", l.on.chars().take(26).collect::<String>()) } else { l.on.clone() };
    format!("{} on {on}, {:.0} mm\u{b2}", l.kind.label(), l.area)
}

impl Widget for FabForcePanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Force");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let v = &self.view;
        if v.button(cx, ids!(to_job)).clicked(actions) {
            cx.action(ShowPanel(Editor::Settings));
        }
        if let Some(anchor) = dropdown_clicked(actions, STUDY_AS_MENU) {
            let names = scope.data.get::<AppState>().map(|s| s.force.filaments.clone()).unwrap_or_default();
            let items = names.iter().enumerate().map(|(i, n)| MenuItem::new(LiveId(i as u64 + 1), n)).collect();
            open_menu(cx, STUDY_AS_MENU, items, anchor, MenuPlace::Below);
        }
        if let Some(id) = menu_picked(actions, STUDY_AS_MENU) {
            cx.action(ForceAction::StudyAs(id.0 as usize - 1));
        }
        if let Some(on) = v.check_box(cx, ids!(solid)).changed(actions) {
            cx.action(ForceAction::AsPrinted(!on));
        }
        let more = v.button(cx, ids!(more)).clicked(actions);
        if let Some(i) = v.radio_button_set(cx, ids_array!(f_vm, f_p1, f_sf, f_u)).selected(cx, actions) {
            cx.action(ForceAction::Field(ForceField::ALL[i]));
        }
        if let Some(d) = v.value_input(cx, ids!(deform)).changed(actions) {
            cx.action(ForceAction::Deform(d));
        }
        if v.button(cx, ids!(find_floor)).clicked(actions) {
            cx.action(ForceAction::FindFloor);
        }
        if v.fab_busy_line(cx, ids!(busy)).acted(actions) {
            // the line's button: Start AI Hub while none answers, else Refine
            let missing = scope.data.get::<AppState>().is_some_and(|s| s.force.hub == ForceHub::Missing);
            cx.action(if missing { ForceAction::StartHub } else { ForceAction::Refine });
        }
        if more {
            self.more = !self.more;
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(s) = scope.data.get::<AppState>() {
            let f = &s.force;
            // the part studied (the parts list's selection, else the last studied, else the
            // plate's first) and what it prints in, or why there is none
            let named = f.note.is_empty() && !f.part.is_empty();
            let head = match (named, f.material.is_empty()) {
                (false, _) => f.note.clone(),
                (true, true) => f.part.clone(),
                (true, false) => format!("{} · {}", f.part, f.material),
            };
            let label = self.view.label(cx, ids!(part));
            label.set_visible(cx, !head.is_empty());
            label.set_text(cx, &head);
            self.view.widget(cx, ids!(to_job)).set_visible(cx, named && !f.material.is_empty());
            self.view.view(cx, ids!(study_as_row)).set_visible(cx, f.strengthless.is_some());
            let picked = f.study_as.and_then(|i| f.filaments.get(i)).map_or("Pick one", |n| n.as_str());
            self.view.view(cx, ids!(study_as)).label(cx, ids!(label)).set_text(cx, picked);
            self.view.check_box(cx, ids!(solid)).set_active(cx, !f.as_printed, Animate::No);
            // solid plastic shows its More open, so it is never on unseen
            let more = self.more || !f.as_printed;
            self.view.view(cx, ids!(more_body)).set_visible(cx, more);
            self.view.button(cx, ids!(more)).set_text(cx, if more { "▾ More" } else { "▸ More" });
            self.sync_radios(cx, &[ids!(f_vm), ids!(f_p1), ids!(f_sf), ids!(f_u)], ForceField::ALL.iter().position(|x| *x == f.field));
            // the busy line heads the study's state, with Refine where it is not converged (the
            // status line then says how it went); the status line says the rest of it
            let (headline, rest) = match &f.refine {
                Some(_) => ("Not converged".to_string(), f.status.as_str()),
                None => (f.headline(), f.status.split_once(": ").map_or("", |(_, rest)| rest)),
            };
            let act = match f.hub {
                ForceHub::Missing => Some("Start AI Hub"),
                ForceHub::Starting => None,
                ForceHub::Answering if f.stopped => Some("Refine"),
                ForceHub::Answering => f.refine.as_deref(),
            };
            // its tip: what its button does, the print settings read, how the study shown was solved
            let does = match f.hub {
                ForceHub::Missing => Some(START_HUB_TIP),
                _ => f.refine.as_ref().map(|_| REFINE_TIP),
            };
            let mut tip: Vec<String> = does.map(|d| format!("{d}\n")).into_iter().collect();
            tip.extend((!f.print.is_empty()).then(|| f.print.clone()));
            tip.extend(f.how.iter().map(|(k, v)| format!("{k}: {v}")));
            self.view.fab_busy_line(cx, ids!(busy)).set(cx, f.busy, &headline, &tip.join("\n"), act);
            let label = self.view.label(cx, ids!(status));
            label.set_visible(cx, !rest.is_empty());
            label.set_text(cx, rest);
            for (rows, ids) in [(&f.stats, &STATS[..]), (&f.floor.rows, &FLOOR[..])] {
                for (i, id) in ids.iter().enumerate() {
                    let row = self.view.view(cx, id);
                    row.set_visible(cx, i < rows.len());
                    if let Some((name, value)) = rows.get(i) {
                        row.label(cx, ids!(stat)).set_text(cx, name);
                        row.label(cx, ids!(value)).set_text(cx, value);
                    }
                }
            }
            self.view.widget(cx, ids!(find_floor)).set_visible(cx, !f.floor.running);
            let floor = self.view.label(cx, ids!(floor_status));
            floor.set_visible(cx, !f.floor.status.is_empty());
            floor.set_text(cx, &f.floor.status);
            let input = self.view.value_input(cx, ids!(deform));
            if !input.is_interacting() && input.value() != f.deform {
                input.set_value(cx, f.deform);
            }
            let note = match f.scene.as_ref().filter(|s| s.answer.is_some()) {
                Some(s) => format!("x{}", round_factor(magnify(s, f.deform))),
                None => String::new(),
            };
            self.view.label(cx, ids!(deform_note)).set_text(cx, &note);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

/// How many times its displacement the deformed shape shows: at `deform` 1 the largest
/// displacement shows as [`FORCE_DEFORM_SHARE`] of the part's size.
pub fn magnify(scene: &ForceScene, deform: f64) -> f64 {
    let size = (0..3).map(|a| (scene.hi[a] - scene.lo[a]) as f64).fold(0.0, f64::max);
    let u = scene.answer.as_ref().map_or(0.0, |a| a.display.max_u as f64);
    if u > 0.0 { deform * FORCE_DEFORM_SHARE * size / u } else { 0.0 }
}

/// A factor to two significant figures, for reading.
fn round_factor(f: f64) -> String {
    if f <= 0.0 {
        return "0".into();
    }
    // two significant figures, as many decimals as they take
    let decimals = (1 - f.log10().floor() as i32).max(0) as usize;
    format!("{f:.decimals$}")
}

/// The study's colour ramps, for the key and the part's shade (`viewport::force`).
pub fn ramp(x: f64) -> Vec4f {
    let stops = [[0.15, 0.30, 0.95], [0.10, 0.80, 0.85], [0.20, 0.85, 0.25], [0.95, 0.85, 0.15], [0.95, 0.20, 0.15]];
    let f = x.clamp(0.0, 1.0) * 4.0;
    let i = (f.floor() as usize).min(3);
    let t = f - i as f64;
    let c: [f64; 3] = std::array::from_fn(|k| stops[i][k] + (stops[i + 1][k] - stops[i][k]) * t);
    vec4(c[0] as f32, c[1] as f32, c[2] as f32, 1.0)
}

pub fn diverge(x: f64) -> Vec4f {
    let t = x.clamp(-1.0, 1.0);
    let mid = [0.80, 0.80, 0.78];
    let end = if t < 0.0 { [0.15, 0.35, 0.95] } else { [0.95, 0.20, 0.15] };
    let c: [f64; 3] = std::array::from_fn(|k| mid[k] + (end[k] - mid[k]) * t.abs());
    vec4(c[0] as f32, c[1] as f32, c[2] as f32, 1.0)
}
