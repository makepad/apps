//! Review the exact proposal revision shown on each row. Text work belongs on the pool.
use super::journal_panel::{bind_diff_editor, JournalDiff, TextDiffError};
use super::widgets::section_rows;
use fab_elec_view::snapshot::ElecLive;
use makepad_code_editor::{CodeDocument, CodeSession};
use makepad_widgets_core::makepad_platform::thread::{checkpoint, Class, TaskHandle};
use makepad_widgets_core::{button::ButtonAction, tip::TipWidgetRefExt};
use makepad_widgets_core::*;
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::{Arc, Weak}};
use fab_elec_core::Severity;
use fab_elec_design::{By, Design};
use fab_elec_view::{snapshot::{ProposalAuthor, ProposalDoc, ProposalId, ProposalState, ProposalView, ReqSummary, RunClock}, ViewAction};
use makepad_git::diff::{diff_lines_bounded, DiffOp};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reason { Refused, Stale }

struct Row {
    id: ProposalId,
    shown: u64,
    author: String,
    label: Arc<str>,
    state: String,
    reason: Option<Reason>,
    seconds: Option<u64>,
    checks: Option<(usize, usize, bool)>,
    requirements: Option<Arc<ReqSummary>>,
    no_problems: bool,
    check_enabled: Option<bool>,
    accept_tip: Option<&'static str>,
}

fn elapsed(clock: &RunClock) -> Option<u64> {
    clock.started().map(|start| start.elapsed().as_secs())
}

fn checking_text(seconds: Option<u64>) -> String {
    match seconds {
        Some(seconds) => format!("Checking… {seconds} s"),
        None => "Waiting…".into(),
    }
}

impl Row {
    fn of(view: &ProposalView) -> Self {
        let author = match &view.by {
            ProposalAuthor::Local(By::Person) => "You".into(),
            ProposalAuthor::Local(By::Ai { .. }) => "AI".into(),
            ProposalAuthor::Local(By::Agent { .. }) => "Agent".into(),
            ProposalAuthor::Local(By::Disk) => "Disk".into(),
            ProposalAuthor::Peer { label } => format!("{label} (peer)"),
        };
        let checks = view.checks.as_ref().map(|c| {
            let count = |level| c.counts.iter().filter(|((_, s), _)| *s == level).map(|(_, n)| n).sum();
            (count(Severity::Error), count(Severity::Warning), c.pending)
        });
        let seconds = if view.state == ProposalState::Checking { elapsed(&view.run) } else { None };
        let (state, accept_tip, reason) = match &view.state {
            ProposalState::Unchecked => ("Text only, not checked".into(), Some("Check it first"), None),
            ProposalState::Checking => (checking_text(seconds),
                checks.is_none().then_some(if seconds.is_some() { "Checking…" } else { "Waiting…" }), None),
            ProposalState::Ready => (String::new(), None, None),
            ProposalState::Refused(why) => (format!("Not usable: {why}"), Some("Not usable"), Some(Reason::Refused)),
            ProposalState::Stale(why) => (format!("Out of date: {why}"), Some("Out of date"), Some(Reason::Stale)),
        };
        let no_problems = view.state == ProposalState::Ready && checks == Some((0, 0, false))
            && view.requirements.as_ref().is_none_or(|r| r.fail == 0 && r.unknown == 0);
        Self { id: view.id, shown: view.shown, author, label: view.label.clone(), checks, state, seconds,
            requirements: view.requirements.clone(), no_problems,
            reason, accept_tip,
            check_enabled: matches!(view.by, ProposalAuthor::Peer { .. }).then_some(view.state == ProposalState::Unchecked) }
    }

    fn check(&self) -> Option<ViewAction> {
        (self.check_enabled == Some(true)).then_some(ViewAction::CheckProposal(self.id))
    }

    fn decide(&self, accept: bool) -> Option<ViewAction> {
        (!accept || self.accept_tip.is_none()).then_some(ViewAction::ProposalDecision { id: self.id, shown: self.shown, accept })
    }
}

fn preview_tip(view: &ProposalView, committed: Option<&Design>) -> Option<&'static str> {
    match &view.state {
        ProposalState::Unchecked => Some("Check it first"),
        ProposalState::Checking => Some("Still checking"),
        ProposalState::Stale(_) => Some("Out of date"),
        ProposalState::Refused(_) => Some("Not usable"),
        ProposalState::Ready => {
            let differs = view.design.as_deref().zip(committed).is_some_and(|(new, old)| {
                new.units.len() != old.units.len() || new.units.iter().zip(&old.units).any(|(a, b)|
                    a.id != b.id || !Arc::ptr_eq(&a.circuit, &b.circuit) || !same_arc(a.board.as_ref(), b.board.as_ref()))
            });
            (!differs).then_some("Nothing to preview: the design does not change")
        }
    }
}

fn document_count(n: usize) -> String {
    format!("in {n} document{}", if n == 1 { "" } else { "s" })
}

// Count the same changed lines as the expanded editor, without building editor rows or syntax tokens.
fn document_summary(docs: &[ProposalDoc], current: &dyn Fn() -> bool) -> Option<String> {
    let (mut added, mut removed) = (0, 0);
    for doc in docs {
        if !current() { return None; }
        match (doc.before.as_deref(), doc.after.as_deref()) {
            (None, Some(text)) => added += text.lines().count(),
            (Some(text), None) => removed += text.lines().count(),
            (Some(before), Some(after)) => {
                let diff = diff_lines_bounded(before, after, &Default::default(), &|| !current());
                for op in diff.ops {
                    match op {
                        DiffOp::Insert { len, .. } => added += len,
                        DiffOp::Delete { len, .. } => removed += len,
                        DiffOp::Equal { .. } => {}
                    }
                }
            }
            (None, None) => {}
        }
    }
    current().then(|| format!("+{added} −{removed} {}", document_count(docs.len())))
}

