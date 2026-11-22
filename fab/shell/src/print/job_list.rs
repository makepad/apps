//! Saved prints in the Jobs editor and Monitor, with the printer and preparation first, then
//! the queue and history.
//!
//! Main-loop cost: a linear list scan on a draw (three passes, no sorting), retaining the row
//! buffer and allocating only row texts once it has room. Only visible rows draw; no previews
//! are loaded and no disk is touched.

use super::jobs::JobState;
use super::*;
use crate::api::AppState;
use std::fmt::Write;

/// One committed job in Monitor's list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobRow {
    pub id: String,
    pub name: String,
    pub state: String,
    pub detail: String,
    pub shown: bool,
}

/// Jobs in display order. Their source is already newest first; reverse only the queue.
pub fn rows(p: &PrintState, _now: f64) -> Vec<JobRow> {
    let mut out = Vec::with_capacity(p.follow.jobs.len());
    fill_rows(p, &mut out);
    out
}

fn fill_rows(p: &PrintState, out: &mut Vec<JobRow>) {
    out.clear();
    let printer = p.follow.job.as_ref();
    let shown = p.follow.shown().map(|j| j.meta.id.as_str());
    let group = |j: &jobs::CommittedJob| {
        if matches!(j.state, JobState::Preparing | JobState::Sent)
            || printer.is_some_and(|live| live.id.as_deref() == Some(j.meta.id.as_str()))
        {
            0
        } else if j.state == JobState::Queued {
            1
        } else {
            2
        }
    };
    let jobs = &p.follow.jobs;
    for job in jobs.iter().filter(|j| group(j) == 0)
        .chain(jobs.iter().rev().filter(|j| group(j) == 1))
        .chain(jobs.iter().filter(|j| group(j) == 2))
    {
        let meta = &job.meta;
        let state = if let Some(live) = printer.filter(|live| live.id.as_deref() == Some(meta.id.as_str())) {
            let mut state = format!("Layer {}", live.layer_text());
            if let Some(progress) = live.progress {
                let _ = write!(state, " · {progress:.0} %");
            }
            state
        } else {
            job.state.label()
        };
        let mut detail = String::new();
        if job.state.ended() && job.state != JobState::EndedUnseen {
            if let Some(ended) = job.ended_at {
                let _ = write!(detail, "Elapsed {} · ", format_time(ended - job.sent_at));
            }
        }
        let _ = write!(detail, "Planned {}", format_time(meta.planned));
        let grams: f64 = meta.filament_g.iter().sum();
        if grams > 0.0 {
            let _ = write!(detail, " · {grams:.1} g");
        }
        out.push(JobRow {
            id: meta.id.clone(), name: meta.name.clone(), state, detail,
            shown: shown == Some(meta.id.as_str()),
        });
    }
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabJobRowBase = #(FabJobRow::register_widget(vm))
    mod.widgets.FabJobRow = set_type_default() do mod.widgets.FabJobRowBase{
        width: Fill height: 40 flow: Down spacing: 2
        padding: Inset{left: 10 right: 8 top: 4 bottom: 4}
        show_bg: true
        // The Parts list's selected row, including its accent at the left edge.
        draw_bg +: {
            selected: instance(0.0)
            hover: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                sdf.fill(fab.color_editor.mix(fab.color_row_hover, self.hover * 0.6).mix(fab.color_selection_bg, self.selected))
                sdf.rect(0.0, 0.0, 2.0 * self.selected, self.rect_size.y)
                sdf.fill(fab.color_accent)
                return sdf.result
            }
        }
        title := mod.widgets.FabLabel{width: Fill max_lines: 1 text_overflow: TextOverflow.Ellipsis}
        detail := mod.widgets.FabLabelSmall{width: Fill max_lines: 1 text_overflow: TextOverflow.Ellipsis}
    }

    mod.widgets.FabJobListBase = #(FabJobList::register_widget(vm))
    mod.widgets.FabJobList = set_type_default() do mod.widgets.FabJobListBase{
        width: Fill height: Fit flow: Down
        fill: false
        heading := View{
            width: Fill height: 28 align: Align{y: 0.5} padding: Inset{left: 10}
            mod.widgets.FabLabelSmall{ text: "Jobs" }
        }
        list := PortalList{
            width: Fill height: Fill flow: Down auto_tail: false
            Row := mod.widgets.FabJobRow{}
        }
    }
}

