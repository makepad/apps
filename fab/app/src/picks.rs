//! The view's picks between the Edit shape mode and the model worker: a click, a hover or a held
//! press goes out as a `select` call with the pointer's ray; the worker keeps the picks (whoever
//! made them: the person, the AI's `fab.select`, an outside agent) and every answer's status
//! carries them back. The view draws them, and the chat's composer shows each as a chip whose
//! detail goes to the model with the person's line.
//!
//! Re-pick: a draft whose statement lost its selector builds that statement as its input and
//! shows so (the code line, the tree row, the view's hint, [`App::mark_lost`]); the next edge or
//! face picked on it is written over the lost selector, as one edit ([`App::repick`]).
//!
//! Cost: a hover is one worker call, one in flight at a time with the newest ray waiting (moves in
//! between are dropped); nothing at rest. A status whose picks are unchanged costs a compare. A
//! preview's marks walk its lost statements (none, mostly); a re-pick walks the draft's syntax
//! once, on the click's answer.

use super::*;
use fab::tools::picks::{OtherList, OtherRow, PickAction, PickView, Under};
use makepad_model::json::{self, Value};

/// A draft that lost selectors, shown and marked: the next pick names the first again.
pub(super) struct RePick {
    /// The draft's program: its text holds the selectors.
    compiled: Arc<fab_model::program::Compiled>,
    lost: Vec<fab_model::eval::Lost>,
    /// The parameter value the draft was built for (a drag that lost the selector): the commit
    /// of the new selector sets it too.
    param: Option<(String, fab_model::params::PValue)>,
    /// What the view says once the draft goes, of the selector a pick wrote for it: written in a
    /// script fn's code, every call takes it.
    notice: Option<String>,
}

impl RePick {
    /// What the first lost statement needs.
    pub(super) fn hint(&self) -> Option<String> {
        self.lost.first().map(|l| l.hint.clone())
    }
}

/// Whether a refused commit's reply says a statement lost its selector.
pub(super) fn lost_failure(text: &str) -> bool {
    makepad_script::syntax::parse_value(text).ok().is_some_and(|v| v.field("lost").is_some())
}

/// A refused commit's lost selector: its kind ("edge") and what lost it, in words
/// ("`bracket`'s fillet lost its edge").
pub(super) fn lost_reply(text: &str) -> Option<(String, String)> {
    let v = makepad_script::syntax::parse_value(text).ok()?;
    let field = |name: &str| v.field(name).and_then(|f| f.value.string().map(str::to_string));
    Some((field("lost")?, field("draft")?))
}

fn v3(p: [f64; 3]) -> Value {
    Value::Arr(p.iter().map(|x| Value::F64(*x)).collect())
}

/// The ray fields of a `select` call.
fn ray(from: [f64; 3], dir: [f64; 3], pixel: [f64; 2]) -> Vec<(&'static str, Value)> {
    vec![("ray", json::obj(vec![("from", v3(from)), ("dir", v3(dir))])), ("pixel", Value::Arr(vec![Value::F64(pixel[0]), Value::F64(pixel[1])]))]
}

/// A pick as the view draws it.
pub fn view(p: &fab_ai::picks::Pick) -> PickView {
    PickView { id: p.id.clone(), kind: p.kind.name().to_string(), label: p.label.clone(), object: p.object.clone(), at: p.at, bounds: p.bounds, outline: Arc::new(p.outline.clone()), tint: p.tint.clone() }
}

/// The chat's chips: one per pick, its detail what the model is told.
pub fn attachments(picks: &[fab_ai::picks::Pick]) -> Vec<makepad_ai_services::Attachment> {
    picks.iter().map(|p| makepad_ai_services::Attachment { id: p.id.clone(), label: p.label.clone(), kind: p.kind.name().to_string(), detail: p.detail() }).collect()
}

/// A field of an answer as text: a string, or a list of strings.
fn words(node: &makepad_script::syntax::Node, k: &str) -> Option<String> {
    node.field(k).and_then(|f| f.value.string()).map(str::to_string)
}

/// A Select Other row's pick as `fab.select`'s `add` takes it.
fn row_pick(pick: &makepad_script::syntax::Node) -> Option<Value> {
    let kind = words(pick, "kind")?;
    let solids: Vec<Value> = pick.field("solids")?.value.items()?.iter().filter_map(|s| s.string().map(|s| Value::Str(s.to_string()))).collect();
    let mut fields = vec![("kind", Value::Str(kind)), ("solids", Value::Arr(solids))];
    if let Some(at) = pick.field("at").and_then(|f| f.value.items()) {
        fields.push(("at", Value::Arr(at.iter().filter_map(|x| x.number()).map(Value::F64).collect())));
    }
    if let Some(sel) = words(pick, "select") {
        fields.push(("select", Value::Str(sel)));
    }
    Some(json::obj(fields))
}

