use super::*;
use makepad_create::docs::OpenDocument;
use makepad_town_core::event::Id;
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
                if wrote && model.state.docs[&open.id].has_linked_tasks() { self.read_workspace(); }
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
            if self.document.is_some() {
                if let Err(error) = self.switch_doc(cx, None) {
                    self.view.page = Page::Docs;
                    self.doc_problem(cx, &error);
                }
            }
            if self.view.page != Page::Docs { return }
        }
        let id = self.view.open_doc.filter(|id| self.view.workspace.selected().is_some_and(|p| p.docs.iter().any(|d| d.id == *id)));
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
        use makepad_town_core::{doc::BreakRef, entity::field};
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
