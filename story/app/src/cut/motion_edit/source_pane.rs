//! The Source view: a document's Splash in Makepad's code editor, live.
//!
//! What is typed is checked on the source checker after a short pause; a
//! draft that loads is handed to the owner, which applies it as one
//! undoable change (so the timeline, the graph and the AI see it at once),
//! and a draft that does not load keeps its diagnostics marked inline, with
//! the first one named in the bar. When the document changes elsewhere (the
//! timeline, the graph, undo, the AI) the view follows, unless a draft is
//! being typed: then the bar offers the other version instead of losing
//! either.
//!
//! Every change the view sees is kept with its author and its line diff;
//! the bar steps through them and "Changes" shows the selected one as a
//! read-only diff, so an AI iteration is exactly readable.
//!
//! A live pane ([`SourcePane::live`], DECK's code view) has no check and no
//! pause: every edit is handed to the owner at once ([`SourcePane::take_edit`])
//! and lands as an undoable change, the picture redrawn from it. An edit
//! that does not load comes back ([`SourcePane::edit_failed`]) with its
//! problems marked on their lines; the document, and so the picture, stays
//! the last one that loaded.

use super::checker::{Check, Checked, Checker, DocKind, Problem};
use makepad_library_browser::draw::DrawEditBlock;
use super::diff;
use makepad_code_editor::code_view::{CodeView, CodeViewAction};
use makepad_code_editor::decoration::{Decoration, DecorationType};
use makepad_code_editor::text::Position;
use makepad_code_editor::{CodeDocument, CodeSession};
use makepad_widgets::badge::advance;
use makepad_widgets::*;

/// The pause after the last keystroke before a draft is checked.
const SETTLE: f64 = 0.45;

/// One change of the document, as the view saw it.
pub struct Change {
    pub author: String,
    pub before: String,
    pub after: String,
    pub rows: Vec<diff::Row>,
}

impl Change {
    pub fn counts(&self) -> (usize, usize) {
        diff::counts(&self.rows)
    }
}

pub struct SourcePane {
    pub draft_revision: u64,
    pub tree: Option<std::sync::Arc<makepad_asset_data::tree::TreeDraft>>,
    pub kind: DocKind,
    /// The pane's widget id in the page.
    pane: LiveId,
    file: String,
    /// The document's text as the view last took it.
    applied: Option<String>,
    /// Typed text, not applied yet.
    draft: Option<String>,
    /// The accepted version this draft started from, retained across conflicts.
    draft_base: Option<String>,
    timer: Timer,
    due: bool,
    /// The checker serial of the draft being checked.
    sent: Option<u64>,
    /// A draft that loads, waiting for its owner to apply it.
    proven: Option<String>,
    problems: Vec<Problem>,
    /// The document changed under a draft: the other version, and who.
    conflict: Option<(String, String)>,
    pub changes: Vec<Change>,
    /// The change the bar points at (`changes` index), and whether its diff
    /// is shown.
    shown: Option<usize>,
    diff_on: bool,
    /// The diff document that is in the diff view, by change index.
    diff_loaded: Option<usize>,
    /// The problems as the loader wrote them (the Tip, "Copy details").
    raw: String,
    repaint: bool,
    /// The lines (first, last) the owner points at, marked in the gutter.
    highlight: Option<(usize, usize)>,
    /// The host's choice: no change-history bar; a change that was not
    /// typed here shows as one small chip instead ([`Self::compact`]).
    compact: bool,
    /// The change the chip offers (a `changes` index), until the next edit
    /// here or [`Self::dismiss_chip`].
    foreign: Option<usize>,
    /// Every edit goes to the owner as typed ([`Self::live`]).
    live: bool,
    /// Live: the text handed to the owner, until it lands or comes back.
    in_flight: Option<String>,
    /// Live: the last text that did not load (not handed over again).
    failed: Option<String>,
    /// The owner's own status line (text, tooltip, error) shown in the bar
    /// in place of the pane's state; see [`Self::set_notice`].
    notice: Option<(String, String, bool)>,
}

impl SourcePane {
    pub fn new(kind: DocKind, pane: LiveId, file: &str) -> Self {
        Self {
            draft_revision: 0,
            tree: None,
            kind,
            pane,
            file: file.to_string(),
            applied: None,
            draft: None,
            draft_base: None,
            timer: Timer::default(),
            due: false,
            sent: None,
            proven: None,
            problems: Vec::new(),
            conflict: None,
            changes: Vec::new(),
            shown: None,
            diff_on: false,
            diff_loaded: None,
            raw: String::new(),
            repaint: true,
            highlight: None,
            compact: false,
            foreign: None,
            live: false,
            in_flight: None,
            failed: None,
            notice: None,
        }
    }

    /// The owner's status (a failed open, a save conflict, a notice): the
    /// bar says it in place of "Live" -- an error in red over anything, a
    /// plain notice only while the pane has nothing of its own to say --
    /// with the full text in the tooltip. None gives the bar back.
    pub fn set_notice(&mut self, notice: Option<(String, String, bool)>) {
        if self.notice != notice {
            self.notice = notice;
            self.repaint = true;
        }
    }

