//! The print job as it goes: the package being made, the upload, the printer's own stages with
//! temperatures against targets, layer, percent and times, then how it ended. One way out at every
//! stage, answered on the frame: Cancel before the printer has it, Pause and Stop while it runs
//! (Stop asks first), Close when it is over. Monitor shows it at the top of the view, the Printer
//! panel elsewhere.
//!
//! Main-loop cost: formatting a few lines on a draw, once a second while the job runs.

use super::jobs::{CommittedJob, JobState};
use super::panel::temp_text;
use super::*;
use crate::api::AppState;
use fab_printer_link::{ConnectionStatus, JobStage};

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabJobCardBase = #(FabJobCard::register_widget(vm))
    mod.widgets.FabJobCard = set_type_default() do mod.widgets.FabJobCardBase{
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
        phase := mod.widgets.FabTitleLabel{ width: Fill text: "" }
        times := mod.widgets.FabTitleLabel{ width: Fill text: "" }
        bar := mod.widgets.FabProgress{ width: Fill height: 8 }
        detail := mod.widgets.FabLabelDim{ width: Fill text: "" }
        accepted := mod.widgets.FabLabelDim{ width: Fill text: "" visible: false }
        steps := mod.widgets.FabLabel{ width: Fill text: "" visible: false }
        heat := mod.widgets.FabLabelDim{ width: Fill text: "" }
        note := mod.widgets.FabLabel{ width: Fill text: "" draw_text +: { color: fab.color_error } }
        actions := View{
            width: Fill height: Fit flow: Right spacing: 6
            cancel := mod.widgets.FabButton{ text: "Cancel" }
            FabTip{ text: "Pause or resume the print"
                pause := mod.widgets.FabButton{ text: "Pause" }
            }
            FabTip{ text: "Stop the print (asks first)"
                stop := mod.widgets.FabButton{ text: "Stop…" }
            }
            close := mod.widgets.FabButton{ text: "Close" }
            FabTip{ text: "Start this job on the printer (asks about the plate first)"
                start := mod.widgets.FabButtonAccent{ text: "Start…" }
            }
            FabTip{ text: "Take this job out of the queue"
                remove := mod.widgets.FabButton{ text: "Remove" }
            }
        }
        start_confirm := View{
            width: Fill height: Fit flow: Down spacing: 6 visible: false
            question := mod.widgets.FabLabel{ width: Fill text: "Is the plate clear? The job starts at once." }
            buttons := View{
                width: Fill height: Fit flow: Right spacing: 6
                go := mod.widgets.FabButtonAccent{ text: "Start print" }
                keep := mod.widgets.FabButton{ text: "Not yet" }
            }
        }
        stop_confirm := View{
            width: Fill height: Fit flow: Down spacing: 6 visible: false
            question := mod.widgets.FabLabel{ width: Fill text: "Stop the print? A stopped print cannot be resumed." }
            buttons := View{
                width: Fill height: Fit flow: Right spacing: 6
                go := mod.widgets.FabButtonAccent{ text: "Stop print" }
                keep := mod.widgets.FabButton{ text: "Keep printing" }
            }
        }
    }
}

/// The job in words, and the ways out it has now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobView {
    /// What is going on now.
    pub phase: String,
    /// `phase` says what went wrong.
    pub attention: bool,
    /// "Elapsed 12:03 · Left 31:00 · Finish about 14:32 · Planned 43:05".
    pub times: String,
    /// How far, for the bar: the upload, then the printer's percent.
    pub fraction: Option<f32>,
    /// "wavy_cube · Layer 12 / 100 · 37 %".
    pub detail: String,
    /// The printability errors it was started with, if the person accepted any.
    pub accepted: String,
    /// Where it stops for items to be placed into it, a line each ([`PreviewPause::step`]).
    pub steps: String,
    /// Bed, nozzles and chamber against their targets.
    pub heat: String,
    /// The connection when it is not up, printer errors, the last command's answer.
    pub note: String,
    pub cancel: bool,
    /// Pause offered (false) or Resume (true).
    pub pause: Option<bool>,
    pub stop: bool,
    pub close: bool,
    /// A queued job: Start (asks about the plate first) and Remove.
    pub start: bool,
    pub remove: bool,
    /// Still going: the times tick.
    pub running: bool,
}