const HEADING_H: f64 = 28.0;
const ROW_H: f64 = 40.0;
/// Monitor leaves the plate visible below these rows. The Jobs editor fills its area.
const MAX_ROWS: usize = 5;

#[derive(Script, ScriptHook, Widget)]
pub struct FabJobRow {
    #[deref]
    view: View,
    #[rust]
    id: String,
    #[rust]
    name: String,
    #[rust]
    shown: bool,
}

impl FabJobRow {
    fn sync(&mut self, cx: &mut Cx, row: &JobRow) {
        self.id.clone_from(&row.id);
        self.name = format!("{}: {}", row.name, row.state);
        self.shown = row.shown;
        self.view.label(cx, ids!(title)).set_text(cx, &self.name);
        self.view.label(cx, ids!(detail)).set_text(cx, &row.detail);
        self.view.draw_bg.set_dyn_instance(cx, live_id!(selected), &[if row.shown { 1.0 } else { 0.0 }]);
    }

    fn press(&self, cx: &mut Cx, scope: &mut Scope) {
        let Some(state) = scope.data.get::<AppState>() else { return };
        let follow = &state.print.follow;
        let back = follow.picked.as_deref() == Some(self.id.as_str())
            && follow.shown().is_some_and(|j| j.meta.id == self.id);
        cx.action(PrintAction::PickJob(if back { None } else { Some(self.id.clone()) }));
    }
}

