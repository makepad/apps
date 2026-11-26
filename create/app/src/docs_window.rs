use super::*;
use makepad_create::docs::OpenDocument;
use makepad_work_core::event::Id;
use makepad_markdown_editor::{MarkdownEditor, MarkdownEditorAction};

impl App {
    fn doc_problem(&self, cx: &mut Cx, problem: &str) {
        let label = self.ui.label(cx, ids!(doc_error));
        label.set_text(cx, problem);
        label.set_visible(cx, !problem.is_empty());
        if !problem.is_empty() { log!("document: {problem}"); }
    }

    fn arm_doc_timer(&mut self, cx: &mut Cx) {
        cx.stop_timer(self.doc_timer);
        self.doc_timer = cx.start_timeout(0.6);
    }

    /// A failed flush keeps the document attached, including on a switch or a polite quit.
    pub(super) fn flush_doc(&mut self, cx: &mut Cx) -> Result<bool, String> {
        cx.stop_timer(std::mem::take(&mut self.doc_timer));
        let (Some(open), Some(model)) = (&mut self.document, &mut self.model) else { return Ok(false) };
        if !open.dirty() { return Ok(false) }
        match open.flush(model) {
            Ok(wrote) => {
                if wrote {
                    if model.state.docs[&open.id].has_linked_tasks() {
                        self.read_workspace();
                    } else {
                        self.view.workspace.update_document_author(model, open.id);
                    }
                }
                self.doc_problem(cx, "");
                Ok(wrote)
            }
            Err(error) => {
                self.doc_problem(cx, &error);
                self.arm_doc_timer(cx);
                Err(error)
            }
        }
    }

    pub(super) fn doc_event(&mut self, cx: &mut Cx, event: &Event) {
        if self.document.is_none() { return }
        let leaving = match event {
            Event::KeyFocus(focus) => focus.prev != focus.focus && focus.prev == self.ui.widget(cx, ids!(markdown)).area(),
            Event::Pause | Event::Background | Event::QuitRequested(_) | Event::WindowCloseRequested(_) | Event::Shutdown => true,
            _ => false,
        };
        if leaving || self.doc_timer.is_event(event).is_some() {
            match self.flush_doc(cx) {
                Ok(true) => self.update_page(cx),
                Err(_) => match event {
                    Event::QuitRequested(q) => q.handled.set(true),
                    Event::WindowCloseRequested(q) => q.accept_close.set(false),
                    _ => {}
                },
                Ok(false) => {}
            }
        }
    }

    fn switch_doc(&mut self, cx: &mut Cx, target: Option<Id>) -> Result<(), String> {
        if self.document.as_ref().map(|d| d.id) == target { return Ok(()) }
        self.flush_doc(cx)?;
        let widget = self.ui.widget(cx, ids!(markdown));
        let Some(mut editor) = widget.borrow_mut::<MarkdownEditor>() else { return Err("document editor unavailable".into()) };
        self.document = target.map(|id| OpenDocument::open(self.model.as_ref().unwrap(), id));
        if let Some(open) = &self.document {
            editor.attach(cx, open.buffer.clone());
        } else {
            editor.detach(cx);
        }
        self.doc_problem(cx, "");
        Ok(())
    }

    pub(super) fn show_docs(&mut self, cx: &mut Cx) {
        if self.view.page != Page::Docs {
            self.ui.widget(cx, ids!(title_prompt)).set_visible(cx, false);
            if self.document.is_some() {
                if let Err(error) = self.switch_doc(cx, None) {
                    self.view.page = Page::Docs;
                    self.doc_problem(cx, &error);
                }
            }
            if self.view.page != Page::Docs { return }
        }
        let rows = self.view.workspace.selected().map_or(&[][..], |p| p.docs.as_slice());
        let selected = rows.iter().find(|r| Some(r.id) == self.view.open_doc).or_else(|| rows.first());
        let id = selected.map(|r| r.id);
        self.ui.label(cx, ids!(doc_heading)).set_text(cx, selected.map_or("", |r| r.title.as_str()));
        self.ui.widget(cx, ids!(docs_empty)).set_visible(cx, id.is_none());
        self.ui.widget(cx, ids!(doc_sheet)).set_visible(cx, id.is_some());
        if let Err(error) = self.switch_doc(cx, id) { self.doc_problem(cx, &error); }
        self.view.open_doc = self.document.as_ref().map(|d| d.id);
    }

