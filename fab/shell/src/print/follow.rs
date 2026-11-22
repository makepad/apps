//! Monitor shows the jobs Print committed ([`Follow::jobs`], [`super::jobs`]), a view of its own:
//! a job's own slice, cut at the layer the printer reports (`layer_num` / `total_layer_num`) with
//! that layer lit, what is still to print dimmed, and the nozzle placed along it by Fab's time model.
//!
//! - The printer's job is found among the committed ones by its name and layer count, never in the
//!   plate's slice. Without a copy, Monitor says what the printer runs and shows what it reports
//!   (name, layer N / M, progress, time left). After a restart the same search finds the running
//!   job again, and its slice is made from its package on a worker.
//! - Monitor has its own playback and camera ([`Follow::hidden_play`], [`crate::api::PaneState::hidden_view`]):
//!   nothing the plate or Design's slice does reaches a job it holds, and nothing done in it
//!   reaches the other workspaces.
//! - The printer reports no position. Every status report re-anchors the playhead: a new layer
//!   starts at its first move, the first report of a job places it from the time left, and in
//!   between the clock runs at print speed and waits at the end of the layer for the next
//!   report. The nozzle shown is therefore Fab's estimate.
//! - Following is on by default; it applies in the Monitor workspace while it holds a job. Any
//!   manual playback (slider, scrub, play) turns it off to look ahead or back: the head stays at
//!   the printer's layer, and "Back to live" follows again.
//!
//! Main-loop cost: a report rebuilds one small [`PrinterJob`] (no geometry); while following,
//! each frame is one span lookup and a clamp, and frames stop while the nozzle waits.

use super::jobs::{CommittedJob, JobState};
use super::*;
use fab_printer_link::JobStage;

/// The printer's job as Monitor shows it (rebuilt on every status report).
#[derive(Clone, Debug, Default)]
pub struct PrinterJob {
    /// Fab's committed copy of this job ([`super::jobs::JobMeta::id`]).
    pub id: Option<String>,
    pub name: Option<String>,
    /// The printer's `gcode_state` (PREPARE, RUNNING, PAUSE ...).
    pub state: String,
    /// What the printer is doing (its stage).
    pub stage: Option<String>,
    /// The printer's layer, 1-based (0 before the first).
    pub layer: u32,
    pub total_layers: Option<u32>,
    pub progress: Option<f64>,
    pub remaining_minutes: Option<f64>,
    /// The slice of Fab's copy of this job, once it is there.
    pub preview: Option<Arc<PrintPreview>>,
    /// Why there is no slice to follow.
    pub unmatched: Option<String>,
    /// The playhead (preview seconds) at `synced_at`, and the latest it may run to before
    /// the next report (the end of the reported layer).
    pub time: f64,
    pub limit: f64,
    /// Wall-clock seconds of the last report.
    pub synced_at: f64,
    /// The nozzle moves (printing, not paused or in a printer routine).
    pub running: bool,
}

impl PrinterJob {
    /// The estimated playhead at `now`.
    pub fn time_at(&self, now: f64) -> f64 {
        if !self.running { return self.time; }
        (self.time + (now - self.synced_at).max(0.0)).min(self.limit.max(self.time))
    }

    /// The preview's layer index of the printer's layer.
    pub fn layer_index(&self, pv: &PrintPreview) -> u32 {
        self.layer.saturating_sub(1).min(pv.max_layer())
    }

    /// "12 / 80", "12", or "–".
    pub fn layer_text(&self) -> String {
        match (self.layer, self.total_layers) {
            (0, Some(t)) => format!("0 / {t}"),
            (l, Some(t)) => format!("{l} / {t}"),
            (0, None) => "–".into(),
            (l, None) => l.to_string(),
        }
    }

    /// What the printer does: its stage, or Printing / Paused / Preparing.
    pub fn doing(&self) -> String {
        self.stage.clone().unwrap_or_else(|| match self.state.as_str() {
            "PAUSE" => "Paused".into(),
            "PREPARE" => "Preparing".into(),
            _ => "Printing".into(),
        })
    }

    /// "37 % · 1:23:00 left"
    pub fn progress_text(&self) -> String {
        let mut out = Vec::new();
        if let Some(p) = self.progress { out.push(format!("{p:.0} %")); }
        if let Some(m) = self.remaining_minutes.filter(|m| *m > 0.0) { out.push(format!("{} left", format_time(m * 60.0))); }
        out.join("  ·  ")
    }

    /// "Printing · 37 % · 1:23:00 left"
    pub fn status_text(&self) -> String {
        let mut out = vec![self.doing()];
        if let Some(p) = self.progress { out.push(format!("{p:.0} %")); }
        if let Some(m) = self.remaining_minutes.filter(|m| *m > 0.0) { out.push(format!("{} left", format_time(m * 60.0))); }
        out.join("  ·  ")
    }
}

/// Monitor's following state (`PrintState::follow`).
#[derive(Clone, Debug)]
pub struct Follow {
    /// The user's toggle (on by default).
    pub on: bool,
    /// The estimated head is moving: Monitor's view is drawn again at its pace.
    pub head_moves: bool,
    /// The Monitor workspace shows.
    pub monitor: bool,
    /// What Print committed, newest first.
    pub jobs: Vec<CommittedJob>,
    /// The saved jobs are read: a job that isn't among them has no copy.
    pub jobs_read: bool,
    /// The job picked in Monitor's list (None: the printer's, or Fab's last).
    pub picked: Option<String>,
    /// The queued job whose start is being confirmed.
    pub confirm_start: Option<String>,
    /// Jobs whose `state.txt` is behind.
    pub unsaved: Vec<String>,
    /// The ended job's card was closed (a new job opens it again).
    pub closed: bool,
    /// The printer's running job (None: idle, finished or no printer).
    pub job: Option<PrinterJob>,
    /// Where the head is by Fab's slice timing (preview seconds): what is later is still to print.
    pub live: f64,
    /// The playback that is not on screen: the other workspaces' while Monitor shows, Monitor's
    /// while they do.
    pub hidden_play: Playback,
    /// The printer's last report: which it was, and when.
    pub heard: Option<(usize, f64)>,
    /// The numbers of the job Monitor holds, and its slice.
    pub stats: Option<(Arc<PrintPreview>, Arc<super::job_stats::JobStats>)>,
}

