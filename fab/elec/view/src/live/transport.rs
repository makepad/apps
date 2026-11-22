//! The Electronics recording bar and its Setup popover.
//! A frame compares the published transport and heard state. Jack rows are rebuilt on
//! board changes, the mix label on routing changes, and Setup diagnostics only while open.

use super::listen::{self, listen_jack, listening_jack, running_jacks, ElecAudio};
use super::overview::{ElecOverview, TakeReading};
use super::sim::{write_run_settings, LiveLimit, LiveStatus, SimAction};
use super::strip::{format_rate, format_reached, format_sample_rate, format_time, RunMode, StripModel, TimeStrip, TimeStripAction, RATES};
use crate::{DesignSnapshot, ElecLive, ViewAction};
use fab_elec_bench::SimCmd;
use fab_elec_core::CompRef;
use fab_elec_design::{CursorSource, TimeCursor};
use fab_elec_lang::sys_sim::{SimMode, SimSettings, SAMPLE_RATES};
use makepad_widgets_core::*;
use makepad_widgets_core::tip::TipWidgetRefExt;
use makepad_widgets_pickers::combo_box::ComboBoxWidgetRefExt;
use std::fmt::Write as _;
use std::sync::{Arc, Weak};

/// Saved traces, as a person asks for them; the app runs each as the AI's tool of that name.
#[derive(Clone, Debug, PartialEq)]
pub enum TraceAction {
    /// The recording bar's Save trace: the Scope's take through the app's trace writer
    /// (`elec.capture.save` without a capture).
    Save,
    /// The Scope menu's saved trace at this project-relative path, shown in place of the live
    /// run (`elec.capture.open`).
    Open(String),
    /// The Scope's Back to live (`elec.capture.close`).
    Close,
    /// The Scope menu opened: list the saved traces into `ElecLive::traces`.
    List,
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    let Well = View{
        height: 22 width: Fit flow: Right align: Align{y: 0.5}
        show_bg: true
        draw_bg +: {
            radius: uniform(fab.radius)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, self.radius)
                sdf.fill(theme.prop_field)
                return sdf.result
            }
        }
    }
    let Field = mod.widgets.PropDropDown{height: 22 draw_bg +: {border_radius: fab.radius}}
    let SetupRow = View{width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}}
    let Caption = mod.widgets.FabLabelSmall{width: 84}
    let Section = mod.widgets.FabLabelSmall{draw_text +: {color: fab.color_text_active}}
    // The live status at the foot of Setup: fixed rows of one line each (an ellipsis, never a
    // wrap), so the panel, hung above the Setup button, keeps its height and its controls stay
    // under the pointer. What changes in a row comes last; the theme's digits are tabular and the
    // rates keep their decimals, so a number never moves what follows it; the shares sit in cells
    // of their own, so a share gaining a digit moves none of the others.
    let Line = mod.widgets.FabLabelSmall{width: Fill max_lines: 1 text_overflow: TextOverflow.Ellipsis}
    let Share = mod.widgets.FabLabelSmall{width: 66 max_lines: 1 text_overflow: TextOverflow.Ellipsis}
    mod.widgets.ElecSetupStatus = View{
        width: Fill height: Fit flow: Down spacing: 5
        // the whole line when an edit's name cuts it
        run_tip := mod.widgets.FabTip{status_run := Line{}}
        status_speed := Line{}
        // what Fast's rate is held accurate to
        steps_tip := mod.widgets.FabTip{status_steps := Line{}}
        View{
            width: Fill height: Fit flow: Right
            share_solver := Share{}
            share_scope := Share{}
            share_emulator := Share{width: Fill}
        }
    }

    mod.widgets.ElecTransportBase = #(ElecTransport::register_widget(vm))
    mod.widgets.ElecTransport = set_type_default() do mod.widgets.ElecTransportBase {
        width: Fill height: Fit flow: Down
        measure: mod.draw.DrawText{text_style: theme.font_regular{font_size: theme.prop_font_size}}
        // the take's timeline along the top edge, directly above the transport (as in a DAW)
        cover := ElecOverview{track: true height: 12}
        controls := View{
            width: Fill height: Fit flow: Right spacing: 6
            padding: Inset{left: 10 right: 10 top: 6 bottom: 7}
            align: Align{y: 0.5} show_bg: true
            draw_bg +: {color: fab.color_editor}
            well := Well{
                height: 30 width: 241 padding: 3 spacing: 0 draw_bg +: {radius: fab.radius_lg}
                strip := TimeStrip{
                    width: 235 height: 24 transport: true
                    face_selected: theme.prop_choice_on face_hover: fab.color_button_hover
                    ink_normal: fab.color_text_active ink_disabled: fab.color_text_muted
                    ink_play: fab.color_ok ink_pause: fab.color_warning ink_record: fab.color_error
                    draw_face +: {
                        pixel: fn() {
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, fab.radius)
                            sdf.fill(self.color)
                            return sdf.result
                        }
                    }
                }
            }
            overview := ElecOverview{min_width: 200 margin: Inset{left: 8}}
            // no circuit runs (none on the plate, or the run has not started): said in the
            // timeline's place, the controls kept where they are
            idle := Line{visible: false width: Fill margin: Inset{left: 8} text: "Nothing is simulating: no circuit on this plate yet"}
            length_well := Well{
                padding: Inset{left: 7}
                length_name := mod.widgets.FabLabelSmall{text: "Recording"}
                length := ComboBox{
                    width: 75 height: 22 margin: 0 padding: Inset{left: 5 right: 16}
                    accepts_typed: true a11y: {label: "Recording length"}
                    input +: {draw_text +: {text_style: theme.font_code{font_size: fab.font_size_ui}}}
                    draw_bg +: {
                        border_size: 0.0 border_radius: fab.radius
                        color: theme.prop_field color_hover: theme.prop_field color_focus: theme.prop_field
                    }
                }
            }
            auto := mod.widgets.FabRerecordToggle{a11y: {label: "Auto re-record"}}
            // the take's lanes into the project's captures folder (`TraceAction::Save`)
            save_trace := mod.widgets.FabButton{height: 22 margin: Inset{left: 6} text: "Save trace"}
            // The field measures and ellipsizes its single line. Its menu fits the full names.
            View{
                width: 110 height: 22
                bench_choice := Field{visible: false width: Fill a11y: {label: "Bench"}}
            }
            // the face is the chosen line itself, one line, as wide as the widest line
            jack_face := View{
                width: 96 height: 22
                jack := Field{width: Fill a11y: {label: "Listen"} labels: ["♪ Nothing"]}
            }
            mute := mod.widgets.FabSegmentTab{
                independent: true width: 22 height: 22 padding: 0 text: "M" a11y: {label: "Mute"}
                draw_bg +: {color: #0000 color_hover: fab.color_button_hover color_active: fab.color_button_active}
                draw_text +: {color_active: #fff}
            }
            // the board restarted from power-on, in every mode and while it runs
            mod.widgets.FabTip{
                text: "Power cycle: restart the board from power-on"
                cycle := mod.widgets.FabPowerCycleButton{a11y: {label: "Power cycle"}}
            }
            setup := Popover{
                placement: TopEnd trigger: Manual panel_padding: 10 trap_focus: true
                draw_panel +: {
                    color: fab.color_popover border_color: fab.color_popover_border
                    border_color_2: fab.color_popover_border border_size: 1.0 border_radius: fab.radius_lg
                }
                open_btn := mod.widgets.FabButton{height: 22 text: "Setup ▾"}
                content := View{
                    width: 220 height: Fit flow: Down spacing: 5
                    SetupRow{Section{text: "SIMULATION"} mod.widgets.FabLabelSmall{text: "changing these re-records"}}
                    SetupRow{Caption{text: "Accuracy"} accuracy := Field{labels: ["Exact" "Fast" "Logic"] a11y: {label: "Accuracy"}}}
                    SetupRow{Caption{text: "Sim rate"} sim_rate := Field{a11y: {label: "Sim rate"}}}
                    SetupRow{Caption{text: "Recording rate"} rec_rate := Field{a11y: {label: "Recording rate"}}}
                    power := mod.widgets.FabButton{text: "Power cycle"}
                    Section{text: "PLAYBACK"}
                    SetupRow{Caption{text: "Playback speed"} speed := Field{a11y: {label: "Playback speed"}}}
                    status := mod.widgets.ElecSetupStatus{}
                }
            }
        }
    }
}

