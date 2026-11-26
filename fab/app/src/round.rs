//! Round, Chamfer, Pull, Hole and Shell each take one number, dragged at a handle on the pick or
//! typed in its box. The worker reads the plan from the picks ([`fab_ai::round::Plan`]); its template
//! writes the changes for any value, so each drag step previews the batch the release commits
//! (one `FabModel::apply`, one undo step), and the AI's `fab.round` and `fab.pull` write the same.
//!
//! The command shows as a parameter of the part it is on (`<instance>/@round`,
//! [`fab::params::CommandView`]): the part's handles give way to its arrow, which drags out of the
//! material from the pick, one millimetre of value per millimetre. A part drawn from its field
//! follows the drag in its field (compiled live, nothing meshed); any other builds a draft of the
//! batch on the model worker, the newest value only. After its first commit a round that wrote a
//! new statement drags that statement's own value; a pull is planned again where its face is now
//! (its value the face's travel from there). Esc ends it.
//! Repeat Last remembers the person's successful commit and sends that value through the same
//! handle commit when the worker has planned the command on the new picks.
//!
//! The kernel's verdict is the drag's stop, as for a part's value ([`fab::params::DragCap`]): past
//! a value whose draft was refused the drag holds at the last that built, and the values between
//! are tried; a value let go before its draft said is committed by the worker, or, refused, the
//! nearest that builds ([`model_host::Job::Command`]), and the view says where it was held.
//!
//! Cost: nothing at rest; per drag step one change written and the existing preview or live
//! compile; the command's handle is one more parameter drawn. The plan is one worker call when the
//! command starts. A refused value costs its draft and up to a dozen more on the worker (the
//! search halves the span to the value's precision); the UI thread does no more.

use super::*;
use fab::params::{ParamAction, ParamKind, ParamValue};
use fab::tools::picks::Repeat;
use fab_ai::round::Op;

/// The command's site in its key.
pub(super) const SITE: &str = "@round";

/// How long the arrow is at value 0, mm along the way out: it is there to grab.
const ARROW_MM: f64 = 4.0;

/// The name the strip and palette send for this operation.
fn command_name(op: Op) -> &'static str {
    match op {
        Op::Round => "round",
        Op::Chamfer => "chamfer",
        Op::Pull => "pull",
        Op::Hole => "hole",
        Op::Shell => "shell",
    }
}

/// The worker operation for a pick command, including one saved for Repeat Last.
pub(super) fn operation(command: &str) -> Option<Op> {
    [Op::Round, Op::Chamfer, Op::Pull, Op::Hole, Op::Shell].into_iter().find(|&op| command_name(op) == command)
}

/// The command while it lasts.
pub(super) struct RoundCmd {
    op: Op,
    plan: fab_ai::round::Plan,
    /// The key its handle's value goes by.
    key: String,
    /// The commit on its way: its call id and the value it writes.
    commit: Option<(u64, f64)>,
}

impl RoundCmd {
    fn kind(&self) -> ParamKind {
        if self.plan.template.kind == fab_model::params::Kind::Radius { ParamKind::Radius } else { ParamKind::Length }
    }

    fn decimals(&self) -> usize {
        fab::params::precision(self.kind(), self.plan.step)
    }
}

impl App {
    /// Start a command on the picks. True when a new plan was requested from the worker.
    pub(super) fn round_start(&mut self, cx: &mut Cx, op: Op) -> bool {
        self.round_again = None;
        if self.state.picks.list.is_empty() {
            let hint = match op {
                Op::Pull => "Pick a face of an exact part to pull first (E)",
                Op::Hole => "Pick the spot on a flat face of an exact part to drill first (E)",
                Op::Shell => "Pick the faces of an exact part to leave open first (E)",
                Op::Round | Op::Chamfer => "Pick what to round first (E): an edge, or a crease or an edit's edges on a part drawn from its field",
            };
            fab::tools::session::with(|s| s.hint = hint.into());
            fab::ui::redraw(cx, &[ids!(tool_overlay)]);
            return false;
        }
        self.round_end(cx);
        let Some(host) = &mut self.host else { return false };
        self.round_ask = Some((host.round_plan(self.state.picks.list.iter().map(|p| p.id.clone()).collect(), op), op));
        true
    }

