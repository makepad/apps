//! The Arm workspace (experimental): the run's controls, figures and checks (`FabArmPanel`)
//! and its view of the cell (`FabArmCell`). The lab itself runs in the app; both show
//! `AppState::arm`.
//!
//! The cell view is two orthographic views under one orbit: the whole cell, each arm drawn
//! where it truly is with its deflection from its setpoint's rigid pose enlarged (the setpoint
//! dashed over it, each link's mass a ring), and the part at 1:1: its surface coloured by its
//! distance off the design, the tool, where the tracker saw the TCP, and the path ahead.

use crate::api::*;
use crate::tools::overlay::{DrawToolLine, DrawToolMarker};
use makepad_widgets_core::tip::{TipAction, TipPlace, TipRequest};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    let Seg = mod.widgets.FabSegmentTab{ height: 22 padding: Inset{left: 10 right: 10 top: 0 bottom: 0} }
    let Segs = View{ width: Fill height: Fit flow: Right{wrap: true} spacing: 2 }
    let Heading = mod.widgets.FabHeaderLabel{ width: Fill height: Fit margin: Inset{top: 4} }
    let Wrapped = mod.widgets.FabLabel{ width: Fill height: Fit flow: Right{wrap: true} text: "" }
    let WrappedDim = mod.widgets.FabLabelDim{ width: Fill height: Fit flow: Right{wrap: true} text: "" }
    let StatRow = View{
        width: Fill height: Fit flow: Right spacing: 8 visible: false
        stat := mod.widgets.FabLabelDim{ width: 120 height: Fit text: "" }
        value := Wrapped{}
    }
    let CheckRow = View{
        width: Fill height: Fit flow: Right spacing: 6 visible: false
        verdict := mod.widgets.FabLabel{ width: 40 height: Fit text: "" }
        about := View{
            width: Fill height: Fit flow: Down spacing: 1
            name := Wrapped{}
            figures := WrappedDim{}
        }
    }

    mod.widgets.FabArmPanelBase = #(FabArmPanel::register_widget(vm))
    mod.widgets.FabArmPanel = set_type_default() do mod.widgets.FabArmPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        body := mod.widgets.FabScroll{
            flow: Down padding: 10 spacing: 6
            WrappedDim{ text: "Experimental. Arms build metal and trim it, or print plastic, in a virtual cell whose arms are softer and heavier than their models; the live loop corrects them from the tracker, the melt pool and the scans." }
            processes := Segs{ p_metal := Seg{ text: "Metal" } p_plastic := Seg{ text: "Plastic" } }
            Heading{ text: "Cell" }
            cells := Segs{ c0 := Seg{} c1 := Seg{} c2 := Seg{} c3 := Seg{} c4 := Seg{} }
            head_row := View{
                width: Fill height: Fit flow: Down spacing: 6 visible: false
                Heading{ text: "Head" }
                heads := Segs{ h0 := Seg{} h1 := Seg{} }
                nozzle_row := View{
                    width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                    mod.widgets.FabLabelDim{ width: Fit height: Fit text: "Nozzle" }
                    nozzle_down := mod.widgets.FabButton{ text: "-" height: 22 }
                    nozzle := mod.widgets.FabLabel{ width: 50 height: Fit text: "" }
                    nozzle_up := mod.widgets.FabButton{ text: "+" height: 22 }
                }
            }
            Heading{ text: "Job" }
            jobs := Segs{ j0 := Seg{} j1 := Seg{} j2 := Seg{} j3 := Seg{} }
            Heading{ text: "Runs on" }
            targets := Segs{ t_sim := Seg{ text: "Sim" } t_dry := Seg{ text: "Dry run" } t_real := Seg{ text: "Real" } }
            WrappedDim{ text: "Dry run and Real need a link to a cell's devices; the virtual cell runs the same planner." }
            Heading{ text: "Loop" }
            loops := Segs{ l_rigid := Seg{ text: "Rigid" } l_ff := Seg{ text: "Feed-forward" } l_live := Seg{ text: "Live" } }
            Heading{ text: "The cell against its models" }
            errors := Segs{ e_exact := Seg{ text: "Exact" } e_typical := Seg{ text: "Typical" } }
            run_row := View{
                width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5} margin: Inset{top: 6}
                play := mod.widgets.FabButtonAccent{ text: "Run" height: 24 padding: Inset{left: 14 right: 14} }
                reset := mod.widgets.FabButton{ text: "Reset" height: 24 }
                Filler{}
                speeds := View{ width: Fit height: Fit flow: Right spacing: 2 s_1 := Seg{ text: "1x" } s_10 := Seg{ text: "10x" } s_max := Seg{ text: "Max" } }
            }
            status := Wrapped{}
            r0 := StatRow{} r1 := StatRow{} r2 := StatRow{} r3 := StatRow{} r4 := StatRow{} r5 := StatRow{}
            r6 := StatRow{} r7 := StatRow{} r8 := StatRow{} r9 := StatRow{} r10 := StatRow{} r11 := StatRow{}
            r12 := StatRow{} r13 := StatRow{} r14 := StatRow{} r15 := StatRow{} r16 := StatRow{}
            rt_row := StatRow{}
            Heading{ text: "View" }
            fields := Segs{ f_built := Seg{ text: "As built" } f_seen := Seg{ text: "As seen" } }
            flexes := Segs{ x_1 := Seg{ text: "Deflection x1" } x_20 := Seg{ text: "x20" } x_100 := Seg{ text: "x100" } }
            Heading{ text: "Events" }
            events := WrappedDim{}
            Heading{ text: "Physics checks" }
            checks_row := View{
                width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                run_checks := mod.widgets.FabButton{ text: "Run checks" height: 24 }
                checks_note := mod.widgets.FabLabelDim{ width: Fill height: Fit text: "" }
            }
            k0 := CheckRow{} k1 := CheckRow{} k2 := CheckRow{} k3 := CheckRow{}
            k4 := CheckRow{} k5 := CheckRow{} k6 := CheckRow{} k7 := CheckRow{}
            k8 := CheckRow{} k9 := CheckRow{} k10 := CheckRow{} k11 := CheckRow{}
            k12 := CheckRow{} k13 := CheckRow{} k14 := CheckRow{} k15 := CheckRow{}
            k16 := CheckRow{} k17 := CheckRow{}
        }
    }

    mod.widgets.FabArmTableBase = #(FabArmTable::register_widget(vm))
    mod.widgets.FabArmTable = set_type_default() do mod.widgets.FabArmTableBase{
        width: Fill
        height: Fit
        draw_bg: mod.draw.DrawColor{ color: #x00000000 }
        draw_row: mod.draw.DrawColor{ color: #x00000000 }
        draw_text: mod.draw.DrawText{
            text_style: theme.font_regular{ font_size: fab.font_size_small }
            color: fab.color_text
        }
        draw_head: mod.draw.DrawText{
            text_style: theme.font_bold{ font_size: fab.font_size_small }
            color: fab.color_text_active
        }
        color_text: fab.color_text
        color_dim: fab.color_text_dim
        color_hover: #xffffff10
        color_select: #xffffff1c
        color_line: fab.color_border
        color_faster: #x62c77e
        color_slower: #xe5a24a
    }

    mod.widgets.FabArmCompareBase = #(FabArmCompare::register_widget(vm))
    mod.widgets.FabArmCompare = set_type_default() do mod.widgets.FabArmCompareBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        body := mod.widgets.FabScroll{
            flow: Down padding: 12 spacing: 6
            WrappedDim{ text: "Each option's time for the part against Fab's own estimate for it on the H2D at its maximum speed. The plastic arms print with the Arm panel's head and nozzle; the metal rows lay the part in steel on one 850. Hover for what each figure weighs; click a row for its sheet." }
            cparts := Segs{ q0 := Seg{} q1 := Seg{} q2 := Seg{} q3 := Seg{} }
            cstatus := WrappedDim{}
            table := mod.widgets.FabArmTable{}
        }
    }

    mod.widgets.FabArmCellBase = #(FabArmCell::register_widget(vm))
    mod.widgets.FabArmCell = set_type_default() do mod.widgets.FabArmCellBase{
        width: Fill
        height: Fill
        draw_bg +: { color: fab.color_vp_bg_bottom }
        draw_pane: mod.draw.DrawColor{ color: fab.color_vp_bg_top }
        draw_line: mod.draw.DrawToolLine{}
        draw_dot: mod.draw.DrawToolMarker{}
        draw_text: mod.draw.DrawText{
            text_style: theme.font_regular{ font_size: fab.font_size_small }
            color: fab.color_text_dim
        }
        color_grid: fab.color_vp_grid
        color_arm_w: #xe96a2b
        color_arm_c: #x39c5ff
        color_sent: #xf0f0f0
        color_mass: #xffd05a
        color_design: #x9a9a9a
        color_beam: #xff3352
        color_text: #xd8d8d8
    }
}