    /// No check before an edit applies: each one is the owner's at once
    /// ([`Self::take_edit`]); one that does not load is marked on its lines
    /// ([`Self::edit_failed`]).
    pub fn live(mut self) -> Self {
        self.live = true;
        self
    }

    /// Live: the text to apply now, and the document text it was typed on
    /// (the change is refused if the document is no longer that). One at a
    /// time: the newest text goes when the one before has landed or failed.
    pub fn take_edit(&mut self) -> Option<(String, Option<String>)> {
        if !self.live || self.in_flight.is_some() || self.conflict.is_some() {
            return None;
        }
        let draft = self.draft.clone()?;
        if self.failed.as_ref() == Some(&draft) {
            return None;
        }
        self.in_flight = Some(draft.clone());
        Some((draft, self.applied.clone()))
    }

    /// Live: the text handed over did not load. Its problems are marked
    /// while it is still the text in the view; the document keeps what it
    /// was.
    pub fn edit_failed(&mut self, cx: &mut Cx, ui: &WidgetRef, text: &str, problems: Vec<Problem>) {
        if self.in_flight.as_deref() == Some(text) {
            self.in_flight = None;
        }
        self.failed = Some(text.to_string());
        if self.draft.as_deref() == Some(text) {
            self.problems = problems;
            self.mark(cx, ui);
        }
        self.repaint = true;
    }

    /// No change-history bar (a host that is not about the history): a
    /// change written elsewhere (the AI, a tool) shows as a chip that opens
    /// its diff once.
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// The chip goes (the host moved on: another slide).
    pub fn dismiss_chip(&mut self) {
        if self.foreign.take().is_some() {
            self.repaint = true;
        }
    }

    pub fn has_draft(&self) -> bool { self.draft.is_some() }

    pub fn draft_text(&self) -> Option<&str> { self.draft.as_deref() }

    pub fn draft_base(&self) -> Option<&str> { self.draft_base.as_deref() }

    pub fn restore_draft(&mut self, text: String, base: Option<String>) {
        self.conflict = self.applied.as_ref().filter(|applied| Some(applied.as_str()) != base.as_deref()).map(|applied| (applied.clone(), "Recovered draft".into()));
        self.draft = Some(text);
        self.draft_base = base;
        self.draft_revision += 1;
        self.due = true;
        self.repaint = true;
    }

    /// Update an inactive file without writing into the shared editor widget.
    pub fn sync_detached(&mut self, text: &str, author: &str) {
        if self.landed(text) { return; }
        if self.applied.as_deref() == Some(text) { return; }
        let own = self.draft.as_deref() == Some(text);
        if let Some(before) = self.applied.replace(text.into()) {
            let rows = diff::rows(&before, text);
            self.changes.push(Change { author: author.into(), before, after: text.into(), rows });
            if !self.diff_on { self.shown = Some(self.changes.len() - 1); }
            self.foreign = (!own).then(|| self.changes.len() - 1);
        }
        match &self.draft {
            Some(draft) if draft == text => { self.draft = None; self.draft_base = None; self.conflict = None; self.problems.clear(); self.proven = None; }
            Some(_) => self.conflict = Some((text.into(), author.into())),
            None => {}
        }
        self.repaint = true;
    }

    pub fn suspend(&mut self) {
        self.sent = None;
        self.due = self.draft.is_some();
    }

    pub fn resume(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let text = self.draft.as_ref().or(self.applied.as_ref()).cloned().unwrap_or_default();
        self.set_code(cx, ui, &text);
        self.mark(cx, ui);
        self.repaint = true;
    }

    fn part(&self, cx: &Cx, ui: &WidgetRef, id: &[LiveId]) -> WidgetRef {
        ui.widget(cx, &[self.pane]).widget(cx, id)
    }

    /// Whether the editor has the keys.
    pub fn has_focus(&self, cx: &Cx, ui: &WidgetRef) -> bool {
        let area = self.part(cx, ui, ids!(code)).area();
        area != Area::Empty && cx.has_key_focus(area)
    }

    /// Forget the document (another one is opened).
    pub fn reset(&mut self, cx: &mut Cx, ui: &WidgetRef, file: &str) {
        let (compact, live) = (self.compact, self.live);
        *self = Self::new(self.kind, self.pane, file);
        self.compact = compact;
        self.live = live;
        self.set_code(cx, ui, "");
    }

    /// Live: `text` is the edit handed over, now the document's. Typing
    /// that went on meanwhile stays the draft (it goes next); the view is
    /// not touched.
    fn landed(&mut self, text: &str) -> bool {
        if self.in_flight.as_deref() != Some(text) {
            return false;
        }
        self.in_flight = None;
        self.failed = None;
        let before = self.applied.replace(text.to_string());
        // What the view shows: the draft, or (typed back meanwhile) the text before.
        let shown = self.draft.clone().or_else(|| before.clone());
        if let Some(before) = before.filter(|before| before != text) {
            let rows = diff::rows(&before, text);
            self.changes.push(Change { author: "You · source".into(), before, after: text.to_string(), rows });
            if !self.diff_on {
                self.shown = Some(self.changes.len() - 1);
            }
        }
        self.problems.clear();
        match shown {
            Some(shown) if shown != text => {
                self.draft = Some(shown);
                self.draft_base = Some(text.to_string());
            }
            _ => {
                self.draft = None;
                self.draft_base = None;
            }
        }
        self.repaint = true;
        true
    }