    /// The worker's plan for the round asked as `id`.
    pub(super) fn round_planned(&mut self, cx: &mut Cx, id: u64, plan: &Result<fab_ai::round::Plan, String>) {
        let Some((_, op)) = self.round_ask.filter(|(ask, _)| *ask == id) else { return };
        self.round_ask = None;
        let plan = match plan {
            Ok(plan) => plan.clone(),
            Err(e) => {
                self.round_again = None;
                fab::tools::session::with(|s| s.hint = e.clone());
                fab::ui::redraw(cx, &[ids!(tool_overlay)]);
                return;
            }
        };
        let instance = plan.object.split('/').next().unwrap_or(&plan.object).to_string();
        let key = format!("{instance}/{SITE}");
        // its part selected, in Edit shape mode: where its handle shows
        if let Some(e) = self.state.plate.parts.iter().find(|p| p.object == plan.object).and_then(|p| p.element) {
            self.state.scene_state.select_only(e);
        }
        cx.action(ShellAction::SetTool(Tool::Select));
        cx.action(StageAction::Gizmo(GizmoMode::Shape));
        fab::tools::session::with(|s| s.hint = String::new());
        // planned again after a commit (a pull, where its face is now): its box stays as it was
        if !self.round.as_ref().is_some_and(|r| r.op == op && r.plan.picks == plan.picks) {
            self.round_rev += 1;
        }
        let r = self.round.insert(RoundCmd { op, plan, key, commit: None });
        let again = self.round_again.take_if(|(again, _)| *again == op).map(|(_, value)| ParamAction::Commit {
            object: r.plan.object.clone(),
            key: r.key.clone(),
            value: ParamValue::Num(value.clamp(r.plan.min, r.plan.max)),
        });
        self.round_view(cx);
        if let Some(commit) = again {
            self.round_param(cx, &commit);
        }
    }

    /// The command's parameter and handle as the view draws them, from its plan.
    fn round_view(&mut self, cx: &mut Cx) {
        let Some(r) = &self.round else {
            self.state.params.command = None;
            cx.redraw_all();
            return;
        };
        let p = &r.plan;
        let on = |q: [f64; 3]| fab::tools::picks::on_plate(&self.state, &p.object, q);
        let along = |k: f64| [p.at[0] + p.dir[0] * k, p.at[1] + p.dir[1] * k, p.at[2] + p.dir[2] * k];
        let handle = match (on(p.at), on(along(1.0)), on(along(ARROW_MM + p.value))) {
            (Some(from), Some(one), Some(to)) => {
                let rate = ((one[0] - from[0]).powi(2) + (one[1] - from[1]).powi(2) + (one[2] - from[2]).powi(2)).sqrt();
                Some(fab::params::ParamHandle::Linear { from, to, rate })
            }
            _ => None,
        };
        let param = fab::params::ParamView {
            key: r.key.clone(),
            label: p.label.clone(),
            group: String::new(),
            kind: r.kind(),
            unit: "mm".into(),
            value: fab::params::ParamValue::Num(p.value),
            formula: None,
            links: false,
            min: p.min,
            max: p.max,
            // a command's range is its kernel's: values past it do not build
            limits: [Some(p.min), Some(p.max)],
            step: p.step,
            choices: Vec::new(),
            declared: false,
            derived: false,
            detail: false,
            hint: p.says.replace('`', ""),
            objects: vec![p.object.clone()],
            handle,
            site: None,
            setting: None,
        };
        self.state.params.command = Some(fab::params::CommandView { object: p.object.clone(), param, says: p.says.replace('`', ""), rev: self.round_rev });
        cx.redraw_all();
    }

    /// The command ends (Esc, another command, the plate switched): its handle goes.
    pub(super) fn round_end(&mut self, cx: &mut Cx) {
        self.round_again = None;
        if self.round.is_none() && self.round_ask.is_none() && self.state.params.command.is_none() {
            return;
        }
        self.round_ask = None;
        if self.step_view.command_draft {
            if let Some(host) = &mut self.host {
                self.preview_floor = host.cancel_preview();
            }
            self.restore_committed(cx);
        }
        if let Some(r) = self.round.take() {
            if self.drag_cap.as_ref().is_some_and(|c| c.key() == r.key) {
                self.forget_drag();
            }
            self.state.params.pending.retain(|(k, _)| *k != r.key);
            if self.state.params.dragging.as_ref().is_some_and(|(k, _)| *k == r.key) {
                self.state.params.dragging = None;
                live_slice::set_drag_live(false);
            }
        }
        self.round_view(cx);
    }