/// How many check rows and figure rows the panel has.
pub const CHECK_ROWS: usize = 18;
pub const STAT_ROWS: usize = 17;

const CELLS: [&[LiveId]; 5] = [ids!(c0), ids!(c1), ids!(c2), ids!(c3), ids!(c4)];
const JOBS: [&[LiveId]; 4] = [ids!(j0), ids!(j1), ids!(j2), ids!(j3)];
const HEADS: [&[LiveId]; 2] = [ids!(h0), ids!(h1)];
const CHECKS: [&[LiveId]; CHECK_ROWS] = [ids!(k0), ids!(k1), ids!(k2), ids!(k3), ids!(k4), ids!(k5), ids!(k6), ids!(k7), ids!(k8), ids!(k9), ids!(k10), ids!(k11), ids!(k12), ids!(k13), ids!(k14), ids!(k15), ids!(k16), ids!(k17)];
/// The nozzle's step (mm).
const NOZZLE_STEP: f64 = 0.1;
const STATS: [&[LiveId]; STAT_ROWS] = [
    ids!(r0), ids!(r1), ids!(r2), ids!(r3), ids!(r4), ids!(r5), ids!(r6), ids!(r7),
    ids!(r8), ids!(r9), ids!(r10), ids!(r11), ids!(r12), ids!(r13), ids!(r14), ids!(r15), ids!(r16),
];
const SPEEDS: [f64; 3] = [1.0, 10.0, 0.0];
const FLEXES: [f64; 3] = [1.0, 20.0, 100.0];

#[derive(Script, ScriptHook, Widget)]
pub struct FabArmPanel {
    #[deref]
    view: View,
}

impl FabArmPanel {
    /// Make a radio row show `on` as its choice.
    fn sync_radios(&self, cx: &mut Cx, ids: &[&[LiveId]], on: Option<usize>) {
        for (i, id) in ids.iter().enumerate() {
            let r = self.view.radio_button(cx, id);
            if r.active(cx) != (on == Some(i)) {
                r.set_active(cx, on == Some(i), Animate::No);
            }
        }
    }
}