/// The listen face's room past its line: the field's left padding and its chevron's.
const JACK_PAD: f64 = 34.0;

/// A choice made in the recording bar, for the shell's per-project preferences.
#[derive(Clone, Debug)]
pub struct BenchChosen {
    pub project: String,
    pub name: Arc<str>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ElecTransport {
    #[deref] view: View,
    #[rust] strip_span: Option<(f64, f64)>,
    #[rust] last_heard: Option<(f64, bool)>,
    #[rust] last_status: Option<LiveStatus>,
    #[rust] shown_settings: Option<(SimMode, Option<f64>, f64, Option<f64>, f64)>,
    #[rust] bench_stop: Option<f64>,
    #[rust] jack_board: Option<Weak<DesignSnapshot>>,
    #[rust] bench_rows: Option<Arc<Vec<fab_elec_lang::BenchView>>>,
    #[rust] bench_choice: Option<Arc<str>>,
    #[rust] jacks: Vec<(CompRef, String)>,
    /// The attached device's speakers, listed after the jacks.
    #[rust] speakers: Vec<(CompRef, String)>,
    /// Measures the listen lines in the field's style (`theme.prop_font_size`).
    #[live] measure: DrawText,
    /// The listen lines' widest, points, once measured (None: measure on the next draw).
    #[rust] jack_w: Option<f64>,
    #[rust] jack_lines: Vec<String>,
    #[rust] tap_generation: Option<u64>,
    #[rust] shown_length: Option<(Keeps, Option<f64>, Option<f64>)>,
    #[rust] length_user: Option<f64>,
    #[rust] rate: Option<f64>,
    #[rust] host_rate: Option<f64>,
    #[rust] lines: StatusLines,
    #[rust] last_edit: Option<Arc<str>>,
    /// Hold and Pause publish the same resting host state.
    #[rust] paused: bool,
}

/// The run row of the Setup status: the wait before playback, or the progress of the run. True when it
/// names an edit, whose name may be longer than the row (the row's tip then holds the line).
pub fn write_take(text: &mut String, status: &LiveStatus, edit: Option<&str>) -> bool {
    if let Some(wait) = status.preparing {
        text.push_str("Preparing");
        if wait.first {
            text.push_str(" · first play, a second or two");
        }
        if wait.wall >= 0.1 {
            let _ = write!(text, " · {:.1} s", wait.wall);
        }
        return false;
    }
    if status.continuous {
        let _ = write!(text, "continuous · {}", format_time(status.t));
        return false;
    }
    let Some(take) = status.take else { return false };
    let end = format_time(take);
    // a run a rounding short of the end (its last step on the sample grid) is at it
    if take - status.t < 1e-9 {
        let _ = write!(text, "take {end}, looping");
        return false;
    }
    let edit = status.rerender.zip(edit);
    text.push_str(if edit.is_some() { "re-rendering · " } else { "rendering · " });
    // the front without its unit when the end has the same one: `1.45 / 2.00 s`
    let front = format_time(status.t);
    let unit = end.rsplit(' ').next().unwrap_or("");
    let bare = front.strip_suffix(unit).filter(|f| f.ends_with(' ')).map_or(front.as_str(), |f| f.trim_end());
    let _ = write!(text, "{bare} / {end}");
    // the edit after the progress: a long name is what the ellipsis cuts
    if let Some((from, name)) = edit {
        let _ = write!(text, " · {name} · from {}", format_time(from));
    }
    edit.is_some()
}

/// The speed row: the measured speed and what holds the run back, or `steady`. The cause comes
/// last, so one that comes and goes moves nothing before it.
pub fn write_speed(text: &mut String, status: &LiveStatus) {
    if status.preparing.is_some() {
        return;
    }
    if status.steady {
        text.push_str("steady");
        return;
    }
    let Some(rate) = status.rate_achieved.filter(|rate| rate.is_finite() && *rate > 0.0) else { return };
    text.push_str(&format_reached(rate));
    text.push_str("× real time");
    let limit = match status.limited_by {
        _ if !status.playing && !status.stepping => None,
        Some(LiveLimit::Solver) => Some("solver"),
        Some(LiveLimit::Scope) => Some("scope"),
        Some(LiveLimit::Emulator) => Some("emulator"),
        Some(LiveLimit::Events) => Some("events"),
        Some(LiveLimit::Input) => Some("input"),
        Some(LiveLimit::Paced) | None => None,
    };
    if let Some(limit) = limit {
        text.push_str(" · ");
        text.push_str(limit);
    }
}

/// The steps row: the mode and the rate Fast and Logic step on (else the resolution they step
/// at), and Logic's timing cells (Exact: none). Its tip, with a rate, says what the rate is held
/// accurate to: half the rate its takes leave at, at most half its own (`PartOpts::band`).
pub fn write_steps(text: &mut String, tip: &mut String, status: &LiveStatus) {
    if status.mode != SimMode::Exact && status.sample_rate > 0.0 {
        let rate = format_sample_rate(status.sample_rate);
        let _ = write!(text, "{} · {rate}", if status.mode == SimMode::Logic { "Logic" } else { "Fast" });
        let band = format_sample_rate(status.sample_rate.min(status.recording_rate) / 2.0);
        if status.recording_rate < status.sample_rate {
            let _ = write!(tip, "Simulates at {rate} and is held accurate across the audible band, up to {band}. Exact simulates every step.");
        } else {
            let _ = write!(tip, "Simulates at {rate} and is held accurate up to {band}, half its rate. Exact simulates every step.");
        }
    } else if status.mode != SimMode::Exact && status.res > 0.0 {
        text.push_str("res ");
        text.push_str(&format_time(status.res));
    }
    if status.mode == SimMode::Logic {
        if !text.is_empty() {
            text.push_str(" · ");
        }
        let _ = write!(text, "{} timing cell{}", status.cells, if status.cells == 1 { "" } else { "s" });
    }
}

/// The Accuracy picker's modes, in its order.
const MODES: [SimMode; 3] = [SimMode::Exact, SimMode::Fast, SimMode::Logic];

/// The shares row: the running solver's, scopes' and emulator's shares of the stepping time in
/// whole percent, one per cell; all empty at rest and before the run measured them.
pub fn write_shares(cells: &mut [String; 3], status: &LiveStatus) {
    let Some(split) = status.split.filter(|_| status.playing || status.stepping) else { return };
    for ((cell, name), share) in cells.iter_mut().zip(["solver", "scope", "emulator"]).zip([split.solver, split.scope, split.emulator]) {
        let _ = write!(cell, "{name} {} %", (f64::from(share) * 100.0).round());
    }
}

/// The Setup status's rows as they read (`ElecSetupStatus`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusLines {
    pub run: String,
    /// What the first play prepares, or the whole run row when it names an edit.
    pub run_tip: String,
    pub speed: String,
    pub steps: String,
    /// The steps row's tip: what a rate is held accurate to (empty: no tip).
    pub steps_tip: String,
    pub shares: [String; 3],
}