    /// Live, a file not in the view: the text handed over did not load.
    pub fn edit_failed_detached(&mut self, text: &str) {
        if self.in_flight.as_deref() == Some(text) {
            self.in_flight = None;
        }
        self.failed = Some(text.to_string());
    }

    /// The document is now `text` (after a change by `author`).
    pub fn sync(&mut self, cx: &mut Cx, ui: &WidgetRef, text: &str, author: &str) {
        if self.landed(text) {
            self.mark(cx, ui);
            return;
        }
        if self.applied.as_deref() == Some(text) {
            return;
        }
        // What was typed here coming back applied is not someone else's.
        let own = self.draft.as_deref() == Some(text);
        if let Some(before) = self.applied.take() {
            let rows = diff::rows(&before, text);
            self.changes.push(Change { author: author.to_string(), before, after: text.to_string(), rows });
            // The bar follows the newest change unless one is being read.
            if !self.diff_on {
                self.shown = Some(self.changes.len() - 1);
            }
            self.foreign = (!own && author != "You · source").then(|| self.changes.len() - 1);
        }
        self.applied = Some(text.to_string());
        match &self.draft {
            // The draft is what was just applied: it is the document now.
            Some(draft) if draft == text => {
                self.draft = None;
                self.draft_base = None;
                self.conflict = None;
            }
            Some(_) => self.conflict = Some((text.to_string(), author.to_string())),
            None => {
                self.set_code(cx, ui, text);
                self.mark(cx, ui);
                // What someone else wrote is brought into view.
                if author != "You · source" {
                    if let Some(line) = self.changes.last().and_then(|change| diff::added_lines(&change.rows).first().copied()) {
                        let code = self.part(cx, ui, ids!(code));
                        if let Some(mut view) = code.borrow_mut::<CodeView>() {
                            let CodeView { editor, session, .. } = &mut *view;
                            if let Some(session) = session {
                                editor.set_cursor_and_scroll(cx, Position { line_index: line, byte_index: 0 }, session);
                            }
                        };
                    }
                }
            }
        }
        self.repaint = true;
    }

    /// Put `text` in the editable view, keeping the caret's line.
    fn set_code(&mut self, cx: &mut Cx, ui: &WidgetRef, text: &str) {
        let code = self.part(cx, ui, ids!(code));
        let Some(mut view) = code.borrow_mut::<CodeView>() else { return };
        let caret = view.session.as_ref().and_then(|session| session.selections().first().map(|selection| selection.cursor.position));
        let prepared = CodeDocument::prepare_for_path(&self.file, text.into());
        let session = CodeSession::new(CodeDocument::from_prepared(prepared));
        if let Some(caret) = caret {
            let caret = session.clamp_position(caret);
            session.set_selection(caret, makepad_code_editor::selection::Affinity::Before, makepad_code_editor::session::SelectionMode::Simple, makepad_code_editor::history::NewGroup::Yes);
        }
        view.session = Some(session);
        view.redraw(cx);
        drop(view);
        // A new document: the readouts go on it again.
        if let Some(mut editor) = self.part(cx, ui, ids!(code_box)).borrow_mut::<SourceEditor>() {
            let readouts = std::mem::take(&mut editor.readouts);
            editor.set_readouts(cx, readouts);
        };
    }

    /// Inline marks: the problems of the draft, or the lines the newest
    /// change wrote.
    fn mark(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let code = self.part(cx, ui, ids!(code));
        let Some(mut view) = code.borrow_mut::<CodeView>() else { return };
        let Some(session) = view.session.as_mut() else { return };
        let mut document = session.document().clone();
        document.clear_decorations();
        let lines: Vec<String> = document.as_text().as_lines().to_vec();
        let line_end = |line: usize| lines.get(line).map_or(0, |text| text.len());
        let mut id = 0;
        if !self.problems.is_empty() {
            for problem in &self.problems {
                let line = (problem.line.max(1) - 1) as usize;
                if line >= lines.len() {
                    continue;
                }
                let text = &lines[line];
                // Columns count characters; the editor counts bytes.
                let start = text.char_indices().nth((problem.col.max(1) - 1) as usize).map_or(text.len(), |(at, _)| at);
                let (start, end) = if start >= line_end(line) { (0, line_end(line)) } else { (start, line_end(line)) };
                id += 1;
                document.add_decoration(Decoration::new(id, Position { line_index: line, byte_index: start }, Position { line_index: line, byte_index: end.max(start + 1).min(line_end(line)) }, DecorationType::Error));
            }
        } else if let Some(change) = self.changes.last().filter(|change| Some(change.after.as_str()) == self.applied.as_deref()) {
            for line in diff::added_lines(&change.rows) {
                if line + 1 >= lines.len() {
                    continue;
                }
                id += 1;
                document.add_decoration(Decoration::new(id, Position { line_index: line, byte_index: 0 }, Position { line_index: line + 1, byte_index: 0 }, DecorationType::DiffAdded));
            }
        }
        if let Some((first, last)) = self.highlight {
            for line in first..=last.min(lines.len().saturating_sub(1)) {
                id += 1;
                document.add_decoration(Decoration::new(id, Position { line_index: line, byte_index: 0 }, Position { line_index: line, byte_index: 0 }, DecorationType::DiffChangedGutter));
            }
        }
        view.redraw(cx);
    }