/// What the printer says it is doing while it has a job.
fn printer_doing(s: &fab_printer_link::model::PrinterState) -> String {
    let gcode = s.gcode_state.as_deref().unwrap_or("");
    let stage = s.stage_name().filter(|_| s.stage.is_some_and(|st| st > 0));
    match gcode {
        "PAUSE" => stage.filter(|n| n.starts_with("Paused")).unwrap_or_else(|| "Paused".to_string()),
        _ if stage.is_some() => stage.unwrap_or_default(),
        "PREPARE" | "SLICING" | "INIT" => match s.prepare_percent.filter(|p| *p > 0.0 && *p < 100.0) {
            Some(p) => format!("The printer is reading the job: {p:.0} %"),
            None => "The printer is preparing".to_string(),
        },
        _ => "Printing".to_string(),
    }
}

/// What is still hot, in words: a stopped print leaves the bed and nozzles to cool.
fn still_hot(s: &fab_printer_link::model::PrinterState) -> String {
    const WARM: f64 = 45.0;
    let mut hot = Vec::new();
    if let Some(c) = s.bed.current.filter(|c| *c >= WARM) {
        hot.push(format!("bed {c:.0} °C"));
    }
    // Numbered heads by number; the first family's in the order it reports them, a head with
    // no known side being "nozzle".
    let heads: Vec<_> = if s.numbered_heads { s.heads() } else { s.extruders.iter().collect() };
    for e in heads {
        if let Some(c) = e.temperature.current.filter(|c| *c >= WARM) {
            let name = e.id.and_then(|id| s.head_name(id)).map_or_else(|| "nozzle".to_string(), |n| if s.numbered_heads { n } else { n.to_lowercase() });
            hot.push(format!("{name} {c:.0} °C"));
        }
    }
    if hot.is_empty() { String::new() } else { format!(" Still hot: {}.", hot.join(", ")) }
}

/// Bed, nozzles and chamber, each against its target.
fn heat_line(s: &fab_printer_link::model::PrinterState) -> String {
    let mut out = Vec::new();
    for e in s.heads() {
        if let Some(name) = e.id.and_then(|id| s.head_name(id)) {
            out.push(format!("{name} {}", temp_text(e.temperature)));
        }
    }
    if s.bed.current.is_some() {
        out.push(format!("Bed {}", temp_text(s.bed)));
    }
    if s.chamber.current.is_some() {
        out.push(format!("Chamber {}", temp_text(s.chamber)));
    }
    out.join("  ·  ")
}