impl StatusLines {
    pub fn write(&mut self, status: &LiveStatus, edit: Option<&str>) {
        for line in [&mut self.run, &mut self.run_tip, &mut self.speed, &mut self.steps, &mut self.steps_tip].into_iter().chain(&mut self.shares) {
            line.clear();
        }
        if write_take(&mut self.run, status, edit) {
            self.run_tip.push_str(&self.run);
        } else if status.preparing.is_some() {
            self.run_tip.push_str("The first play works out how to share the circuit across your CPU cores. Playing it again unchanged reuses that.");
        }
        write_speed(&mut self.speed, status);
        write_steps(&mut self.steps, &mut self.steps_tip, status);
        write_shares(&mut self.shares, status);
    }

    /// Shows the rows in `status`, an `ElecSetupStatus`.
    pub fn show(&self, cx: &mut Cx, status: &WidgetRef) {
        status.label(cx, ids!(status_run)).set_text(cx, &self.run);
        status.tip(cx, ids!(run_tip)).set_text(&self.run_tip);
        status.label(cx, ids!(status_speed)).set_text(cx, &self.speed);
        status.label(cx, ids!(status_steps)).set_text(cx, &self.steps);
        status.tip(cx, ids!(steps_tip)).set_text(&self.steps_tip);
        for (id, text) in [ids!(share_solver), ids!(share_scope), ids!(share_emulator)].into_iter().zip(&self.shares) {
            status.label(cx, id).set_text(cx, text);
        }
    }
}

/// The recording length's presets, seconds.
pub const LENGTHS: [f64; 7] = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 30.0];
/// The lengths a person may type, seconds.
pub const LENGTH_RANGE: (f64, f64) = (0.1, 60.0);
/// The ring's presets, seconds: how much of an open run Continuous and Realtime keep (the host
/// starts at 10 s). A typed ring has no upper end: the record's budget thins old detail instead.
pub const RINGS: [f64; 6] = [2.0, 5.0, 10.0, 30.0, 60.0, 120.0];
/// What the length field says when a typed ring does not read.
pub const RING_HINT: &str = "Type seconds from 0.1: 10, 30 s, 500 ms";

/// What the length field sets, after the record button last used (v8.1): the take's length
/// (Loop), or the ring an open run keeps (Continuous, Realtime).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Keeps {
    Take,
    Ring,
    Live,
}

impl Keeps {
    fn of(status: &LiveStatus) -> Self {
        if status.realtime { Keeps::Live } else if status.continuous { Keeps::Ring } else { Keeps::Take }
    }

    /// The word before the field: `Recording 2.00 s`, `Keeps the last 10.00 s`, `Live · keeps 10.00 s`.
    fn word(self) -> &'static str {
        match self {
            Keeps::Take => "Recording",
            Keeps::Ring => "Keeps the last",
            Keeps::Live => "Live · keeps",
        }
    }
}
/// The project's run settings the transport shows (its `sim` statement's), copied each frame:
/// the statement's jack loads are the Bench's, so the copy holds no list.
#[derive(Clone, Copy, Default)]
struct Shown {
    mode: Option<SimMode>,
    sample_rate: Option<f64>,
    recording_rate: Option<f64>,
    recording_length: Option<f64>,
}