impl Default for Follow {
    fn default() -> Self {
        Follow { on: true, head_moves: false, monitor: false, jobs: Vec::new(), jobs_read: false, picked: None, confirm_start: None, unsaved: Vec::new(), closed: false, job: None, live: 0.0, hidden_play: Playback::default(), heard: None, stats: None }
    }
}

/// Whether the printer's name for a job is Fab's job `fab`. A printer that numbers its heads
/// (`numbered`, the second family) names it by the file it was uploaded as.
fn same_name(fab: &str, printer: &str, numbered: bool) -> bool {
    if numbered {
        return printer == fab_printer_link::job::plain_file_name(fab);
    }
    let strip = |s: &str| s.trim().trim_end_matches(".gcode.3mf").trim_end_matches(".3mf").to_string();
    strip(fab) == strip(printer)
}

/// Fab's copy of a job the printer reports: the one it sent, by name and layer count, or an ended
/// one of the same name and count printed again. A printer that doesn't say what it prints runs
/// what Fab has just sent it. Queued jobs aren't on the printer, whatever their name.
fn job_of_report(f: &Follow, name: Option<&str>, total: Option<u32>, numbered: bool) -> Option<usize> {
    let fits = |j: &CommittedJob| total.filter(|t| *t > 0).is_none_or(|t| t == j.meta.layers) && name.is_none_or(|n| same_name(&j.meta.name, n, numbered));
    f.jobs.iter().position(|j| j.state == JobState::Sent && fits(j))
        .or_else(|| f.jobs.iter().position(|j| name.is_some() && j.state.ended() && fits(j)))
}

impl Follow {
    pub fn job_of(&self, id: &str) -> Option<&CommittedJob> {
        self.jobs.iter().find(|j| j.meta.id == id)
    }

    pub fn job_of_mut(&mut self, id: &str) -> Option<&mut CommittedJob> {
        self.jobs.iter_mut().find(|j| j.meta.id == id)
    }

    /// The job Monitor shows: the one picked in its list, the printer's when it has a copy, or
    /// the last one until its card is closed.
    pub fn shown(&self) -> Option<&CommittedJob> {
        if let Some(job) = self.picked.as_deref().and_then(|id| self.job_of(id)) {
            return Some(job);
        }
        match &self.job {
            Some(job) => job.id.as_deref().and_then(|id| self.job_of(id)),
            None if self.closed => None,
            None => self.jobs.iter().find(|j| j.state != JobState::Queued),
        }
    }

    /// The printer's job, when it is what Monitor shows: it has a live head and layer then.
    pub fn shown_printing(&self) -> Option<&PrinterJob> {
        self.job.as_ref().filter(|j| self.picked.is_none() || self.picked == j.id)
    }

    /// A job's `state.txt` is behind.
    pub fn changed(&mut self, id: &str) {
        if !self.unsaved.iter().any(|u| u == id) {
            self.unsaved.push(id.to_string());
        }
    }
}

impl PrintState {
    /// The printer has a job, or one is on its way to it: a new one waits in the queue.
    pub fn printer_busy(&self) -> bool {
        self.printer.as_ref().is_some_and(|p| p.state.busy() || p.job.as_ref().is_some_and(|j| j.stage.active()))
    }

    /// Monitor shows a job: the one the printer runs, or a committed one until its card is
    /// closed. The view is then the job's alone.
    pub fn monitor_holds(&self) -> bool {
        self.follow.monitor && (self.follow.job.is_some() || self.follow.shown().is_some())
    }

    /// The slice of the job Monitor shows. None: no job, one Fab has no copy of, or one whose
    /// slice isn't there yet.
    pub fn monitor_preview(&self) -> Option<&Arc<PrintPreview>> {
        if !self.follow.monitor { return None; }
        self.follow.shown().and_then(|j| j.preview.as_ref())
    }

    /// The job's slice while Monitor follows the printer.
    pub fn following(&self) -> Option<&Arc<PrintPreview>> {
        if !self.follow.on { return None; }
        self.monitor_preview()
    }

    /// The job Monitor holds and its numbers.
    pub fn job_stats(&self) -> Option<(&Arc<PrintPreview>, &super::job_stats::JobStats)> {
        let pv = self.monitor_preview()?;
        self.follow.stats.as_ref().filter(|(of, _)| Arc::ptr_eq(of, pv)).map(|(_, stats)| (pv, &**stats))
    }

    /// Monitor lets go of a job: its view shows what the other workspaces show.
    pub(super) fn release_monitor(&mut self) {
        self.play = self.follow.hidden_play.clone();
    }
}