const CHECK_WIDTH: f64 = 72.0;
const REVIEW_WIDTH: f64 = 92.0;
const PREVIEW_WIDTH: f64 = 104.0;
const ACCEPT_WIDTH: f64 = 64.0;
const DECLINE_WIDTH: f64 = 72.0;
const ACTION_GAP: f64 = 6.0;
const ACTIONS_WIDTH: f64 = CHECK_WIDTH + REVIEW_WIDTH + PREVIEW_WIDTH + ACCEPT_WIDTH + DECLINE_WIDTH + 4.0 * ACTION_GAP;
// The band's horizontal padding, the text group's minimum width and the inter-group gap.
const WIDE_WIDTH: f64 = 12.0 + 240.0 + 8.0 + ACTIONS_WIDTH + 8.0;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    let Band = View{
        width: Fill height: Fit flow: Flow.Right{wrap: true} align: Align{y: 0.5} spacing: 8
        padding: Inset{left: 12 right: 8 top: 4 bottom: 4}
        show_bg: true draw_bg +: {color: fab.color_header}
    }
    let Strip = #(ReviewStrip::register_widget(vm))
    mod.widgets.ReviewStrip = set_type_default() do Strip{
        width: Fill height: Fit flow: Down
        row_template: View{
            width: Fill height: Fit flow: Down
            band := Band{
                flow: Right
                words := View{width: Fill height: Fit flow: Flow.Right{wrap: true} align: Align{y: 0.5} spacing: 8
                    chip := RoundedView{
                        width: Fit height: Fit padding: Inset{left: 5 right: 5 top: 2 bottom: 2}
                        draw_bg +: {color: fab.color_header border_color: fab.color_border border_size: 1.0 border_radius: fab.radius}
                        author := FabLabel{}
                    }
                    label := FabLabel{width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}}
                    spinner := LoadingSpinner{visible: false width: 14 height: 14
                        draw_bg +: {color: fab.color_text_dim stroke_width: 2.0}
                    }
                    refusal := FabLabel{visible: false width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}
                        max_lines: 1 text_overflow: TextOverflow.Ellipsis draw_text +: {color: fab.color_error}
                    }
                    state := FabLabelDim{width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}
                        max_lines: 1 text_overflow: TextOverflow.Ellipsis
                    }
                    errors := FabLabel{draw_text +: {color: fab.color_error}}
                    warnings := FabLabel{draw_text +: {color: fab.color_warning}}
                    review_req_fail := FabLabel{visible: false draw_text +: {color: fab.color_error}}
                    review_req_unknown := FabLabelDim{visible: false}
                    ok := FabLabel{text: "No problems" draw_text +: {color: fab.color_ok}}
                    pending := FabLabelDim{text: "checks still running"}
                    summary := FabLabelDim{}
                }
                actions := View{
                    width: #(ACTIONS_WIDTH) height: Fit flow: Flow.Right{wrap: true}
                    align: Align{x: 1.0 y: 0.5} spacing: #(ACTION_GAP)
                    check := FabButton{width: #(CHECK_WIDTH) text: "Check it"}
                    review := FabButton{width: #(REVIEW_WIDTH) text: "Review"}
                    preview_tip := FabTip{preview := FabButton{width: #(PREVIEW_WIDTH) text: "Preview"}}
                    accept_tip := FabTip{accept := FabButton{width: #(ACCEPT_WIDTH) text: "Accept"}}
                    decline := FabButton{width: #(DECLINE_WIDTH) text: "Decline"}
                }
            }
            refusal_detail := FabLabel{visible: false width: Fill height: Fit padding: Inset{left: 12 right: 8 top: 4}
                draw_text +: {color: fab.color_error}
            }
            stale_detail := FabLabelDim{visible: false width: Fill height: Fit padding: Inset{left: 12 right: 8 top: 4}}
            review_req_fail_detail := FabLabel{visible: false width: Fill height: Fit padding: Inset{left: 12 right: 8 top: 4}
                draw_text +: {color: fab.color_error}
            }
            review_req_unknown_detail := FabLabelDim{visible: false width: Fill height: Fit padding: Inset{left: 12 right: 8 top: 4}}
            content := View{width: Fill height: Fit flow: Down}
            SolidView{width: Fill height: 1 draw_bg +: {color: fab.color_border}}
        }
        doc_template: View{
            width: Fill height: Fit flow: Down
            padding: Inset{left: 12 right: 8 top: 4 bottom: 4}
            label := FabLabel{}
            diff := View{visible: false width: Fill height: Fit editor := mod.widgets.FabJournalDiffEditor{height: Fit{max: FitBound.Abs(172)}}}
        }
        notice := Band{
            visible: false
            note := FabLabel{width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}}
            dismiss := FabButton{text: "Dismiss"}
        }
        list := View{width: Fill height: Fit flow: Down
            content := View{width: Fill height: Fit flow: Down}
        }
    }
}

fn instance(cx: &mut Cx, template: &ScriptObjectRef) -> WidgetRef {
    cx.with_vm(|vm| WidgetRef::script_from_value(vm, template.as_object().into()))
}

fn same_arc<T: ?Sized>(a: Option<&Arc<T>>, b: Option<&Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn same_doc(a: &ProposalDoc, b: &ProposalDoc) -> bool {
    a.name == b.name && same_arc(a.before.as_ref(), b.before.as_ref()) && same_arc(a.after.as_ref(), b.after.as_ref())
}

// A request lives exactly as long as its displayed text key.
struct Job<T>(Option<TaskHandle<T>>);

impl<T: Send + 'static> Job<T> {
    fn start(cx: &mut Cx, f: impl FnOnce() -> T + Send + 'static) -> Self {
        match cx.task_pool().submit(Class::Visible, f) {
            Ok(handle) => Self(Some(handle)),
            Err(error) => { log!("fab.review: {error}"); Self(None) }
        }
    }

    fn take(&mut self) -> Option<Result<T, makepad_widgets_core::makepad_platform::thread::TaskError>> {
        let result = self.0.as_mut()?.try_take()?;
        self.0 = None;
        Some(result)
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 { handle.cancel(); }
    }
}

struct Document {
    key: ProposalDoc,
    widget: WidgetRef,
    job: Job<Result<JournalDiff, TextDiffError>>,
}

impl Document {
    fn label(&self, cx: &mut Cx, name: &str, status: &str) {
        let presence = match (&self.key.before, &self.key.after) {
            (None, Some(_)) => " · new",
            (Some(_), None) => " · removed",
            _ => "",
        };
        self.widget.label(cx, ids!(label)).set_text(cx, &format!("{name}{presence}{status}"));
    }

    fn new(cx: &mut Cx, template: &ScriptObjectRef, key: &ProposalDoc) -> Self {
        let doc = key.clone();
        let job = Job::start(cx, move || JournalDiff::text(&doc.name, doc.before.as_deref().unwrap_or(""),
            doc.after.as_deref().unwrap_or(""), &|| checkpoint().is_ok()));
        let document = Self { key: key.clone(), widget: instance(cx, template), job };
        let status = if document.job.0.is_some() { " · Preparing diff…" } else { " · Diff unavailable" };
        document.label(cx, &key.name, status);
        document
    }

    fn poll(&mut self, cx: &mut Cx) {
        let Some(result) = self.job.take() else { return };
        if let Ok(Ok(diff)) = result {
            if let Some((doc, view, first)) = diff.text {
                if let Ok(session) = CodeSession::from_prepared(CodeDocument::from_prepared_diff(doc), view) {
                    let editor = self.widget.widget(cx, ids!(diff.editor));
                    bind_diff_editor(cx, &editor, Rc::new(RefCell::new(session)), first);
                    self.widget.view(cx, ids!(diff)).set_visible(cx, true);
                    self.label(cx, &diff.label, "");
                    return;
                }
            }
        }
        self.label(cx, &self.key.name, " · Diff unavailable");
    }
}

struct ShownRow {
    bound: Weak<ProposalView>,
    model: Row,
    widget: WidgetRef,
    docs: Arc<Vec<ProposalDoc>>,
    summary: String,
    count: Job<Option<String>>,
    expanded: bool,
    editors: HashMap<String, Document>,
    // A check summary may change between pointer-down and click. Keep the press's drawn revision.
    pressed: HashMap<WidgetUid, ViewAction>,
    preview_press: Option<ProposalId>,
    preview_bound: Weak<ProposalView>,
}

impl ShownRow {
    fn new(cx: &mut Cx, template: &ScriptObjectRef, proposal: &Arc<ProposalView>, compact: bool) -> Self {
        let mut row = Self { bound: Arc::downgrade(proposal), model: Row::of(proposal), widget: instance(cx, template),
            docs: proposal.docs.clone(), summary: String::new(), count: Job(None),
            expanded: false, editors: HashMap::new(), pressed: HashMap::new(), preview_press: None, preview_bound: Weak::new() };
        if compact { row.set_compact(cx, true); }
        row.count_lines(cx);
        row.header(cx);
        row
    }

    fn count_lines(&mut self, cx: &mut Cx) {
        let docs = self.docs.clone();
        self.count = Job::start(cx, move || document_summary(&docs, &|| checkpoint().is_ok()));
        self.summary = document_count(self.docs.len());
    }

    fn header(&self, cx: &mut Cx) {
        let words = self.widget.widget(cx, ids!(band.words));
        let actions = self.widget.widget(cx, ids!(band.actions));
        let refused = self.model.reason == Some(Reason::Refused);
        words.label(cx, ids!(chip.author)).set_text(cx, &self.model.author);
        words.label(cx, ids!(label)).set_text(cx, &self.model.label);
        let state = if refused { ids!(refusal) } else { ids!(state) };
        words.label(cx, state).set_text(cx, &self.model.state);
        words.widget(cx, ids!(state)).set_visible(cx, !refused && !self.model.state.is_empty());
        words.widget(cx, ids!(refusal)).set_visible(cx, refused);
        words.widget(cx, ids!(spinner)).set_visible(cx, self.model.seconds.is_some());
        actions.button(cx, ids!(decline)).set_text(cx, if refused { "Dismiss" } else { "Decline" });
        let (errors, warnings, pending) = self.model.checks.unwrap_or_default();
        let has_problems = errors > 0 || warnings > 0;
        for (id, count, word) in [(ids!(errors), errors, "error"), (ids!(warnings), warnings, "warning")] {
            let field = words.label(cx, id);
            if has_problems {
                field.set_text(cx, &format!("{count} {word}{}", if count == 1 { "" } else { "s" }));
            }
            field.set_visible(cx, has_problems);
        }
        let (fail, unknown) = self.model.requirements.as_ref().map_or((0, 0), |r| (r.fail, r.unknown));
        let failures = words.label(cx, ids!(review_req_fail));
        if fail > 0 {
            failures.set_text(cx, &format!("{fail} {}", if fail == 1 { "requirement fails" } else { "requirements fail" }));
        }
        failures.set_visible(cx, fail > 0);
        let unjudged = words.label(cx, ids!(review_req_unknown));
        if unknown > 0 { unjudged.set_text(cx, &format!("{unknown} not judged")); }
        unjudged.set_visible(cx, unknown > 0);
        words.widget(cx, ids!(ok)).set_visible(cx, self.model.no_problems);
        words.widget(cx, ids!(pending)).set_visible(cx, pending);
        words.label(cx, ids!(summary)).set_text(cx, &self.summary);
        let check = actions.button(cx, ids!(check));
        check.set_visible(cx, self.model.check_enabled.is_some());
        check.set_enabled(cx, self.model.check_enabled == Some(true));
        actions.button(cx, ids!(accept_tip.accept)).set_enabled(cx, self.model.accept_tip.is_none());
        actions.tip(cx, ids!(accept_tip)).set_text(self.model.accept_tip.unwrap_or(""));
        for (path, action) in [(ids!(accept_tip.accept).as_slice(), self.model.decide(true)),
            (ids!(decline), self.model.decide(false)), (ids!(check), self.model.check())] {
            actions.widget(cx, path).set_action_data(action);
        }
        self.show_details(cx);
    }

    fn show_details(&self, cx: &mut Cx) {
        for (path, reason) in [(ids!(refusal_detail), Reason::Refused), (ids!(stale_detail), Reason::Stale)] {
            let label = self.widget.label(cx, path);
            let show = self.expanded && self.model.reason == Some(reason);
            if show { label.set_text(cx, &self.model.state); }
            label.set_visible(cx, show);
        }
        let req = self.model.requirements.as_deref();
        for (path, rows) in [(ids!(review_req_fail_detail), req.map(|r| r.failures.as_slice())),
            (ids!(review_req_unknown_detail), req.map(|r| r.unjudged.as_slice()))] {
            let rows = rows.unwrap_or_default();
            let label = self.widget.label(cx, path);
            let show = self.expanded && !rows.is_empty();
            if show {
                let text = rows.iter().map(|(name, why)| format!("{name}: {why}")).collect::<Vec<_>>().join("\n");
                label.set_text(cx, &text);
            }
            label.set_visible(cx, show);
        }
    }

    fn set_compact(&self, cx: &Cx, compact: bool) {
        if let Some(mut band) = self.widget.view(cx, ids!(band)).borrow_mut() {
            band.layout.flow = if compact { Flow::Down } else { Flow::right() };
        }
        if let Some(mut actions) = self.widget.view(cx, ids!(band.actions)).borrow_mut() {
            actions.walk.width = if compact { Size::fill() } else { Size::Fixed(ACTIONS_WIDTH) };
        }
    }

    fn show_seconds(&mut self, cx: &mut Cx, seconds: u64) {
        if self.model.seconds == Some(seconds) { return; }
        if self.model.seconds.is_none() {
            self.widget.view(cx, ids!(band.words.spinner)).set_visible(cx, true);
            if self.model.accept_tip.is_some() {
                self.model.accept_tip = Some("Checking…");
                self.widget.tip(cx, ids!(band.actions.accept_tip)).set_text("Checking…");
            }
        }
        self.model.seconds = Some(seconds);
        self.model.state = checking_text(Some(seconds));
        self.widget.label(cx, ids!(band.words.state)).set_text(cx, &self.model.state);
    }

    fn documents(&mut self, cx: &mut Cx, template: &ScriptObjectRef) {
        let mut old = std::mem::take(&mut self.editors);
        let mut children = Vec::new();
        if self.expanded {
            for (i, key) in self.docs.iter().enumerate() {
                let doc = old.remove(&key.name).filter(|d| same_doc(&d.key, key))
                    .unwrap_or_else(|| Document::new(cx, template, key));
                children.push((LiveId(i as u64 + 1), doc.widget.clone()));
                self.editors.insert(key.name.clone(), doc);
            }
        }
        section_rows(cx, &self.widget, children);
        self.widget.button(cx, ids!(band.actions.review)).set_text(cx, if self.expanded { "Hide review" } else { "Review" });
    }

    fn update(&mut self, cx: &mut Cx, doc_template: &ScriptObjectRef, view: &Arc<ProposalView>) {
        if self.bound.as_ptr() == Arc::as_ptr(view) { return }
        let changed = !Arc::ptr_eq(&self.docs, &view.docs) && (self.docs.len() != view.docs.len()
            || !self.docs.iter().zip(view.docs.iter()).all(|(a, b)| same_doc(a, b)));
        self.docs = view.docs.clone();
        if changed { self.count_lines(cx); }
        if self.expanded && changed { self.documents(cx, doc_template); }
        self.model = Row::of(view);
        self.header(cx);
        self.bound = Arc::downgrade(view);
    }

    fn poll(&mut self, cx: &mut Cx) -> bool {
        if let Some(Ok(Some(summary))) = self.count.take() {
            self.summary = summary;
            self.widget.label(cx, ids!(band.words.summary)).set_text(cx, &self.summary);
        }
        let mut pending = self.count.0.is_some();
        for editor in self.editors.values_mut() {
            editor.poll(cx);
            pending |= editor.job.0.is_some();
        }
        pending
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ReviewStrip {
    #[deref] view: View,
    #[live] row_template: ScriptObjectRef,
    #[live] doc_template: ScriptObjectRef,
    #[rust] rows: HashMap<Arc<str>, ShownRow>,
    #[rust] waiting: Vec<(Arc<str>, Arc<RunClock>)>,
    #[rust] running: Vec<(Arc<str>, Arc<RunClock>)>,
    #[rust] frame: NextFrame,
    #[rust] notice: Option<Arc<str>>,
    #[rust] busy: bool,
    #[rust] compact: bool,
    #[rust] previewing: Option<ProposalId>,
    #[rust] preview_base: Weak<Design>,
    #[rust] gesture: bool,
}

impl ReviewStrip {
    pub(super) fn reveal(&mut self, cx: &mut Cx, id: ProposalId) -> Option<f64> {
        let row = self.rows.values_mut().find(|row| row.model.id == id)?;
        if !row.expanded {
            row.expanded = true;
            row.documents(cx, &self.doc_template);
            row.show_details(cx);
            self.busy = true;
            self.view.redraw(cx);
        }
        Some((row.widget.area().rect(cx).pos.y - self.view.area().rect(cx).pos.y).max(0.0))
    }

    /// A pool start posts a signal. Waiting rows need no timer or frame to discover it.
    fn started(&mut self, cx: &mut Cx) {
        self.waiting.retain(|(slot, clock)| {
            let Some(seconds) = elapsed(clock) else { return true };
            self.rows.get_mut(slot).unwrap().show_seconds(cx, seconds);
            self.running.push((slot.clone(), clock.clone()));
            false
        });
        self.request_frame(cx);
    }

    fn request_frame(&mut self, cx: &mut Cx) {
        if self.running.is_empty() {
            self.frame = NextFrame::default();
        } else if self.frame == NextFrame::default() {
            self.frame = cx.new_next_frame();
        }
    }

    fn arrange(&mut self, cx: &Cx, width: f64) {
        let compact = width < WIDE_WIDTH;
        if self.compact == compact { return; }
        self.compact = compact;
        for row in self.rows.values() { row.set_compact(cx, compact); }
    }

    pub(super) fn sync(&mut self, cx: &mut Cx, proposals: &[Arc<ProposalView>], notice: Option<Arc<str>>) {
        if !same_arc(self.notice.as_ref(), notice.as_ref()) {
            self.view.view(cx, ids!(notice)).set_visible(cx, notice.is_some());
            if let Some(text) = &notice { self.view.label(cx, ids!(notice.note)).set_text(cx, text); }
            self.notice = notice;
        }
        self.waiting.clear();
        self.running.clear();
        let mut old = std::mem::take(&mut self.rows);
        let live = cx.get_global_ref::<ElecLive>();
        let base = live.map(|l| l.snap.design.clone());
        let gesture = live.is_some_and(|l| l.snap.preview.is_some());
        let base_changed = base.as_ref().is_some_and(|d| self.preview_base.as_ptr() != Arc::as_ptr(d)) || self.gesture != gesture;
        self.preview_base = base.as_ref().map(Arc::downgrade).unwrap_or_default();
        self.gesture = gesture;
        let mut children = Vec::with_capacity(proposals.len());
        for proposal in proposals {
            let mut row = old.remove(&proposal.slot).unwrap_or_else(|| ShownRow::new(cx, &self.row_template, proposal, self.compact));
            row.update(cx, &self.doc_template, proposal);
            if base_changed || row.preview_bound.as_ptr() != Arc::as_ptr(proposal) {
                let tip = preview_tip(proposal, base.as_deref()).or(gesture.then_some("A gesture preview is showing"));
                let button = row.widget.button(cx, ids!(band.actions.preview_tip.preview));
                button.set_enabled(cx, tip.is_none());
                row.widget.widget(cx, ids!(band.actions.preview_tip.preview)).set_action_data(proposal.id);
                button.set_text(cx, if self.previewing == Some(proposal.id) { "End preview" } else { "Preview" });
                row.widget.tip(cx, ids!(band.actions.preview_tip)).set_text(tip.unwrap_or(""));
                row.preview_bound = Arc::downgrade(proposal);
            }
            self.busy |= row.count.0.is_some() || row.expanded;
            children.push((LiveId::from_str(&proposal.slot), row.widget.clone()));
            if proposal.state == ProposalState::Checking {
                self.waiting.push((proposal.slot.clone(), proposal.run.clone()));
            }
            self.rows.insert(proposal.slot.clone(), row);
        }
        let list = self.view.widget(cx, ids!(list));
        section_rows(cx, &list, children);
        self.started(cx);
    }
}

impl Widget for ReviewStrip {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Escape && !k.is_repeat)
            && !cx.key_focus_takes_text() && cx.get_global_ref::<ElecLive>().is_some_and(|live| live.previewing.is_some()) {
            ElecLive::select_preview(cx, None);
            return;
        }
        self.view.handle_event(cx, event, scope);
        if !self.waiting.is_empty() && matches!(event, Event::Signal) {
            self.started(cx);
        }
        if self.frame.is_event(event).is_some() {
            self.frame = NextFrame::default();
            for (slot, clock) in &self.running {
                if let Some(seconds) = elapsed(clock) {
                    self.rows.get_mut(slot).unwrap().show_seconds(cx, seconds);
                }
            }
            self.view.area().redraw(cx);
        }
        if self.busy && matches!(event, Event::Signal) {
            self.busy = false;
            for row in self.rows.values_mut() { self.busy |= row.poll(cx); }
        }
        let Event::Actions(actions) = event else { return };
        if self.view.button(cx, ids!(notice.dismiss)).clicked(actions) {
            if let Some(live) = cx.get_global_ref::<ElecLive>() {
                if same_arc(self.notice.as_ref(), live.ended_proposals.as_ref()) {
                    cx.get_global::<ElecLive>().ended_proposals = None;
                    self.view.redraw(cx);
                }
            }
        }
        let uid = self.widget_uid();
        for row in self.rows.values_mut() {
            let controls = row.widget.widget(cx, ids!(band.actions));
            let preview = controls.widget(cx, ids!(preview_tip.preview)).widget_uid();
            for event in actions.filter_widget_actions(preview) {
                let Some(&drawn) = event.data.as_deref().and_then(|d| d.downcast_ref::<ProposalId>()) else { continue };
                match event.cast::<ButtonAction>() {
                    ButtonAction::Pressed(_) => row.preview_press = Some(drawn),
                    ButtonAction::Released(_) => row.preview_press = None,
                    ButtonAction::Clicked(_) => {
                        let id = row.preview_press.take().unwrap_or(drawn);
                        let Some(live) = cx.get_global_ref::<ElecLive>() else { continue };
                        if live.proposals_reloading || live.snap.preview.is_some() || !live.snap.proposals.iter().any(|p|
                            p.id == id && preview_tip(p, Some(&live.snap.design)).is_none()) { continue; }
                        let next = (live.previewing != Some(id)).then_some(id);
                        ElecLive::select_preview(cx, next);
                    }
                    _ => {}
                }
            }
            if controls.button(cx, ids!(review)).clicked(actions) {
                row.expanded = !row.expanded;
                row.documents(cx, &self.doc_template);
                row.show_details(cx);
                self.busy |= row.expanded;
            }
            for path in [ids!(accept_tip.accept).as_slice(), ids!(decline), ids!(check)] {
                let button = controls.widget(cx, path).widget_uid();
                for event in actions.filter_widget_actions(button) {
                    let Some(Some(command)) = event.data.as_deref().and_then(|d| d.downcast_ref::<Option<ViewAction>>()) else { continue };
                    match event.cast::<ButtonAction>() {
                        ButtonAction::Pressed(_) => { row.pressed.insert(button, command.clone()); }
                        ButtonAction::Released(_) => { row.pressed.remove(&button); }
                        ButtonAction::Clicked(_) => {
                            // Accessibility can activate a control with a click alone.
                            cx.widget_action(uid, row.pressed.remove(&button).unwrap_or_else(|| command.clone()));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let selected = cx.get_global_ref::<ElecLive>().and_then(|l| l.previewing);
        if self.previewing != selected {
            for row in self.rows.values().filter(|r| Some(r.model.id) == selected || Some(r.model.id) == self.previewing) {
                row.widget.button(cx, ids!(band.actions.preview_tip.preview)).set_text(cx,
                    if Some(row.model.id) == selected { "End preview" } else { "Preview" });
            }
            self.previewing = selected;
        }
        let width = cx.peek_walk_turtle(walk).size.x;
        self.arrange(cx, width);
        self.request_frame(cx);
        self.view.draw_walk(cx, scope, walk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_view::snapshot::ProposalChecks;
    use makepad_widgets_core::widget::WidgetActionData;

    #[test]
    fn review_rows_bind_the_shown_revision_and_report_the_proposed_edits() {
        let doc = |name: &str, before: Option<&str>, after: Option<&str>| ProposalDoc {
            name: name.into(), before: before.map(Arc::from), after: after.map(Arc::from),
        };
        let mut view = ProposalView {
            id: ProposalId(17), shown: 3, slot: "schematic".into(), label: "Sense input".into(),
            by: ProposalAuthor::Peer { label: "Partner".into() }, state: ProposalState::Unchecked,
            docs: Arc::new(vec![doc("brief.req.splash", None, Some("one\ntwo\n")),
                doc("board.ckt.splash", Some("keep\nold\n"), Some("keep\nnew\n")),
                doc("old.req.splash", Some("remove\n"), None)]),
            run: Arc::default(), design: None, boards: Arc::default(), sheets: Arc::default(), created: Arc::default(), checks: None, requirements: None,
        };
        let committed = Design::default();
        assert_eq!(preview_tip(&view, Some(&committed)), Some("Check it first"), "unchecked text explains its disabled Preview");
        let unchecked = Row::of(&view);
        assert_eq!((&*unchecked.author, &*unchecked.state), ("Partner (peer)", "Text only, not checked"), "peer text names its origin and verification state");
        assert_eq!(unchecked.check(), Some(ViewAction::CheckProposal(view.id)), "the unchecked peer offers a local check of its own id");
        assert!(unchecked.decide(true).is_none() && unchecked.accept_tip == Some("Check it first"), "unchecked text explains its disabled Accept");
        view.state = ProposalState::Checking;
        assert_eq!(preview_tip(&view, Some(&committed)), Some("Still checking"), "checking text cannot be previewed");
        let first = Row::of(&view);
        assert!(first.check_enabled == Some(false) && first.check().is_none(), "the peer check stays present and disabled during checking");
        assert!(first.decide(true).is_none() && first.accept_tip == Some("Waiting…"), "waiting for first verification disables Accept");
        view.checks = Some(Arc::new(ProposalChecks { counts: Default::default(), pending: true }));
        assert!(Row::of(&view).checks == Some((0, 0, true)) && Row::of(&view).decide(true).is_some(),
            "a recheck with shown checks permits a parked accept and reports pending work");
        view.by = ProposalAuthor::Local(By::Ai { turn: 1, call: 1 });
        view.state = ProposalState::Ready;
        view.design = Some(Arc::new(committed.clone()));
        assert_eq!(preview_tip(&view, Some(&committed)), Some("Nothing to preview: the design does not change"), "a Ready text-only edit leaves Preview disabled");
        let changed = Arc::make_mut(view.design.as_mut().unwrap());
        changed.units.push(fab_elec_design::Unit { id: fab_elec_core::BoardId(0), name: "probe".into(),
            kind: fab_elec_design::UnitKind::Board, circuit: Arc::default(), board: None, supplied: vec![], factory: None });
        assert!(preview_tip(&view, Some(&committed)).is_none(), "a proposal adding its first unit can be previewed");
        let mut same = (**view.design.as_ref().unwrap()).clone();
        assert!(preview_tip(&view, Some(&same)).is_some(), "a separate design with shared circuit and board has no visual change");
        same.units[0].circuit = Arc::new((*same.units[0].circuit).clone());
        assert!(preview_tip(&view, Some(&same)).is_none(), "a changed circuit enables Preview for an existing unit");
        let ready = Row::of(&view);
        assert!(ready.author == "AI" && ready.state.is_empty() && ready.check_enabled.is_none(), "ready local work has no peer check or extra state label");
        view.id = ProposalId(18);
        view.shown += 1;
        for accept in [true, false] {
            assert_eq!(ready.decide(accept), Some(ViewAction::ProposalDecision { id: ProposalId(17), shown: 3, accept }),
                "both decisions retain the row's drawn id and revision");
        }
        view.state = ProposalState::Stale("circuit changed".into());
        let stale = Row::of(&view);
        assert!(stale.state == "Out of date: circuit changed" && stale.accept_tip == Some("Out of date")
            && stale.decide(true).is_none() && stale.decide(false).is_some(), "stale work shows the reason and can still be declined");
        assert_eq!(document_summary(&view.docs, &|| true).as_deref(), Some("+3 −2 in 3 documents"),
            "created, replaced and removed lines count across documents, excluding equal lines");
        assert_eq!(document_summary(&view.docs[..1], &|| true).as_deref(), Some("+2 −0 in 1 document"), "a single document uses the singular");

        // Keep the same texts through a check update between press and release.
        view.state = ProposalState::Checking;
        let checks = view.checks.take();
        let mut proposal = Arc::new(view);
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut strip = cx.with_vm(|vm| {
            makepad_widgets_core::script_mod_with(vm, WindowFamilies::default(), &[makepad_widgets_fab::fab_mod]);
            crate::theme::script_mod(vm);
            makepad_code_editor::script_mod(vm);
            crate::ui::script_mod_kit(vm);
            super::super::journal_panel::script_mod(vm);
            super::script_mod(vm);
            ReviewStrip::script_new_with_default(vm)
        });
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        let slot = proposal.slot.clone();
        let band = strip.rows[&slot].widget.view(&cx, ids!(band));
        let actions = strip.rows[&slot].widget.view(&cx, ids!(band.actions));
        assert!(actions.borrow().unwrap().children.iter().map(|(id, _)| *id)
            .eq([id!(check), id!(review), id!(preview_tip), id!(accept_tip), id!(decline)]),
            "Check it occupies the optional leftmost place before Review, Preview, Accept and Decline");
        strip.arrange(&cx, WIDE_WIDTH - 1.0);
        assert!(band.borrow().unwrap().layout.flow == Flow::Down && actions.borrow().unwrap().walk.width == Size::fill()
            && actions.borrow().unwrap().layout.flow == Flow::right_wrap(),
            "below the text and action budget the controls wrap beneath the text");
        strip.arrange(&cx, WIDE_WIDTH);
        assert!(band.borrow().unwrap().layout.flow == Flow::right() && actions.borrow().unwrap().walk.width == Size::Fixed(ACTIONS_WIDTH)
            && actions.borrow().unwrap().layout.align.x == 1.0 && !strip.rows[&slot].widget.button(&cx, ids!(band.actions.check)).visible(),
            "a wide local row reserves the whole right-aligned column even with Check it hidden");
        let row = &strip.rows[&slot];
        assert!(row.model.state == "Waiting…" && !row.widget.view(&cx, ids!(band.words.spinner)).visible()
            && strip.frame == NextFrame::default(), "queued verification shows Waiting without animation");
        proposal.run.start();
        strip.handle_event(&mut cx, &Event::Signal, &mut Scope::empty());
        let row = &strip.rows[&slot];
        assert!(row.model.state.starts_with("Checking… ") && row.model.state.ends_with(" s")
            && row.model.accept_tip == Some("Checking…") && row.widget.view(&cx, ids!(band.words.spinner)).visible()
            && strip.frame != NextFrame::default(),
            "a worker start wakes the row, shows elapsed seconds and requests spinner frames");
        let ready = Arc::make_mut(&mut proposal);
        ready.state = ProposalState::Ready;
        ready.checks = checks;
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        let row = &strip.rows[&slot];
        assert!(strip.frame == NextFrame::default() && !row.widget.view(&cx, ids!(band.words.spinner)).visible(),
            "publishing Ready stops the frame requests and hides the spinner");
        assert!(row.editors.is_empty(), "a collapsed row owns no prepared editor or diff job");
        let review = row.widget.widget(&cx, ids!(band.actions.review));
        let click = cx.capture_actions(|cx| cx.widget_action(review.widget_uid(), ButtonAction::Clicked(Default::default())));
        strip.handle_event(&mut cx, &Event::Actions(click), &mut Scope::empty());
        let row = &strip.rows[&slot];
        let doc = proposal.docs[0].name.clone();
        let editor = row.editors[&doc].widget.widget_uid();
        let accept = row.widget.widget(&cx, ids!(band.actions.accept_tip.accept));
        let press = |cx: &mut Cx, button: ButtonAction| {
            let mut data = WidgetActionData::default();
            data.set_box(accept.action_data().expect("drawn Accept has a proposal binding"));
            cx.capture_actions(|cx| cx.widget_action_with_data(&data, accept.widget_uid(), button))
        };
        let start = press(&mut cx, ButtonAction::Pressed(Default::default()));
        strip.handle_event(&mut cx, &Event::Actions(start), &mut Scope::empty());
        let expected = ViewAction::ProposalDecision { id: proposal.id, shown: proposal.shown, accept: true };
        Arc::make_mut(&mut proposal).shown += 1;
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        assert_eq!(strip.rows[&slot].editors[&doc].widget.widget_uid(), editor, "a revision with the same texts keeps its diff editor");
        let end = press(&mut cx, ButtonAction::Clicked(Default::default()));
        let emitted = cx.capture_actions(|cx| strip.handle_event(cx, &Event::Actions(end), &mut Scope::empty()));
        let decisions: Vec<_> = emitted.iter().filter_map(|a| a.as_widget_action()?.action.downcast_ref::<ViewAction>()).collect();
        assert_eq!(decisions, [&expected], "a check update during the press sends exactly the revision the person pressed");

        // Bind a prepared session directly so this identity check is independent of pool speed.
        let (document, layout, first) = JournalDiff::text(&doc, "", "one\ntwo\n", &|| true)
            .expect("the created document prepares").text.unwrap();
        let session = Rc::new(RefCell::new(CodeSession::from_prepared(CodeDocument::from_prepared_diff(document), layout).unwrap()));
        let bound_session = Rc::downgrade(&session);
        let row = &strip.rows[&slot];
        let unchanged = &row.editors[&doc].widget;
        let changed_doc = proposal.docs[1].name.clone();
        let old_editor = row.editors[&changed_doc].widget.widget_uid();
        let wrapper = unchanged.widget(&cx, ids!(diff.editor));
        bind_diff_editor(&mut cx, &wrapper, session, first);
        let Size::Fit { max: Some(FitBound::Abs(cap)), .. } = wrapper.walk(&mut cx).height else {
            panic!("the Review editor caps its content height")
        };
        let pass = DrawPass::new(&mut cx);
        let size = dvec2(WIDE_WIDTH, cap * 2.0);
        pass.set_size(&mut cx, size);
        let mut list = DrawList2d::new(&mut cx);
        {
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(&mut cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, Some(1.0));
            list.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            wrapper.draw_all(&mut frame, &mut Scope::empty());
            let used = frame.turtle().used().y;
            assert!(used > 0.0 && used < cap, "a short prepared diff uses its content height below the cap: {used}");
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
        }
        drop(wrapper);
        let old = proposal.id;
        let next = Arc::make_mut(&mut proposal);
        next.id = ProposalId(old.0 + 1);
        next.shown = 1;
        next.run = Arc::default();
        Arc::make_mut(&mut next.docs)[1].after = Some("replacement".into());
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        let row = &strip.rows[&slot];
        assert!(strip.rows.len() == 1 && row.expanded && row.editors[&doc].widget.widget_uid() == editor
            && bound_session.upgrade().is_some(), "same-slot replacement keeps the expansion and the unchanged document's bound session");
        assert!(row.editors[&changed_doc].widget.widget_uid() != old_editor && row.editors[&changed_doc].job.0.is_some(),
            "only the changed document receives a new editor and preparation job");
        let click = press(&mut cx, ButtonAction::Clicked(Default::default()));
        let emitted = cx.capture_actions(|cx| strip.handle_event(cx, &Event::Actions(click), &mut Scope::empty()));
        assert!(emitted.iter().filter_map(|a| a.as_widget_action()?.action.downcast_ref::<ViewAction>())
            .eq([&ViewAction::ProposalDecision { id: proposal.id, shown: 1, accept: true }]),
            "a fresh click on the retained row carries the replacement's id and shown revision");
        let next = Arc::make_mut(&mut proposal);
        let why = "missing circuit at the proposed connection\ncheck the source document for the other end of the connection";
        next.state = ProposalState::Refused(why.into());
        next.checks = None;
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        let row = &strip.rows[&slot];
        assert!(row.model.reason == Some(Reason::Refused) && row.model.state == format!("Not usable: {why}")
            && row.widget.widget(&cx, ids!(band.words.refusal)).visible() && !row.widget.widget(&cx, ids!(band.words.state)).visible(),
            "the refused row shows its reason in the error-coloured label");
        assert!(row.model.accept_tip == Some("Not usable") && row.model.decide(true).is_none()
            && row.model.decide(false).is_some() && strip.frame == NextFrame::default(),
            "a refusal explains disabled Accept, remains dismissible and requests no animation");
        let header = row.widget.label(&cx, ids!(band.words.refusal));
        let detail = row.widget.label(&cx, ids!(refusal_detail));
        assert!(header.borrow().unwrap().max_lines == 1 && header.borrow().unwrap().text_overflow
            == makepad_widgets_core::makepad_draw::shader::draw_text::TextOverflow::Ellipsis,
            "even a multiline refusal is drawn in one header line with an ellipsis");
        assert!(detail.visible() && detail.borrow().unwrap().max_lines == 0 && detail.text() == row.model.state,
            "the expanded error label wraps the complete refusal");
        let click = cx.capture_actions(|cx| cx.widget_action(review.widget_uid(), ButtonAction::Clicked(Default::default())));
        strip.handle_event(&mut cx, &Event::Actions(click), &mut Scope::empty());
        assert!(!detail.visible(), "collapsing Review hides the full reason");
        Arc::make_mut(&mut proposal).state = ProposalState::Stale("the circuit changed".into());
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        assert!(strip.reveal(&mut cx, ProposalId(proposal.id.0 + 1)).is_none() && !strip.rows[&slot].expanded,
            "a missing proposal must not expand the row that occupies its slot");
        let y = strip.reveal(&mut cx, proposal.id);
        assert!(y.is_some_and(|y| y >= 0.0) && strip.rows[&slot].editors.len() == proposal.docs.len(),
            "revealing a proposal returns its scroll offset and opens every document diff");
        let editor = strip.rows[&slot].editors[&doc].widget.widget_uid();
        assert!(strip.reveal(&mut cx, proposal.id) == y && strip.rows[&slot].editors[&doc].widget.widget_uid() == editor,
            "revealing an expanded proposal keeps its open editor");
        let stale = strip.rows[&slot].widget.label(&cx, ids!(stale_detail));
        assert!(stale.visible() && stale.text() == "Out of date: the circuit changed" && !detail.visible(),
            "expanding stale work shows its complete reason in the dim label");

        let widget = &strip.rows[&slot].widget;
        let ok = widget.label(&cx, ids!(band.words.ok));
        let fail = widget.label(&cx, ids!(band.words.review_req_fail));
        let unknown = widget.label(&cx, ids!(band.words.review_req_unknown));
        let failure_detail = widget.label(&cx, ids!(review_req_fail_detail));
        let unknown_detail = widget.label(&cx, ids!(review_req_unknown_detail));
        let ready = Arc::make_mut(&mut proposal);
        ready.state = ProposalState::Ready;
        ready.checks = Some(Arc::new(ProposalChecks { counts: Default::default(), pending: false }));
        ready.requirements = Some(Arc::new(ReqSummary { fail: 1,
            failures: vec![("fits opening".into(), "12 mm, required max 10 mm".into())], ..Default::default() }));
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        assert!(fail.visible() && fail.text() == "1 requirement fails" && !ok.visible(),
            "a failed requirement replaces the all-clear even with clean checks");
        assert!(failure_detail.visible() && failure_detail.text().contains("fits opening: 12 mm, required max 10 mm"),
            "expanded Review names the failing requirement and its reason");

        Arc::make_mut(&mut proposal).requirements = Some(Arc::new(ReqSummary { unknown: 1,
            unjudged: vec![("starts up".into(), "not judged for a proposal".into())], ..Default::default() }));
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        assert!(unknown.visible() && unknown.text() == "1 not judged" && !ok.visible() && !fail.visible(),
            "an unjudged requirement has its own count and prevents an all-clear");
        assert!(unknown_detail.visible() && unknown_detail.text().contains("starts up: not judged for a proposal") && !failure_detail.visible(),
            "expanded Review gives the current unknown reason and drops the old failure");

        Arc::make_mut(&mut proposal).requirements = Some(Arc::new(ReqSummary { pass: 1, ..Default::default() }));
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        assert!(ok.visible() && ok.text() == "No problems" && !unknown.visible() && !unknown_detail.visible(),
            "a passing brief with settled clean checks shows the all-clear");
        Arc::make_mut(Arc::make_mut(&mut proposal).checks.as_mut().unwrap()).pending = true;
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        assert!(!ok.visible() && strip.rows[&slot].widget.label(&cx, ids!(band.words.pending)).visible(),
            "pending checks never share the row with an all-clear");
        let checking = Arc::make_mut(&mut proposal);
        Arc::make_mut(checking.checks.as_mut().unwrap()).pending = false;
        checking.state = ProposalState::Checking;
        strip.sync(&mut cx, std::slice::from_ref(&proposal), None);
        assert!(!ok.visible(), "a recheck cannot give an all-clear from the last verified results");
    }
}