    /// The picks changed: a pick the person added that the round is not on ends it (a pick the
    /// build lost does not: a rounded edge is gone, its round's value still drags).
    pub(super) fn round_picks(&mut self, cx: &mut Cx) {
        let Some(r) = &self.round else { return };
        if self.state.picks.list.iter().any(|p| !r.plan.picks.contains(&p.id)) {
            self.round_end(cx);
        }
    }

    fn suspend_step_picking(&mut self) {
        if self.state.steps.marker.is_some() && !self.step_view.command_draft {
            self.step_view.command_draft = true;
            self.step_view.gen += 1;
            self.step_view.committed_gen = None;
            self.state.steps.installed = None;
            self.forget_view_points();
            self.filter_view_picks();
        }
    }

    /// A value action of the command's handle or box: previewed or committed here. False: it is
    /// not the command's.
    pub(super) fn round_param(&mut self, cx: &mut Cx, a: &fab::params::ParamAction) -> bool {
        use fab::params::ParamAction as P;
        let Some(r) = &self.round else { return false };
        match a {
            P::Preview { object, key, value } if *key == r.key => {
                let Some(v) = value.num() else { return true };
                let mut changes = r.plan.template.changes(v);
                let start = fab::params::ParamValue::Num(r.plan.value);
                let key = key.clone();
                // The drag learns how far the command builds, as a part's value's drag does: past a
                // value the kernel refused it holds at the last that built, and the values between
                // are tried (`cap_step`), so it stops where the kernel says, on any part.
                if self.drag_cap.as_ref().is_none_or(|c| c.key() != key.as_str()) {
                    self.forget_drag();
                    self.shown_ok = Some((key.clone(), start.clone()));
                    self.drag_cap = Some(fab::params::DragCap::new(&key, &start));
                }
                if let Some(hold) = self.drag_cap.as_ref().and_then(|c| c.blocked(value)) {
                    self.state.params.cap(&key, hold, Cx::time_now(), true);
                    cx.redraw_all();
                    return true;
                }
                self.state.params.dragging = Some((key.clone(), value.clone()));
                live_slice::set_drag_live(true);
                self.state.params.set_pending(&key, value.clone());
                // (a field part follows one change live; a batch of several builds a draft)
                self.suspend_step_picking();
                if self.state.steps.marker.is_none() && self.field_drawn(object) && changes.len() == 1 {
                    self.request_live_with(&key, value, changes.pop());
                } else if let Some(host) = &mut self.host {
                    let gen = host.preview_changes(key.clone(), param_view::model_value(value), changes, self.state.steps.marker.is_some());
                    self.preview_asks.insert(gen, (key, value.clone()));
                    self.preview_latest = gen;
                }
                cx.redraw_all();
                true
            }
            P::Commit { object, key, value } if *key == r.key => {
                let Some(mut v) = value.num() else { return true };
                // let go past where the drag found the kernel's stop: the last value that built
                let cap = self.drag_cap.take().filter(|c| c.key() == key.as_str());
                if let Some(hold) = cap.as_ref().and_then(|c| c.blocked(value)).and_then(|h| h.num()) {
                    v = hold;
                }
                // let go before its draft said: refused, the worker searches back toward the
                // nearest value the drag saw build (or where the model is) for the kernel's stop
                let good = cap.as_ref().and_then(|c| c.hold_for(value).num()).unwrap_or(r.plan.value);
                let (from, template) = (r.plan.value, r.plan.template.clone());
                let decimals = r.decimals();
                self.held = None;
                self.shown_ok = None;
                let key = key.clone();
                self.state.params.dragging = None;
                live_slice::set_drag_live(false);
                // a value where the model is (a box closed unchanged) commits nothing
                let unchanged = (v - r.plan.value).abs() < 1e-9 || (r.op != Op::Pull && v <= 0.0 && r.plan.then.is_some());
                if unchanged {
                    self.state.params.pending.retain(|(k, _)| *k != key);
                    self.param_action(cx, &P::Cancel { object: object.clone() });
                    return true;
                }
                let changes = r.plan.template.changes(v);
                let value = &fab::params::ParamValue::Num(v);
                self.state.params.set_pending(&key, value.clone());
                self.suspend_step_picking();
                let field = self.state.steps.marker.is_none() && self.field_drawn(object) && changes.len() == 1;
                if field {
                    self.request_live_with(&key, value, changes.first().cloned());
                } else {
                    // the view waits for a mesh part's rebuild; a field part's view is current
                    self.state.params.rebuilding = Some(fab::params::Rebuilding { object: object.clone(), since: Cx::time_now() });
                }
                self.preview_asks.clear();
                let Some(host) = &mut self.host else { return true };
                self.preview_floor = host.cancel_preview();
                let id = if field { host.edit(changes) } else { host.command(template, v, from, good, decimals) };
                if let Some(r) = &mut self.round {
                    r.commit = Some((id, v));
                }
                cx.redraw_all();
                true
            }
            _ => false,
        }
    }