/// What Monitor's view of the job shows, in words.
pub fn view_text(p: &PrintState, pv: &PrintPreview) -> String {
    let job = p.follow.shown_printing();
    let sent = p.follow.shown();
    // A printer that numbers its heads names a job by the file it was uploaded as: once Fab
    // knows the job, it goes by Fab's name.
    let numbered = p.printer.as_ref().is_some_and(|printer| printer.state.numbered_heads);
    let known = job.filter(|_| numbered).and_then(|j| j.id.as_deref()).and_then(|id| p.follow.job_of(id)).map(|c| c.meta.name.as_str());
    let name = known.or(job.and_then(|j| j.name.as_deref())).or(sent.map(|s| s.meta.name.as_str())).unwrap_or("");
    if p.following().is_some() {
        return match (job, sent) {
            (Some(j), _) if j.layer > 0 => format!("{name}  ·  {}  ·  head position estimated from Fab's slice timing", j.doing()),
            (Some(j), _) => format!("{name}  ·  {}  ·  nothing printed yet", j.doing()),
            (None, Some(s)) if s.state == JobState::Queued => format!("{name}  ·  queued, as it will go to the printer"),
            (None, Some(s)) if s.state == JobState::EndedUnseen => format!("{name}  ·  as it went to the printer: it ended while Fab was closed"),
            (None, Some(s)) if s.state == JobState::Finished || s.reached >= pv.total => format!("{name}  ·  printed, as it went to the printer"),
            (None, Some(s)) if s.reached > 0.0 => {
                let layer = pv.layers.get(pv.layer_at(s.reached)).map_or(0, |l| l.layer) + 1;
                format!("{name}  ·  printed up to layer {layer} of {}", pv.max_layer() + 1)
            }
            (None, Some(s)) if !s.state.ended() => format!("{name}  ·  on its way to the printer"),
            _ => format!("{name}  ·  nothing printed, as it went to the printer"),
        };
    }
    let top = timeline::shown_top_layer(p, pv) + 1;
    match job {
        Some(j) if j.layer > 0 => {
            let looking = match top.cmp(&j.layer) {
                std::cmp::Ordering::Greater => "Looking ahead at",
                std::cmp::Ordering::Less => "Looking back at",
                std::cmp::Ordering::Equal => "Looking at",
            };
            format!("{looking} layer {top}: the printer is on layer {}, the head shows where it is", j.layer_text())
        }
        Some(j) => format!("Looking at layer {top}: nothing printed yet, the printer is {}", j.doing().to_lowercase()),
        None => format!("Looking at layer {top} of {name}"),
    }
}

/// The log line of a layer the printer reports for the first time: the real time against the
/// time Fab's slice planned for it, from the reports Fab already has. `now`: as
/// `CommittedJob::sent_at`. `state` still holds the previous report; only the copy of the job this
/// report is about gives the times.
pub fn layer_line(state: &PrintState, new: &fab_printer_link::Snapshot, now: f64) -> Option<String> {
    let s = &new.state;
    let layer = s.layer.filter(|l| *l > 0 && s.busy())?;
    if state.printer.as_ref().is_some_and(|old| old.state.layer == Some(layer)) {
        return None;
    }
    let copy = job_of_report(&state.follow, s.job_name.as_deref(), s.total_layers, s.numbered_heads).map(|i| &state.follow.jobs[i]);
    let total = s.total_layers.map_or("?".to_string(), |t| t.to_string());
    let elapsed = copy.filter(|j| !j.state.ended()).map_or("no Start print here".to_string(), |j| format!("{:.1} s since Start print", now - j.sent_at));
    let planned = copy.and_then(|j| j.preview.as_ref()).map_or("no slice".to_string(), |pv| format!("planned start {:.1} s", timeline::layer_span(pv, (layer - 1).min(pv.max_layer())).0));
    let percent = s.progress.map_or("?".to_string(), |p| format!("{p:.0}"));
    let left = s.remaining_minutes.map_or("?".to_string(), |m| format!("{m:.0}"));
    Some(format!("layer {layer} / {total}: {elapsed}, {planned}, printer {percent} %, {left} min left"))
}

/// How the jobs on the printer ended, from this report: the link's tracker for what it sent in
/// this run of the app (a tracker from before the send says nothing about it), the printer's
/// state for what a restart found.
fn end_jobs(state: &mut PrintState, now: f64) {
    let Some(p) = state.printer.clone() else { return };
    if let Some(tracked) = p.job.as_ref().filter(|t| !t.stage.active()) {
        // The link's tracker carries Fab's own name for the job, in every family.
        let of_it = |j: &CommittedJob| j.state == JobState::Sent && !j.restored && p.job_sends >= j.link_send && same_name(&j.meta.name, &tracked.name, false);
        if let Some(i) = state.follow.jobs.iter().position(of_it) {
            let ended = match &tracked.stage {
                JobStage::Finished => JobState::Finished,
                JobStage::Failed(why) => JobState::Failed(why.clone()),
                _ => JobState::Stopped,
            };
            end_job(&mut state.follow, i, ended, now);
        }
    }
    let s = &p.state;
    let Some(gcode) = s.gcode_state.as_deref() else { return };
    let running = job_of_report(&state.follow, s.job_name.as_deref(), s.total_layers, s.numbered_heads).filter(|_| s.busy());
    for i in 0..state.follow.jobs.len() {
        let job = &state.follow.jobs[i];
        if job.state != JobState::Sent || !job.restored || running == Some(i) {
            continue;
        }
        let same = s.job_name.as_deref().is_some_and(|n| same_name(&job.meta.name, n, s.numbered_heads));
        let ended = match gcode {
            "FINISH" if same => JobState::Finished,
            "FAILED" if same => JobState::Failed("the printer reports the print failed or was stopped".into()),
            _ => JobState::EndedUnseen,
        };
        end_job(&mut state.follow, i, ended, now);
    }
}