impl Shown {
    fn of(s: &SimSettings) -> Self {
        Self { mode: s.mode, sample_rate: s.sample_rate, recording_rate: s.recording_rate, recording_length: s.recording_length }
    }
}

/// What the length field says when a typed value does not read.
pub const LENGTH_HINT: &str = "Type seconds from 0.1 to 60: 2, 2.5 s, 500 ms";

/// A typed recording length in seconds: `2`, `2.5 s`, `1.5s`, `500 ms` (a bare number is
/// seconds), within [`LENGTH_RANGE`]; None when it does not read.
pub fn parse_length(text: &str) -> Option<f64> {
    parse_seconds(text).filter(|seconds| (LENGTH_RANGE.0..=LENGTH_RANGE.1).contains(seconds))
}

/// A typed ring in seconds, as [`parse_length`] reads it, from [`LENGTH_RANGE`]'s low end up.
pub fn parse_ring(text: &str) -> Option<f64> {
    parse_seconds(text).filter(|seconds| *seconds >= LENGTH_RANGE.0)
}

fn parse_seconds(text: &str) -> Option<f64> {
    let text = text.trim().to_ascii_lowercase();
    let (number, scale) = if let Some(n) = text.strip_suffix("ms") {
        (n, 1e-3)
    } else if let Some(n) = text.strip_suffix('s') {
        (n, 1.0)
    } else {
        (text.as_str(), 1.0)
    };
    let seconds = number.trim().parse::<f64>().ok()? * scale;
    seconds.is_finite().then_some(seconds)
}

/// A recording length as the field shows it: `2.00 s`.
pub fn length_text(seconds: f64) -> String {
    format!("{seconds:.2} s")
}

/// The length field's rows: the bench's own length first when it names one, then the presets.
fn length_labels(bench: Option<f64>) -> Vec<String> {
    bench.map(|s| format!("From the bench ({})", length_text(s))).into_iter()
        .chain(LENGTHS.iter().map(|s| format!("{s} s")))
        .collect()
}

/// Include the effective rate in the automatic row and preserve off-menu project values.
fn sample_field(field: DropDownRef, cx: &mut Cx, default: &str, chosen: Option<f64>, running: f64) {
    let first = if running > 0.0 && chosen.is_none() {
        format!("{default} ({})", format_sample_rate(running))
    } else { default.to_owned() };
    let mut labels = vec![first];
    labels.extend(SAMPLE_RATES.into_iter().map(format_sample_rate));
    let at = chosen.map_or(0, |hz| {
        SAMPLE_RATES.iter().position(|r| *r == hz).map_or_else(|| {
            labels.push(format_sample_rate(hz));
            labels.len() - 1
        }, |i| i + 1)
    });
    field.set_labels(cx, labels);
    field.set_selected_item(cx, at);
}

impl ElecTransport {
    fn strip(&self, cx: &Cx) -> WidgetRef {
        self.view.widget(cx, ids!(strip))
    }

    fn show_rate(&mut self, cx: &mut Cx, rate: f64) {
        self.rate = Some(rate);
        if let Some(mut strip) = self.strip(cx).borrow_mut::<TimeStrip>() { strip.set_rate(cx, rate); }
        let field = self.view.drop_down(cx, ids!(speed));
        let mut labels: Vec<_> = RATES.into_iter().map(format_rate).collect();
        let at = RATES.iter().position(|r| *r == rate).unwrap_or_else(|| {
            labels.push(format_rate(rate));
            labels.len() - 1
        });
        field.set_labels(cx, labels);
        field.set_selected_item(cx, at);
    }

    fn set_rate(&mut self, cx: &mut Cx, rate: f64) {
        self.show_rate(cx, rate);
        cx.action(SimAction::Cmd(SimCmd::SetRate(rate)));
    }

    fn sync(&mut self, cx: &mut Cx) {
        let live = cx.get_global_ref::<ElecLive>();
        let (record, t, moving, status, settings, edit) = live.map(|live| {
            let sim = &live.sim;
            (sim.record, sim.scrub.unwrap_or(sim.t), sim.heard.map_or(sim.playing, |(_, moving)| moving),
                sim.status.clone(), live.snap.docs.as_ref().map(|docs| Shown::of(&docs.sim)), sim.edit.clone())
        }).unwrap_or_default();
        // no circuit: no part in any unit (O(units))
        let idle = live.is_none_or(|live| live.snap.design.units.iter().all(|u| u.circuit.comps.is_empty()));
        // read back from the widgets, not kept here: a design reload puts their defaults back
        let overview = self.view.widget(cx, ids!(overview));
        if overview.visible() == idle {
            overview.set_visible(cx, !idle);
        }
        let line = self.view.widget(cx, ids!(idle));
        if line.visible() != idle {
            line.set_visible(cx, idle);
        }
        let reading = TakeReading::read(cx, self.paused);
        let strip = self.strip(cx);
        if let Some(mut strip) = strip.borrow_mut::<TimeStrip>() {
            strip.set_transport(cx, reading.state, reading.can_play, if status.auto_rerecord { 0 } else { status.waiting });
            if self.strip_span != Some(record) {
                strip.set_model(cx, StripModel { span: record, live: true, ..Default::default() });
                self.strip_span = Some(record);
            }
            if self.last_heard != Some((t, moving)) {
                strip.hear(cx, TimeCursor { t, playing: moving, rate: self.rate.unwrap_or(status.rate_asked), source: CursorSource::Sim, fw: None });
                self.last_heard = Some((t, moving));
            }
        }
        for id in [ids!(cover), ids!(overview)] {
            if let Some(mut overview) = self.view.widget(cx, id).borrow_mut::<ElecOverview>() {
                overview.show(cx, reading.clone());
            }
        }
        let setup = self.view.popover(cx, ids!(setup));
        if setup.is_open() {
            let edit_changed = self.last_edit.as_ref().map(Arc::as_ptr) != edit.as_ref().map(Arc::as_ptr);
            if self.last_status.as_ref() != Some(&status) || edit_changed {
                self.last_status = Some(status.clone());
                self.last_edit = edit;
                self.lines.write(&status, self.last_edit.as_deref());
                self.lines.show(cx, &self.view.widget(cx, ids!(status)));
            }
        } else { self.last_status = None; }

        if self.host_rate != Some(status.rate_asked) {
            self.host_rate = Some(status.rate_asked);
            self.show_rate(cx, status.rate_asked);
        }
        let (mode, sample_rate, rec_rate) = settings.map_or(
            (status.mode, status.sample_rate_chosen, status.recording_rate_chosen),
            |s| (s.mode.unwrap_or_default(), s.sample_rate, s.recording_rate));
        let shown = (mode, sample_rate, status.sample_rate, rec_rate, status.recording_rate);
        if self.shown_settings != Some(shown) {
            self.view.drop_down(cx, ids!(accuracy)).set_selected_item(cx, MODES.iter().position(|&m| m == mode).unwrap_or(1));
            sample_field(self.view.drop_down(cx, ids!(sim_rate)), cx, "Auto", sample_rate, status.sample_rate);
            sample_field(self.view.drop_down(cx, ids!(rec_rate)), cx, "Default", rec_rate, status.recording_rate);
            self.shown_settings = Some(shown);
        }
        self.sync_listen(cx);
        // the length field, after the record button last used: in Loop the person's length, else
        // the bench's, else the run's; in Continuous and Realtime the ring the run keeps
        let bench_stop = self.bench_stop;
        let user = settings.map_or(self.length_user, |settings| settings.recording_length);
        let ring = (status.ring > 0.0).then_some(status.ring);
        let keeps = Keeps::of(&status);
        let length = match keeps {
            Keeps::Take => (keeps, bench_stop, user.or(bench_stop).or(status.take).or(ring)),
            Keeps::Ring | Keeps::Live => (keeps, None, ring),
        };
        if self.shown_length != Some(length) {
            let field = self.view.widget(cx, ids!(length)).as_combo_box();
            field.set_labels(cx, match keeps {
                Keeps::Take => length_labels(bench_stop),
                Keeps::Ring | Keeps::Live => RINGS.iter().map(|s| format!("{s} s")).collect(),
            });
            field.set_text(cx, &length.2.map_or_else(|| "–".to_string(), length_text));
            if self.shown_length.is_none_or(|(was, ..)| was != keeps) {
                self.view.label(cx, ids!(length_name)).set_text(cx, keeps.word());
            }
            self.shown_length = Some(length);
        }
    }