    /// The changes the command's value `v` writes, when `key` is its handle's: a drag's probe of
    /// the values between one that built and one that did not builds them as its draft.
    pub(super) fn round_changes(&self, key: &str, v: f64) -> Option<Vec<fab_model::Change>> {
        self.round.as_ref().filter(|r| r.key == key).map(|r| r.plan.template.changes(v))
    }

    /// The command's parameter as its handle shows it, when `key` is its handle's.
    pub(super) fn round_param_view(&self, key: &str) -> Option<&fab::params::ParamView> {
        self.state.params.command.as_ref().map(|c| &c.param).filter(|p| p.key == key)
    }

    /// A worker job ended: the command's commit, when it is that. Committed, the value is the
    /// script's own (a new statement's radius drags from here); refused, the view says why.
    pub(super) fn round_done(&mut self, cx: &mut Cx, done: &model_host::Done) {
        let Some(r) = &mut self.round else { return };
        let Some((_, v)) = r.commit.filter(|(id, _)| *id == done.id) else { return };
        r.commit = None;
        // refused where it was let go, committed where the kernel stops: that value, and why
        let held = done.capped.as_ref().and_then(|(c, why)| Some((c.num()?, why.clone())));
        let v = held.as_ref().map_or(v, |(c, _)| *c);
        let key = r.key.clone();
        self.state.params.pending.retain(|(k, _)| *k != key);
        if done.ok {
            let decimals = r.decimals();
            let number = format!("{v:.decimals$}");
            // trailing zeros only after a decimal point: 20 stays 20, 2.50 reads 2.5
            let number = if number.contains('.') { number.trim_end_matches('0').trim_end_matches('.') } else { number.as_str() };
            let words = format!("{} {} mm", r.plan.label, number);
            self.state.picks.repeat = Some(Repeat { command: command_name(r.op).into(), value: v, words });
            r.plan.value = v;
            if let Some(then) = r.plan.then.take() {
                // its words now say the step it wrote (the face's old name is gone with it)
                if let Some(words) = fab_ai::round::step_words(&then) {
                    r.plan.says = words;
                }
                r.plan.template = then;
            }
            // a pull's face moved by its values: its handle and values are read again where it is
            // now (a pull step's own `by`, set as it is, drags on from here)
            if r.op == Op::Pull && !matches!(r.plan.template.writes, fab_model::params::Writes::Set { .. }) {
                let picks = r.plan.picks.clone();
                if let Some(host) = &mut self.host {
                    self.round_ask = Some((host.round_plan(picks, Op::Pull), Op::Pull));
                }
            }
            // (said with the plate it built, which stands its own notice first)
            self.round_said = held.map(|(c, why)| format!("{} capped at {} mm: {}", r.plan.label, c, fab::params::round_numbers(&fab::params::failure_reason(&model_host::reason(&why)), 1)));
            self.state.ui.notices.clear(NoticeView::Plate);
        } else {
            self.state.ui.notices.say(NoticeView::Plate, format!("{} not committed: {}", r.plan.label, model_host::reason(&done.text)));
            self.state.params.rebuilding = None;
            self.restore_committed(cx);
        }
        self.round_view(cx);
    }
}