fn end_job(f: &mut Follow, i: usize, ended: JobState, now: f64) {
    let job = &mut f.jobs[i];
    log!("fab jobs: {} ended: {} after {:.0} s, planned {:.0} s, as the {} says", job.meta.id, ended.label(), now - job.sent_at, job.meta.planned, if job.restored { "printer's report" } else { "printer link" });
    job.ended_at = Some(now);
    if ended == JobState::Finished {
        job.reached = job.meta.planned;
    }
    job.state = ended;
    let id = job.meta.id.clone();
    f.changed(&id);
}

/// Which job a report of the printer's was matched to, when that changes: what only a real
/// printer's reports can show is a wrong match, or none.
fn match_line(prev: Option<&PrinterJob>, f: &Follow) -> Option<String> {
    let same = |a: &PrinterJob, b: &PrinterJob| a.name == b.name && a.total_layers == b.total_layers && a.id == b.id && a.preview.is_some() == b.preview.is_some() && a.unmatched == b.unmatched;
    let job = match (prev, f.job.as_ref()) {
        (None, None) => return None,
        (Some(_), None) => return Some("the printer is on no job".into()),
        (Some(a), Some(b)) if same(a, b) => return None,
        (_, Some(job)) => job,
    };
    let name = job.name.as_deref().unwrap_or("-");
    let on = format!("the printer is on {name}, layer {}", job.layer_text());
    Some(match (job.id.as_deref().and_then(|id| f.job_of(id)), &job.unmatched) {
        (Some(copy), None) => format!("{on}: job {}, {}, {} layers, planned {:.0} s, head at {:.0} s of it", copy.meta.id, copy.state.label(), copy.meta.layers, copy.meta.planned, job.time),
        (_, why) => {
            let named: Vec<String> = f.jobs.iter().filter(|j| job.name.as_deref().is_some_and(|n| same_name(&j.meta.name, n, false) || same_name(&j.meta.name, n, true))).map(|j| format!("{} layers {}", j.meta.layers, j.state.label())).collect();
            format!("{on}: no slice: {}; saved under that name: [{}]", why.as_deref().unwrap_or("-"), named.join(", "))
        }
    })
}

/// A new printer snapshot: rebuild the job and re-anchor the playhead. Returns true while
/// the followed nozzle keeps moving (the caller asks for frames).
pub fn printer_changed(state: &mut PrintState, now: f64) -> bool {
    let held = state.monitor_holds();
    let prev = state.follow.job.take();
    if let Some(report) = state.printer.as_ref().map(|p| Arc::as_ptr(&p.state) as usize) {
        if state.follow.heard.is_none_or(|(last, _)| last != report) {
            state.follow.heard = Some((report, now));
        }
    }
    if state.printer.as_ref().is_some_and(|p| p.state.busy()) {
        state.follow.closed = false;
    }
    // How Fab's job went, frozen for its card and its view.
    end_jobs(state, now);
    let printer = state.printer.clone().filter(|p| p.state.busy());
    let job = printer.map(|p| {
        let s = &p.state;
        let gcode = s.gcode_state.clone().unwrap_or_default();
        let printing_stage = s.stage.is_none_or(|st| st <= 0 || st == 255);
        let mut job = PrinterJob {
            name: s.job_name.clone(), state: gcode.clone(),
            stage: s.stage_name().filter(|_| !printing_stage || gcode != "RUNNING"),
            layer: s.layer.unwrap_or(0), total_layers: s.total_layers.filter(|t| *t > 0),
            progress: s.progress, remaining_minutes: s.remaining_minutes,
            running: gcode == "RUNNING" && printing_stage && s.layer.unwrap_or(0) > 0,
            synced_at: now,
            ..Default::default()
        };
        let copy = job_of_report(&state.follow, job.name.as_deref(), job.total_layers, s.numbered_heads).map(|i| &state.follow.jobs[i]);
        job.id = copy.map(|c| c.meta.id.clone());
        match copy.and_then(|c| c.preview.clone()) {
            Some(pv) => {
                let (t0, t1) = timeline::layer_span(&pv, job.layer_index(&pv));
                job.limit = (t1 - 1e-3).max(t0);
                let same_job = prev.as_ref().filter(|p| p.id == job.id && p.preview.as_ref().is_some_and(|q| Arc::ptr_eq(q, &pv)));
                job.time = if job.layer == 0 {
                    0.0
                } else if let Some(p) = same_job.filter(|p| p.layer == job.layer) {
                    // The same layer: the clock carries on.
                    p.time_at(now).clamp(t0, job.limit)
                } else if same_job.is_some_and(|p| p.layer + 1 == job.layer) {
                    // Just started this layer.
                    t0
                } else {
                    // First sight (or a jump): place it by the time left.
                    job.remaining_minutes.map(|m| pv.total - m * 60.0).unwrap_or(t0).clamp(t0, job.limit)
                };
                job.preview = Some(pv);
            }
            None => job.unmatched = Some(match copy {
                Some(c) => c.unreadable.clone().unwrap_or_else(|| "Reading Fab's copy of this job…".into()),
                None if !state.follow.jobs_read => "Looking for Fab's copy of this job…".into(),
                None if job.name.is_none() => "The printer does not say which file it prints.".into(),
                None => "Fab has no copy of this job, so there is no slice to show.".into(),
            }),
        }
        job
    });
    state.follow.job = job;
    if let Some(line) = match_line(prev.as_ref(), &state.follow) {
        log!("fab printer: {line}");
    }
    if let Some((id, time)) = state.follow.job.as_ref().filter(|j| j.preview.is_some()).and_then(|j| Some((j.id.clone()?, j.time))) {
        if let Some(copy) = state.follow.job_of_mut(&id).filter(|c| !c.state.ended()) {
            copy.reached = time;
        }
    }
    if held && !state.monitor_holds() {
        state.release_monitor();
    }
    state.bump();
    place(state, now)
}