    fn sync_listen(&mut self, cx: &mut Cx) {
        let field = self.view.drop_down(cx, ids!(jack));
        let live = cx.get_global_ref::<ElecLive>();
        let board = live.map(|live| Arc::downgrade(&live.snap));
        let choice = live.and_then(|live| live.chosen_bench.as_ref());
        if self.jack_board.as_ref().map(Weak::as_ptr) != board.as_ref().map(Weak::as_ptr) || self.bench_choice.as_ref() != choice {
            self.bench_choice = choice.cloned();
            self.bench_stop = live.and_then(ElecLive::run_bench).and_then(|b| b.out.stop);
            let benches = live.map(|live| live.snap.benches.clone());
            let list = self.view.drop_down(cx, ids!(bench_choice));
            if self.bench_rows.as_ref().map(Arc::as_ptr) != benches.as_ref().map(Arc::as_ptr) {
                let labels: Vec<String> = benches.as_ref().map_or_else(Vec::new, |rows| rows.iter().map(|b| b.out.name.clone()).collect());
                self.view.widget(cx, ids!(bench_choice)).set_visible(cx, !labels.is_empty());
                list.set_labels(cx, labels);
                self.bench_rows = benches;
            }
            if let Some(rows) = &self.bench_rows {
                list.set_selected_item(cx, crate::snapshot::bench_index(rows, self.bench_choice.as_deref()).unwrap_or(0));
            }
            let jacks = running_jacks(cx);
            for (jack, _) in &self.jacks {
                if listening_jack(cx, *jack) && !jacks.iter().any(|(comp, _)| comp == jack) {
                    listen_jack(cx, *jack, false);
                }
            }
            self.jacks = jacks;
            self.jack_board = board;
            self.tap_generation = None;
        }
        let speakers = listen::running_speakers(cx);
        if self.speakers[..] != *speakers {
            self.speakers = speakers.to_vec();
            self.tap_generation = None;
        }
        let audio = cx.get_global_ref::<ElecAudio>();
        let generation = audio.map_or(0, |a| a.tap().2);
        if self.tap_generation != Some(generation) {
            let heard = self.jacks.iter().position(|(jack, _)| listening_jack(cx, *jack))
                .or_else(|| self.speakers.iter().position(|(speaker, _)| listen::listening_speaker(cx, *speaker)).map(|i| self.jacks.len() + i));
            let mix = audio.is_some_and(|a| a.tap().0.iter().any(|net| listen::listening(cx, net)));
            // ♪ and the jack, "(mix)" on the heard one while Scope lanes are heard too
            let lines: Vec<String> = std::iter::once("♪ Nothing".to_owned()).chain(self.jacks.iter().enumerate().map(|(i, (_, name))| {
                format!("♪ {name}{}", if mix && heard == Some(i) { " (mix)" } else { "" })
            })).chain(self.speakers.iter().map(|(_, name)| format!("♪ {name} speaker"))).collect();
            if lines != self.jack_lines {
                field.set_labels(cx, lines.clone());
                self.jack_lines = lines;
                self.jack_w = None;
                self.view.redraw(cx);
            }
            field.set_selected_item(cx, heard.map_or(0, |i| i + 1));
            self.tap_generation = Some(generation);
        }
        let mute = self.view.radio_button(cx, ids!(mute));
        let on = listen::muted(cx);
        if mute.active(cx) != on { mute.set_active(cx, on, Animate::No); }
        let auto = self.view.radio_button(cx, ids!(auto));
        let on = cx.get_global_ref::<ElecLive>().is_none_or(|live| live.sim.status.auto_rerecord);
        if auto.active(cx) != on { auto.set_active(cx, on, Animate::No); }
    }

    /// Set the project or standalone take length. None restores the bench length.
    fn set_length(&mut self, cx: &mut Cx, seconds: Option<f64>) {
        if !write_run_settings(cx, |settings| settings.recording_length = seconds) {
            self.length_user = seconds;
            cx.action(SimAction::Cmd(SimCmd::SetTakeLen(seconds)));
        }
    }

