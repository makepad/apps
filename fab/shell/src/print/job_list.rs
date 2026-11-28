//! The Jobs editor and Monitor show active jobs first, the queue in print order, then history newest first.
//!
//! History and printing-job changes regroup job indices. Draw resolves the selected job once and builds labels only for visible portal rows.
//! Workers load the history and previews.

use super::jobs::{CommittedJob, JobState};
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

/// All rows in the same order as the Jobs editor and Monitor.
pub fn rows(p: &PrintState, _now: f64) -> Vec<JobRow> {
    let shown = p.follow.shown().map(|j| j.meta.id.as_str());
    let mut order = Vec::new();
    fill_order(&p.follow, &mut order);
    order.into_iter().map(|i| row(&p.follow.jobs[i], p, shown)).collect()
}

pub(super) fn fill_order(f: &follow::Follow, order: &mut Vec<usize>) {
    let len = f.jobs.len();
    order.resize(len, 0);
    let printing = f.job.as_ref().and_then(|live| live.id.as_deref());
    let mut active = 0;
    let mut history = len;
    // Walk oldest first. Fill history from the back and keep the queue in print order past len.
    for (i, job) in f.jobs.iter().enumerate().rev() {
        if matches!(job.state, JobState::Preparing | JobState::Sent) || printing == Some(job.meta.id.as_str()) {
            order[active] = i;
            active += 1;
        } else if job.state == JobState::Queued {
            order.push(i);
        } else {
            history -= 1;
            order[history] = i;
        }
    }
    order[..active].reverse();
    order.copy_within(len.., active);
    order.truncate(len);
}

#[derive(Default)]
pub(super) struct JobOrder {
    indices: Vec<usize>,
    key: Option<(u64, Option<String>)>,
}

impl JobOrder {
    pub(super) fn indices(&mut self, f: &follow::Follow) -> &[usize] {
        let rev = f.jobs.rev();
        let printing = f.job.as_ref().and_then(|j| j.id.as_deref());
        if !self.key.as_ref().is_some_and(|(r, id)| *r == rev && id.as_deref() == printing) {
            fill_order(f, &mut self.indices);
            self.key = Some((rev, printing.map(str::to_owned)));
        }
        &self.indices
    }
}