    /// Point at lines `first..=last`: marked in the gutter and brought into
    /// view (not while a draft is being typed).
    pub fn reveal(&mut self, cx: &mut Cx, ui: &WidgetRef, first: usize, last: usize) {
        self.highlight = Some((first, last.max(first)));
        self.mark(cx, ui);
        if self.draft.is_some() {
            return;
        }
        let code = self.part(cx, ui, ids!(code));
        if let Some(mut view) = code.borrow_mut::<CodeView>() {
            let CodeView { editor, session, .. } = &mut *view;
            if let Some(session) = session {
                // Centred, as CUT's code view jumps: not clipped to the
                // length the previous document was laid out at.
                editor.set_cursor_and_scroll(cx, Position { line_index: first, byte_index: 0 }, session);
                editor.center_line(cx, first);
            }
        };
    }

    /// Clear the externally selected node without changing the source.
    pub fn clear_selection(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.highlight = None;
        self.mark(cx, ui);
        let code = self.part(cx, ui, ids!(code));
        if let Some(mut view) = code.borrow_mut::<CodeView>() {
            view.editor.set_external_selection_focus(cx, false);
        };
    }

    /// Select the authored node itself using its exact UTF-8 byte span.
    pub fn reveal_span(&mut self, cx: &mut Cx, ui: &WidgetRef, span: std::ops::Range<usize>) {
        if self.draft.is_some() { return; }
        let Some(text) = self.applied.as_deref() else { return };
        let position = |byte: usize| {
            let before = &text[..byte.min(text.len())];
            Position { line_index: before.bytes().filter(|b| *b == b'\n').count(), byte_index: before.rfind('\n').map_or(before.len(), |at| before.len()-at-1) }
        };
        let (start,end) = (position(span.start), position(span.end));
        self.highlight = Some((start.line_index, end.line_index));
        self.mark(cx, ui);
        let code = self.part(cx, ui, ids!(code));
        if let Some(mut view) = code.borrow_mut::<CodeView>() {
            let CodeView { editor, session, .. } = &mut *view;
            if let Some(session) = session {
                // The node selected, its first line in the middle of the
                // view (as CUT's code view jumps), even right after another
                // file was put in the view.
                editor.set_selection_and_scroll(cx, end, start, session);
                editor.center_line(cx, start.line_index);
                editor.set_external_selection_focus(cx, true);
            }
        };
    }

    /// The settle timer: the draft is due for checking.
    pub fn handle_event(&mut self, event: &Event) -> bool {
        if self.timer.is_event(event).is_some() {
            self.due = true;
            return true;
        }
        false
    }

    /// The values at the playhead, at the end of their lines: (line, text).
    pub fn set_readouts(&mut self, cx: &mut Cx, ui: &WidgetRef, readouts: Vec<(usize, String)>) {
        if let Some(mut editor) = self.part(cx, ui, ids!(code_box)).borrow_mut::<SourceEditor>() {
            editor.set_readouts(cx, readouts);
        };
    }