impl Widget for FabJobRow {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::ListItem;
        makepad_platform::a11y::set_string(&mut node.name, &self.name);
        node.actions = a11y_tree::Actions::PRESS | a11y_tree::Actions::FOCUS;
        node.states = a11y_tree::States::FOCUSABLE;
        node.states.set(a11y_tree::States::SELECTED, self.shown);
    }

    fn handle_action(&mut self, cx: &mut Cx, action: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        if action.action != a11y_tree::Actions::PRESS {
            return a11y_tree::ActionResult::Unhandled;
        }
        self.press(cx, scope);
        a11y_tree::ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        match event.hits(cx, self.view.area()) {
            Hit::FingerDown(_) => cx.set_key_focus(self.view.area()),
            Hit::FingerUp(e) if e.was_tap() => self.press(cx, scope),
            Hit::KeyDown(e) if matches!(e.key_code, KeyCode::ReturnKey | KeyCode::Space) => self.press(cx, scope),
            Hit::FingerHoverIn(_) => {
                cx.set_cursor(MouseCursor::Hand);
                self.view.draw_bg.set_dyn_instance(cx, live_id!(hover), &[1.0]);
                self.view.redraw(cx);
            }
            Hit::FingerHoverOver(_) => cx.set_cursor(MouseCursor::Hand),
            Hit::FingerHoverOut(_) => {
                self.view.draw_bg.set_dyn_instance(cx, live_id!(hover), &[0.0]);
                self.view.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabJobList {
    #[deref]
    view: View,
    #[live]
    fill: bool,
    #[rust]
    rows: Vec<JobRow>,
}

impl FabJobList {
    fn sync(&mut self, state: &AppState) {
        fill_rows(&state.print, &mut self.rows);
    }
}

impl Widget for FabJobList {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Jobs");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if scope.data.get::<AppState>().is_some_and(|state| !state.print.follow.jobs.is_empty()) {
            self.view.handle_event(cx, event, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, mut walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        self.sync(state);
        if self.rows.is_empty() { return DrawStep::done(); }
        self.view.view(cx, ids!(heading)).set_visible(cx, !self.fill);
        if !self.fill {
            walk.height = Size::Fixed(HEADING_H + self.rows.len().min(MAX_ROWS) as f64 * ROW_H);
        }
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = step.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else { continue };
            list.set_item_range(cx, 0, self.rows.len());
            while let Some(index) = list.next_visible_item(cx) {
                let Some(row) = self.rows.get(index) else { continue };
                let item = list.item(cx, index, live_id!(Row));
                if let Some(mut widget) = item.borrow_mut::<FabJobRow>() {
                    widget.sync(cx, row);
                }
                item.draw_all(cx, scope);
            }
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::follow::PrinterJob;
    use super::super::jobs::{CommittedJob, JobMeta};

    fn job(id: &str, state: JobState, created: f64) -> CommittedJob {
        CommittedJob {
            meta: Arc::new(JobMeta { id: id.into(), name: id.into(), created, planned: 1912.0, ..Default::default() }),
            state,
            ..Default::default()
        }
    }

    #[test]
    fn active_then_queue_then_history() {
        let mut p = PrintState::default();
        p.follow.jobs = vec![
            job("unseen", JobState::EndedUnseen, 8.0),
            job("queue_new", JobState::Queued, 7.0),
            job("stopped", JobState::Stopped, 6.0),
            job("sent", JobState::Sent, 5.0),
            job("failed", JobState::Failed("Stopped by error".into()), 4.0),
            job("preparing", JobState::Preparing, 3.0),
            job("queue_old", JobState::Queued, 2.0),
            job("finished", JobState::Finished, 1.0),
        ];
        let listed = rows(&p, 100.0);
        assert_eq!(listed.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["sent", "preparing", "queue_old", "queue_new", "unseen", "stopped", "failed", "finished"],
            "active and ended jobs newest first, queue oldest first");
        for row in &listed {
            let committed = p.follow.job_of(&row.id).unwrap();
            assert_eq!(row.state, committed.state.label(), "saved state for {}", row.id);
        }
    }

    #[test]
    fn printer_layer_and_picked_job() {
        let mut p = PrintState::default();
        p.follow.jobs = vec![job("queued", JobState::Queued, 2.0), job("printing", JobState::Sent, 1.0)];
        p.follow.job = Some(PrinterJob {
            id: Some("printing".into()), layer: 12, total_layers: Some(100), progress: Some(40.0), ..Default::default()
        });
        let listed = rows(&p, 100.0);
        assert_eq!(listed[0].state, "Layer 12 / 100 · 40 %", "printer report supplies the live state");
        assert!(listed[0].shown && !listed[1].shown, "printer job shown by default");
        p.follow.picked = Some("queued".into());
        let listed = rows(&p, 100.0);
        assert!(!listed[0].shown && listed[1].shown, "picked job is shown");
        p.follow.picked = None;
        assert!(rows(&p, 100.0)[0].shown, "clearing the pick returns to the printer");
        p.follow.job = None;
        p.follow.closed = true;
        assert!(rows(&p, 100.0).iter().all(|r| !r.shown), "closed history has no shown row");
    }

    #[test]
    fn elapsed_planned_and_filament() {
        let mut p = PrintState::default();
        let mut ended = job("ended", JobState::Finished, 1.0);
        ended.sent_at = 10.0;
        ended.ended_at = Some(1820.0);
        Arc::make_mut(&mut ended.meta).filament_g = vec![1.5, 2.5];
        p.follow.jobs.push(ended);
        assert_eq!(rows(&p, 9999.0)[0].detail, "Elapsed 30:10 · Planned 31:52 · 4.0 g", "elapsed uses the end, and grams sum all slots");
        p.follow.jobs[0].state = JobState::EndedUnseen;
        assert_eq!(rows(&p, 9999.0)[0].detail, "Planned 31:52 · 4.0 g", "unseen end has no known elapsed time");
        p.follow.jobs[0].state = JobState::Finished;
        p.follow.jobs[0].ended_at = None;
        Arc::make_mut(&mut p.follow.jobs[0].meta).filament_g = vec![0.0, 0.0];
        assert_eq!(rows(&p, 9999.0)[0].detail, "Planned 31:52", "unknown elapsed and zero grams are omitted");
    }
}