    fn actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if actions.is_empty() { return; }
        if let (Some(at), Some(rows)) = (self.view.drop_down(cx, ids!(bench_choice)).selected(actions), &self.bench_rows) {
            let live = cx.get_global::<ElecLive>();
            if let Some(row) = rows.get(at).filter(|_| Arc::ptr_eq(rows, &live.snap.benches)) {
                let name: Arc<str> = Arc::from(row.out.name.as_str());
                live.chosen_bench = Some(name.clone());
                let project = live.title.clone();
                cx.action(BenchChosen { project, name });
                cx.redraw_all();
                cx.new_next_frame();
            }
        }
        if self.view.button(cx, ids!(open_btn)).clicked(actions) {
            self.view.popover(cx, ids!(setup)).toggle(cx);
        }
        let field = self.view.widget(cx, ids!(length)).as_combo_box();
        let keeps = self.shown_length.map_or(Keeps::Take, |(keeps, ..)| keeps);
        if keeps != Keeps::Take {
            // the ring an open run keeps: one command, the host keeps it across runs
            if let Some(&seconds) = field.selected(actions).and_then(|at| RINGS.get(at)) {
                cx.action(SimAction::Cmd(SimCmd::SetRing(seconds)));
                self.shown_length = None;
            }
            if let Some(text) = field.typed(actions) {
                match parse_ring(&text) {
                    Some(seconds) => {
                        field.set_text(cx, &length_text(seconds));
                        cx.action(SimAction::Cmd(SimCmd::SetRing(seconds)));
                    }
                    None => field.set_error(cx, Some(RING_HINT)),
                }
            }
        } else if let Some(at) = field.selected(actions) {
            let bench = self.shown_length.and_then(|(_, bench, _)| bench);
            let picked = match (bench, at) {
                (Some(_), 0) => Some(None),
                (Some(_), at) => LENGTHS.get(at - 1).map(|&s| Some(s)),
                (None, at) => LENGTHS.get(at).map(|&s| Some(s)),
            };
            if let Some(seconds) = picked {
                self.set_length(cx, seconds);
            }
            // the field shows the length in effect, not the picked row's words
            self.shown_length = None;
        }
        if let Some(text) = field.typed(actions).filter(|_| keeps == Keeps::Take) {
            match parse_length(&text) {
                Some(seconds) => {
                    field.set_text(cx, &length_text(seconds));
                    self.set_length(cx, Some(seconds));
                }
                None => field.set_error(cx, Some(LENGTH_HINT)),
            }
        }
        if let Some(i) = self.view.drop_down(cx, ids!(accuracy)).selected(actions) {
            let mode = MODES.get(i).copied().unwrap_or_default();
            if !write_run_settings(cx, |s| s.mode = Some(mode)) { cx.action(SimAction::Cmd(SimCmd::SetSimMode(mode))); }
        }
        let picked = |at: usize, chosen: Option<f64>| at.checked_sub(1).and_then(|i| SAMPLE_RATES.get(i).copied().or(chosen));
        if let Some(i) = self.view.drop_down(cx, ids!(sim_rate)).selected(actions) {
            let rate = picked(i, self.shown_settings.and_then(|s| s.1));
            if !write_run_settings(cx, |s| s.sample_rate = rate) { cx.action(SimAction::Cmd(SimCmd::SetSampleRate(rate))); }
        }
        if let Some(i) = self.view.drop_down(cx, ids!(rec_rate)).selected(actions) {
            let rate = picked(i, self.shown_settings.and_then(|s| s.3));
            if !write_run_settings(cx, |s| s.recording_rate = rate) { cx.action(SimAction::Cmd(SimCmd::SetRecordingRate(rate))); }
        }
        if let Some(rate) = self.view.drop_down(cx, ids!(speed)).selected(actions).and_then(|i| RATES.get(i)) {
            self.set_rate(cx, *rate);
        }
        if self.view.button(cx, ids!(power)).clicked(actions) || self.view.button(cx, ids!(cycle)).clicked(actions) {
            self.restart(cx);
        }
        if self.view.button(cx, ids!(save_trace)).clicked(actions) {
            cx.action(TraceAction::Save);
        }
        if let Some(at) = self.view.drop_down(cx, ids!(jack)).selected(actions) {
            for (i, (jack, _)) in self.jacks.iter().enumerate() {
                let on = at == i + 1;
                if listening_jack(cx, *jack) != on { listen_jack(cx, *jack, on); }
            }
            for (i, (speaker, _)) in self.speakers.iter().enumerate() {
                let on = at == self.jacks.len() + i + 1;
                if listen::listening_speaker(cx, *speaker) != on { listen::listen_speaker(cx, *speaker, on); }
            }
        }
        let mute = self.view.radio_button(cx, ids!(mute));
        if mute.clicked(actions) {
            ElecAudio::get(cx);
            let on = !listen::muted(cx);
            listen::set_muted(cx, on);
        }
        // Auto re-record: off, edits wait for the next record; turned on again, the changes that
        // waited are recorded at once
        if self.view.radio_button(cx, ids!(auto)).clicked(actions) {
            let host = cx.get_global_ref::<ElecLive>().map(|live| live.sim.status.clone()).unwrap_or_default();
            let on = !host.auto_rerecord;
            cx.action(SimAction::Cmd(SimCmd::SetAutoRerecord(on)));
            if on && host.waiting > 0 { self.restart(cx); }
        }