impl Widget for FabArmPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Arm");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if scope.data.get::<AppState>().is_some_and(|s| !s.ui.on_screen(crate::ui::editors::Editor::Arm)) {
            // off screen: none of its controls can be pressed
            return;
        }
        let running = scope.data.get::<AppState>().is_some_and(|s| s.arm.running);
        let nozzle = scope.data.get::<AppState>().map(|s| (s.arm.nozzle, s.arm.nozzles));
        let v = &self.view;
        if let Some(i) = v.radio_button_set(cx, ids_array!(p_metal, p_plastic)).selected(cx, actions) {
            cx.action(ArmAction::Plastic(i == 1));
        }
        if let Some(i) = v.radio_button_set(cx, ids_array!(c0, c1, c2, c3, c4)).selected(cx, actions) {
            cx.action(ArmAction::Cell(i));
        }
        if let Some(i) = v.radio_button_set(cx, ids_array!(j0, j1, j2, j3)).selected(cx, actions) {
            cx.action(ArmAction::Job(i));
        }
        if let Some(i) = v.radio_button_set(cx, ids_array!(h0, h1)).selected(cx, actions) {
            cx.action(ArmAction::Head(i));
        }
        if let Some((n, (lo, hi))) = nozzle {
            for (id, step) in [(ids!(nozzle_down), -NOZZLE_STEP), (ids!(nozzle_up), NOZZLE_STEP)] {
                if v.button(cx, id).clicked(actions) {
                    let next = ((n + step) * 10.0).round() / 10.0;
                    if next >= lo - 1e-9 && next <= hi + 1e-9 {
                        cx.action(ArmAction::Nozzle(next));
                    }
                }
            }
        }
        if let Some(i) = v.radio_button_set(cx, ids_array!(t_sim, t_dry, t_real)).selected(cx, actions) {
            cx.action(ArmAction::Target(ArmTarget::ALL[i]));
        }
        if let Some(i) = v.radio_button_set(cx, ids_array!(l_rigid, l_ff, l_live)).selected(cx, actions) {
            cx.action(ArmAction::Mode(ArmLoop::ALL[i]));
        }
        if let Some(i) = v.radio_button_set(cx, ids_array!(e_exact, e_typical)).selected(cx, actions) {
            cx.action(ArmAction::Errors(ArmErrors::ALL[i]));
        }
        if let Some(i) = v.radio_button_set(cx, ids_array!(s_1, s_10, s_max)).selected(cx, actions) {
            cx.action(ArmAction::Speed(SPEEDS[i]));
        }
        if let Some(i) = v.radio_button_set(cx, ids_array!(f_built, f_seen)).selected(cx, actions) {
            cx.action(ArmAction::Field(ArmField::ALL[i]));
        }
        if let Some(i) = v.radio_button_set(cx, ids_array!(x_1, x_20, x_100)).selected(cx, actions) {
            cx.action(ArmAction::Flex(FLEXES[i]));
        }
        if v.button(cx, ids!(play)).clicked(actions) {
            cx.action(if running { ArmAction::Pause } else { ArmAction::Play });
        }
        if v.button(cx, ids!(reset)).clicked(actions) {
            cx.action(ArmAction::Reset);
        }
        if v.button(cx, ids!(run_checks)).clicked(actions) {
            cx.action(ArmAction::RunChecks);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(s) = scope.data.get::<AppState>() {
            let a = &s.arm;
            self.sync_radios(cx, &[ids!(p_metal), ids!(p_plastic)], Some(a.plastic as usize));
            self.view.view(cx, ids!(head_row)).set_visible(cx, a.plastic);
            self.view.label(cx, ids!(nozzle)).set_text(cx, &format!("{:.1} mm", a.nozzle));
            self.view.widget(cx, ids!(nozzle_down)).set_disabled(cx, a.nozzle - NOZZLE_STEP < a.nozzles.0 - 1e-9);
            self.view.widget(cx, ids!(nozzle_up)).set_disabled(cx, a.nozzle + NOZZLE_STEP > a.nozzles.1 + 1e-9);
            for (slots, names, on) in [(&CELLS[..], &a.cells, a.cell), (&JOBS[..], &a.jobs, a.job), (&HEADS[..], &a.heads, a.head)] {
                for (i, id) in slots.iter().enumerate() {
                    let r = self.view.radio_button(cx, id);
                    r.set_visible(cx, i < names.len());
                    if let Some(n) = names.get(i) {
                        r.set_text(n);
                    }
                }
                self.sync_radios(cx, slots, Some(on));
            }
            self.sync_radios(cx, &[ids!(t_sim), ids!(t_dry), ids!(t_real)], ArmTarget::ALL.iter().position(|t| *t == a.target));
            for (id, t) in [(ids!(t_dry), ArmTarget::DryRun), (ids!(t_real), ArmTarget::Real)] {
                self.view.widget(cx, id).set_disabled(cx, !t.available());
            }
            self.sync_radios(cx, &[ids!(l_rigid), ids!(l_ff), ids!(l_live)], ArmLoop::ALL.iter().position(|m| *m == a.mode));
            self.sync_radios(cx, &[ids!(e_exact), ids!(e_typical)], ArmErrors::ALL.iter().position(|e| *e == a.errors));
            self.sync_radios(cx, &[ids!(s_1), ids!(s_10), ids!(s_max)], SPEEDS.iter().position(|v| *v == a.speed));
            self.sync_radios(cx, &[ids!(f_built), ids!(f_seen)], ArmField::ALL.iter().position(|f| *f == a.field));
            self.sync_radios(cx, &[ids!(x_1), ids!(x_20), ids!(x_100)], FLEXES.iter().position(|v| *v == a.flex));
            let play = if a.running { "Pause" } else if a.ended { "Run again" } else { "Run" };
            self.view.button(cx, ids!(play)).set_text(cx, play);
            self.view.label(cx, ids!(status)).set_text(cx, &a.status);
            for (i, id) in STATS.iter().enumerate() {
                let row = self.view.view(cx, id);
                row.set_visible(cx, i < a.stats.len());
                if let Some((name, value)) = a.stats.get(i) {
                    row.label(cx, ids!(stat)).set_text(cx, name);
                    row.label(cx, ids!(value)).set_text(cx, value);
                }
            }
            let rt = self.view.view(cx, ids!(rt_row));
            rt.set_visible(cx, !a.realtime.is_empty());
            rt.label(cx, ids!(stat)).set_text(cx, "Real-time path");
            rt.label(cx, ids!(value)).set_text(cx, &a.realtime);
            self.view.label(cx, ids!(events)).set_text(cx, &a.events.join("\n"));
            let done = a.checks.iter().filter(|c| matches!(c.state, ArmCheckState::Pass | ArmCheckState::Fail)).count();
            let failed = a.checks.iter().filter(|c| c.state == ArmCheckState::Fail).count();
            let note = if a.checks.is_empty() {
                "The dynamics' checks and the loop's end-to-end runs, as the tests run them".to_string()
            } else if done < a.checks.len() {
                format!("{done} of {} run", a.checks.len())
            } else if failed == 0 {
                format!("all {} pass", a.checks.len())
            } else {
                format!("{failed} of {} FAIL", a.checks.len())
            };
            self.view.label(cx, ids!(checks_note)).set_text(cx, &note);
            for (i, id) in CHECKS.iter().enumerate() {
                let row = self.view.view(cx, id);
                let Some(c) = a.checks.get(i) else {
                    row.set_visible(cx, false);
                    continue;
                };
                row.set_visible(cx, true);
                let verdict = match c.state {
                    ArmCheckState::Waiting => "",
                    ArmCheckState::Running => "...",
                    ArmCheckState::Pass => "Pass",
                    ArmCheckState::Fail => "FAIL",
                };
                row.label(cx, ids!(verdict)).set_text(cx, verdict);
                row.label(cx, ids!(name)).set_text(cx, &c.name);
                let figures = if c.figures.is_empty() { String::new() } else { format!("{} ({:.0} ms)", c.figures, c.ms) };
                row.label(cx, ids!(figures)).set_text(cx, &figures);
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

// ---- the comparison --------------------------------------------------------

const COMPARE_PARTS: [&[LiveId]; 4] = [ids!(q0), ids!(q1), ids!(q2), ids!(q3)];

/// The comparison tab: its part, what it is doing, the table.
#[derive(Script, ScriptHook, Widget)]
pub struct FabArmCompare {
    #[deref]
    view: View,
}

impl Widget for FabArmCompare {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Arm comparison");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if let Some(i) = self.view.radio_button_set(cx, ids_array!(q0, q1, q2, q3)).selected(cx, actions) {
            cx.action(ArmAction::ComparePart(i));
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(s) = scope.data.get::<AppState>() {
            let a = &s.arm;
            for (i, id) in COMPARE_PARTS.iter().enumerate() {
                let r = self.view.radio_button(cx, id);
                r.set_visible(cx, i < a.parts.len());
                if let Some(n) = a.parts.get(i) {
                    r.set_text(n);
                }
                if r.active(cx) != (i == a.compare_part) {
                    r.set_active(cx, i == a.compare_part, Animate::No);
                }
            }
            self.view.label(cx, ids!(cstatus)).set_text(cx, &a.compare_status);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

/// What the pointer is over in the table.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Spot {
    Column(usize),
    Row(usize),
}

const COLUMNS: [&str; 5] = ["Option", "Job time", "× H2D", "Lowest mode", "Limited by"];
const COLUMN_HINTS: [&str; 5] = [
    "The H2D at its maximum speed is the reference. Each arm prints the part with the Arm panel's head and nozzle; the metal rows lay it in steel. Click a row for its sheet.",
    "Plastic: the planner's dry run of Fab's plan for the arm (its acceleration, corners, setpoint ticks, travels, layer changes, cooling). The H2D: Fab's own estimate, printing only. Metal: the dry run of the deposit, scan and cut route.",
    "The job's time over the H2D's for the same part: under 1 is faster than the H2D.",
    "The lowest vibration mode where it works (an arm at its park over the part, with its head). It sets the print acceleration: a 0.05 mm ring after a speed change.",
    "Each limit lifted in turn (melt flow, acceleration, cooling; a metal step's whole time): the one whose lifting saves the most, with the share of the time it saves.",
];
const ROW_H: f64 = 22.0;
const SHEET_H: f64 = 18.0;
const PAD: f64 = 16.0;

/// The comparison table and the chosen row's sheet, drawn; its tips float over it.
#[derive(Script, ScriptHook, Widget)]
pub struct FabArmTable {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_row: DrawColor,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_head: DrawText,
    #[live]
    color_text: Vec4f,
    #[live]
    color_dim: Vec4f,
    #[live]
    color_hover: Vec4f,
    #[live]
    color_select: Vec4f,
    #[live]
    color_line: Vec4f,
    #[live]
    color_faster: Vec4f,
    #[live]
    color_slower: Vec4f,
    /// Each spot as last drawn.
    #[rust]
    spots: Vec<(Rect, Spot)>,
    #[rust]
    hover: Option<Spot>,
    /// The table shown is not the one asked for: ask on the next frame.
    #[rust]
    ask: NextFrame,
    /// Each spot's tip as last drawn.
    #[rust]
    tips: Vec<String>,
}

impl FabArmTable {
    fn width(&self, cx: &mut Cx2d, head: bool, text: &str) -> f64 {
        let d = if head { &self.draw_head } else { &self.draw_text };
        d.prepare_single_line_run(cx, text).map(|r| r.width_in_lpxs as f64).unwrap_or(8.0 * text.len() as f64)
    }

    fn put(&mut self, cx: &mut Cx2d, head: bool, at: DVec2, text: &str, color: Vec4f) {
        let d = if head { &mut self.draw_head } else { &mut self.draw_text };
        d.color = color;
        d.draw_abs(cx, at, text);
    }

    fn fill(&mut self, cx: &mut Cx2d, r: Rect, color: Vec4f) {
        self.draw_row.color = color;
        self.draw_row.draw_abs(cx, r);
    }

    fn spot_at(&self, abs: DVec2) -> Option<(usize, Spot)> {
        self.spots.iter().enumerate().find(|(_, (r, _))| r.contains(abs)).map(|(i, (_, s))| (i, *s))
    }
}

impl Widget for FabArmTable {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Grid;
        makepad_platform::a11y::set_string(&mut node.name, "Job times against the H2D");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if self.ask.is_event(event).is_some() {
            cx.action(ArmAction::Compare);
        }
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fh) | Hit::FingerHoverOver(fh) => {
                let now = self.spot_at(fh.abs);
                if now.map(|n| n.1) != self.hover {
                    self.hover = now.map(|n| n.1);
                    match now {
                        Some((i, spot)) if !self.tips[i].is_empty() => {
                            let mut request = TipRequest::plain(self.tips[i].clone(), self.spots[i].0);
                            request.place = if matches!(spot, Spot::Column(_)) { TipPlace::Top } else { TipPlace::Bottom };
                            request.wrap_width = 340.0;
                            cx.widget_action(self.uid, TipAction::HoverInWith(request));
                        }
                        _ => cx.widget_action(self.uid, TipAction::HoverOut),
                    }
                    self.draw_bg.redraw(cx);
                }
                let hand = matches!(now, Some((_, Spot::Row(_))));
                cx.set_cursor(if hand { MouseCursor::Hand } else { MouseCursor::Default });
            }
            Hit::FingerHoverOut(_) => {
                cx.widget_action(self.uid, TipAction::HoverOut);
                self.hover = None;
                self.draw_bg.redraw(cx);
            }
            Hit::FingerUp(fe) if !fe.cancelled => {
                if let Some((_, Spot::Row(i))) = self.spot_at(fe.abs) {
                    cx.action(ArmAction::CompareRow(i));
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.spots.clear();
        self.tips.clear();
        let Some(a) = scope.data.get::<AppState>().map(|s| &s.arm) else { return DrawStep::done() };
        // Drawn only while the tab shows: a table not made for the choices is asked for here.
        if a.compare_asked != Some(a.compare_key()) {
            self.ask = cx.new_next_frame();
        }
        let Some(c) = a.compare.clone() else {
            cx.walk_turtle(Walk { height: Size::Fixed(0.0), ..walk });
            return DrawStep::done();
        };
        let cells = |r: &ArmCompareRow| [r.name.clone(), r.time.clone(), r.ratio.clone(), r.mode.clone(), r.limit.clone()];
        let mut widths: [f64; 5] = std::array::from_fn(|k| self.width(cx, true, COLUMNS[k]));
        for r in &c.rows {
            for (k, t) in cells(r).iter().enumerate() {
                widths[k] = widths[k].max(self.width(cx, false, t));
            }
            if !r.group.is_empty() {
                widths[0] = widths[0].max(self.width(cx, true, &r.group) - widths[1] - widths[2]);
            }
        }
        let selected = a.compare_row.min(c.rows.len().saturating_sub(1));
        let sheet = c.rows.get(selected).map(|r| r.sheet.clone()).unwrap_or_default();
        let groups = c.rows.iter().filter(|r| !r.group.is_empty()).count();
        let table_h = ROW_H * (2 + groups + c.rows.len()) as f64;
        let sheet_h = ROW_H + SHEET_H * (sheet.len() + 1) as f64;
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(table_h + 14.0 + sheet_h), ..walk });
        self.draw_bg.draw_abs(cx, rect);
        let x0 = rect.pos.x;
        let cols: [f64; 5] = std::array::from_fn(|k| x0 + widths[..k].iter().map(|w| w + PAD).sum::<f64>());
        let right = (cols[4] + widths[4]).max(x0 + 200.0).min(rect.pos.x + rect.size.x);
        let (text, dim) = (self.color_text, self.color_dim);
        let mut y = rect.pos.y;
        self.put(cx, true, dvec2(x0, y + 3.0), &c.title, text);
        y += ROW_H;
        for k in 0..5 {
            let at = dvec2(cols[k], y + 4.0);
            let w = self.width(cx, true, COLUMNS[k]);
            self.put(cx, true, at, COLUMNS[k], dim);
            self.spots.push((Rect { pos: dvec2(cols[k], y), size: dvec2(w.max(30.0), ROW_H) }, Spot::Column(k)));
            self.tips.push(COLUMN_HINTS[k].to_string());
        }
        y += ROW_H;
        self.fill(cx, Rect { pos: dvec2(x0, y - 1.0), size: dvec2(right - x0, 1.0) }, self.color_line);
        for (i, r) in c.rows.iter().enumerate() {
            if !r.group.is_empty() {
                self.put(cx, true, dvec2(x0, y + 6.0), &r.group, dim);
                y += ROW_H;
            }
            let line = Rect { pos: dvec2(x0 - 4.0, y), size: dvec2(right - x0 + 8.0, ROW_H) };
            if i == selected {
                self.fill(cx, line, self.color_select);
            } else if self.hover == Some(Spot::Row(i)) {
                self.fill(cx, line, self.color_hover);
            }
            for (k, t) in cells(r).iter().enumerate() {
                let color = match k {
                    2 if i > 0 && !t.is_empty() => if r.faster { self.color_faster } else { self.color_slower },
                    3 | 4 => dim,
                    _ => text,
                };
                self.put(cx, false, dvec2(cols[k], y + 4.0), t, color);
            }
            self.spots.push((line, Spot::Row(i)));
            self.tips.push(r.hint.clone());
            y += ROW_H;
        }
        y += 14.0;
        if let Some(r) = c.rows.get(selected) {
            self.put(cx, true, dvec2(x0, y + 3.0), &format!("Sheet: {}", r.name), text);
        }
        y += ROW_H;
        let key_w = sheet.iter().map(|(k, _, _)| self.width(cx, false, k)).fold(0.0f64, f64::max);
        for (k, v, tag) in &sheet {
            self.put(cx, true, dvec2(x0, y + 2.0), tag, dim);
            self.put(cx, false, dvec2(x0 + 20.0, y + 2.0), k, dim);
            self.put(cx, false, dvec2(x0 + 20.0 + key_w + PAD, y + 2.0), v, text);
            y += SHEET_H;
        }
        self.put(cx, false, dvec2(x0, y + 2.0), "P published · M measured by you · E our estimate", dim);
        DrawStep::done()
    }
}

// ---- the cell view ---------------------------------------------------------

/// An orthographic view: `c` at `o` on screen, `s` points per mm, looking along `-eye`.
#[derive(Clone, Copy)]
struct Proj {
    c: [f64; 3],
    s: f64,
    o: DVec2,
    right: [f64; 3],
    up: [f64; 3],
    eye: [f64; 3],
}

impl Proj {
    /// Orbit `yaw` about Z and `pitch` up from the horizon, fitting a sphere (`c`, `radius`)
    /// into `pane`, `zoom` times nearer.
    fn new(yaw: f64, pitch: f64, c: [f64; 3], radius: f64, pane: Rect, zoom: f64) -> Proj {
        let (sy, cy, sp, cp) = (yaw.sin(), yaw.cos(), pitch.sin(), pitch.cos());
        let s = 0.5 * pane.size.x.min(pane.size.y) / radius.max(1e-3) * zoom;
        Proj { c, s, o: pane.pos + pane.size * 0.5, right: [-sy, cy, 0.0], up: [-sp * cy, -sp * sy, cp], eye: [cp * cy, cp * sy, sp] }
    }

    /// Where `p` shows and its depth (larger is nearer).
    fn at(&self, p: [f32; 3]) -> (DVec2, f64) {
        let d = [p[0] as f64 - self.c[0], p[1] as f64 - self.c[1], p[2] as f64 - self.c[2]];
        let dot = |a: [f64; 3]| a[0] * d[0] + a[1] * d[1] + a[2] * d[2];
        (dvec2(self.o.x + self.s * dot(self.right), self.o.y - self.s * dot(self.up)), dot(self.eye))
    }

    fn pt(&self, p: [f32; 3]) -> DVec2 {
        self.at(p).0
    }
}

fn mix(a: Vec4f, b: Vec4f, t: f32) -> Vec4f {
    vec4(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, a.z + (b.z - a.z) * t, a.w + (b.w - a.w) * t)
}

fn alpha(c: Vec4f, a: f32) -> Vec4f {
    vec4(c.x, c.y, c.z, c.w * a)
}

/// `sent + flex (truly - sent)`: a deflection enlarged.
fn flexed(sent: [f32; 3], truly: [f32; 3], flex: f32) -> [f32; 3] {
    std::array::from_fn(|k| sent[k] + flex * (truly[k] - sent[k]))
}

/// How far off the design (mm) the heat map's ends are.
const HEAT_RANGE: f32 = 0.4;

/// The surface's colour at `d` mm off the design: grey on it, blue where material is missing,
/// orange where there is excess, full at [`HEAT_RANGE`].
fn heat(d: f32) -> Vec4f {
    let t = (d / HEAT_RANGE).clamp(-1.0, 1.0);
    let on = vec4(0.82, 0.82, 0.8, 1.0);
    if t < 0.0 {
        mix(on, vec4(0.16, 0.5, 1.0, 1.0), -t)
    } else {
        mix(on, vec4(1.0, 0.45, 0.12, 1.0), t)
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabArmCell {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_pane: DrawColor,
    #[live]
    draw_line: DrawToolLine,
    #[live]
    draw_dot: DrawToolMarker,
    #[live]
    draw_text: DrawText,
    #[live]
    color_grid: Vec4f,
    #[live]
    color_arm_w: Vec4f,
    #[live]
    color_arm_c: Vec4f,
    #[live]
    color_sent: Vec4f,
    #[live]
    color_mass: Vec4f,
    #[live]
    color_design: Vec4f,
    #[live]
    color_beam: Vec4f,
    #[live]
    color_text: Vec4f,
    /// The orbit both views share: about Z, and up from the horizon (rad).
    #[rust(-1.2)]
    yaw: f64,
    #[rust(0.42)]
    pitch: f64,
    /// Each view's zoom.
    #[rust([1.0, 1.0])]
    zoom: [f64; 2],
    #[rust]
    drag: Option<(DVec2, f64, f64)>,
    /// The two views as last drawn: the cell, the part.
    #[rust]
    panes: [Rect; 2],
}

impl FabArmCell {
    fn seg(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, color: Vec4f, width: f64, dash: f64) {
        let pad = width + 3.0;
        let pos = dvec2(a.x.min(b.x) - pad, a.y.min(b.y) - pad);
        let size = dvec2((a.x - b.x).abs() + pad * 2.0, (a.y - b.y).abs() + pad * 2.0);
        if size.x > 8000.0 || size.y > 8000.0 || !size.x.is_finite() || !size.y.is_finite() {
            return;
        }
        self.draw_line.color = color;
        self.draw_line.line_a = vec2((a.x - pos.x) as f32, (a.y - pos.y) as f32);
        self.draw_line.line_b = vec2((b.x - pos.x) as f32, (b.y - pos.y) as f32);
        self.draw_line.line_width = width as f32;
        self.draw_line.dash = dash as f32;
        self.draw_line.draw_abs(cx, Rect { pos, size });
    }

    fn polyline(&mut self, cx: &mut Cx2d, p: &Proj, pts: &[[f32; 3]], closed: bool, color: Vec4f, width: f64, dash: f64) {
        for w in pts.windows(2) {
            self.seg(cx, p.pt(w[0]), p.pt(w[1]), color, width, dash);
        }
        if closed && pts.len() > 2 {
            self.seg(cx, p.pt(pts[pts.len() - 1]), p.pt(pts[0]), color, width, dash);
        }
    }

    fn dot(&mut self, cx: &mut Cx2d, at: DVec2, r: f64, color: Vec4f, shape: f32, hollow: bool) {
        if !at.x.is_finite() || !at.y.is_finite() {
            return;
        }
        self.draw_dot.color = color;
        self.draw_dot.shape = shape;
        self.draw_dot.hollow = if hollow { 1.0 } else { 0.0 };
        self.draw_dot.draw_abs(cx, Rect { pos: at - dvec2(r, r), size: dvec2(r * 2.0, r * 2.0) });
    }

    fn text(&mut self, cx: &mut Cx2d, at: DVec2, text: &str, color: Vec4f) {
        self.draw_text.color = color;
        self.draw_text.draw_abs(cx, at, text);
    }

    fn arm_color(&self, i: usize) -> Vec4f {
        if i % 2 == 0 { self.color_arm_w } else { self.color_arm_c }
    }

    /// The whole cell: the floor, what the part stands on, the arms with their deflection
    /// enlarged `flex` times, the setpoints' rigid poses dashed over them, the links' masses.
    fn draw_cell(&mut self, cx: &mut Cx2d, p: &Proj, scene: &ArmScene, flex: f32) {
        let (lo, hi) = (scene.cell_lo, scene.cell_hi);
        let step = 100.0f32;
        let floor = lo[2];
        let (x0, x1) = ((lo[0] / step).floor() as i32, (hi[0] / step).ceil() as i32);
        let (y0, y1) = ((lo[1] / step).floor() as i32, (hi[1] / step).ceil() as i32);
        for k in x0..=x1 {
            let x = k as f32 * step;
            self.seg(cx, p.pt([x, y0 as f32 * step, floor]), p.pt([x, y1 as f32 * step, floor]), self.color_grid, 1.0, 0.0);
        }
        for k in y0..=y1 {
            let y = k as f32 * step;
            self.seg(cx, p.pt([x0 as f32 * step, y, floor]), p.pt([x1 as f32 * step, y, floor]), self.color_grid, 1.0, 0.0);
        }
        for l in scene.stand.iter() {
            self.polyline(cx, p, l, true, self.color_design, 1.2, 0.0);
        }
        // The links far to near, each outlined dark.
        let mut links = Vec::new();
        for (i, arm) in scene.arms.iter().enumerate() {
            for (k, ((ta, tb, r), (sa, sb, _))) in arm.links.iter().zip(arm.sent.iter()).enumerate() {
                let (a, b) = (flexed(*sa, *ta, flex), flexed(*sb, *tb, flex));
                let depth = 0.5 * (p.at(a).1 + p.at(b).1);
                let shade = if k % 2 == 0 { 0.0 } else { 0.12 };
                links.push((depth, p.pt(a), p.pt(b), (2.0 * *r as f64 * p.s).max(2.0), mix(self.arm_color(i), vec4(1.0, 1.0, 1.0, 1.0), shade)));
            }
        }
        links.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, a, b, w, c) in &links {
            self.seg(cx, *a, *b, vec4(0.05, 0.05, 0.05, 0.9), w + 2.0, 0.0);
            self.seg(cx, *a, *b, *c, *w, 0.0);
        }
        for arm in &scene.arms {
            for (a, b, _) in &arm.sent {
                self.seg(cx, p.pt(*a), p.pt(*b), alpha(self.color_sent, 0.85), 1.2, 7.0);
            }
            for (truly, sent, kg) in &arm.masses {
                let r = 2.5 + 3.5 * kg.max(0.0).cbrt() as f64;
                self.dot(cx, p.pt(flexed(*sent, *truly, flex)), r, self.color_mass, 0.0, true);
            }
        }
        for (i, arm) in scene.arms.iter().enumerate() {
            let Some((base, _, _)) = arm.links.first() else { continue };
            let at = p.pt(*base) + dvec2(-30.0, 14.0);
            let c = self.arm_color(i);
            self.text(cx, at, &format!("{}: {}", arm.name, arm.doing), c);
        }
    }

    /// The part at 1:1: its surface by distance off the design, the design's outline, the
    /// tools near it, where the tracker saw each TCP, and the path ahead.
    fn draw_part(&mut self, cx: &mut Cx2d, p: &Proj, scene: &ArmScene) {
        for l in scene.stand.iter() {
            self.polyline(cx, p, l, true, alpha(self.color_design, 0.6), 1.0, 0.0);
        }
        let mut pts: Vec<(f64, DVec2, f32)> = scene.surface.iter().map(|(q, d)| {
            let (at, depth) = p.at(*q);
            (depth, at, *d)
        }).collect();
        pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        let r = (0.16 * p.s).clamp(1.0, 5.0);
        for (_, at, d) in pts {
            self.dot(cx, at, r, heat(d), 0.0, false);
        }
        for l in scene.design.iter() {
            self.polyline(cx, p, l, true, alpha(self.color_sent, 0.55), 1.0, 5.0);
        }
        let near = |q: [f32; 3]| (0..3).all(|k| q[k] > scene.part_lo[k] - 40.0 && q[k] < scene.part_hi[k] + 40.0);
        for (i, arm) in scene.arms.iter().enumerate() {
            let c = self.arm_color(i);
            let trail: Vec<[f32; 3]> = arm.trail.iter().copied().filter(|q| near(*q)).collect();
            self.polyline(cx, p, &trail, false, alpha(c, 0.8), 1.5, 0.0);
            if !near(arm.tcp) {
                continue;
            }
            // The head itself is wider than the part: its axis and tip stand for it.
            let tip = p.pt(arm.tcp);
            let into = |k: f32| std::array::from_fn(|j| arm.tcp[j] + arm.axis[j] * k);
            let beam = if arm.on { self.color_beam } else { alpha(self.color_sent, 0.5) };
            self.seg(cx, p.pt(into(-6.0)), p.pt(into(2.0)), beam, if arm.on { 2.5 } else { 1.0 }, if arm.on { 0.0 } else { 4.0 });
            self.dot(cx, tip, 3.5, c, 0.0, false);
            self.dot(cx, p.pt(arm.target), 4.0, self.color_sent, 1.0, true);
        }
        self.polyline(cx, p, &scene.horizon, false, alpha(self.color_sent, 0.9), 1.5, 4.0);
    }

    /// The heat map's key along the bottom of `pane`.
    fn draw_key(&mut self, cx: &mut Cx2d, pane: Rect) {
        let n = 24;
        let w = (pane.size.x - 24.0).min(220.0);
        let y = pane.pos.y + pane.size.y - 22.0;
        let x0 = pane.pos.x + 12.0;
        for k in 0..n {
            let t = k as f32 / (n - 1) as f32 * 2.0 - 1.0;
            let a = dvec2(x0 + w * k as f64 / n as f64, y);
            self.seg(cx, a, a + dvec2(w / n as f64, 0.0), heat(t * HEAT_RANGE), 6.0, 0.0);
        }
        let c = self.color_text;
        self.text(cx, dvec2(x0, y - 18.0), &format!("missing {HEAT_RANGE} mm"), c);
        self.text(cx, dvec2(x0 + w - 70.0, y - 18.0), &format!("excess {HEAT_RANGE} mm"), c);
    }
}

impl Widget for FabArmCell {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Image;
        makepad_platform::a11y::set_string(&mut node.name, "Arm cell");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerDown(fe) => {
                if fe.tap_count >= 2 {
                    self.yaw = -1.2;
                    self.pitch = 0.42;
                    self.zoom = [1.0, 1.0];
                    self.draw_bg.redraw(cx);
                }
                self.drag = Some((fe.abs, self.yaw, self.pitch));
            }
            Hit::FingerMove(fe) => {
                if let Some((from, yaw, pitch)) = self.drag {
                    let d = fe.abs - from;
                    self.yaw = yaw - d.x * 0.01;
                    self.pitch = (pitch + d.y * 0.01).clamp(-0.3, 1.5);
                    self.draw_bg.redraw(cx);
                }
            }
            Hit::FingerUp(_) => self.drag = None,
            Hit::FingerScroll(fe) => {
                if let Some(k) = self.panes.iter().position(|r| r.contains(fe.abs)) {
                    let notches = (-fe.scroll.y / 40.0).clamp(-3.0, 3.0);
                    self.zoom[k] = (self.zoom[k] * 1.12f64.powf(notches)).clamp(0.3, 30.0);
                    self.draw_bg.redraw(cx);
                }
                event.set_scroll_handled(Vec2Index::X);
                event.set_scroll_handled(Vec2Index::Y);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.draw_abs(cx, rect);
        let Some(arm) = scope.data.get::<AppState>().map(|s| &s.arm) else { return DrawStep::done() };
        let Some(scene) = arm.scene.clone() else {
            let c = self.color_text;
            self.text(cx, rect.pos + dvec2(24.0, 24.0), "Run a job: the cell shows here, the part beside it.", c);
            return DrawStep::done();
        };
        // Side by side when wide, the part under the cell when tall.
        let gap = 6.0;
        let (cell, part) = if rect.size.x >= rect.size.y {
            let w = (rect.size.x - gap) * 0.5;
            (Rect { pos: rect.pos, size: dvec2(w, rect.size.y) }, Rect { pos: rect.pos + dvec2(w + gap, 0.0), size: dvec2(w, rect.size.y) })
        } else {
            let h = (rect.size.y - gap) * 0.5;
            (Rect { pos: rect.pos, size: dvec2(rect.size.x, h) }, Rect { pos: rect.pos + dvec2(0.0, h + gap), size: dvec2(rect.size.x, h) })
        };
        self.panes = [cell, part];
        let mid = |lo: [f32; 3], hi: [f32; 3]| -> ([f64; 3], f64) {
            let c = std::array::from_fn(|k| 0.5 * (lo[k] + hi[k]) as f64);
            let r = (0..3).map(|k| ((hi[k] - lo[k]) as f64 * 0.5).powi(2)).sum::<f64>().sqrt();
            (c, r)
        };
        let flex = arm.flex as f32;
        let text = self.color_text;
        for (k, pane) in [cell, part].into_iter().enumerate() {
            self.draw_pane.draw_abs(cx, pane);
            cx.begin_turtle(Walk::abs_rect(pane), Layout::default().with_clip(true, true));
            if k == 0 {
                let (c, r) = mid(scene.cell_lo, scene.cell_hi);
                let p = Proj::new(self.yaw, self.pitch, c, r * 0.85, pane, self.zoom[0]);
                self.draw_cell(cx, &p, &scene, flex);
                let title = format!("Cell, {:.1} s; deflection from the setpoint x{}", scene.time, arm.flex);
                self.text(cx, pane.pos + dvec2(10.0, 8.0), &title, text);
            } else {
                let (c, r) = mid(scene.part_lo, scene.part_hi);
                let p = Proj::new(self.yaw, self.pitch, c, r * 1.15, pane, self.zoom[1]);
                self.draw_part(cx, &p, &scene);
                let title = format!("Part 1:1, {} (design dashed; diamond: where the TCP was sent)", arm.field.label().to_lowercase());
                self.text(cx, pane.pos + dvec2(10.0, 8.0), &title, text);
                self.draw_key(cx, pane);
            }
            cx.end_turtle();
        }
        DrawStep::done()
    }
}