/// The job shown at `now` (`Cx::time_now`): the one on its way or on the printer, whoever started
/// it, or how Fab's last one ended until its card is closed. None: nothing to show. A job on
/// the resin printer comes first.
pub fn job_view(p: &PrintState, now: f64) -> Option<JobView> {
    if let Some(job) = &p.resin_job {
        return Some(super::resin::job_view(job, now));
    }
    let shown = p.follow.shown();
    // A job picked in the list that isn't on the printer, and any job with no printer to ask.
    let Some(printer) = p.printer.as_ref().filter(|_| p.follow.picked.is_none() || p.follow.shown_printing().is_some()) else {
        return shown.map(|job| saved_view(job, now));
    };
    let s = &*printer.state;
    let tracked = printer.job.as_ref().filter(|_| shown.is_none_or(|j| printer.job_sends >= j.link_send));
    // The job this report is about: its own times, never another's.
    let sent = shown.filter(|j| p.sending.as_ref().is_none_or(|name| *name == j.meta.name));
    let mut v = JobView::default();
    let mut name = p.sending.clone();
    let printer_percent = |v: &mut JobView| {
        v.fraction = s.progress.map(|pc| (pc / 100.0).clamp(0.0, 1.0) as f32);
    };
    if p.sending.is_some() {
        v.phase = "Preparing the package for the printer".to_string();
        v.cancel = true;
        v.running = true;
    } else if let Some(why) = &p.print_blocked {
        v.phase = why.clone();
        v.attention = true;
        v.close = true;
    } else {
        match tracked.map(|j| &j.stage) {
            Some(JobStage::Uploading { sent, total }) => {
                let f = *sent as f64 / (*total).max(1) as f64;
                v.phase = format!("Uploading to the printer's USB drive: {:.0} %", 100.0 * f);
                v.fraction = Some(f as f32);
                v.cancel = true;
                v.running = true;
            }
            Some(JobStage::Starting) => {
                v.phase = "Starting: waiting for the printer to answer".to_string();
                v.stop = true;
                v.running = true;
            }
            Some(JobStage::Accepted) if !s.busy() => {
                v.phase = "The printer took the job: waiting for it to begin".to_string();
                v.stop = true;
                v.running = true;
            }
            Some(JobStage::Finished) if !s.busy() => {
                v.phase = "Finished".to_string();
                v.close = true;
                printer_percent(&mut v);
            }
            Some(JobStage::Failed(why)) if !s.busy() => {
                v.phase = format!("Failed: {why}.{}", still_hot(s));
                v.attention = true;
                v.close = true;
            }
            Some(JobStage::Cancelled) if !s.busy() => {
                v.phase = format!("Stopped.{}", still_hot(s));
                v.close = true;
            }
            _ if s.busy() => {
                v.phase = printer_doing(s);
                v.pause = Some(s.gcode_state.as_deref() == Some("PAUSE"));
                v.stop = true;
                v.running = true;
                printer_percent(&mut v);
            }
            _ => return shown.map(|job| saved_view(job, now)),
        }
        if p.follow.closed && !v.running {
            return None;
        }
        name = tracked.map(|j| j.name.clone()).or_else(|| s.job_name.clone());
    }
    let name = name.or_else(|| sent.map(|j| j.meta.name.clone()));

    let planned = sent.map(|j| j.meta.planned);
    let elapsed = sent.filter(|_| p.print_blocked.is_none()).map(|j| j.ended_at.unwrap_or(now) - j.sent_at);
    let mut times = Vec::new();
    if let Some(e) = elapsed {
        times.push(format!("Elapsed {}", format_time(e)));
    }
    if let Some(m) = s.remaining_minutes.filter(|m| *m > 0.0 && s.busy()) {
        times.push(format!("Left {}", format_time(m * 60.0)));
        times.push(format!("Finish about {}", super::job_stats::finish_at(m)));
    }
    if let Some(t) = planned {
        times.push(format!("Planned {}", format_time(t)));
    }
    v.times = times.join("  ·  ");

    let mut detail: Vec<String> = name.into_iter().collect();
    if s.busy() {
        if let (Some(l), Some(t)) = (s.layer, s.total_layers.filter(|t| *t > 0)) {
            detail.push(format!("Layer {l} / {t}"));
        }
        if let Some(pc) = s.progress {
            detail.push(format!("{pc:.0} %"));
        }
    }
    v.detail = detail.join("  ·  ");
    v.accepted = sent.map(|j| super::jobs::accepted_line(&j.meta.accepted)).unwrap_or_default();
    v.steps = sent.map(|j| steps_text(&j.meta.pauses, &j.meta.melts)).unwrap_or_default();
    // Paused at one of its own stops: what to place before resuming. The printer counts the
    // layer it finished or the one it starts, so either names the stop.
    if v.pause == Some(true) {
        let due = s.layer.and_then(|l| sent?.meta.pauses.iter().find(|p| l == p.layer || l == p.layer + 1));
        if let Some(p) = due {
            v.phase = format!("Paused for you: place {} in its part, then Resume", p.items.join(", "));
        }
    }
    v.heat = heat_line(s);

    let mut note = Vec::new();
    if printer.status != ConnectionStatus::Connected {
        note.push(format!("Connection: {}. The printer carries on by itself; Fab shows nothing new until it is back.", printer.status.label()));
    }
    if let Some(e) = s.print_error.filter(|e| *e != 0) {
        note.push(format!("Printer error {e:08X}"));
    }
    match &printer.command {
        Some((c, fab_printer_link::CommandState::Pending)) => note.push(format!("{c}: waiting for the printer")),
        Some((c, fab_printer_link::CommandState::Failed(e))) => note.push(format!("{c}: {e}")),
        _ => {}
    }
    v.note = note.join("\n");
    Some(v)
}