impl App {
    pub(super) fn pick_action(&mut self, cx: &mut Cx, action: &PickAction) {
        let filter = self.state.picks.filter.name();
        match action {
            PickAction::At { from, dir, pixel, add, body } => {
                let Some(host) = &mut self.host else { return };
                let mut args = ray(*from, *dir, *pixel);
                args.push(("kind", json::s(if *body { "body" } else { filter })));
                args.push(("add", Value::Bool(*add)));
                self.pick_calls.push(host.call(fab_ai::picks::TOOL, json::obj(args)));
            }
            PickAction::Hover(None) => {
                self.pick_hover_next = None;
                if self.state.picks.under.take().is_some() {
                    fab::ui::redraw(cx, &[ids!(tool_overlay), ids!(viewport)]);
                }
            }
            PickAction::Hover(Some(r)) => {
                self.pick_hover_next = Some(*r);
                self.send_pick_hover();
            }
            PickAction::Other { from, dir, pixel, at, add } => {
                let Some(host) = &mut self.host else { return };
                let mut args = ray(*from, *dir, *pixel);
                args.push(("along", Value::Bool(true)));
                self.pick_other = Some((host.call(fab_ai::picks::TOOL, json::obj(args)), *at, *add));
                self.state.picks.under = None;
            }
            PickAction::Row(row) => {
                let Some(other) = self.state.picks.other.take() else { return };
                if let (Some(row), Some(host)) = (row.and_then(|i| other.rows.get(i)), &mut self.host) {
                    if let Ok(pick) = json::parse(row.pick.as_bytes()) {
                        self.pick_calls.push(host.call(fab_ai::picks::TOOL, json::obj(vec![(if other.add { "add" } else { "set" }, Value::Arr(vec![pick]))])));
                    }
                }
                fab::ui::redraw(cx, &[ids!(tool_overlay)]);
            }
            PickAction::Filter(kind) => {
                self.state.picks.filter = *kind;
                cx.redraw_all();
            }
        }
    }

    /// The newest hover ray to the worker, unless one is out already (its answer sends the next).
    fn send_pick_hover(&mut self) {
        if self.pick_hover.is_some() {
            return;
        }
        let (Some(hover), Some(host)) = (self.pick_hover_next.take(), &mut self.host) else { return };
        let mut args = ray(hover.from, hover.dir, hover.pixel);
        args.push(("hover", Value::Bool(true)));
        args.push(("kind", json::s(self.state.picks.filter.name())));
        self.pick_hover = Some((host.call(fab_ai::picks::TOOL, json::obj(args)), hover));
    }