/// Where the head of Monitor's job is at `now`, and the playback there while following: the
/// layer it prints, what is on the plate of a job that stopped, or the whole job when none of it
/// is known to be printed. Returns true while the nozzle keeps moving, also while the view looks
/// elsewhere (the head moves on).
pub fn place(state: &mut PrintState, now: f64) -> bool {
    let moving = place_on_job(state, now);
    state.follow.head_moves = moving;
    moving
}

fn place_on_job(state: &mut PrintState, now: f64) -> bool {
    let Some(pv) = state.monitor_preview().cloned() else { return false };
    if state.follow.stats.as_ref().is_none_or(|(of, _)| !Arc::ptr_eq(of, &pv)) {
        state.follow.stats = Some((pv.clone(), Arc::new(super::job_stats::JobStats::of(&pv))));
        state.bump();
    }
    let (live, printing, moving) = match state.follow.shown_printing() {
        Some(job) if job.layer > 0 => {
            let t = job.time_at(now);
            (t, Some(job.layer_index(&pv)), job.running && t < job.limit)
        }
        Some(_) => (0.0, None, false),
        None => (state.follow.shown().map_or(0.0, |s| if s.state == JobState::Finished { pv.total } else { s.reached }), None, false),
    };
    state.follow.live = live;
    if !state.follow.on {
        if !state.play.active {
            state.play.active = true;
            state.bump();
        }
        return moving;
    }
    let unseen = state.follow.shown().is_some_and(|s| s.state == JobState::EndedUnseen);
    let (time, hi) = match printing {
        Some(layer) => (live, layer),
        None if live > 0.0 && live < pv.total && !unseen => (live, pv.layers.get(pv.layer_at(live)).map_or(pv.max_layer(), |l| l.layer)),
        None => (pv.total, pv.max_layer()),
    };
    let p = &mut state.play;
    if p.active && !p.playing && p.layer_lo == 0 && p.layer_hi == hi && p.time == time && state.focus_layer == Some(hi) {
        return moving;
    }
    let changed = !p.active || p.playing || p.layer_lo != 0 || p.layer_hi != hi || state.focus_layer != Some(hi);
    p.active = true;
    p.playing = false;
    p.layer_lo = 0;
    p.layer_hi = hi;
    p.time = time;
    state.focus_layer = Some(hi);
    // In-layer clock motion only affects the nozzle and playback readouts.
    if changed { state.bump(); }
    moving
}