    pub fn handle_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        let code = self.part(cx, ui, ids!(code));
        if actions.find_widget_action(code.widget_uid()).is_some_and(|action| matches!(action.cast::<CodeViewAction>(), CodeViewAction::Changed)) {
            self.draft_revision += 1;
            let text = code.text();
            self.proven = None;
            self.foreign = None;
            if self.applied.as_deref() == Some(text.as_str()) {
                self.draft = None;
                self.draft_base = None;
                self.conflict = None;
                self.problems.clear();
            } else {
                if self.draft.is_none() { self.draft_base = self.applied.clone(); }
                self.draft = Some(text);
                if !self.live {
                    cx.stop_timer(self.timer);
                    self.timer = cx.start_timeout(SETTLE);
                }
            }
            self.repaint = true;
        }
        if self.part(cx, ui, ids!(revert)).as_button().clicked(actions) {
            self.draft_revision += 1;
            self.draft = None;
            self.draft_base = None;
            self.proven = None;
            self.problems.clear();
            self.conflict = None;
            let applied = self.applied.clone().unwrap_or_default();
            self.set_code(cx, ui, &applied);
            self.mark(cx, ui);
            self.repaint = true;
        }
        if self.part(cx, ui, ids!(keep)).as_button().clicked(actions) {
            self.draft_revision += 1;
            // Keep typing: the other version stays in the history.
            self.conflict = None;
            self.draft_base = self.applied.clone();
            self.due = self.draft.is_some();
            self.repaint = true;
        }
        if self.part(cx, ui, ids!(copy)).as_button().clicked(actions) {
            cx.copy_to_clipboard(&self.raw);
        }
        if self.part(cx, ui, ids!(state_button)).as_button().clicked(actions) {
            // To the first problem's line.
            if let Some(problem) = self.problems.first() {
                let code = self.part(cx, ui, ids!(code));
                if let Some(mut view) = code.borrow_mut::<CodeView>() {
                    let CodeView { editor, session, .. } = &mut *view;
                    if let Some(session) = session {
                        editor.set_cursor_and_scroll(cx, Position { line_index: (problem.line.max(1) - 1) as usize, byte_index: 0 }, session);
                    }
                };
            }
        }
        if self.part(cx, ui, ids!(take)).as_button().clicked(actions) {
            self.draft_revision += 1;
            // The other version wins; the draft is dropped.
            self.draft = None;
            self.draft_base = None;
            self.proven = None;
            self.problems.clear();
            if let Some((text, _)) = self.conflict.take() {
                self.set_code(cx, ui, &text);
                self.mark(cx, ui);
            }
            self.repaint = true;
        }
        let step = |pane: &mut Self, delta: isize| {
            if pane.changes.is_empty() {
                return;
            }
            let last = pane.changes.len() as isize - 1;
            let at = pane.shown.map_or(last, |index| index as isize);
            pane.shown = Some((at + delta).clamp(0, last) as usize);
            pane.repaint = true;
        };
        if self.part(cx, ui, ids!(change_prev)).as_button().clicked(actions) {
            step(self, -1);
        }
        if self.part(cx, ui, ids!(change_next)).as_button().clicked(actions) {
            step(self, 1);
        }
        if let Some(index) = self.foreign.filter(|_| self.part(cx, ui, ids!(foreign_chip)).as_button().clicked(actions)) {
            // The change, as a diff, once: the chip is spent.
            self.shown = Some(index);
            self.diff_on = true;
            self.foreign = None;
            self.repaint = true;
        }
        if self.part(cx, ui, ids!(diff_toggle)).as_button().clicked(actions) && !self.changes.is_empty() {
            self.diff_on = !self.diff_on;
            if self.shown.is_none() {
                self.shown = Some(self.changes.len() - 1);
            }
            self.repaint = true;
        }
    }

    /// Send a settled draft to the checker.
    pub fn pump(&mut self, checker: &Checker, serial: &mut u64) {
        if self.live || !self.due || self.sent.is_some() || self.conflict.is_some() {
            return;
        }
        let Some(draft) = self.draft.clone() else {
            self.due = false;
            return;
        };
        let check = Check { serial: *serial + 1, kind: self.kind, source: draft, modules: Vec::new(), activity: false, film: None, tree: self.tree.clone().map(|tree| (tree, self.file.clone())), diagnose: false };
        if checker.try_send(check).is_ok() {
            *serial += 1;
            self.sent = Some(*serial);
            self.due = false;
        }
    }

    /// A checker answer; true when it was this pane's.
    pub fn checked(&mut self, cx: &mut Cx, ui: &WidgetRef, checked: &Checked) -> bool {
        if self.sent != Some(checked.serial) {
            return false;
        }
        self.sent = None;
        // Typed on meanwhile: that newer draft is checked next.
        if self.draft.as_deref() != Some(checked.source.as_str()) {
            self.due = self.draft.is_some();
            return true;
        }
        self.problems = checked.problems.clone();
        if self.problems.is_empty() {
            self.proven = Some(checked.source.clone());
        }
        self.mark(cx, ui);
        self.repaint = true;
        true
    }

    /// Live: the problems found for a text that did not load, on their
    /// lines, while it is still the text in the view.
    pub fn diagnosed(&mut self, cx: &mut Cx, ui: &WidgetRef, source: &str, problems: Vec<Problem>) {
        if problems.is_empty() || self.failed.as_deref() != Some(source) || self.draft.as_deref() != Some(source) {
            return;
        }
        self.problems = problems;
        self.mark(cx, ui);
        self.repaint = true;
    }

    /// A proven draft for the owner to apply.
    pub fn take_proven(&mut self) -> Option<String> {
        self.proven.take().filter(|text| self.conflict.is_none() && self.draft.as_ref() == Some(text))
    }

    pub fn paint(&mut self, cx: &mut Cx, ui: &WidgetRef, p: &makepad_look::palette::Palette) {
        if !std::mem::take(&mut self.repaint) {
            return;
        }
        // The status (§4.9): a dot and a human sentence; the raw trace is
        // in the Tip, copied by "Copy details".
        let (dot, state, raw) = if self.conflict.is_some() {
            (p.amber, "Changed elsewhere".to_string(), String::new())
        } else if let Some(problem) = self.problems.first() {
            let more = if self.problems.len() > 1 { format!(" · {} more", self.problems.len() - 1) } else { String::new() };
            let raw = self.problems.iter().map(|problem| format!("{}:{}: {}", problem.line, problem.col, problem.message)).collect::<Vec<_>>().join("\n");
            (p.danger, clip(&format!("Line {}:{} · {}{more}", problem.line, problem.col, human(&problem.message)), 80), raw)
        } else if !self.live && (self.sent.is_some() || (self.draft.is_some() && self.proven.is_none())) {
            (p.amber, "Checking…".to_string(), String::new())
        } else if !self.live && self.proven.is_some() {
            (p.amber, "Applying…".to_string(), String::new())
        } else {
            (p.live, "Live · every edit here applies and can be undone".to_string(), String::new())
        };
        let mut notice_tip = None;
        let mut notice_error = false;
        let (dot, state) = match &self.notice {
            Some((text, tip, error)) if *error || (self.conflict.is_none() && self.problems.is_empty()) => {
                notice_tip = Some(tip.clone());
                notice_error = *error;
                (if *error { p.danger } else { p.amber }, clip(text, 90))
            }
            _ => (dot, state),
        };
        self.raw = raw.clone();
        let mut dot_view = self.part(cx, ui, ids!(state_dot));
        let color = Vec4f::from_u32(dot);
        script_apply_eval!(cx, dot_view, {draw_bg +: {color: #(color)}});
        self.part(cx, ui, ids!(state)).set_text(cx, &state);
        // The owner's error reads in red; every other state in the bar's ink.
        let ink = Vec4f::from_u32(if notice_error { p.danger } else { p.secondary });
        let mut state_label = self.part(cx, ui, ids!(state));
        script_apply_eval!(cx, state_label, {draw_text +: {color: #(ink)}});
        let tip = if let Some(tip) = notice_tip { tip } else if raw.is_empty() { "The document's state".to_string() } else { format!("{raw}\n(click to go to the line)") };
        let mut tip_view = self.part(cx, ui, ids!(state_tip));
        script_apply_eval!(cx, tip_view, {text: #(tip)});
        self.part(cx, ui, ids!(copy)).set_visible(cx, !self.raw.is_empty());
        self.part(cx, ui, ids!(file)).set_text(cx, &self.file.to_uppercase());
        self.part(cx, ui, ids!(revert)).set_visible(cx, self.draft.is_some() && !self.problems.is_empty() && self.conflict.is_none());
        self.part(cx, ui, ids!(take)).set_visible(cx, self.conflict.is_some());
        self.part(cx, ui, ids!(keep)).set_visible(cx, self.conflict.is_some());
        // The history: hidden until there is a change.
        let shown = self.shown.and_then(|index| self.changes.get(index).map(|change| (index, change)));
        self.part(cx, ui, ids!(nav)).set_visible(cx, shown.is_some() && !self.compact);
        let chip = self.foreign.filter(|_| self.compact && !self.diff_on).and_then(|index| self.changes.get(index));
        self.part(cx, ui, ids!(foreign_chip)).set_visible(cx, chip.is_some());
        if let Some(change) = chip {
            let who = change.author.split(" · ").next().unwrap_or("Someone");
            let who = if who.starts_with("AI") { "AI" } else { who };
            self.part(cx, ui, ids!(foreign_chip)).set_text(cx, &format!("{who} changed this · view"));
        }
        if let Some((index, change)) = shown {
            let (added, removed) = change.counts();
            self.part(cx, ui, ids!(change)).set_text(cx, &format!("{} of {}", index + 1, self.changes.len()));
            let (who, what) = match change.author.split_once(" · ") {
                Some((who, what)) => (who.to_string(), what.to_string()),
                None => (change.author.clone(), String::new()),
            };
            self.part(cx, ui, ids!(author)).set_text(cx, &who);
            let face = Vec4f::from_u32(if who.starts_with("AI") { p.accent_wash } else { p.raised });
            let mut chip = self.part(cx, ui, ids!(author_chip));
            script_apply_eval!(cx, chip, {draw_bg +: {color: #(face)}});
            self.part(cx, ui, ids!(what)).set_text(cx, &what);
            self.part(cx, ui, ids!(plus)).set_text(cx, &format!("+{added}"));
            self.part(cx, ui, ids!(minus)).set_text(cx, &format!("−{removed}"));
        }
        // Compact: the diff's own toggle only while it is open (to close it).
        self.part(cx, ui, ids!(diff_toggle)).set_visible(cx, shown.is_some() && (!self.compact || self.diff_on));
        let look = p.pick(self.diff_on, false, p.control);
        let (lit, edge, ink) = (Vec4f::from_u32(look.fill), Vec4f::from_u32(if self.diff_on { look.edge } else { p.border_hi }), Vec4f::from_u32(if self.diff_on { look.ink } else { p.label }));
        let mut toggle = self.part(cx, ui, ids!(diff_toggle));
        script_apply_eval!(cx, toggle, {draw_bg +: {color: #(lit) color_focus: #(lit) border_color: #(edge)} draw_text +: {color: #(ink) color_focus: #(ink)}});
        // Inline chips at the ends of the lines with problems.
        // One chip a line: its first problem, and how many more it has.
        let mut chips: Vec<(usize, String)> = Vec::new();
        let mut counts: Vec<usize> = Vec::new();
        for problem in &self.problems {
            let line = (problem.line.max(1) - 1) as usize;
            match chips.iter().position(|(l, _)| *l == line) {
                Some(at) => counts[at] += 1,
                None => {
                    chips.push((line, clip(&human(&problem.message), 60)));
                    counts.push(0);
                }
            }
        }
        for ((_, message), more) in chips.iter_mut().zip(counts) {
            if more > 0 {
                message.push_str(&format!(" · +{more}"));
            }
        }
        if let Some(mut editor) = self.part(cx, ui, ids!(code_box)).borrow_mut::<SourceEditor>() {
            editor.set_chips(cx, chips);
        };
        self.part(cx, ui, ids!(diff_toggle)).set_text(cx, "Changes");
        let diff_on = self.diff_on && self.shown.is_some();
        self.part(cx, ui, ids!(code_box)).set_visible(cx, !diff_on);
        self.part(cx, ui, ids!(diff_box)).set_visible(cx, diff_on);
        if diff_on && self.diff_loaded != self.shown {
            self.diff_loaded = self.shown;
            if let Some(change) = self.shown.and_then(|index| self.changes.get(index)) {
                let specs = diff::specs(&change.rows);
                let view = self.part(cx, ui, ids!(diff));
                if let Some(mut view) = view.borrow_mut::<CodeView>() {
                    match CodeDocument::prepare_diff_for_path(&self.file, &specs, &change.before, &change.after) {
                        Ok(prepared) => {
                            let mut session = CodeSession::new(CodeDocument::from_prepared_diff(prepared));
                            // The first changed row in view, a little context above.
                            if let Some(row) = specs.iter().position(|spec| spec.hunk != 0) {
                                let CodeView { editor, .. } = &mut *view;
                                editor.set_cursor_and_scroll(cx, Position { line_index: row.saturating_sub(3), byte_index: 0 }, &mut session);
                                editor.set_cursor_and_scroll(cx, Position { line_index: row, byte_index: 0 }, &mut session);
                            }
                            view.session = Some(session);
                        }
                        Err(error) => log!("source view: the diff of change {} did not prepare: {error:?}", self.diff_loaded.unwrap_or(0)),
                    }
                    view.redraw(cx);
                };
            }
        }
    }
}

/// At most `max` characters, with an ellipsis.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max.saturating_sub(1)).collect::<String>())
}

/// A token as people write it.
fn token(name: &str) -> String {
    match name.trim() {
        "CloseSquare" => "`]`".into(),
        "OpenSquare" => "`[`".into(),
        "CloseCurly" => "`}`".into(),
        "OpenCurly" => "`{`".into(),
        "CloseRound" => "`)`".into(),
        "OpenRound" => "`(`".into(),
        "End" | "StreamEnd" => "the end of the text".into(),
        "StringUnfinished" => "an unfinished string".into(),
        other if other.starts_with("Identifier") => "a name".into(),
        other if other.starts_with("String") => "a string".into(),
        other if other.starts_with("Operator") => "an operator".into(),
        other if other.starts_with("Separator") => "a separator".into(),
        other if other.starts_with(|c: char| c == 'F' || c == 'U' || c == 'I') && other.contains('(') => "a number".into(),
        other => format!("`{other}`"),
    }
}

/// A loader's or parser's message as a sentence for people: the parser's
/// state dumps and object ids go (they stay in the Tip).
pub fn human(message: &str) -> String {
    let message = message.trim();
    // The interpreter's own origin goes.
    let message = [" (from: ", " (platform/"].iter().filter_map(|marker| message.find(marker)).min().map_or(message, |at| &message[..at]);
    // "Expected expression after [..] found CloseSquare"
    if let Some(rest) = message.strip_prefix("Expected ") {
        if let Some(found_at) = rest.rfind(" found ") {
            let expected = rest[..found_at].split(" after ").next().unwrap_or("").trim();
            let found = token(rest[found_at + 7..].split_whitespace().next().unwrap_or(""));
            let expected = match expected {
                "code" | "expression" => "a value".to_string(),
                other => other.to_string(),
            };
            let found = found[..1].to_uppercase() + &found[1..];
            return format!("Unexpected {found}: expected {expected}");
        }
    }
    // "Named assign size not allowed in array literal context"
    if let Some(rest) = message.strip_prefix("Named assign ").or_else(|| message.strip_prefix("named assign ")) {
        if let Some(name) = rest.split(" not allowed in array").next().filter(|_| rest.contains(" not allowed in array")) {
            return format!("`{name}:` is loose in a list: a field goes inside a layer's {{ }}");
        }
    }
    // "Pod vec2f expects number, bool or vector, got object"
    if let Some(rest) = message.strip_prefix("Pod ").or_else(|| message.strip_prefix("pod ")) {
        let ty = rest.split_whitespace().next().unwrap_or("");
        let got = rest.rsplit(", got ").next().filter(|_| rest.contains(", got ")).unwrap_or("something else");
        let ty = match ty {
            "vec2f" | "vec2" => "vec2(x, y)",
            "vec3f" | "vec3" => "vec3(x, y, z)",
            "vec4f" | "vec4" => "colour",
            "f32" | "f64" => "number",
            other => other,
        };
        return format!("A {ty} goes here, not {} {got}", if got.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" });
    }
    // "variable nope not found in scope. Did you mean: Node(obj414) or …"
    if let Some(rest) = message.strip_prefix("variable ") {
        if let Some(name) = rest.split(" not found").next().filter(|_| rest.contains(" not found")) {
            return format!("`{name}` is not defined here");
        }
    }
    // Any other trace in brackets or object ids goes.
    let mut out = String::with_capacity(message.len());
    let mut depth = 0usize;
    for c in message.chars() {
        match c {
            '[' if message.contains("EndStmt") || message.contains("Emit") => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.split(". Did you mean").next().unwrap_or(&out).to_string();
    let out = out.replace("  ", " ");
    let mut chars = out.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}


script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.SourceEditorBase = #(SourceEditor::register_widget(vm))
    mod.widgets.SourceEditor = set_type_default() do mod.widgets.SourceEditorBase{
        width: Fill height: Fill
        draw_chip_text +: {color: stage.warm text_style: theme.font_regular{font_size: 8}}
    }
}

/// The editable code view, with each problem's message in a chip at its
/// line's end (§4.9 inline diagnostics).
#[derive(Script, ScriptHook, Widget)]
pub struct SourceEditor {
    #[deref]
    view: View,
    #[live]
    draw_chip: DrawEditBlock,
    #[live]
    draw_chip_text: DrawText,
    /// Drawn after the editor (so over its text), in its own draw list's
    /// parent: `(line, message)`.
    #[rust]
    chips: Vec<(usize, String)>,
    /// The values at the playhead, at the end of their lines: (line, text).
    #[rust]
    readouts: Vec<(usize, String)>,
}

impl SourceEditor {
    /// The values at the playhead after their lines (`= 0.42  ×12`):
    /// drawn by the editor as inlays at each line's end, so they scroll and
    /// fold with the text.
    pub fn set_readouts(&mut self, cx: &mut Cx, readouts: Vec<(usize, String)>) {
        if self.readouts == readouts {
            return;
        }
        self.readouts = readouts;
        let code = self.view.widget(cx, ids!(code));
        if let Some(mut view) = code.borrow_mut::<CodeView>() {
            if let Some(session) = view.session.as_mut() {
                let lines: Vec<usize> = session.document().as_text().as_lines().iter().map(|l| l.len()).collect();
                let inlays = self.readouts.iter().filter_map(|(line, text)| Some((*line, *lines.get(*line)?, makepad_code_editor::inlays::InlineInlay::Text(format!("   = {text}"))))).collect();
                session.document().set_inline_inlays(inlays);
                session.relayout_inline();
            }
        }
        self.view.redraw(cx);
    }

    pub fn set_chips(&mut self, cx: &mut Cx, chips: Vec<(usize, String)>) {
        if self.chips != chips {
            self.chips = chips;
            self.view.redraw(cx);
        }
    }
}

impl Widget for SourceEditor {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // The chips follow the text when it scrolls.
        if !self.chips.is_empty() && matches!(event, Event::Scroll(_) | Event::KeyDown(_)) {
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while self.view.draw_walk(cx, scope, walk).is_step() {}
        if self.chips.is_empty() {
            return DrawStep::done();
        }
        let p = makepad_look::palette::get(cx);
        let code = self.view.widget(cx, ids!(code));
        let Some(view) = code.borrow::<CodeView>() else { return DrawStep::done() };
        let Some(session) = view.session.as_ref() else { return DrawStep::done() };
        // A cell is placed from the editor's corner; the viewport is in the
        // window's coordinates.
        let origin = view.editor.area().rect(cx).pos;
        let clip = view.editor.viewport_rect();
        let visible = view.editor.visible_lines();
        let text = session.document().as_text();
        let lines = text.as_lines();
        for (line, message) in self.chips.iter().filter(|(line, _)| visible.contains(line)) {
            let Some(end) = lines.get(*line).map(|text| text.len()) else { continue };
            let Some(cell) = view.editor.position_rect(session, Position { line_index: *line, byte_index: end }) else { continue };
            let pos = origin + cell.pos + dvec2(16.0, 0.0);
            let w = advance(&self.draw_chip_text, cx, &message) + 12.0;
            let chip = Rect { pos: dvec2(pos.x, pos.y + 1.0), size: dvec2(w, (cell.size.y - 2.0).max(14.0)) };
            if chip.pos.y < clip.pos.y - 2.0 || chip.pos.y + chip.size.y > clip.pos.y + clip.size.y || chip.pos.x > clip.pos.x + clip.size.x - 40.0 {
                continue;
            }
            self.draw_chip.fill = Vec4f::from_u32(p.danger_wash);
            self.draw_chip.edge = Vec4f::from_u32(p.danger);
            self.draw_chip.stroke = 1.0;
            self.draw_chip.radius = 3.0;
            self.draw_chip.shape = 0.0;
            self.draw_chip.draw_abs(cx, chip);
            self.draw_chip_text.draw_abs(cx, dvec2(chip.pos.x + 6.0, chip.pos.y + (chip.size.y - 11.0) * 0.5), &message);
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_traces_read_as_sentences() {
        let raw = "Expected expression after [EndStmt { last: 755 }, EmitLetDyn { index: 755, name: main }, EndProto, ArrayIndex] found CloseSquare (from: platform/script/src/parser.rs:4251)";
        assert_eq!(human(raw), "Unexpected `]`: expected a value");
        assert_eq!(human("variable nope not found in scope. Did you mean: Node(obj414) or line(fn635)"), "`nope` is not defined here");
        assert_eq!(human("Named assign size not allowed in array literal context"), "`size:` is loose in a list: a field goes inside a layer's { }");
        assert_eq!(human("Pod vec2f expects number, bool or vector, got object"), "A vec2(x, y) goes here, not an object");
        assert_eq!(human("layers[1].opacity: `opacity` 2 is outside 0..1"), "Layers[1].opacity: `opacity` 2 is outside 0..1");
        assert!(clip(&"x".repeat(100), 80).chars().count() == 80);
    }
}