    /// A worker job ended: a pick call's answer, and the picks its status carries.
    pub(super) fn picks_done(&mut self, cx: &mut Cx, done: &model_host::Done) {
        let parsed = || makepad_script::syntax::parse_value(&done.text).ok();
        if self.pick_hover.is_some_and(|h| h.0 == done.id) {
            let (_, hover) = self.pick_hover.take().unwrap();
            // still in Edit shape mode and still hovering: what a click would take
            let under = parsed().filter(|_| done.ok && fab::tools::picks::active(&self.state)).and_then(|v| {
                let pick = &v.field("hover")?.value;
                let point = |p: &makepad_script::syntax::Node| -> Option<[f64; 3]> {
                    let xs = p.items()?;
                    Some([xs.first()?.number()?, xs.get(1)?.number()?, xs.get(2)?.number()?])
                };
                let outline = v.field("outline").and_then(|f| f.value.items()).map(|lines| {
                    lines.iter().filter_map(|l| l.items().map(|ps| ps.iter().filter_map(point).collect::<Vec<_>>())).collect::<Vec<_>>()
                });
                let object = words(pick, "object")?;
                let outline = outline.map(|lines| (object.clone(), lines));
                // a part drawn from its field: the features to tint, a crease's tick
                let tint = v.field("tint").and_then(|f| f.value.items()).map(|rows| rows.iter().filter_map(|r| Some((r.items()?.first()?.number()? as u32, r.items()?.get(1)?.number()? as f32))).collect()).unwrap_or_default();
                let tick = v.field("tick").and_then(|f| f.value.items()).and_then(|t| Some((point(t.first()?)?, point(t.get(1)?)?)));
                Some(Under { at: hover.at, words: format!("{} {}", words(pick, "kind")?, words(pick, "label")?), view: hover.view, outline, object, tint, tick })
            });
            if under.is_some() || self.state.picks.under.is_some() {
                // the view draws its tint and outline: it records again only when they change
                let shown = |u: &Option<Under>| u.as_ref().map(|u| (u.object.clone(), u.tint.clone(), u.outline.clone()));
                let view = shown(&under) != shown(&self.state.picks.under);
                self.state.picks.under = under;
                if view {
                    fab::ui::redraw(cx, &[ids!(tool_overlay), ids!(viewport)]);
                } else {
                    fab::ui::redraw(cx, &[ids!(tool_overlay)]);
                }
            }
            self.send_pick_hover();
        }
        if self.pick_other.is_some_and(|o| o.0 == done.id) {
            let (_, at, add) = self.pick_other.take().unwrap();
            let rows = parsed()
                .filter(|_| done.ok)
                .and_then(|v| {
                    let rows = v.field("rows")?.value.items()?.to_vec();
                    Some(rows.iter().filter_map(|r| Some(OtherRow { kind: words(r, "kind")?, label: words(r, "label")?, pick: {
                        let mut text = String::new();
                        row_pick(&r.field("pick")?.value)?.write_into(&mut text);
                        text
                    } })).collect::<Vec<_>>())
                })
                .unwrap_or_default();
            self.state.picks.other = Some(OtherList { at, rows, hover: None, add });
            fab::ui::redraw(cx, &[ids!(tool_overlay)]);
        }
        let mut clicked = false;
        if let Some(at) = self.pick_calls.iter().position(|&c| c == done.id) {
            self.pick_calls.remove(at);
            clicked = done.ok;
            // a pick refused says why where the tools say things
            let hint = if done.ok { String::new() } else { model_host::reason(&done.text) };
            fab::tools::session::with(|s| s.hint = hint);
            fab::ui::redraw(cx, &[ids!(tool_overlay)]);
        }
        let (rev, picks) = &done.status.picks;
        if *rev != self.state.picks.rev {
            self.state.picks.rev = *rev;
            self.state.picks.list = Arc::new(picks.iter().map(view).collect());
            if self.state.picks.lit.as_ref().is_some_and(|id| !picks.iter().any(|p| &p.id == id)) {
                self.state.picks.lit = None;
            }
            self.picks = picks.clone();
            self.chat.refresh();
            fab::ui::redraw(cx, &[ids!(tool_overlay), ids!(viewport)]);
            // a click on a draft that lost a selector names it again
            if clicked {
                if let Some(pick) = picks.last().cloned() {
                    self.repick(cx, &pick);
                }
            }
        }
    }

    /// A preview showing: its statements that lost their selector marked where a person looks
    /// (the code line, the tree row, the view's hint); the next pick names the first again.
    /// None: the marks of the draft before go.
    pub(super) fn mark_lost(&mut self, cx: &mut Cx, lost: &[fab_model::eval::Lost], program: Option<&Arc<fab_model::program::Compiled>>, param: Option<(String, fab_model::params::PValue)>) {
        let was = self.repick.take();
        if let Some(was) = &was {
            for l in &was.lost {
                if let Some(tab) = self.state.source.tabs.iter_mut().find(|t| t.file == l.file && t.problem.as_ref().is_some_and(|(_, m)| *m == l.hint)) {
                    tab.problem = None;
                }
            }
        }
        self.state.source.lost = lost.iter().map(|l| (l.file.clone(), l.id.clone(), format!("lost its {}", l.kind))).collect();
        match (lost.first(), program) {
            (Some(first), Some(c)) => {
                for l in lost {
                    if let Some(tab) = self.state.source.tabs.iter_mut().find(|t| t.file == l.file) {
                        tab.problem = Some((Some(l.line.saturating_sub(1) as usize), l.hint.clone()));
                    }
                }
                let notice = format!("Draft: {}", first.hint);
                fab::tools::session::with(|s| s.draft = notice);
                self.repick = Some(RePick { compiled: c.clone(), lost: lost.to_vec(), param, notice: None });
            }
            _ => {
                // the draft went: what its pick wrote says so in its place, until the next build
                let notice = was.and_then(|w| w.notice).unwrap_or_default();
                fab::tools::session::with(|s| s.draft = notice);
                self.dismiss_draft_job();
            }
        }
        self.state.source.problem = self.state.source.tabs.iter().find(|t| t.file == self.state.source.file).and_then(|t| t.problem.clone());
        fab::ui::redraw(cx, &[ids!(tool_overlay)]);
    }