/// A playback action the user makes by hand ends following.
pub(super) fn manual(action: &PrintAction) -> bool {
    matches!(action, PrintAction::Play(true) | PrintAction::Seek(_) | PrintAction::Scrub(_) | PrintAction::SetTopLayer(_) | PrintAction::SetBottomLayer(_))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fab_printer_link::{model::Reports, ConnectionStatus, Snapshot};

    struct Still;
    impl PrintTimeline for Still {
        fn now(&self, _t: f64) -> PrintNow { PrintNow::default() }
    }

    /// Four layers of ten seconds after ten seconds of printer routines.
    fn preview() -> Arc<PrintPreview> {
        preview_of(4)
    }

    pub(crate) fn preview_of(count: u32) -> Arc<PrintPreview> {
        let layers = (0..count).map(|l| PreviewLayer { layer: l, z: 0.2 * (l + 1) as f32, t_start: 10.0 + l as f64 * 10.0, t_end: 20.0 + l as f64 * 10.0, starts: [l, 0, 0] }).collect();
        let mut data = Vec::new();
        for l in 0..count {
            let t = 10.0 + l as f32 * 10.0;
            data.extend_from_slice(&super::super::Bead { a: [0.0, 0.0, 0.2], b: [10.0, 0.0, 0.2], t0: t, dur: 10.0, width: 0.4, height: 0.2, value: 100.0, layer: l as f32, role: 1.0, filament: 1.0, ..Default::default() }.pack());
        }
        Arc::new(PrintPreview {
            pieces: Vec::new(),
            shared: (0.0, 0.0),
            gcode_key: None,
            printability: Arc::new(fab_printability::Report::default()),
            markers: RetainedInstances::new(PREVIEW_SLOTS, Vec::<f32>::new().into()).unwrap(),
            beads: super::super::BeadStreams::surface(RetainedInstances::new(PREVIEW_SLOTS, data.into()).unwrap()),
            travel: RetainedInstances::new(PREVIEW_SLOTS, Vec::<f32>::new().into()).unwrap(),
            blocks: Vec::new(),
            part_layers: Vec::new(),
            chunks: Vec::new(),
            layers, total: 10.0 + count as f64 * 10.0, grown: None,
            bounds: [[0.0; 3], [10.0, 0.0, 0.8]], speed_range: [100.0, 100.0], filament_colors: vec![[1.0; 4]],
            roles: Vec::new(), changes: Vec::new(), timeline: Arc::new(Still), summary: Vec::new(), pauses: Vec::new(), melts: Vec::new(), modes: Vec::new(), max_z_rate: 0.0, nonplanar: None,
            ghosts: RetainedInstances::new(PREVIEW_SLOTS, Vec::<f32>::new().into()).unwrap(), filament_use: Vec::new(), purges: 0, purge_seconds: 0.0, prepare_seconds: 10.0, id: 7, part_offsets: Arc::from([]), part_objects: Arc::from([]), tools: Vec::new(),
        })
    }

    /// A job on the printer's way, as Start print leaves it.
    fn sent(name: &str, preview: Arc<PrintPreview>, sent_at: f64) -> CommittedJob {
        CommittedJob {
            meta: Arc::new(super::super::jobs::JobMeta { id: format!("id-{name}-{sent_at}"), name: name.into(), layers: preview.max_layer() + 1, planned: preview.total, ..Default::default() }),
            state: JobState::Sent, preview: Some(preview), sent_at, ..Default::default()
        }
    }

    fn report(state: &mut PrintState, reports: &mut Reports, json: &str, now: f64) -> bool {
        let s = reports.apply(json.as_bytes()).unwrap().unwrap();
        state.printer = Some(Arc::new(Snapshot { name: "Sim".into(), status: ConnectionStatus::Connected, state: Arc::new(s), ..Default::default() }));
        printer_changed(state, now)
    }

    #[test]
    fn a_new_layer_logs_its_real_time_against_the_planned_one() {
        let mut s = PrintState::default();
        s.follow.jobs.push(sent("block", preview(), 100.0));
        let mut r = Reports::default();
        report(&mut s, &mut r, r#"{"print":{"msg":0,"gcode_state":"PREPARE","subtask_name":"block","layer_num":0,"total_layer_num":4}}"#, 100.0);
        let next = |r: &mut Reports, json: &str| Snapshot { state: Arc::new(r.apply(json.as_bytes()).unwrap().unwrap()), ..Default::default() };
        // Nothing printed yet, no line.
        let snapshot = next(&mut r, r#"{"print":{"msg":1,"stg_cur":2}}"#);
        assert_eq!(layer_line(&s, &snapshot, 110.0), None);
        // Layer 3, the printer's numbering: index 2 of the slice, planned at 30 s.
        let snapshot = next(&mut r, r#"{"print":{"msg":1,"gcode_state":"RUNNING","layer_num":3,"mc_percent":60,"mc_remaining_time":12}}"#);
        assert_eq!(layer_line(&s, &snapshot, 142.5).as_deref(), Some("layer 3 / 4: 42.5 s since Start print, planned start 30.0 s, printer 60 %, 12 min left"));
        // The same layer again says nothing.
        report(&mut s, &mut r, r#"{"print":{"msg":1}}"#, 142.5);
        let snapshot = next(&mut r, r#"{"print":{"msg":1,"mc_percent":61}}"#);
        assert_eq!(layer_line(&s, &snapshot, 143.0), None);
        // A job Fab has no copy of: the printer's numbers alone.
        s.follow.jobs.clear();
        s.printer = None;
        s.follow.job = None;
        let snapshot = next(&mut r, r#"{"print":{"msg":1,"layer_num":4}}"#);
        assert_eq!(layer_line(&s, &snapshot, 150.0).as_deref(), Some("layer 4 / 4: no Start print here, no slice, printer 61 %, 12 min left"));
    }

    /// A reconnect that missed job A's end: the report of job B doesn't borrow A's times.
    #[test]
    fn a_layer_of_another_job_logs_none_of_the_last_jobs_times() {
        let mut s = PrintState::default();
        s.follow.jobs.push(sent("a", preview(), 100.0));
        let mut r = Reports::default();
        report(&mut s, &mut r, r#"{"print":{"msg":0,"gcode_state":"RUNNING","subtask_name":"a","layer_num":2,"total_layer_num":4}}"#, 110.0);
        let snapshot = Snapshot { state: Arc::new(r.apply(br#"{"print":{"msg":1,"subtask_name":"b","layer_num":3,"total_layer_num":4,"mc_percent":60,"mc_remaining_time":12}}"#).unwrap().unwrap()), ..Default::default() };
        assert_eq!(layer_line(&s, &snapshot, 150.0).as_deref(), Some("layer 3 / 4: no Start print here, no slice, printer 60 %, 12 min left"));
        // The other way round: job A's own report has its times.
        let snapshot = Snapshot { state: Arc::new(r.apply(br#"{"print":{"msg":1,"subtask_name":"a","layer_num":4}}"#).unwrap().unwrap()), ..Default::default() };
        assert_eq!(layer_line(&s, &snapshot, 150.0).as_deref(), Some("layer 4 / 4: 50.0 s since Start print, planned start 40.0 s, printer 60 %, 12 min left"));
    }

    #[test]
    fn monitor_follows_the_reported_layer_and_hands_back_on_manual_playback() {
        let mut s = PrintState::default();
        s.set_preview(preview());
        s.follow.jobs.push(sent("block", preview(), 0.0));
        s.follow.monitor = true;
        let mut r = Reports::default();
        // Preparing: following, nothing printed yet.
        assert!(!report(&mut s, &mut r, r#"{"print":{"msg":0,"gcode_state":"PREPARE","subtask_name":"block","layer_num":0,"total_layer_num":4,"stg_cur":2}}"#, 100.0));
        assert!(s.following().is_some());
        // Nothing printing yet: the whole job, all of it still to print.
        assert_eq!((s.follow.live, s.play.time, s.play.layer_hi), (0.0, 50.0, 3));
        assert_eq!(s.follow.job.as_ref().unwrap().stage.as_deref(), Some("Heating the bed"));
        // First sight of layer 2 with 25 s left: 25 s from the end, inside layer 2 (20..30).
        assert!(report(&mut s, &mut r, r#"{"print":{"msg":1,"gcode_state":"RUNNING","stg_cur":0,"layer_num":2,"mc_remaining_time":0.4166666667}}"#, 200.0));
        assert_eq!(s.play.layer_hi, 1);
        assert!((s.play.time - 25.0).abs() < 1e-6, "{}", s.play.time);
        // The clock runs, and waits at the end of the layer.
        assert!(place(&mut s, 203.0));
        assert!(s.follow.head_moves, "the view draws at the head's pace, with nothing that waits for it to stand");
        assert!((s.play.time - 28.0).abs() < 1e-6);
        assert!(!place(&mut s, 260.0), "waits for the next report");
        assert!(!s.follow.head_moves);
        assert!(s.play.time < 30.0 && s.play.time > 29.9);
        // The next layer starts at its first move.
        report(&mut s, &mut r, r#"{"print":{"msg":1,"layer_num":3}}"#, 261.0);
        assert_eq!((s.play.layer_hi, s.play.time), (2, 30.0));
        // Paused: the nozzle holds.
        report(&mut s, &mut r, r#"{"print":{"msg":1,"gcode_state":"PAUSE"}}"#, 262.0);
        assert!(!place(&mut s, 270.0));
        assert_eq!(s.play.time, 31.0);
        // Out of Monitor: the slider is the user's again.
        s.follow.monitor = false;
        assert!(s.following().is_none());
        s.follow.monitor = true;
        place(&mut s, 270.0);
        apply(&mut s, &PrintAction::SetTopLayer(0));
        assert!(!s.follow.on && s.following().is_none(), "manual playback ends following");
        assert_eq!(s.play.layer_hi, 0);
        apply(&mut s, &PrintAction::FollowPrinter(true));
        assert_eq!(s.play.layer_hi, 2, "following again jumps to the printer's layer");
    }

    /// The plate's model may arrive after Monitor framed its job: the framing goes to the view
    /// Design comes back to.
    #[test]
    fn a_plate_arriving_behind_monitors_job_frames_design_not_the_job() {
        use crate::api::{AppState, PaneId, ShellAction, Workspace};
        let mut state = AppState::default();
        assert!(state.apply_core(&ShellAction::SetWorkspace(Workspace::Monitor)));
        state.print.follow.monitor = true;
        state.print.follow.jobs.push(sent("block", preview(), 0.0));
        let job = *state.view.camera(PaneId::PRIMARY);
        let scene = crate::api::Scene::from_model(crate::model::fixture::synthetic_model(20_000, 40), &mut |_| {});
        state.update_scene(Arc::new(scene), true);
        assert_eq!(*state.view.camera(PaneId::PRIMARY), job);
        assert!(state.apply_core(&ShellAction::SetWorkspace(Workspace::Design)));
        assert_ne!(*state.view.camera(PaneId::PRIMARY), job, "Design's view fits the plate");
    }

    #[test]
    fn the_log_says_which_job_a_report_went_to_only_when_that_changes() {
        let mut s = PrintState::default();
        s.follow.jobs_read = true;
        s.follow.jobs.push(sent("block", preview(), 0.0));
        let mut r = Reports::default();
        report(&mut s, &mut r, r#"{"print":{"msg":0,"gcode_state":"RUNNING","subtask_name":"block","layer_num":3,"total_layer_num":4}}"#, 1.0);
        let line = match_line(None, &s.follow).expect("a new job says so");
        assert!(line.starts_with("the printer is on block, layer 3 / 4: job id-block"), "{line}");
        assert!(line.contains("Printing, 4 layers"), "{line}");
        let was = s.follow.job.clone();
        assert_eq!(match_line(was.as_ref(), &s.follow), None, "the same match");
        report(&mut s, &mut r, r#"{"print":{"msg":0,"gcode_state":"RUNNING","subtask_name":"block","layer_num":3,"total_layer_num":80}}"#, 2.0);
        let line = match_line(was.as_ref(), &s.follow).expect("no match now");
        assert!(line.contains("no slice: Fab has no copy of this job"), "{line}");
        assert!(line.ends_with("saved under that name: [4 layers Printing]"), "{line}");
    }

    #[test]
    fn a_job_fab_did_not_commit_says_so() {
        let mut s = PrintState::default();
        s.set_preview(preview());
        s.follow.jobs_read = true;
        s.follow.monitor = true;
        let mut r = Reports::default();
        report(&mut s, &mut r, r#"{"print":{"msg":0,"gcode_state":"RUNNING","subtask_name":"vase","layer_num":12,"total_layer_num":80,"mc_percent":37,"mc_remaining_time":83}}"#, 1.0);
        let job = s.follow.job.as_ref().unwrap();
        assert!(job.preview.is_none() && job.unmatched.is_some());
        assert_eq!((job.layer_text(), job.status_text()), ("12 / 80".to_string(), "Printing  ·  37 %  ·  1:23:00 left".to_string()));
        assert!(s.following().is_none());
        // Same name, other layer count: not the same file. The plate's slice is never a copy.
        s.follow.jobs.push(sent("block", preview(), 0.0));
        report(&mut s, &mut r, r#"{"print":{"msg":0,"gcode_state":"RUNNING","subtask_name":"block","layer_num":2,"total_layer_num":80}}"#, 2.0);
        assert!(s.follow.job.as_ref().unwrap().preview.is_none() && s.following().is_none());
        report(&mut s, &mut r, r#"{"print":{"msg":1,"total_layer_num":4}}"#, 3.0);
        assert!(s.following().is_some_and(|p| p.id == 7));
        // Finished: nothing to follow.
        report(&mut s, &mut r, r#"{"print":{"msg":1,"gcode_state":"FINISH"}}"#, 4.0);
        assert!(s.follow.job.is_none());
    }

    /// A job Fab has no copy of shows no slice at all, followed or not.
    #[test]
    fn a_job_fab_has_no_copy_of_shows_no_preview() {
        let mut s = PrintState::default();
        s.set_preview(preview());
        s.play.active = true;
        s.follow.monitor = true;
        let mut r = Reports::default();
        report(&mut s, &mut r, r#"{"print":{"msg":0,"gcode_state":"RUNNING","subtask_name":"vase","layer_num":12,"total_layer_num":80}}"#, 1.0);
        assert!(s.monitor_holds() && s.monitor_preview().is_none());
        assert!(s.showing().is_none());
        s.follow.on = false;
        assert!(s.showing().is_none());
    }

    #[test]
    fn monitor_shows_the_job_that_was_sent_whatever_the_plate_does() {
        let mut s = PrintState::default();
        s.set_preview(preview());
        s.play.active = true;
        let job = preview();
        s.follow.jobs.push(sent("block", job.clone(), 100.0));
        let sent = job;
        s.follow.monitor = true;
        s.follow.hidden_play = s.play.clone();
        // On its way: the whole job, nothing printed.
        place(&mut s, 100.0);
        assert!(Arc::ptr_eq(s.showing().unwrap(), &sent));
        assert_eq!((s.follow.live, s.play.time, s.play.layer_hi), (0.0, 50.0, 3));
        assert!(view_text(&s, &sent).contains("on its way"));

        // Printing: the head on its layer, by Fab's timing, and it says so.
        let mut r = Reports::default();
        assert!(report(&mut s, &mut r, r#"{"print":{"msg":0,"gcode_state":"RUNNING","stg_cur":0,"subtask_name":"block","layer_num":2,"total_layer_num":4,"mc_remaining_time":0.4166666667}}"#, 200.0));
        assert_eq!(s.play.layer_hi, 1);
        assert!((s.follow.live - 25.0).abs() < 1e-6);
        assert!(view_text(&s, &sent).contains("estimated"), "{}", view_text(&s, &sent));

        // Looking ahead: the view stays there, the head moves on and frames keep coming.
        apply(&mut s, &PrintAction::SetTopLayer(3));
        assert!(s.following().is_none() && Arc::ptr_eq(s.showing().unwrap(), &sent));
        let looked = s.play.time;
        assert!(place(&mut s, 203.0), "the head moves while the view looks ahead");
        assert!((s.follow.live - 28.0).abs() < 1e-6, "{}", s.follow.live);
        assert_eq!((s.play.layer_hi, s.play.time), (3, looked));
        assert_eq!(view_text(&s, &sent), "Looking ahead at layer 4: the printer is on layer 2 / 4, the head shows where it is");
        apply(&mut s, &PrintAction::SetTopLayer(0));
        assert!(view_text(&s, &sent).starts_with("Looking back at layer 1:"));
        apply(&mut s, &PrintAction::SetTopLayer(3));

        // The plate slices on (Design's business): the job, its layers and its view don't move,
        // Design's playback does.
        let looked = s.play.clone();
        let design = preview_of(9);
        s.time_preview(preview_of(7));
        s.stream_preview(preview_of(8));
        s.set_preview(design.clone());
        s.stale = true;
        assert!(Arc::ptr_eq(s.showing().unwrap(), &sent));
        assert_eq!((s.play.layer_hi, s.play.time, s.play.playing), (looked.layer_hi, looked.time, looked.playing));
        assert_eq!(s.follow.hidden_play.layer_hi, 8);
        assert!(s.markers().is_none(), "the plate's issue markers don't show over the job");
        report(&mut s, &mut r, r#"{"print":{"msg":1,"layer_num":3}}"#, 204.0);
        assert!(Arc::ptr_eq(s.follow.job.as_ref().unwrap().preview.as_ref().unwrap(), &sent));

        // Back to live, in one action.
        apply(&mut s, &PrintAction::FollowPrinter(true));
        assert!(s.following().is_some());
        assert_eq!(s.play.layer_hi, 2);
        assert!((30.0..40.0).contains(&s.play.time), "{}", s.play.time);
        report(&mut s, &mut r, r#"{"print":{"msg":1,"layer_num":3,"gcode_state":"PAUSE"}}"#, 205.0);

        // Over: the job stays with how far it got, until its card is closed.
        report(&mut s, &mut r, r#"{"print":{"msg":1,"gcode_state":"IDLE"}}"#, 210.0);
        assert!(s.follow.job.is_none() && Arc::ptr_eq(s.showing().unwrap(), &sent));
        assert_eq!(s.play.layer_hi, 2, "what is on the plate");
        assert!((30.0..40.0).contains(&s.follow.live), "{}", s.follow.live);
        assert_eq!(s.play.time, s.follow.live);
        assert_eq!(view_text(&s, &sent), "block  ·  printed up to layer 3 of 4");
        apply(&mut s, &PrintAction::CloseJob);
        assert!(!s.monitor_holds());
        assert_eq!(s.play.layer_hi, 8, "closed, the view is what the other workspaces show");
        assert!(Arc::ptr_eq(s.showing().unwrap(), &design));
    }

    /// Monitor without a job shows the plate's slice, and each new one.
    #[test]
    fn an_idle_monitor_moves_with_the_plates_slice() {
        let mut s = PrintState::default();
        s.set_preview(preview());
        s.play.active = true;
        s.follow.monitor = true;
        s.follow.hidden_play = s.play.clone();
        s.set_preview(preview_of(9));
        assert_eq!((s.play.layer_hi, s.follow.hidden_play.layer_hi), (8, 8));
        assert!(s.showing().is_some_and(|p| p.max_layer() == 8));
    }

    #[test]
    fn reported_upload_name_matches_the_saved_job() {
        assert!(same_name("Block", &fab_printer_link::job::plain_file_name("Block"), true));
        assert!(!same_name("Other", &fab_printer_link::job::plain_file_name("Block"), true));
        assert!(!same_name("Block", "Block", true), "a numbered printer names the uploaded file");
        assert!(same_name("Block", "Block.gcode.3mf", false));
        // The first family's names are its own: no upload-name match, so no collisions.
        assert!(!same_name("Block", &fab_printer_link::job::plain_file_name("Block"), false));
        assert!(!same_name("a b", "a-b.gcode", false));
    }
}
