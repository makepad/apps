//! `FabBusyLine`: one job of the process's job board (`makepad_jobs`) in a panel's own fixed
//! slot. While the job runs it shows what the job is doing in words (its stage: the panel says
//! whose job it is), how far it has got (a bar, where the work can be counted), how long it has
//! run, and a Cancel. When the job is idle the slot shows the panel's last result line, with that panel's tooltip, and an optional action
//! button ("Refine"). The caption's job strip reads the same board, so the job shows in both
//! places without being reported twice. The height never changes, so a job starting or ending
//! moves nothing else in the panel.
//!
//! Main-loop cost: on `Event::Signal` it compares the board's sequence number, and only redraws
//! when that number moved. While a job runs, a one-second timer updates the elapsed time.

use makepad_widgets_core::jobs::{board, JobEnd, JobId};
use makepad_widgets_core::job_strip::job_time;
use makepad_widgets_core::tip::TipWidgetRefExt;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabBusyLineBase = #(FabBusyLine::register_widget(vm))
    mod.widgets.FabBusyLine = set_type_default() do mod.widgets.FabBusyLineBase{
        width: Fill
        height: 30
        flow: Down
        spacing: 5
        head := View{
            width: Fill
            height: 20
            flow: Right
            spacing: 6
            align: Align{y: 0.5}
            // the idle line's tip can run to several lines (how a result was made)
            tip := mod.widgets.FabTip{
                width: Fill
                wrap_width: 360.
                text: ""
                // it reports the pointer over it, so its tip shows on hover
                words := mod.widgets.FabLabel{ width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis hover_actions_enabled: true }
            }
            elapsed := mod.widgets.FabLabelDim{ width: Fit text: "" }
            act := mod.widgets.FabButton{ text: "" height: 20 visible: false }
        }
        bar := mod.widgets.FabProgress{ width: Fill height: 4 visible: false }
    }
}

/// What a press on the line asks of its panel.
#[derive(Clone, Debug, Default)]
pub enum BusyLineAction {
    #[default]
    None,
    /// The idle line's action button.
    Act,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabBusyLine {
    #[deref]
    view: View,
    /// The job shown while it runs, and what shows otherwise.
    #[rust]
    job: Option<JobId>,
    #[rust]
    idle: String,
    #[rust]
    tip: String,
    #[rust]
    action: Option<String>,
    #[rust]
    seen_seq: u64,
    #[rust]
    tick: Timer,
}

impl FabBusyLine {
    /// The job running now, if `job` is on the board and running.
    fn running(&self) -> Option<makepad_widgets_core::jobs::JobSnapshot> {
        self.job.and_then(|id| board().get(id)).filter(|j| j.end.is_running())
    }
}

impl Widget for FabBusyLine {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.tick.is_event(event).is_some() {
            self.tick = Timer::empty();
            self.view.redraw(cx);
        }
        if let Event::Signal = event {
            let seq = board().seq();
            if seq != self.seen_seq {
                self.seen_seq = seq;
                self.view.redraw(cx);
            }
        }
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if self.view.button(cx, ids!(act)).clicked(actions) {
            match self.running() {
                Some(j) => {
                    board().cancel(j.id);
                }
                None => cx.widget_action(self.widget_uid(), BusyLineAction::Act),
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let running = self.running();
        let (words, elapsed, act, fraction) = match &running {
            Some(j) => {
                let words = match (&j.end, j.cancelling) {
                    (JobEnd::Running, true) => "Cancelling...".to_string(),
                    // the panel says whose job it is: its stage in words, else its title
                    _ => {
                        let stage = j.stage_words();
                        let mut c = stage.chars();
                        match c.next() {
                            Some(first) => first.to_uppercase().chain(c).collect(),
                            None => j.title.clone(),
                        }
                    }
                };
                // tick the elapsed time while it runs
                if self.tick.is_empty() {
                    self.tick = cx.start_timeout(1.0);
                }
                (words, job_time(j.elapsed(makepad_widgets_core::jobs::now())), Some("Cancel".to_string()), j.fraction)
            }
            None => (self.idle.clone(), String::new(), self.action.clone(), None),
        };
        self.view.label(cx, ids!(words)).set_text(cx, &words);
        self.view.label(cx, ids!(elapsed)).set_text(cx, &elapsed);
        self.view.widget(cx, ids!(head)).tip(cx, ids!(tip)).set_text(if running.is_some() { "" } else { &self.tip });
        let button = self.view.button(cx, ids!(act));
        button.set_visible(cx, act.is_some());
        if let Some(a) = &act {
            button.set_text(cx, a);
        }
        let bar = self.view.view(cx, ids!(bar));
        bar.set_visible(cx, fraction.is_some());
        if let Some(f) = fraction {
            crate::ui::widgets::set_progress(cx, &bar, f as f32);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

impl FabBusyLineRef {
    /// Show `job` while it runs, else `idle` (its tooltip `tip`, and a button labelled
    /// `action` that sends [`BusyLineAction::Act`]).
    pub fn set(&self, cx: &mut Cx, job: Option<JobId>, idle: &str, tip: &str, action: Option<&str>) {
        let Some(mut inner) = self.borrow_mut() else { return };
        let action = action.map(str::to_string);
        if inner.job != job || inner.idle != idle || inner.tip != tip || inner.action != action {
            inner.job = job;
            inner.idle = idle.to_string();
            inner.tip = tip.to_string();
            inner.action = action;
            inner.view.redraw(cx);
        }
    }

    /// The idle line's action button was pressed.
    pub fn acted(&self, actions: &Actions) -> bool {
        matches!(actions.find_widget_action(self.widget_uid()).map(|a| a.cast()), Some(BusyLineAction::Act))
    }
}