    /// The draft is gone (a commit landed, or the committed plate shows again): so are its marks.
    pub(super) fn clear_lost(&mut self, cx: &mut Cx) {
        self.dismiss_draft_job();
        if self.repick.is_some() || !self.state.source.lost.is_empty() {
            self.mark_lost(cx, &[], None, None);
        }
    }

    /// The job strip's line of a refused typed commit (a draft, waiting for a pick) goes with
    /// its draft.
    fn dismiss_draft_job(&mut self) {
        if let Some(job) = self.draft_job.take() {
            makepad_widgets::jobs::board().dismiss(job);
        }
    }

    /// A commit refused for a lost selector: its job's line waits in the strip with the pick it
    /// offers, until the draft goes.
    pub(super) fn keep_draft_job(&mut self, call: u64) {
        let Some(job) = self.host.as_ref().and_then(|h| h.call_job(call)) else { return };
        if self.draft_job.is_some_and(|was| was != job) {
            self.dismiss_draft_job();
        }
        self.draft_job = Some(job);
    }

    /// The draft job's step pressed: the pick tool on, its filter on the lost kind.
    pub(super) fn draft_step(&mut self, cx: &mut Cx, job: makepad_widgets::jobs::JobId) -> bool {
        if self.draft_job != Some(job) {
            return false;
        }
        let Some(lost) = self.repick.as_ref().and_then(|r| r.lost.first()) else { return false };
        self.state.picks.filter = if lost.kind == "face" { fab::tools::picks::PickKind::Face } else { fab::tools::picks::PickKind::Edge };
        cx.action(ShellAction::SetTool(Tool::Select));
        cx.action(StageAction::Gizmo(GizmoMode::Shape));
        cx.redraw_all();
        true
    }

    /// `pick`, taken on a draft that lost a selector, written over the first lost one: in the
    /// code panel showing its script as one edit (its preview and commit follow as for typing),
    /// else committed as one undo step. A pick of the wrong kind, or a selector Fab cannot find
    /// in the text, says so in the view.
    fn repick(&mut self, cx: &mut Cx, pick: &fab_ai::picks::Pick) {
        use fab_ai::picks::Kind;
        let Some(rp) = &self.repick else { return };
        let Some(lost) = rp.lost.first().cloned() else { return };
        let say = |cx: &mut Cx, hint: String| {
            fab::tools::session::with(|s| s.hint = hint);
            fab::ui::redraw(cx, &[ids!(tool_overlay)]);
        };
        let fits = match pick.kind {
            Kind::Edge | Kind::Chain => lost.kind == "edge",
            Kind::Face => lost.kind == "face",
            _ => false,
        };
        if !fits {
            return say(cx, format!("{}: pick an {} (the strip's {})", lost.hint, lost.kind, if lost.kind == "edge" { "Edge" } else { "Face" }));
        }
        let Some(selector) = pick.selector.clone() else {
            return say(cx, format!("{}: this {} has no name a script can write; pick another", lost.hint, lost.kind));
        };
        let (compiled, param) = (rp.compiled.clone(), rp.param.clone());
        let Some(prog) = compiled.instances.iter().find(|i| i.script.file == lost.file).map(|i| &i.script.program) else { return };
        let site = match makepad_model_script::lang::edit::selector_site(prog, &lost.id, lost.index) {
            Ok(site) => site,
            Err(e) => return say(cx, e),
        };
        // in a script fn's code, its parts by the names that code has for them
        let selector = match (&site.names, &site.shared) {
            (Some(names), Some(f)) => match pick.written.as_ref().map(|w| w.text(|id| names.iter().find(|(part, _)| part == id).map(|(_, name)| name.clone()))) {
                Some(Ok(text)) => text,
                Some(Err(part)) => return say(cx, format!("{}: `{part}` has no name in {f}(..)'s code; pick on a part it names, or name a new selector there", lost.hint)),
                None => return say(cx, format!("{}: this {} has no name {f}(..)'s code can write; pick another", lost.hint, lost.kind)),
            },
            _ => selector,
        };
        let span = site.span;
        let source = prog.source();
        // typed on since the draft built: its text is not the one the selector is found in
        let typed = self.state.source.tabs.iter().find(|t| t.file == lost.file).and_then(|t| t.draft.clone());
        if typed.is_some_and(|t| t.as_str() != source) {
            return say(cx, format!("{}: the code changed since this draft built; pick again once it shows", lost.hint));
        }
        let text = format!("{}{selector}{}", &source[..span.start], &source[span.end..]);
        // the pick named it: it is no pick of its own
        if let Some(host) = &mut self.host {
            host.call(fab_ai::picks::TOOL, json::obj(vec![("remove", json::s(&pick.id))]));
        }
        say(cx, String::new());
        if let (Some(f), Some(rp)) = (&site.shared, &mut self.repick) {
            rp.notice = Some(format!("In fn `{f}`: every call takes the new selector"));
        }
        // A text draft is the editor's: the new selector is one edit there. A parameter's draft
        // commits here, the selector with the value it was picked for.
        let shown = self.state.ui.code_shown() && self.state.source.viewed.as_ref() == Some(&lost.file) && self.state.source.tab().is_some_and(|t| t.shown_text() == source);
        if shown && param.is_none() {
            cx.action(fab::ui::code::InsertCode { file: lost.file.clone(), text: selector, at: Some(span) });
            return;
        }
        let Some(host) = &mut self.host else { return };
        if let Some(tab) = self.state.source.tabs.iter_mut().find(|t| t.file == lost.file) {
            tab.draft = Some(Arc::new(text.clone()));
        }
        // the code panel shows the new selector as it commits
        if self.state.source.viewed.as_ref() == Some(&lost.file) {
            self.state.source.rev += 1;
        }
        let sent = Arc::new(text.clone());
        let id = match param {
            Some((key, value)) => host.set_source_param(lost.file.clone(), text, key, value),
            None => host.set_source(lost.file.clone(), text),
        };
        self.preview_floor = host.cancel_preview();
        self.source_commits.insert(id, (lost.file, sent));
    }