/// A job's stops for items to place and then its inserts to melt in after it, a line each, or
/// nothing.
fn steps_text(pauses: &[super::PreviewPause], melts: &[super::PreviewMelt]) -> String {
    pauses.iter().map(super::PreviewPause::step).chain(melts.iter().map(super::PreviewMelt::step)).collect::<Vec<_>>().join("\n")
}

/// A saved job not on the printer: what it is, how it went, and what can be done with it.
fn saved_view(job: &CommittedJob, now: f64) -> JobView {
    let mut v = JobView { close: true, ..Default::default() };
    let m = &job.meta;
    v.phase = match &job.state {
        JobState::Preparing => "Preparing the package for the printer".into(),
        JobState::Queued => "Queued: it starts when you press Start".into(),
        JobState::Sent => "On its way to the printer".into(),
        JobState::Finished => "Finished".into(),
        JobState::Failed(why) => format!("Failed: {why}."),
        JobState::Stopped => "Stopped.".into(),
        JobState::EndedUnseen => "It ended while Fab was closed: how isn't known.".into(),
    };
    v.attention = matches!(job.state, JobState::Failed(_));
    v.start = job.state == JobState::Queued;
    v.remove = job.state == JobState::Queued;
    let mut times = Vec::new();
    if !matches!(job.state, JobState::Queued | JobState::Preparing | JobState::EndedUnseen) {
        times.push(format!("Elapsed {}", format_time(job.ended_at.unwrap_or(now) - job.sent_at)));
    }
    times.push(format!("Planned {}", format_time(m.planned)));
    v.times = times.join("  ·  ");
    let grams: f64 = m.filament_g.iter().sum();
    let mut detail = vec![m.name.clone(), format!("{} layers", m.layers)];
    if grams > 0.0 { detail.push(format!("{grams:.1} g")); }
    v.detail = detail.join("  ·  ");
    v.accepted = super::jobs::accepted_line(&m.accepted);
    v.steps = steps_text(&m.pauses, &m.melts);
    v.note = job.unreadable.clone().unwrap_or_else(|| if job.loading { "Reading the job's slice…".into() } else { String::new() });
    v
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabJobCard {
    #[deref]
    view: View,
    #[rust]
    tick: Timer,
}

impl FabJobCard {
    fn sync(&mut self, cx: &mut Cx, state: &AppState) {
        let p = &state.print;
        let Some(job) = job_view(p, Cx::time_now()) else { return };
        // the resin printer's link is plain TCP: everywhere
        let supported = p.resin_job.is_some() || fab_printer_link::printing_supported();
        let v = &self.view;
        v.label(cx, ids!(phase)).set_text(cx, &job.phase);
        v.label(cx, ids!(times)).set_visible(cx, !job.times.is_empty());
        v.label(cx, ids!(times)).set_text(cx, &job.times);
        let bar = v.view(cx, ids!(bar));
        bar.set_visible(cx, job.fraction.is_some());
        if let Some(f) = job.fraction {
            crate::ui::widgets::set_progress(cx, &bar, f as f32);
        }
        v.label(cx, ids!(detail)).set_text(cx, &job.detail);
        v.label(cx, ids!(accepted)).set_visible(cx, !job.accepted.is_empty());
        v.label(cx, ids!(accepted)).set_text(cx, &job.accepted);
        v.label(cx, ids!(steps)).set_visible(cx, !job.steps.is_empty());
        v.label(cx, ids!(steps)).set_text(cx, &job.steps);
        v.label(cx, ids!(heat)).set_text(cx, &job.heat);
        v.label(cx, ids!(note)).set_visible(cx, !job.note.is_empty());
        v.label(cx, ids!(note)).set_text(cx, &job.note);
        v.button(cx, ids!(actions.cancel)).set_visible(cx, supported && job.cancel);
        v.button(cx, ids!(actions.pause)).set_visible(cx, supported && job.pause.is_some());
        v.button(cx, ids!(actions.pause)).set_text(cx, if job.pause == Some(true) { "Resume" } else { "Pause" });
        v.button(cx, ids!(actions.stop)).set_visible(cx, supported && job.stop);
        v.button(cx, ids!(actions.close)).set_visible(cx, job.close);
        v.view(cx, ids!(stop_confirm)).set_visible(cx, supported && job.stop && p.confirm_stop);
        v.button(cx, ids!(actions.start)).set_visible(cx, supported && job.start);
        v.button(cx, ids!(actions.remove)).set_visible(cx, job.remove);
        let asking = p.follow.confirm_start.is_some() && p.follow.confirm_start == p.follow.picked;
        v.view(cx, ids!(start_confirm)).set_visible(cx, supported && job.start && asking);
        if job.running && self.tick.is_empty() {
            self.tick = cx.start_timeout(1.0);
        }
    }
}

impl Widget for FabJobCard {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Print job");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.tick.is_event(event).is_some() {
            self.tick = Timer::empty();
            self.view.redraw(cx);
        }
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get::<AppState>() else { return };
        let v = &self.view;
        // Before the printer has it: the package stops, or the upload does.
        if v.button(cx, ids!(actions.cancel)).clicked(actions) {
            cx.action(if state.print.sending.is_some() { PrintAction::CancelSend } else { PrintAction::PrinterControl(PrinterControl::Stop) });
        }
        if v.button(cx, ids!(actions.close)).clicked(actions) {
            cx.action(PrintAction::CloseJob);
        }
        if v.button(cx, ids!(actions.stop)).clicked(actions) {
            cx.action(PrintAction::AskStop(!state.print.confirm_stop));
        }
        if v.button(cx, ids!(stop_confirm.buttons.keep)).clicked(actions) {
            cx.action(PrintAction::AskStop(false));
        }
        if v.button(cx, ids!(stop_confirm.buttons.go)).clicked(actions) {
            cx.action(PrintAction::PrinterControl(PrinterControl::Stop));
        }
        let picked = || state.print.follow.picked.clone().or_else(|| state.print.follow.shown().map(|j| j.meta.id.clone()));
        if v.button(cx, ids!(actions.start)).clicked(actions) {
            cx.action(PrintAction::AskStartJob(picked()));
        }
        if v.button(cx, ids!(start_confirm.buttons.keep)).clicked(actions) {
            cx.action(PrintAction::AskStartJob(None));
        }
        if v.button(cx, ids!(start_confirm.buttons.go)).clicked(actions) {
            if let Some(id) = picked() { cx.action(PrintAction::StartJob(id)); }
        }
        if v.button(cx, ids!(actions.remove)).clicked(actions) {
            if let Some(id) = picked() { cx.action(PrintAction::RemoveJob(id)); }
        }
        if v.button(cx, ids!(actions.pause)).clicked(actions) {
            // the button says what it does: Resume while the job is paused
            let paused = job_view(&state.print, Cx::time_now()).and_then(|j| j.pause) == Some(true);
            cx.action(PrintAction::PrinterControl(if paused { PrinterControl::Resume } else { PrinterControl::Pause }));
        }
        if actions.iter().any(|a| a.downcast_ref::<PrintAction>().is_some()) {
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
mod tests {
    use super::*;
    use fab_printer_link::{model::Reports, JobStatus, Snapshot};

    fn state(reports: &[&str], job: Option<JobStage>) -> PrintState {
        let mut r = Reports::default();
        let mut s = None;
        for report in reports {
            s = r.apply(report.as_bytes()).unwrap();
        }
        let mut p = PrintState::default();
        p.printer = Some(Arc::new(Snapshot {
            name: "Sim".into(), status: ConnectionStatus::Connected, state: Arc::new(s.unwrap()),
            job: job.map(|stage| JobStatus { name: "block".into(), file: "block.gcode.3mf".into(), stage }),
            ..Default::default()
        }));
        p
    }

    const IDLE: &str = r#"{"print":{"msg":0,"gcode_state":"IDLE","bed_temper":24,"bed_target_temper":0}}"#;
    const HEATING: &str = r#"{"print":{"msg":0,"gcode_state":"PREPARE","stg_cur":2,"subtask_name":"block","bed_temper":41,"bed_target_temper":60,"device":{"extruder":{"info":[{"id":1,"temp":3932370}]}},"mc_percent":0,"mc_remaining_time":40}}"#;

    #[test]
    fn every_stage_says_what_is_going_on_and_has_its_way_out() {
        // Nothing going on.
        assert_eq!(job_view(&state(&[IDLE], None), 0.0), None);
        // The package.
        let mut p = state(&[IDLE], None);
        p.sending = Some("block".into());
        let v = job_view(&p, 0.0).unwrap();
        assert!(v.phase.starts_with("Preparing the package") && v.cancel && !v.stop && v.detail == "block");
        // The upload.
        let v = job_view(&state(&[IDLE], Some(JobStage::Uploading { sent: 25, total: 100 })), 0.0).unwrap();
        assert_eq!((v.phase.as_str(), v.fraction, v.cancel, v.stop), ("Uploading to the printer's USB drive: 25 %", Some(0.25), true, false));
        // No answer yet: Stop is the way out.
        let v = job_view(&state(&[IDLE], Some(JobStage::Starting)), 0.0).unwrap();
        assert!(v.phase.contains("waiting for the printer to answer") && v.stop && !v.cancel);
        // The printer's own stage, temperatures against targets.
        let v = job_view(&state(&[HEATING], Some(JobStage::Accepted)), 0.0).unwrap();
        assert_eq!(v.phase, "Heating the bed");
        assert!(v.heat.contains("Left nozzle 210 → 60 °C"), "{}", v.heat);
        assert!(v.heat.contains("Bed 41 → 60 °C"), "{}", v.heat);
        assert_eq!(v.pause, Some(false));
        assert!(v.stop);
        assert!(v.times.contains("Left 40:00"), "{}", v.times);
        // Paused: Resume.
        let paused = r#"{"print":{"msg":1,"gcode_state":"PAUSE","stg_cur":16}}"#;
        let v = job_view(&state(&[HEATING, paused], Some(JobStage::Paused)), 0.0).unwrap();
        assert_eq!((v.phase.as_str(), v.pause), ("Paused", Some(true)));
        // A job Fab has no tracker for still shows and can be stopped.
        let v = job_view(&state(&[HEATING], None), 0.0).unwrap();
        assert!(v.stop && v.detail.starts_with("block"), "{v:?}");
    }

    /// A job that stops for items to be placed lists its stops, saved or printing, then the inserts
    /// to melt in after it, and paused at one says what to place.
    #[test]
    fn a_job_that_pauses_for_items_lists_its_stops_and_says_what_to_place() {
        use super::super::jobs::{JobMeta, JobState, SavedState};
        let pauses = vec![super::super::PreviewPause { layer: 35, z: 7.2, items: vec!["M3 nut".into()] }, super::super::PreviewPause { layer: 60, z: 12.2, items: vec!["Magnet".into(), "Board".into()] }];
        let melts = vec![super::super::PreviewMelt { size: "M3".into(), count: 4, into: "Base".into(), iron: 215 }, super::super::PreviewMelt { size: "M2".into(), count: 1, into: "Lid".into(), iron: 0 }];
        let meta = JobMeta { name: "block".into(), pauses, melts, ..Default::default() };
        let steps = "Pause before layer 36, at 7.20 mm: place M3 nut\nPause before layer 61, at 12.20 mm: place Magnet, Board\nAfter the print: melt in 4 × M3 inserts into Base, the iron at 215 °C\nAfter the print: melt in 1 × M2 insert into Lid, the iron at the filament's print temperature";
        let queued = CommittedJob::restored(meta.clone(), SavedState { state: JobState::Queued, ..Default::default() }, 0.0, 0.0);
        assert_eq!(saved_view(&queued, 0.0).steps, steps);
        let paused = |layer: u32| {
            let report = format!(r#"{{"print":{{"msg":1,"gcode_state":"PAUSE","stg_cur":16,"layer_num":{layer},"total_layer_num":100}}}}"#);
            let mut p = state(&[HEATING, &report], Some(JobStage::Paused));
            p.follow.jobs.push(CommittedJob::restored(meta.clone(), SavedState { state: JobState::Sent, started: Some(0.0), ..Default::default() }, 0.0, 0.0));
            job_view(&p, 0.0).unwrap()
        };
        let v = paused(36);
        assert_eq!((v.phase.as_str(), v.steps.as_str()), ("Paused for you: place M3 nut in its part, then Resume", steps));
        assert_eq!(paused(60).phase, "Paused for you: place Magnet, Board in its part, then Resume");
        assert_eq!(paused(20).phase, "Paused", "a pause of the person's own");
    }

    #[test]
    fn a_stop_says_what_happened_and_what_is_still_hot() {
        let stopped = r#"{"print":{"msg":0,"gcode_state":"FAILED","bed_temper":58,"bed_target_temper":0,"device":{"extruder":{"info":[{"id":1,"temp":190}]}}}}"#;
        let v = job_view(&state(&[stopped], Some(JobStage::Cancelled)), 0.0).unwrap();
        assert_eq!(v.phase, "Stopped. Still hot: bed 58 °C, left nozzle 190 °C.");
        assert!(v.close && !v.stop && !v.running);
        let cold = r#"{"print":{"msg":0,"gcode_state":"IDLE","bed_temper":24,"bed_target_temper":0}}"#;
        assert_eq!(job_view(&state(&[cold], Some(JobStage::Cancelled)), 0.0).unwrap().phase, "Stopped.");
        // Closed: gone, until the next job.
        let mut p = state(&[cold], Some(JobStage::Cancelled));
        p.follow.closed = true;
        assert_eq!(job_view(&p, 0.0), None);
    }

    /// The first family's still-hot heads come in the printer's order, one with no known side
    /// being "nozzle"; numbered heads by number.
    #[test]
    fn still_hot_keeps_the_report_order_and_names_each_head() {
        use fab_printer_link::model::{CloudNozzleId, Extruder, PrinterState, Temperature};
        let hot = |id: Option<u8>, t: f64| Extruder { id: id.map(CloudNozzleId), temperature: Temperature { current: Some(t), target: None }, ..Default::default() };
        let mut s = PrinterState { extruders: vec![hot(Some(0), 200.0), hot(Some(1), 210.0)], ..Default::default() };
        assert_eq!(still_hot(&s), " Still hot: right nozzle 200 °C, left nozzle 210 °C.");
        s.extruders = vec![hot(None, 200.0)];
        assert_eq!(still_hot(&s), " Still hot: nozzle 200 °C.");
        s.numbered_heads = true;
        s.extruders = vec![hot(Some(2), 200.0), hot(Some(0), 60.0)];
        assert_eq!(still_hot(&s), " Still hot: Tool 1 60 °C, Tool 3 200 °C.");
    }

    #[test]
    fn a_printer_that_stops_answering_is_said() {
        let mut p = state(&[HEATING], Some(JobStage::Accepted));
        let snapshot = Arc::get_mut(p.printer.as_mut().unwrap()).unwrap();
        snapshot.status = ConnectionStatus::Offline("no printer report for 10 seconds");
        assert!(job_view(&p, 0.0).unwrap().note.starts_with("Connection: no printer report for 10 seconds"));
    }

    #[test]
    fn numbered_heat_and_still_hot_keep_tool_names() {
        let state = super::super::panel::tests::numbered_report();
        assert_eq!(heat_line(&state), "Tool 1 212 → 220 °C  ·  Tool 2 212 → 220 °C  ·  Tool 3 212 → 220 °C  ·  Tool 4 212 → 220 °C");
        assert_eq!(still_hot(&state), " Still hot: Tool 1 212 °C, Tool 2 212 °C, Tool 3 212 °C, Tool 4 212 °C.");
    }
}
