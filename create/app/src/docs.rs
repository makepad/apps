//! The open Markdown buffer and the history of the person's signed text edits.
use crate::model::{Action, Model, Origin};
use makepad_code_editor::{text::Text, CodeDocument};
use makepad_work_core::doc;
use makepad_work_core::event::{kind, Actor, Content, Id};
use std::collections::HashSet;

#[derive(Default)]
struct History {
    undo: Vec<Id>,
    redo: Vec<Id>,
    event_count: usize,
}

impl History {
    fn sync(&mut self, model: &Model, doc: Id) {
        let events = model.doc_events(doc);
        // Events are immutable. A late insertion grows the timeline even when
        // its last event stays the same.
        if self.event_count == events.len() { return }
        self.event_count = events.len();
        self.undo.clear();
        self.redo.clear();
        let me = model.me();
        let mut retired = HashSet::new();
        for event in events {
            let header = &event.header;
            let mine = header.author == me && header.actor == Actor::Human;
            match header.kind {
                kind::TEXT if mine => {
                    self.undo.push(event.id);
                    self.redo.clear();
                }
                kind::UNDO => {
                    let Some(Content::Undo { of }) = event.content().and_then(|bytes|
                        Content::decode(kind::UNDO, header.kind_version, bytes)) else { continue };
                    if self.undo.last() == Some(&of) {
                        self.undo.pop();
                        if mine { self.redo.push(event.id); }
                    } else if self.redo.last() == Some(&of) {
                        self.redo.pop();
                        if mine { self.undo.push(event.id); }
                    } else {
                        // An undo can retire an older entry. Skip it when it
                        // reaches the top so replay stays linear in this timeline.
                        retired.insert(of);
                    }
                    for stack in [&mut self.undo, &mut self.redo] {
                        while stack.last().is_some_and(|id| retired.remove(id)) { stack.pop(); }
                    }
                }
                _ => {}
            }
        }
    }

    fn target(&self, redo: bool) -> Option<Id> {
        if redo { self.redo.last() } else { self.undo.last() }.copied()
    }
}

pub struct OpenDocument {
    pub id: Id,
    pub buffer: CodeDocument,
    saved_version: u64,
    history: History,
}

impl OpenDocument {
    pub fn open(model: &Model, id: Id) -> Self {
        let markdown = doc::editor_markdown(&model.state.docs[&id]);
        let prepared = CodeDocument::prepare_for_path("document.md", Text::from(markdown.as_str()));
        let buffer = CodeDocument::from_prepared(prepared);
        let mut history = History::default();
        history.sync(model, id);
        Self { id, saved_version: buffer.version(), buffer, history }
    }

    pub fn dirty(&self) -> bool { self.buffer.version() != self.saved_version }

    fn refresh(&mut self, model: &Model) {
        self.history.sync(model, self.id);
        self.buffer.replace_changed(&doc::editor_markdown(&model.state.docs[&self.id]));
        self.saved_version = self.buffer.version();
    }

    /// Return true if a new event was signed. A failed write keeps the buffer dirty.
    pub fn flush(&mut self, model: &mut Model) -> Result<bool, String> {
        if !self.dirty() { return Ok(false) }
        let markdown = self.buffer.as_text().to_string();
        let event = model.act(Action::EditDoc { doc: self.id, markdown }, Origin::Ui)?;
        self.refresh(model);
        Ok(event != self.id)
    }

    pub fn step(&mut self, model: &mut Model, redo: bool) -> Result<(), String> {
        self.flush(model)?;
        self.history.sync(model, self.id);
        if let Some(of) = self.history.target(redo) {
            model.act(Action::Undo { of }, Origin::Ui)?;
            self.refresh(model);
        }
        Ok(())
    }

    /// A write, a field change or an undo can affect this document indirectly
    /// through a linked task. Sign any typing before those events reach the fold.
    pub fn act(&mut self, model: &mut Model, action: Action, origin: Origin) -> Result<Id, String> {
        let affects_doc = match &action {
            Action::WriteDoc { doc, .. } | Action::EditDoc { doc, .. } | Action::LineToTask { doc, .. } =>
                *doc == self.id || model.state.docs[&self.id].has_linked_tasks(),
            Action::SetFields { changes, .. } => changes.iter().any(|change| change.entity == self.id
                || model.state.docs[&self.id].links_task(&change.entity)),
            Action::Undo { .. } => true,
            _ => false,
        };
        if !affects_doc { return model.act(action, origin) }
        self.flush(model)?;
        let result = model.act(action, origin)?;
        self.refresh(model);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_in_a_long_document_round_trips() {
        use std::time::Instant;
        let dir = std::env::temp_dir().join(format!("doc-editor-timing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (mut model, _) = Model::open(&dir).unwrap();
        let id = model.act(Action::AddDoc { project: model.project().id, title: "Notes".into() }, Origin::Ui).unwrap();
        let initial = (0..2000).map(|i| format!("Draft paragraph {i}")).collect::<Vec<_>>().join("\n");
        model.act(Action::EditDoc { doc: id, markdown: initial.clone() }, Origin::Ui).unwrap();
        let mut editor = OpenDocument::open(&model, id);
        let edited = initial.replace("paragraph 1000", "section 1000");
        editor.buffer.replace_changed(&edited);
        let start = Instant::now();
        assert!(editor.flush(&mut model).unwrap());
        let flush = start.elapsed();

        let of = editor.history.target(false).unwrap();
        model.act(Action::Undo { of }, Origin::Ui).unwrap();
        let start = Instant::now();
        editor.refresh(&model);
        let replace = start.elapsed();
        eprintln!("2,000-line document, one word: flush {flush:?}, regenerate and replace {replace:?}");
        assert_eq!(editor.buffer.as_text().to_string(), initial);
        assert!(!editor.dirty(), "putting the fold back in the editor does not start another burst");
        drop(model);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