    pub(super) fn doc_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.view.page != Page::Docs { return }
        let widget = self.ui.widget(cx, ids!(markdown));
        let mut changed = false;
        for action in actions.filter_widget_actions_cast::<MarkdownEditorAction>(widget.widget_uid()) {
            match action {
                MarkdownEditorAction::Changed => changed = true,
                MarkdownEditorAction::TaskToggled => {
                    changed = false;
                    if let Ok(true) = self.flush_doc(cx) { self.update_page(cx); }
                }
                MarkdownEditorAction::Undo | MarkdownEditorAction::Redo => {
                    if let (Some(open), Some(model)) = (&mut self.document, &mut self.model) {
                        match open.step(model, action == MarkdownEditorAction::Redo) {
                            Ok(()) => { self.doc_problem(cx, ""); self.refresh(cx); }
                            Err(error) => self.doc_problem(cx, &error),
                        }
                    }
                }
                MarkdownEditorAction::Problem(message) => self.doc_problem(cx, &message),
                _ => {}
            }
        }
        if changed && self.document.as_ref().is_some_and(OpenDocument::dirty) { self.arm_doc_timer(cx); }
        let list = self.ui.portal_list(cx, ids!(document_list.list));
        for (i, row) in list.items_with_actions(actions) {
            if row.button(cx, ids!(open_doc)).clicked(actions) {
                let target = self.view.workspace.selected().and_then(|p| p.docs.get(i)).map(|r| r.id);
                if target.is_some() && self.switch_doc(cx, target).is_ok() {
                    self.view.open_doc = target;
                    self.update_page(cx);
                }
            }
        }
        let prompt = self.ui.widget(cx, ids!(title_prompt));
        let input = self.ui.text_input(cx, ids!(doc_title_input));
        if self.ui.button(cx, ids!(new_doc)).clicked(actions) {
            prompt.set_visible(cx, true);
            input.set_key_focus(cx);
        }
        if self.ui.button(cx, ids!(cancel_doc)).clicked(actions) || input.escaped(actions) {
            prompt.set_visible(cx, false);
            input.set_text(cx, "");
        }
        if prompt.visible() && (input.returned(actions).is_some() || self.ui.button(cx, ids!(add_doc)).clicked(actions)) {
            let Some(project) = self.view.workspace.current else { return };
            if self.flush_doc(cx).is_err() { return }
            match self.dispatch(Action::AddDoc { project, title: input.text() }, Origin::Ui) {
                Ok(id) => {
                    self.view.open_doc = Some(id);
                    input.set_text(cx, "");
                    prompt.set_visible(cx, false);
                    self.refresh(cx);
                    if let Some(editor) = widget.borrow::<MarkdownEditor>() { editor.set_key_focus(cx); }
                }
                Err(error) => self.doc_problem(cx, &error),
            }
        }
    }

    pub(super) fn dispatch(&mut self, action: Action, origin: Origin) -> Result<Id, String> {
        let model = self.model.as_mut().ok_or("workspace unavailable")?;
        match self.document.as_mut() {
            Some(open) => open.act(model, action, origin),
            None => model.act(action, origin),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_last_typing_timeout_signs_the_burst() {
        use makepad_work_core::{doc::BreakRef, entity::field};
        use MarkdownEditorAction::{Changed, TaskToggled};

        let dir = std::env::temp_dir().join(format!("doc-editor-pause-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (mut model, _) = Model::open(&dir).unwrap();
        let id = model.act(Action::AddDoc { project: model.project().id, title: "Note".into() }, Origin::Ui).unwrap();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut app = cx.with_vm(App::script_new);
        let open = OpenDocument::open(&model, id);
        let buffer = open.buffer.clone();
        app.document = Some(open);
        app.model = Some(model);
        app.read_workspace();
        app.view.page = Page::Docs;
        app.view.open_doc = Some(id);
        let edit = |app: &mut App, cx: &mut Cx, notifications: &[MarkdownEditorAction]| {
            let uid = app.ui.widget(cx, ids!(markdown)).widget_uid();
            let actions = cx.capture_actions(|cx| {
                for action in notifications { cx.widget_action(uid, action.clone()); }
            });
            app.doc_actions(cx, &actions);
        };
        buffer.replace_changed("a");
        edit(&mut app, &mut cx, &[Changed]);
        let timeout = |timer: Timer| Event::Timer(TimerEvent { timer_id: timer.0, time: None });
        let early = timeout(app.doc_timer);
        buffer.replace_changed("abc");
        edit(&mut app, &mut cx, &[Changed]);
        let pause = timeout(app.doc_timer);
        app.doc_event(&mut cx, &early);
        assert_eq!(app.model.as_ref().unwrap().state.docs[&id].text(), "", "the superseded timer cannot sign a burst");
        app.doc_event(&mut cx, &pause);
        let model = app.model.as_ref().unwrap();
        assert_eq!(model.state.docs[&id].text(), "abc", "the last timeout signs all the typing");
        let revision = model.state.doc_revision(id);
        app.doc_event(&mut cx, &pause);
        assert_eq!(app.model.as_ref().unwrap().state.doc_revision(id), revision, "a repeated timeout signs nothing");

        app.dispatch(Action::LineToTask { doc: id, at: BreakRef::Root }, Origin::Ui).unwrap();
        let model = app.model.as_ref().unwrap();
        let task = *model.state.task_numbers.keys().next().unwrap();
        let before = model.state.doc_event_ids(id).len();
        buffer.replace_changed("- [ ] abc\npending");
        edit(&mut app, &mut cx, &[Changed]);
        let pending = timeout(app.doc_timer);
        buffer.replace_changed("- [x] abc\npending");
        edit(&mut app, &mut cx, &[Changed, TaskToggled]);
        let state = &app.model.as_ref().unwrap().state;
        assert_eq!(state.entities[&task].text(field::STATUS), "done", "the linked task updates in the toggle handler");
        assert_eq!(state.docs[&id].text(), "abc\npending");
        assert_eq!(state.doc_event_ids(id).len(), before + 1, "the tick and pending typing sign together");

        buffer.replace_changed("- [ ] abc\npending");
        edit(&mut app, &mut cx, &[Changed]);
        app.doc_event(&mut cx, &pending);
        assert_eq!(app.model.as_ref().unwrap().state.entities[&task].text(field::STATUS), "done",
            "a typed marker waits for its own pause");
        let pause = timeout(app.doc_timer);
        app.doc_event(&mut cx, &pause);
        assert_eq!(app.model.as_ref().unwrap().state.entities[&task].text(field::STATUS), "open");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