        let strip = self.strip(cx);
        for action in actions.filter_widget_actions_cast::<TimeStripAction>(strip.widget_uid()) {
            if let TimeStripAction::Start = action {
                let start = self.strip_span.unwrap_or_default().0;
                listen::seek_audio(cx, start);
                let moving = self.last_heard.is_some_and(|h| h.1);
                cx.action(ViewAction::Cursor(TimeCursor { t: start, playing: moving, rate: self.rate.unwrap_or(1.0), source: CursorSource::Sim, fw: None }));
                if let Some((a, b)) = cx.get_global_ref::<super::overview::ScopeWindowShown>().and_then(|w| w.0).filter(|(a, _)| *a > start) {
                    cx.action(super::overview::PanScopeWindow(start, start + b - a));
                }
            }
            if let TimeStripAction::Record(mode) = action {
                record(cx, mode);
            }
            if let TimeStripAction::Stop = action {
                self.paused = false;
                cx.action(SimAction::Cmd(SimCmd::Hold));
            }
            if let TimeStripAction::Cursor(cursor) = action {
                self.last_heard = None;
                self.paused = !cursor.playing;
                if cursor.playing {
                    let from = play_from(cx).unwrap_or(cursor.t);
                    listen::seek_audio(cx, from);
                    cx.action(ViewAction::Cursor(TimeCursor { t: from, ..cursor }));
                    // a whole take plays round; a take held before its end goes on from there
                    cx.action(SimAction::Cmd(SimCmd::Play));
                } else {
                    listen::seek_audio(cx, cursor.t);
                    cx.action(ViewAction::Cursor(cursor));
                    cx.action(SimAction::Cmd(SimCmd::Pause));
                }
            }
        }
    }

    fn restart(&mut self, cx: &mut Cx) {
        self.paused = false;
        cx.action(SimAction::Cmd(SimCmd::Stop));
        cx.action(SimAction::Cmd(SimCmd::Play));
    }
}

/// Records in `mode` from the start, as the bar's record buttons do (and R, from the bar or a
/// focused Scope): the mode's commands, then Stop and Play (the transport clears its pause on
/// seeing them).
pub fn record(cx: &mut Cx, mode: RunMode) {
    let cmds: &[SimCmd] = match mode {
        RunMode::Loop => &[SimCmd::SetContinuous(false)],
        RunMode::Continuous => &[SimCmd::SetRealtime(false), SimCmd::SetContinuous(true)],
        RunMode::Realtime => &[SimCmd::SetRealtime(true)],
    };
    for cmd in cmds.iter().chain(&[SimCmd::Stop, SimCmd::Play]) {
        cx.action(SimAction::Cmd(cmd.clone()));
    }
}

/// Where Play starts the sound: the instant a review holds the line at, else, when the wave view
/// shows a stretch without the held head (the person scrolled there), its left edge; None goes on
/// from where it is heard.
fn play_from(cx: &Cx) -> Option<f64> {
    let sim = &cx.get_global_ref::<ElecLive>()?.sim;
    if let Some(t) = sim.scrub {
        return Some(t);
    }
    let (w0, w1) = cx.get_global_ref::<super::overview::ScopeWindowShown>()?.0?;
    let head = sim.heard.map(|(t, _)| t)?;
    (!(w0..w1).contains(&head)).then_some(w0)
}

