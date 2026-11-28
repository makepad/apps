//! The plate's journal. The app supplies a page and the selected detail from its reader worker.
use crate::api::AppState;
use makepad_widgets_core::*;
use makepad_git::diff::{diff_lines_bounded, DiffOp, Exhaustion};
use makepad_code_editor::{CodeDocument, CodeEditor, CodeSession, diff::PreparedDiffDocument, session::PreparedView};
pub use makepad_code_editor::diff::{DiffRowKind, DiffRowSpec};
use std::{cell::RefCell, rc::Rc, sync::Arc};

/// Prepared on the journal reader, including syntax tokens and the unwrapped editor layout.
pub struct JournalDiff {
    pub label: String,
    pub text: Option<(PreparedDiffDocument, PreparedView, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextDiffError {
    Cancelled,
    Invalid,
}

impl JournalDiff {
    /// Prepare the line diff, syntax tokens and editor layout on the calling worker.
    pub fn text(name: &str, before: &str, after: &str, current: &dyn Fn() -> bool) -> Result<Self, TextDiffError> {
        let diff = diff_lines_bounded(before, after, &Default::default(), &|| !current());
        if !current() { return Err(TextDiffError::Cancelled) }
        let mut rows = Vec::new();
        let mut hunk = 0;
        let mut changed = false;
        for op in diff.ops {
            let equal = matches!(op, DiffOp::Equal { .. });
            if !equal && !changed { hunk += 1; }
            changed = !equal;
            let (kind, old, new, len) = match op {
                DiffOp::Equal { old_index, new_index, len } => (DiffRowKind::Equal, Some(old_index), Some(new_index), len),
                DiffOp::Delete { old_index, len } => (DiffRowKind::Removed, Some(old_index), None, len),
                DiffOp::Insert { new_index, len } => (DiffRowKind::Added, None, Some(new_index), len),
            };
            for line in 0..len {
                let number = |i: usize| u32::try_from(i + line).map_err(|_| TextDiffError::Invalid);
                rows.push(DiffRowSpec { kind, old_line: old.map(number).transpose()?, new_line: new.map(number).transpose()?, hunk: if equal { 0 } else { hunk } });
            }
        }
        let mut prepared = Self::prepare(name, &rows, before, after).map_err(|_| TextDiffError::Invalid)?;
        if diff.unavailable.is_some() {
            prepared.label.push_str(" · ");
            prepared.label.push_str(Exhaustion::LABEL);
        } else if hunk == 0 && before != after {
            prepared.label.push_str(" · Line endings changed");
        }
        Ok(prepared)
    }

    pub fn note(label: String) -> Self { Self { label, text: None } }
    pub fn prepare(name: &str, rows: &[DiffRowSpec], old: &str, new: &str) -> Result<Self, makepad_code_editor::diff::PrepareDiffError> {
        let document = CodeDocument::prepare_diff_for_path(name, rows, old, new)?;
        let first = document.metadata().rows.iter().position(|r| matches!(r.kind, Some(DiffRowKind::Added | DiffRowKind::Removed))).unwrap_or(0);
        let view = CodeSession::prepare_view(document.document(), None);
        Ok(Self { label: name.into(), text: Some((document, view, first)) })
    }
}

pub enum JournalDiffUpdate {
    Keep(usize),
    Prepared(JournalDiff),
}

#[derive(Clone)]
struct ShownDiff {
    item: LiveId,
    label: String,
    text: Option<(Rc<RefCell<CodeSession>>, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordKey {
    pub journal: u64,
    pub folder: Arc<str>,
    pub number: u64,
    /// A damaged line has a byte offset instead of an order.
    pub damaged: bool,
}

pub struct JournalRow {
    pub key: RecordKey,
    pub title: String,
    pub context: String,
}

#[derive(Clone)]
pub struct JournalSession {
    pub token: Option<u64>,
    pub folder: Arc<str>,
    pub title: String,
    pub description: String,
    pub copy_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovedRevision {
    pub incarnation: Option<u128>,
    pub length: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct RemovedSession {
    pub name: std::ffi::OsString,
    /// None when the inventory could not verify the archive. Empty keeps it.
    pub revision: Option<RemovedRevision>,
}

#[derive(Default)]
pub enum JournalSource {
    #[default]
    Plate,
    Sessions,
    Session(JournalSession),
}

#[derive(Default)]
pub struct JournalPage {
    /// The dock's visibility mask starts with every bit set. A draw confirms this tab is on screen.
    pub drawn: bool,
    pub rows: Arc<[JournalRow]>,
    pub source: JournalSource,
    pub sessions: Arc<[JournalSession]>,
    pub removed: Arc<[RemovedSession]>,
    pub empty_confirm: Option<Arc<[RemovedSession]>>,
    pub remove_confirm: Option<Arc<str>>,
    pub selected: Option<RecordKey>,
    pub detail: String,
    pub replay_status: String,
    pub replay_waiting: bool,
    pub through: Option<RecordKey>,
    pub count: usize,
    diffs: Rc<[ShownDiff]>,
    pub health: String,
    pub message: String,
    pub actor: usize,
    pub kind: usize,
    pub older: bool,
    pub newer: bool,
    pub revision: u64,
    pub navigation: u64,
}

impl JournalPage {
    pub fn can_empty(&self) -> bool {
        matches!(self.source, JournalSource::Sessions) && !self.removed.is_empty()
    }

    pub fn reset(&mut self) {
        *self = Self { revision: self.revision + 1, navigation: self.navigation + 1, ..Self::default() };
    }

    pub fn diff_session(&self, index: usize) -> Option<makepad_code_editor::session::SessionId> {
        self.diffs.get(index)?.text.as_ref().map(|(session, _)| session.borrow().id())
    }

    pub fn set_diffs(&mut self, diffs: Vec<JournalDiffUpdate>) {
        // A new file can sort before an open diff. Keep that editor's widget as
        // well as its session, and recycle free widget slots for the new files.
        let retained: std::collections::HashSet<_> = diffs.iter().filter_map(|update| match update {
            JournalDiffUpdate::Keep(index) => self.diffs.get(*index).map(|d| d.item.0),
            JournalDiffUpdate::Prepared(_) => None,
        }).collect();
        let mut free = (1..).filter(|id| !retained.contains(id));
        self.diffs = diffs.into_iter().filter_map(|update| {
            let d = match update {
                JournalDiffUpdate::Keep(index) => return self.diffs.get(index).cloned(),
                JournalDiffUpdate::Prepared(d) => d,
            };
            let mut label = d.label;
            let text = d.text.and_then(|(document, view, first)| {
                match CodeSession::from_prepared(CodeDocument::from_prepared_diff(document), view) {
                    Ok(session) => Some((Rc::new(RefCell::new(session)), first)),
                    Err(_) => { label.push_str(" · Diff unavailable"); None }
                }
            });
            Some(ShownDiff { item: LiveId(free.next()?), label, text })
        }).collect();
    }
}

impl RecordKey {
    fn between(&self, anchor: Option<&RecordKey>, through: Option<&RecordKey>) -> bool {
        let Some(a) = anchor else { return false };
        let b = through.unwrap_or(a);
        let position = |k: &RecordKey| (k.damaged, k.number);
        self.folder == a.folder && self.folder == b.folder && self.journal == a.journal && self.journal == b.journal && (position(a).min(position(b))..=position(a).max(position(b))).contains(&position(self))
    }
}

#[derive(Clone, Debug)]
pub enum JournalAction {
    ThisPlate,
    Sessions,
    BrowseSession(Arc<str>),
    RemoveSession { folder: Arc<str>, token: u64 },
    EmptyRemoved,
    Filter { actor: usize, kind: usize },
    Select(RecordKey),
    Extend(RecordKey),
    Copy,
    Replay,
    CancelReplay,
    Older,
    Newer,
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    let DiffEditor = #(JournalDiffEditor::register_widget(vm))
    mod.widgets.FabJournalDiffEditor = DiffEditor{width: Fill height: 172
        editor +: {read_only: true show_gutter: true word_wrap: false empty_page_at_end: false
            draw_bg +: {color: fab.color_editor}
            draw_text +: {text_style: theme.font_code{font_size: 9.0}}
        }
    }
    let Entry = #(JournalItem::register_widget(vm))
    mod.widgets.FabJournalPanelBase = #(FabJournalPanel::register_widget(vm))
    mod.widgets.FabJournalPanel = set_type_default() do mod.widgets.FabJournalPanelBase{
        width: Fill height: Fill flow: Down
        padding: Inset{left: 8 right: 8 top: 6 bottom: 6} spacing: 5
        show_bg: true draw_bg +: {color: fab.color_editor}
        health := FabLabel{width: Fill height: Fit visible: false draw_text +: {color: fab.color_warning}}
        filters := View{width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 6
            source := PropDropDown{width: 198 labels: ["This plate" "Sessions without a saved plate"] a11y: {label: "Journal source"}}
            sessions := FabButton{text: "Back to sessions" visible: false}
            remove := FabButton{text: "Remove session…" visible: false}
            empty := FabButton{text: "Empty removed sessions…" visible: false}
            actor := PropDropDown{width: 104 labels: ["All actors" "Person" "AI" "Agent"] a11y: {label: "Journal actor"}}
            kind := PropDropDown{width: 110 labels: ["All records" "Call" "Result" "Edit" "History" "Reload" "Checkpoint" "Lost" "Damaged" "Other"] a11y: {label: "Record kind"}}
        }
        session_bar := View{visible: false width: Fill height: 26 flow: Right align: Align{y: 0.5} spacing: 4
            session := FabLabel{width: Fill height: Fit}
            confirm := View{visible: false width: Fill height: Fill flow: Right align: Align{y: 0.5} spacing: 4
                FabLabelSmall{width: Fill text: "Remove this session?" max_lines: 1 text_overflow: TextOverflow.Ellipsis}
                remove_yes := FabButton{text: "Remove"}
                keep := FabButton{text: "Cancel"}
            }
            empty_confirm := View{visible: false width: Fill height: Fill flow: Right align: Align{y: 0.5} spacing: 4
                question := FabLabelSmall{width: Fill max_lines: 1 text_overflow: TextOverflow.Ellipsis}
                empty_yes := FabButton{text: "Delete"}
                keep := FabButton{text: "Cancel"}
            }
        }
        message := FabLabelDim{width: Fill height: Fit visible: false}
        list := PortalList{
            width: Fill height: Fill flow: Down auto_tail: false reuse_items: true
            Row := Entry{
                width: Fill height: Fit flow: Right spacing: 6
                padding: Inset{left: 5 right: 5 top: 4 bottom: 4}
                cursor: MouseCursor.Hand show_bg: true
                draw_bg +: { selected: instance(0.0)
                    pixel: fn() { return fab.color_editor.mix(fab.color_selection_bg, self.selected) }
                }
                title := FabLabel{width: Fill height: Fit}
                context := FabLabelDim{width: Fit height: Fit}
            }
        }
        pages := View{width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 5
            newer := FabButton{text: "Newer"}
            older := FabButton{text: "Older"}
        }
        copy_bar := View{visible: false width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 6 align: Align{y: 0.5}
            count := FabLabelDim{}
            copy := FabButton{text: "Copy as script"}
        }
        detail := ScrollYView{
            visible: false width: Fill height: Fill flow: Down spacing: 5
            FabHr{}
            replay_bar := View{width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 5
                replay := FabButton{text: "Replay from here"}
                cancel := FabButton{text: "Cancel" visible: false}
            }
            replay_status := FabLabelDim{width: Fill height: Fit visible: false}
            diffs := FlatList{width: Fill height: Fit flow: Down
                Document := View{width: Fill height: Fit flow: Down spacing: 3
                    name := FabLabel{width: Fill height: Fit}
                    editor := mod.widgets.FabJournalDiffEditor{}
                }
            }
            text := FabLabel{width: Fill height: Fit}
        }
    }
}

#[derive(Clone)]
enum Items {
    Records(Arc<[JournalRow]>),
    Sessions(Arc<[JournalSession]>),
}

impl Items {
    fn len(&self) -> usize {
        match self { Self::Records(rows) => rows.len(), Self::Sessions(rows) => rows.len() }
    }

    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Records(a), Self::Records(b)) => Arc::ptr_eq(a, b),
            (Self::Sessions(a), Self::Sessions(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    fn labels(&self, i: usize) -> Option<(&str, &str)> {
        match self {
            Self::Records(rows) => rows.get(i).map(|r| (r.title.as_str(), r.context.as_str())),
            Self::Sessions(rows) => rows.get(i).map(|s| (s.title.as_str(), s.description.as_str())),
        }
    }

    fn press(&self, i: usize, extend: bool) -> Option<JournalAction> {
        match self {
            Self::Records(rows) => rows.get(i).map(|r| if extend { JournalAction::Extend(r.key.clone()) } else { JournalAction::Select(r.key.clone()) }),
            Self::Sessions(rows) => rows.get(i).map(|s| JournalAction::BrowseSession(s.folder.clone())),
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
struct JournalItem {
    #[deref] view: View,
    #[rust] bound: Option<(Items, usize)>,
    #[rust] selected: bool,
}

impl JournalItem {
    fn press(&self, extend: bool) -> Option<JournalAction> {
        let (items, index) = self.bound.as_ref()?;
        items.press(*index, extend)
    }
}

impl Widget for JournalItem {
    fn semantics(&self, _: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::ListItem;
        node.actions = a11y_tree::Actions::PRESS;
        node.states.set(a11y_tree::States::SELECTED, self.selected);
        if let Some((title, description)) = self.bound.as_ref().and_then(|(items, i)| items.labels(*i)) {
            makepad_platform::a11y::set_string(&mut node.name, title);
            makepad_platform::a11y::set_string(&mut node.description, description);
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _: &mut Scope) -> a11y_tree::ActionResult {
        if request.action == a11y_tree::Actions::PRESS {
            if let Some(action) = self.press(false) {
                cx.action(action);
                return a11y_tree::ActionResult::Handled
            }
        }
        a11y_tree::ActionResult::Unhandled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        if let Some(ViewAction::FingerUp(up)) = actions.find_widget_action(self.view.widget_uid()).map(|a| a.cast()) {
            if !up.cancelled && up.is_over && !up.is_sweep && up.device.is_primary_hit() {
                if let Some(action) = self.press(up.modifiers.shift) {
                    cx.action(action);
                }
            }
        }
        cx.extend_actions(actions);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabJournalPanel {
    #[deref] view: View,
    #[rust] revision: Option<u64>,
    #[rust] navigation: Option<u64>,
}

impl Widget for FabJournalPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !scope.data.get::<AppState>().is_some_and(|s| s.ui.on_screen(super::editors::Editor::Journal)) { return }
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let page = &mut scope.data.get_mut::<AppState>().unwrap().journal;
        if let Some(selected) = self.view.drop_down(cx, ids!(filters.source)).selected(actions) {
            cx.action(if selected == 0 { JournalAction::ThisPlate } else { JournalAction::Sessions });
        }
        if self.view.button(cx, ids!(filters.sessions)).clicked(actions) { cx.action(JournalAction::Sessions); }
        if matches!(page.source, JournalSource::Sessions) {
            if page.empty_confirm.is_some() {
                if self.view.button(cx, ids!(session_bar.empty_confirm.empty_yes)).clicked(actions) {
                    cx.action(JournalAction::EmptyRemoved);
                } else if self.view.button(cx, ids!(session_bar.empty_confirm.keep)).clicked(actions) {
                    page.empty_confirm = None;
                    page.revision += 1;
                    self.view.redraw(cx);
                }
            } else if page.can_empty() && self.view.button(cx, ids!(filters.empty)).clicked(actions) {
                page.empty_confirm = Some(page.removed.clone());
                page.revision += 1;
                self.view.redraw(cx);
            }
        }
        if let JournalSource::Session(session) = &page.source {
            if let Some(token) = session.token {
                if self.view.button(cx, ids!(filters.remove)).clicked(actions) {
                    page.remove_confirm = Some(session.folder.clone());
                    page.revision += 1;
                    self.view.redraw(cx);
                } else if page.remove_confirm.as_ref() == Some(&session.folder) {
                    let yes = self.view.button(cx, ids!(session_bar.confirm.remove_yes)).clicked(actions);
                    if yes || self.view.button(cx, ids!(session_bar.confirm.keep)).clicked(actions) {
                        page.remove_confirm = None;
                        page.revision += 1;
                        self.view.redraw(cx);
                        if yes { cx.action(JournalAction::RemoveSession { folder: session.folder.clone(), token }); }
                    }
                }
            }
        }
        let actor = self.view.drop_down(cx, ids!(filters.actor)).selected(actions);
        let kind = self.view.drop_down(cx, ids!(filters.kind)).selected(actions);
        if actor.is_some() || kind.is_some() {
            cx.action(JournalAction::Filter { actor: actor.unwrap_or(page.actor), kind: kind.unwrap_or(page.kind) });
        }
        if self.view.button(cx, ids!(detail.replay_bar.replay)).clicked(actions) { cx.action(JournalAction::Replay); }
        if self.view.button(cx, ids!(detail.replay_bar.cancel)).clicked(actions) { cx.action(JournalAction::CancelReplay); }
        if self.view.button(cx, ids!(copy_bar.copy)).clicked(actions) { cx.action(JournalAction::Copy); }
        if self.view.button(cx, ids!(pages.older)).clicked(actions) { cx.action(JournalAction::Older); }
        if self.view.button(cx, ids!(pages.newer)).clicked(actions) { cx.action(JournalAction::Newer); }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get_mut::<AppState>() else { return DrawStep::done() };
        state.journal.drawn = true;
        let page = &state.journal;
        let compact = cx.peek_walk_turtle(walk).size.x < 350.0;
        if self.revision != Some(page.revision) {
            self.revision = Some(page.revision);
            let listing = matches!(page.source, JournalSource::Sessions);
            self.view.drop_down(cx, ids!(filters.source)).set_selected_item(cx, usize::from(!matches!(page.source, JournalSource::Plate)));
            self.view.widget(cx, ids!(filters.actor)).set_visible(cx, !listing);
            self.view.widget(cx, ids!(filters.kind)).set_visible(cx, !listing);
            let session = match &page.source { JournalSource::Session(s) => Some(s), _ => None };
            self.view.widget(cx, ids!(filters.sessions)).set_visible(cx, session.is_some());
            let confirming = session.is_some_and(|s| s.token.is_some() && page.remove_confirm.as_ref() == Some(&s.folder));
            self.view.widget(cx, ids!(filters.remove)).set_visible(cx, session.is_some_and(|s| s.token.is_some()));
            let empty_count = page.empty_confirm.as_ref().filter(|_| listing).map(|a| a.len());
            self.view.widget(cx, ids!(filters.empty)).set_visible(cx, page.can_empty() || empty_count.is_some());
            self.view.widget(cx, ids!(session_bar)).set_visible(cx, session.is_some() || page.can_empty() || empty_count.is_some());
            self.view.widget(cx, ids!(session_bar.confirm)).set_visible(cx, confirming);
            self.view.widget(cx, ids!(session_bar.session)).set_visible(cx, session.is_some() && !confirming);
            self.view.widget(cx, ids!(session_bar.empty_confirm)).set_visible(cx, empty_count.is_some());
            if let Some(n) = empty_count {
                // Short enough for a narrow panel; the Empty button above says what they are.
                self.view.label(cx, ids!(session_bar.empty_confirm.question)).set_text(cx, &format!("Delete {n} for good?"));
            }
            if let Some(s) = session { self.view.label(cx, ids!(session_bar.session)).set_text(cx, &s.title); }
            self.view.drop_down(cx, ids!(filters.actor)).set_selected_item(cx, page.actor);
            self.view.drop_down(cx, ids!(filters.kind)).set_selected_item(cx, page.kind);
            for (path, text) in [(ids!(health).as_slice(), page.health.as_str()), (ids!(message).as_slice(), page.message.as_str())] {
                self.view.label(cx, path).set_text(cx, text);
                self.view.widget(cx, path).set_visible(cx, !text.is_empty());
            }
            self.view.widget(cx, ids!(detail)).set_visible(cx, !page.detail.is_empty() || !page.diffs.is_empty() || !page.replay_status.is_empty());
            self.view.widget(cx, ids!(detail.replay_bar.replay)).set_visible(cx, page.count == 1 && !page.replay_waiting);
            self.view.widget(cx, ids!(detail.replay_bar.cancel)).set_visible(cx, page.replay_waiting);
            self.view.label(cx, ids!(detail.replay_status)).set_text(cx, &page.replay_status);
            self.view.widget(cx, ids!(detail.replay_status)).set_visible(cx, !page.replay_status.is_empty());
            self.view.widget(cx, ids!(copy_bar)).set_visible(cx, page.count > 0);
            self.view.label(cx, ids!(copy_bar.count)).set_text(cx, &format!("{} {} selected", page.count, if page.count == 1 { "record" } else { "records" }));
            self.view.label(cx, ids!(detail.text)).set_text(cx, &page.detail);
            self.view.widget(cx, ids!(pages)).set_visible(cx, page.newer || page.older);
            self.view.widget(cx, ids!(pages.newer)).set_visible(cx, page.newer);
            self.view.widget(cx, ids!(pages.older)).set_visible(cx, page.older);
        }
        if self.navigation != Some(page.navigation) {
            self.view.portal_list(cx, ids!(list)).set_first_id_and_scroll(0, 0.0);
            self.navigation = Some(page.navigation);
        }
        let rows = if matches!(page.source, JournalSource::Sessions) { Items::Sessions(page.sessions.clone()) } else { Items::Records(page.rows.clone()) };
        let diffs = page.diffs.clone();
        let anchor = page.selected.clone();
        let through = page.through.clone();

        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = step.borrow_mut::<FlatList>() {
                for diff in diffs.iter() {
                    let Some(item) = list.item(cx, diff.item, live_id!(Document)) else { continue };
                    item.label(cx, ids!(name)).set_text(cx, &diff.label);
                    let editor = item.widget(cx, ids!(editor));
                    editor.set_visible(cx, diff.text.is_some());
                    if let Some(mut editor) = editor.borrow_mut::<JournalDiffEditor>() { editor.bind(cx, &diff.text); }
                    item.draw_all(cx, &mut Scope::empty());
                }
                continue
            }
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, rows.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some((title, context)) = rows.labels(i) else { continue };
                let item = list.item(cx, i, live_id!(Row));
                if let Some(mut widget) = item.borrow_mut::<JournalItem>() {
                    widget.view.layout.flow = if compact { Flow::Down } else { Flow::right() };
                    if !widget.bound.as_ref().is_some_and(|(bound, index)| bound.same(&rows) && *index == i) {
                        widget.view.label(cx, ids!(title)).set_text(cx, title);
                        widget.view.label(cx, ids!(context)).set_text(cx, context);
                        widget.bound = Some((rows.clone(), i));
                    }
                    widget.selected = matches!(&rows, Items::Records(records) if records[i].key.between(anchor.as_ref(), through.as_ref()));
                    let selected = widget.selected as u8 as f32;
                    widget.view.draw_bg.set_dyn_instance(cx, live_id!(selected), &[selected]);
                }
                item.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }
}

#[derive(Script, ScriptHook, Widget)]
struct JournalDiffEditor {
    #[uid] uid: WidgetUid,
    #[walk] walk: Walk,
    #[redraw] #[rust] area: Area,
    #[live] editor: CodeEditor,
    #[rust] session: Option<Rc<RefCell<CodeSession>>>,
}

pub(super) fn bind_diff_editor(cx: &mut Cx, widget: &WidgetRef, session: Rc<RefCell<CodeSession>>, first: usize) {
    if let Some(mut editor) = widget.borrow_mut::<JournalDiffEditor>() {
        editor.bind(cx, &Some((session, first)));
    }
}

impl JournalDiffEditor {
    fn bind(&mut self, cx: &mut Cx, text: &Option<(Rc<RefCell<CodeSession>>, usize)>) {
        if let Some((session, first)) = text {
            if self.session.as_ref().is_none_or(|old| !Rc::ptr_eq(old, session)) {
                self.session = Some(session.clone());
                self.editor.center_line(cx, *first);
            }
        } else { self.session = None; }
    }
}

impl Widget for JournalDiffEditor {
    fn semantics(&self, cx: &Cx, node: &mut a11y_tree::Node) {
        if let Some(session) = &self.session { self.editor.semantics(cx, &session.borrow(), node); }
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Some(session) = &self.session { self.editor.handle_event(cx, event, scope, &mut session.borrow_mut()); }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, _: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(session) = &self.session {
            self.editor.draw_walk_editor(cx, &mut session.borrow_mut(), walk);
            self.area = self.editor.area();
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_text_diff_prepares_edits_line_endings_and_created_documents() {
        let name = "stage.ckt.splash";
        let prepare = |before: &str, after: &str| JournalDiff::text(name, before, after, &|| true).unwrap();
        let (edited, _, _) = prepare("old\nsame\nlast\n", "new\nsame\nend\n").text.expect("an edit has a prepared document");
        for kind in [DiffRowKind::Added, DiffRowKind::Removed] {
            let hunks: Vec<_> = edited.metadata().rows.iter().filter(|r| r.kind == Some(kind)).map(|r| r.hunk).collect();
            assert_eq!(hunks, [Some(1), Some(2)], "{kind:?} rows number the two separated edits from one");
        }

        assert_eq!(prepare("a\r\n", "a\n").label, format!("{name} · Line endings changed"),
            "a change only to the terminator is identified in the label");
        let (created, _, _) = prepare("", "one\ntwo\nthree\n").text.expect("a creation has a prepared document");
        let rows = &created.metadata().rows;
        assert!(rows.len() == 3 && rows.iter().all(|r| r.kind == Some(DiffRowKind::Added)),
            "an empty before text shows all three new lines as added");
        assert!(matches!(JournalDiff::text(name, "old", "new", &|| false), Err(TextDiffError::Cancelled)),
            "a cancelled comparison returns Cancelled");
    }

    #[test]
    fn journal_panel_remove_and_empty_confirmations_keep_the_list_rect() {
        use makepad_widgets_core::makepad_draw::cx_draw::CxDraw;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let panel = cx.with_vm(|vm| {
            makepad_widgets_core::script_mod_with(vm, WindowFamilies::default(), &[makepad_widgets_fab::fab_mod]);
            crate::theme::script_mod(vm);
            makepad_code_editor::script_mod(vm);
            crate::ui::script_mod_kit(vm);
            super::script_mod(vm);
            let value = vm.eval(script! { use mod.widgets.* FabJournalPanel{} });
            WidgetRef::script_from_value(vm, value)
        });
        let mut state = AppState::default();
        let session = JournalSession {
            folder: "working-a-b-c".into(), token: Some(17), title: "Unsaved session".into(),
            description: String::new(), copy_name: String::new(),
        };
        state.journal.source = JournalSource::Session(session.clone());
        let pass = DrawPass::new(&mut cx);
        let mut list = DrawList2d::new(&mut cx);
        let overlay = makepad_widgets_core::makepad_draw::overlay::Overlay { draw_list: DrawList::new(&mut cx) };
        let mut draw = |cx: &mut Cx, state: &mut AppState, width| {
            let size = dvec2(width, 390.);
            pass.set_size(cx, size);
            cx.redraw_all();
            let event = std::mem::take(&mut cx.new_draw_event);
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, Some(1.0));
            list.begin_always(&mut frame);
            overlay.begin(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            panel.draw_all(&mut frame, &mut Scope::with_data(state));
            overlay.end(&mut frame);
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
            panel.widget(&frame, ids!(list)).area().rect(&frame)
        };
        let click = |cx: &mut Cx, state: &mut AppState, path: &[LiveId]| {
            let actions = cx.capture_actions(|cx| {
                cx.widget_action(panel.widget(cx, path).widget_uid(), ButtonAction::Clicked(Default::default()));
            });
            cx.capture_actions(|cx| panel.handle_event(cx, &Event::Actions(actions), &mut Scope::with_data(state)))
        };
        for width in [640., 320.] {
            let before = draw(&mut cx, &mut state, width);
            assert!(before.size.x > 0. && before.size.y > 0., "the list must have been laid out");
            assert!(panel.widget(&cx, ids!(filters.remove)).visible());
            click(&mut cx, &mut state, ids!(filters.remove));
            let confirming = draw(&mut cx, &mut state, width);
            assert!(panel.widget(&cx, ids!(session_bar.confirm)).visible());
            assert!(!panel.widget(&cx, ids!(session_bar.session)).visible());
            assert_eq!(confirming, before, "confirmation moved the list at width {width}");
            let cancelled = click(&mut cx, &mut state, ids!(session_bar.confirm.keep));
            assert!(!cancelled.iter().any(|a| a.downcast_ref::<JournalAction>().is_some()), "Cancel must only close the confirmation");
            assert_eq!(draw(&mut cx, &mut state, width), before);
            assert!(!panel.widget(&cx, ids!(session_bar.confirm)).visible());
            assert!(panel.widget(&cx, ids!(session_bar.session)).visible());
        }
        click(&mut cx, &mut state, ids!(filters.remove));
        draw(&mut cx, &mut state, 320.);
        let confirmed = click(&mut cx, &mut state, ids!(session_bar.confirm.remove_yes));
        assert_eq!(confirmed.iter().filter(|a| matches!(a.downcast_ref::<JournalAction>(),
            Some(JournalAction::RemoveSession { folder, token: 17 }) if folder == &session.folder)).count(), 1);
        state.journal.reset();
        state.journal.source = JournalSource::Session(JournalSession { token: None, ..session });
        draw(&mut cx, &mut state, 320.);
        assert!(!panel.widget(&cx, ids!(filters.remove)).visible(), "a folder without a log cannot be confirmed");

        state.journal.reset();
        state.journal.source = JournalSource::Sessions;
        draw(&mut cx, &mut state, 320.);
        assert!(!panel.widget(&cx, ids!(filters.empty)).visible(), "no archive means no Empty button");
        let first = RemovedSession { name: "first".into(), revision: None };
        let inventory: Arc<[RemovedSession]> = vec![first.clone()].into();
        for width in [640., 250.] {
            state.journal.removed = inventory.clone();
            state.journal.revision += 1;
            let before = draw(&mut cx, &mut state, width);
            assert!(panel.widget(&cx, ids!(filters.empty)).visible());
            click(&mut cx, &mut state, ids!(filters.empty));
            state.journal.removed = vec![first.clone(), RemovedSession { name: "later".into(), revision: None }].into();
            assert_eq!(draw(&mut cx, &mut state, width), before, "Empty confirmation moved the list at width {width}");
            assert!(panel.widget(&cx, ids!(session_bar.empty_confirm)).visible());
            assert_eq!(panel.label(&cx, ids!(session_bar.empty_confirm.question)).text(), "Delete 1 for good?");
            assert!(Arc::ptr_eq(state.journal.empty_confirm.as_ref().unwrap(), &inventory), "a refresh cannot enlarge the confirmation");
            let cancelled = click(&mut cx, &mut state, ids!(session_bar.empty_confirm.keep));
            assert!(!cancelled.iter().any(|a| a.downcast_ref::<JournalAction>().is_some()));
            assert_eq!(draw(&mut cx, &mut state, width), before);
            assert!(!panel.widget(&cx, ids!(session_bar.empty_confirm)).visible());
        }
        click(&mut cx, &mut state, ids!(filters.empty));
        draw(&mut cx, &mut state, 250.);
        assert_eq!(panel.label(&cx, ids!(session_bar.empty_confirm.question)).text(), "Delete 2 for good?");
        let confirmed = click(&mut cx, &mut state, ids!(session_bar.empty_confirm.empty_yes));
        assert_eq!(confirmed.iter().filter(|a| matches!(a.downcast_ref::<JournalAction>(), Some(JournalAction::EmptyRemoved))).count(), 1);
    }
}