    /// The chat's chip `id` was removed: the pick goes.
    pub(super) fn pick_removed(&mut self, id: &str) {
        if let Some(host) = &mut self.host {
            host.call(fab_ai::picks::TOOL, json::obj(vec![("remove", json::s(id))]));
        }
    }

    /// The picks' selectors at the code's caret, as a script writes them: an exact part's face,
    /// edge or chain by its names (`b.top`, `m.edge(b.top, b.front)`), a feature, an edit or a
    /// body by its binding; several as a list. Only into the script the picks are on: their names
    /// are that script's. What stops it says so in the view.
    pub(super) fn insert_selectors(&mut self, cx: &mut Cx) {
        use fab_ai::picks::Kind;
        let script = |object: &str| {
            let instance = object.split_once('/').map_or(object, |(i, _)| i);
            self.program.as_ref().and_then(|(c, _)| c.instances.iter().find(|i| i.name == instance)).map(|i| i.script.file.clone())
        };
        let written: Vec<(String, Option<String>)> = self.picks.iter().filter_map(|p| {
            let named = match p.kind {
                Kind::Feature | Kind::Body | Kind::Edit => p.solids.first().map(|s| s.rsplit_once('/').map_or(s.as_str(), |(_, s)| s).to_string()),
                _ => None,
            };
            Some((p.selector.clone().or(named)?, script(&p.object)))
        }).collect();
        let viewed = self.state.source.viewed.clone();
        let hint = if written.is_empty() {
            Some("Pick a face, an edge or a feature first (E): its selector goes in the code".to_string())
        } else if !self.state.ui.code_shown() {
            Some("Show the code (`) and put the caret where the selector goes".to_string())
        } else if let Some(other) = written.iter().find(|(_, file)| file.is_none() || *file != viewed) {
            Some(format!("The picks name {}'s solids: insert them in its script", other.1.as_deref().unwrap_or("another script")))
        } else {
            None
        };
        if let Some(hint) = hint {
            fab::tools::session::with(|s| s.hint = hint);
            fab::ui::redraw(cx, &[ids!(tool_overlay)]);
            return;
        }
        let words: Vec<String> = written.into_iter().map(|(w, _)| w).collect();
        let text = if words.len() == 1 { words[0].clone() } else { format!("[{}]", words.join(" ")) };
        cx.action(fab::ui::code::InsertCode { file: viewed.unwrap_or_default(), text, at: None });
    }

    /// The pointer is over the chat's chip `id` (or none): that pick lights up.
    pub(super) fn pick_lit(&mut self, cx: &mut Cx, id: Option<&str>) {
        let id = id.map(str::to_string);
        if self.state.picks.lit != id {
            self.state.picks.lit = id;
            fab::ui::redraw(cx, &[ids!(tool_overlay), ids!(viewport)]);
        }
    }
}