impl Widget for ElecTransport {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.view.visible() { return; }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            for action in actions {
                match action.downcast_ref::<SimAction>() {
                    Some(SimAction::Cmd(SimCmd::Pause)) => self.paused = true,
                    Some(SimAction::Cmd(SimCmd::Hold | SimCmd::Stop | SimCmd::Play)) => self.paused = false,
                    _ => {}
                }
            }
            self.actions(cx, actions);
        }
        if matches!(event, Event::Signal | Event::NextFrame(_) | Event::Actions(_)) {
            self.sync(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.view.visible() {
            return self.view.draw_walk(cx, scope, walk);
        }
        self.sync(cx);
        // A wrapping turtle consumes Fill immediately. Use deferred Fill on a full row so the
        // overview gets the space left by the length, listen and Setup controls.
        // the listen face as wide as its widest line, up to a width past which the line ellipsizes
        if self.jack_w.is_none() {
            let widest = self.jack_lines.iter().map(|line| {
                let text = self.measure.layout(cx, 0.0, 0.0, None, false, Align::default(), line);
                f64::from(text.size_in_lpxs.width * self.measure.font_scale)
            }).fold(0.0, f64::max);
            let w = (widest + JACK_PAD).clamp(96.0, 200.0);
            self.jack_w = Some(w);
            if let Some(mut face) = self.view.widget(cx, ids!(jack_face)).borrow_mut::<View>() {
                face.walk.width = Size::Fixed(w);
            }
        }
        let room = cx.peek_walk_turtle(walk).size.x;
        if let Some(mut row) = self.view.widget(cx, ids!(controls)).borrow_mut::<View>() {
            // Padding, gaps, the seven-button well, readable coverage and the trailing controls.
            let single_row = 20.0 + 6.0 * 8.0 + 241.0 + 208.0 + 138.0 + 22.0 + 110.0 + self.jack_w.unwrap_or(96.0) + 22.0 + 22.0 + 60.0;
            row.layout.flow = if room < single_row { Flow::right_wrap() } else { Flow::right() };
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::sim::{LiveSplit, Preparing};

    /// The transport explains the wait on first and later plays, then shows the reached rate
    /// once, alongside the run's progress, accuracy and measured shares.
    #[test]
    fn transport_reads_out_the_reached_rate_once_in_human_terms() {
        let cursor = super::super::TimeCursorView::heard(TimeCursor {
            t: 0.25, playing: true, rate: f64::INFINITY, source: CursorSource::Sim, fw: None,
        }, 0.0);
        for now in [0.0, 1.0] {
            assert_eq!(cursor.at(now), 0.25, "max follows publications without infinite cursor arithmetic at {now}");
        }
        let lines = |status: LiveStatus, edit: Option<&str>| { let mut lines = StatusLines::default(); lines.write(&status, edit); lines };
        let speed = |status: LiveStatus| lines(status, None).speed;
        let running = LiveStatus { playing: true, rate_achieved: Some(0.7142), limited_by: Some(LiveLimit::Solver), ..LiveStatus::default() };
        for (first, wall, row) in [
            (true, 0.099, "Preparing · first play, a second or two"),
            (true, 0.64, "Preparing · first play, a second or two · 0.6 s"),
            (false, 0.0, "Preparing"),
            (false, 0.1, "Preparing · 0.1 s"),
        ] {
            let wait = lines(LiveStatus { preparing: Some(Preparing { first, wall }), continuous: true, ..running.clone() }, None);
            assert_eq!(wait.run, row, "the preparing row describes this play and its elapsed wait (first={first}, wall={wall})");
            assert_eq!(wait.run_tip, "The first play works out how to share the circuit across your CPU cores. Playing it again unchanged reuses that.",
                "the preparing tip explains the first play and that only an unchanged replay reuses it");
            assert!(wait.speed.is_empty(), "the speed row stays empty while preparing, even with a measured rate");
        }
        assert_eq!(speed(running.clone()), "0.71× real time · solver", "two decimals about real time, and what holds the run back last");
        assert_eq!(speed(LiveStatus { rate_achieved: Some(0.01234), limited_by: Some(LiveLimit::Paced), ..running.clone() }), "0.0123× real time",
            "a paced run keeps up with what was asked: nothing holds it back");
        assert_eq!(speed(LiveStatus { rate_achieved: Some(23.44), ..running.clone() }), "23.4× real time · solver", "one decimal above ten");
        assert_eq!(speed(LiveStatus { rate_achieved: Some(9.996), ..running.clone() }), "10.0× real time · solver", "rounding up to ten keeps three digits");
        assert_eq!(speed(LiveStatus { rate_achieved: Some(2.5), limited_by: Some(LiveLimit::Scope), ..running.clone() }), "2.50× real time · scope",
            "the scopes' acquisition holds the run back");
        assert_eq!(speed(LiveStatus { rate_achieved: Some(0.42), limited_by: Some(LiveLimit::Input), ..running.clone() }), "0.42× real time · input",
            "a streamed input (a motion track) holds the run back");
        assert_eq!(speed(LiveStatus { playing: false, ..running.clone() }), "0.71× real time", "paused, the last reached rate without a cause");
        assert_eq!(speed(LiveStatus { steady: true, ..running.clone() }), "steady", "a settled run shows steady in place of a rate");
        assert_eq!(lines(LiveStatus::default(), None), StatusLines::default(), "nothing before the run: every row empty, never a dash or a zero");
        // a rate about real time keeps its width, so nothing after it moves (the digits are tabular)
        let widths: Vec<usize> = [0.994, 0.999, 1.0, 1.01].map(|rate| speed(LiveStatus { rate_achieved: Some(rate), ..running.clone() }).chars().count()).to_vec();
        assert!(widths.iter().all(|w| *w == widths[0]), "the speed row keeps its length either side of real time: {widths:?}");

        let run = |status: LiveStatus| lines(status, None).run;
        let take = LiveStatus { take: Some(2.0), t: 0.3, ..LiveStatus::default() };
        assert_eq!(run(take.clone()), "rendering · 300 ms / 2.00 s", "the first render and how far it is");
        assert_eq!(run(LiveStatus { continuous: true, t: 3.25, ..LiveStatus::default() }), "continuous · 3.25 s", "Continuous shows elapsed simulation time");
        assert_eq!(run(LiveStatus { t: 1.45, take: Some(2.0), ..running.clone() }), "rendering · 1.45 / 2.00 s",
            "one unit for the front and the end when they share it");
        let edited = lines(LiveStatus { t: 1.45, rerender: Some(1.2), ..take.clone() }, Some("R7 = 22k"));
        assert_eq!(edited.run, "re-rendering · 1.45 / 2.00 s · R7 = 22k · from 1.20 s",
            "an edit heard names itself and where the take renders again from, after the progress a long name must not cut");
        assert_eq!(edited.run_tip, edited.run, "a row naming an edit holds the whole line in its tip");
        assert_eq!(lines(take.clone(), None).run_tip, "", "a row with no edit has no tip");
        assert_eq!(run(LiveStatus { t: 2.0, ..take.clone() }), "take 2.00 s, looping", "a whole take says its length and that it loops");

        let steps = |status: LiveStatus| lines(status, None).steps;
        let fast = LiveStatus { mode: SimMode::Fast, res: 37.5e-6, ..running.clone() };
        assert_eq!(steps(fast.clone()), "res 37.5 µs", "Fast reads out the resolution it steps at, before and after a measured rate");
        assert_eq!(steps(LiveStatus { mode: SimMode::Exact, ..fast.clone() }), "", "Exact reads out no resolution");
        assert_eq!(steps(LiveStatus { mode: SimMode::Logic, cells: 8, ..fast.clone() }), "res 37.5 µs · 8 timing cells",
            "Logic reads out its resolution and its timing cells");
        assert_eq!(lines(fast.clone(), None).steps_tip, "", "a run with no rate has no steps tip");
        // a heard run reads out its mode and rate; the tip says what the rate is held accurate to
        let heard = LiveStatus { sample_rate: 60_000.0, recording_rate: 48_000.0, ..fast.clone() };
        let rated = lines(heard.clone(), None);
        assert_eq!(rated.steps, "Fast · 60 kHz", "Fast with a rate reads out the mode and the rate, never \"held\"");
        assert_eq!(rated.steps_tip, "Simulates at 60 kHz and is held accurate across the audible band, up to 24 kHz. Exact simulates every step.",
            "takes under the run's rate: held to half the take's rate");
        assert_eq!(lines(LiveStatus { recording_rate: 96_000.0, sample_rate: 96_000.0, ..heard.clone() }, None).steps_tip,
            "Simulates at 96 kHz and is held accurate up to 48 kHz, half its rate. Exact simulates every step.", "takes at the run's rate or over: held to half the run's");
        assert_eq!(steps(LiveStatus { mode: SimMode::Logic, cells: 8, ..heard.clone() }), "Logic · 60 kHz · 8 timing cells", "Logic with a rate, then its cells");
        let exact = lines(LiveStatus { mode: SimMode::Exact, ..heard }, None);
        assert_eq!((exact.steps.as_str(), exact.steps_tip.as_str()), ("", ""), "Exact reads out no rate and has no steps tip");

        let shares = LiveSplit { solver: 0.124, scope: 0.836, emulator: 0.04 };
        assert_eq!(lines(LiveStatus { split: Some(shares), limited_by: Some(LiveLimit::Scope), ..running.clone() }, None).shares,
            ["solver 12 %", "scope 84 %", "emulator 4 %"], "each part's share of the stepping time in whole percent, a cell each");
        assert_eq!(lines(LiveStatus { split: Some(shares), playing: false, ..running.clone() }, None).shares, <[String; 3]>::default(), "at rest no shares");
        assert_eq!(lines(running, None).shares, <[String; 3]>::default(), "no shares before the run measured them");
    }
}