fn row(job: &CommittedJob, p: &PrintState, shown: Option<&str>) -> JobRow {
    let meta = &job.meta;
    let state = if let Some(live) = p.follow.job.as_ref().filter(|live| live.id.as_deref() == Some(meta.id.as_str())) {
        let mut state = format!("Layer {}", live.layer_text());
        if let Some(progress) = live.progress {
            let _ = write!(state, " · {progress:.0} %");
        }
        state
    } else {
        if p.unwatched_while_virtual(job) { "Not watched while Virtual".into() } else { job.state.label() }
    };
    let mut detail = String::new();
    if let Some(id) = &meta.printer {
        if let Some(name) = id.strip_prefix("virtual:") {
            use makepad_fab_settings::family::{VIRTUAL_NOZZLES, VIRTUAL_TOOLCHANGER};
            // Generated machines carry their virtual printer's id in the model. A twin
            // carries its catalogue model, regardless of the name in the saved record.
            let generated = meta.printer_model.rsplit_once(' ').is_some_and(|(family, id)|
                id == name && matches!(family, VIRTUAL_NOZZLES | VIRTUAL_TOOLCHANGER));
            let name = if meta.virtual_printer && !meta.machine_preset.is_empty() && !meta.printer_model.is_empty() && !generated {
                &meta.machine_preset
            } else { name };
            let mode = if meta.virtual_printer { " (virtual)" } else { "" };
            let _ = write!(detail, "{name}{mode} · ");
        } else if let Some(name) = id.strip_prefix("printer:") {
            let mode = if meta.virtual_printer { " (Virtual)" } else { "" };
            let _ = write!(detail, "{name}{mode} · ");
        }
    }
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
    JobRow {
        id: meta.id.clone(), name: meta.name.clone(), state, detail,
        shown: shown == Some(meta.id.as_str()),
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

    // One printer's jobs in the Printers editor's Jobs tab, in the Jobs list's order: rows made
    // from `Row`, one per job, and kept.
    mod.widgets.FabJobRowsBase = #(FabJobRows::register_widget(vm))
    mod.widgets.FabJobRows = set_type_default() do mod.widgets.FabJobRowsBase{
        width: Fill height: Fit flow: Down
        Row := mod.widgets.FabJobRow{}
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
    order: JobOrder,
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
        let order = self.order.indices(&state.print.follow);
        let len = order.len();
        if len == 0 { return DrawStep::done(); }
        let shown = state.print.follow.shown().map(|j| j.meta.id.clone());
        self.view.view(cx, ids!(heading)).set_visible(cx, !self.fill);
        if !self.fill {
            walk.height = Size::Fixed(HEADING_H + len.min(MAX_ROWS) as f64 * ROW_H);
        }
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = step.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else { continue };
            list.set_item_range(cx, 0, len);
            while let Some(index) = list.next_visible_item(cx) {
                let Some(&job_index) = order.get(index) else { continue };
                let Some(state) = scope.data.get::<AppState>() else { continue };
                let f = &state.print.follow;
                let row = row(&f.jobs[job_index], &state.print, shown.as_deref());
                let item = list.item(cx, index, live_id!(Row));
                if let Some(mut widget) = item.borrow_mut::<FabJobRow>() {
                    widget.sync(cx, &row);
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
    use super::super::jobs::JobMeta;

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
        ].into();
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
        p.follow.jobs = vec![job("queued", JobState::Queued, 2.0), job("printing", JobState::Sent, 1.0)].into();
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
        p.follow.jobs.at_mut(0).state = JobState::EndedUnseen;
        assert_eq!(rows(&p, 9999.0)[0].detail, "Planned 31:52 · 4.0 g", "unseen end has no known elapsed time");
        {
            let mut job = p.follow.jobs.at_mut(0);
            job.state = JobState::Finished;
            job.ended_at = None;
            Arc::make_mut(&mut job.meta).filament_g = vec![0.0, 0.0];
        }
        assert_eq!(rows(&p, 9999.0)[0].detail, "Planned 31:52", "unknown elapsed and zero grams are omitted");
    }
}

/// Which printer's jobs a [`FabJobRows`] lists: one printer's (its id, `printer:…` /
/// `virtual:…`), or those of none of `listed` (jobs of printers no longer set up, and without one).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum JobsOf {
    #[default]
    Nobody,
    Printer(String),
    Others(Vec<String>),
}

impl JobsOf {
    fn takes(&self, job: &CommittedJob) -> bool {
        match self {
            JobsOf::Nobody => false,
            JobsOf::Printer(id) => job.meta.printer.as_deref() == Some(id.as_str()),
            JobsOf::Others(listed) => job.meta.printer.as_ref().is_none_or(|id| !listed.contains(id)),
        }
    }
}

/// How many jobs `of` has, and whether one of them is on its way or printing. O(jobs).
pub fn jobs_count(p: &PrintState, of: &JobsOf) -> (usize, bool) {
    let printing = p.follow.job.as_ref().and_then(|live| live.id.as_deref());
    let mut count = 0;
    let mut active = false;
    for job in p.follow.jobs.iter().filter(|j| of.takes(j)) {
        count += 1;
        active |= matches!(job.state, JobState::Preparing | JobState::Sent) || printing == Some(job.meta.id.as_str());
    }
    (count, active)
}

/// One printer's jobs, a row each (the Jobs list's rows: a press shows that job), active first,
/// then the queue, then history. Rows are made from the `Row` template and kept by job.
#[derive(Script, Widget)]
pub struct FabJobRows {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[layout] layout: Layout,
    /// Whose jobs.
    #[rust] pub of: JobsOf,
    #[rust] templates: std::collections::HashMap<LiveId, ScriptObjectRef>,
    #[rust] items: ComponentMap<LiveId, WidgetRef>,
    #[rust] order: Vec<usize>,
    /// The rows drawn last.
    #[rust] shown: Vec<LiveId>,
    #[redraw] #[rust] area: Area,
}

impl ScriptHook for FabJobRows {
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
            if let Some(template) = self.templates.get(&live_id!(Row)) {
                let v: ScriptValue = template.as_object().into();
                for (_, w) in self.items.iter_mut() {
                    w.script_apply(vm, apply, scope, v);
                }
            }
        }
    }
}

impl FabJobRows {
    fn item(&mut self, cx: &mut Cx, id: LiveId) -> Option<WidgetRef> {
        if let Some(w) = self.items.get(&id) {
            return Some(w.clone());
        }
        let value: ScriptValue = self.templates.get(&live_id!(Row))?.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        cx.widget_tree_insert_child(self.uid, id, w.clone());
        self.items.insert(id, w.clone());
        Some(w)
    }
}

impl Widget for FabJobRows {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Jobs");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        for id in &self.shown {
            if let Some(w) = self.items.get(id) {
                w.handle_event(cx, event, scope);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, self.layout);
        self.shown.clear();
        if let Some(state) = scope.data.get::<AppState>() {
            let p = &state.print;
            fill_order(&p.follow, &mut self.order);
            let shown = p.follow.shown().map(|j| j.meta.id.clone());
            let order = std::mem::take(&mut self.order);
            for &i in &order {
                let job = &p.follow.jobs[i];
                if !self.of.takes(job) { continue; }
                let id = LiveId::from_str(&job.meta.id);
                let Some(w) = self.item(cx, id) else { continue };
                if let Some(mut widget) = w.borrow_mut::<FabJobRow>() {
                    widget.sync(cx, &row(job, p, shown.as_deref()));
                }
                // (a row needs nothing of the app's state to draw)
                w.draw_all(cx, &mut Scope::empty());
                self.shown.push(id);
            }
            self.order = order;
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}
