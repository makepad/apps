//! Both file navigation surfaces use the shared Structure widget. Models are
//! rebuilt only on a tree revision, a selection, or a fold change.
use super::*;
use crate::document_tree::Operation;
use makepad_app_shell::tool_host::Change;
use crate::structure::{StructureAction, StructureContextAction, StructureTree, TreeRow};
use makepad_motion::structure::NodeKind;

pub(super) struct FilePrompt {
    path: String,
    delete: bool,
    revision: u64,
}

impl DeckPage {
    pub(crate) fn open_file(&mut self, cx: &mut Cx, ui: &WidgetRef, path: &str) -> Result<(), String> {
        let tree = self.session.as_ref().and_then(|session| session.tree.as_ref()).ok_or("no deck tree open")?;
        if !tree.files.contains_key(path) { return Err(format!("no file `{path}`")); }
        if self.active_file == path { return Ok(()); }
        if let Some(mut pane) = self.source.take() {
            if let Some(text) = pane.draft_text() {
                self.pending.push_back(Operation::Document(Change::Write { path: self.active_file.clone(), text: text.into(), expected: pane.draft_base().map(str::to_string) }));
            }
            pane.suspend();
            if tree.files.contains_key(&self.active_file) || pane.has_draft() { self.file_sources.insert(self.active_file.clone(), pane); }
        }
        self.active_file = path.into();
        self.source = self.file_sources.remove(path);
        if let Some(pane) = &mut self.source { pane.resume(cx, ui); }
        self.source_seen = None;
        self.revealed = None;
        self.files_seen = None;
        Ok(())
    }

    fn reveal_file(&mut self, cx: &mut Cx, ui: &WidgetRef, path: &str) -> Result<(), String> {
        self.open_file(cx, ui, path)?;
        let index = self.session.as_ref().and_then(|session| session.doc.slides.iter().position(|slide| slide.graphic.as_deref() == Some(path)));
        if let Some(index) = index {
            if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() { view.command(cx, Command::Jump(index)); }
            self.file_slide = Some(index);
        }
        let split = ui.splitter(cx, ids!(deck_code_split));
        if split.collapse() == SplitterCollapse::B { split.set_collapse(cx, SplitterCollapse::None); }
        self.panels_seen = None;
        cx.redraw_all();
        Ok(())
    }

    pub(super) fn close_file_menu(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let menu = ui.menu_layer(cx, ids!(menus));
        if menu.open_owner() == Some(live_id!(deck_file_menu)) { menu.close(cx); }
        ui.popover(cx, ids!(deck_file_prompt)).close(cx);
        self.file_menu = None;
        self.file_prompt = None;
    }

    fn file_context_menu(&mut self, cx: &mut Cx, ui: &WidgetRef, path: String, abs: DVec2) {
        let Some(tree) = self.session.as_ref().and_then(|session| session.tree.as_ref()) else { return };
        let file = tree.files.contains_key(&path);
        let rows = vec![
            MenuRow::new(live_id!(deck_new_slide), "New slide"),
            MenuRow::new(live_id!(deck_rename_file), "Rename").enabled(file),
            MenuRow::new(live_id!(deck_delete_file), "Delete").enabled(file && path != tree.root.1).danger(true),
            MenuRow::separator(),
            MenuRow::new(live_id!(deck_reveal_file), "Reveal").enabled(file),
        ];
        self.close_file_menu(cx, ui);
        let anchor = Rect { pos: abs, size: dvec2(0.0, 0.0) };
        self.file_menu = Some((path, anchor));
        ui.menu_layer(cx, ids!(menus)).open(cx, live_id!(deck_file_menu), rows, anchor, MenuPlace::At);
    }

    fn file_change_ready(&self, path: &str, delete: bool) -> Result<(), String> {
        let tree = self.session.as_ref().and_then(|session| session.tree.as_ref()).ok_or("No deck is open")?;
        if !tree.files.contains_key(path) { return Err(format!("No file `{path}`")); }
        if delete && path == tree.root.1 { return Err("The deck's root file cannot be deleted".into()); }
        if self.source.as_ref().is_some_and(SourcePane::has_draft) || self.file_sources.values().any(SourcePane::has_draft) {
            return Err("Apply or revert file drafts before renaming or deleting a file".into());
        }
        if self.editing || !self.pending.is_empty() || self.history_busy() {
            return Err("Wait for the current edit to finish, then try again".into());
        }
        Ok(())
    }

    fn prompt_file_change(&mut self, cx: &mut Cx, ui: &WidgetRef, path: String, anchor: Rect, delete: bool) {
        if let Err(error) = self.file_change_ready(&path, delete) { self.load_error = error; return; }
        let Some(session) = &self.session else { return };
        let prompt = ui.popover(cx, ids!(deck_file_prompt));
        let content = prompt.content();
        content.label(cx, ids!(title)).set_text(cx, &if delete { format!("Delete {path}?") } else { "Rename file".into() });
        content.widget(cx, ids!(name)).set_visible(cx, !delete);
        content.text_input(cx, ids!(name)).set_text(cx, &path);
        content.button(cx, ids!(accept)).set_text(cx, if delete { "Delete" } else { "Rename" });
        let slides = session.doc.slides.iter().filter(|slide| slide.graphic.as_deref() == Some(path.as_str())).count();
        let note = if delete {
            if slides == 0 { "Remove this file from the deck? You can undo this change.".into() }
            else { format!("Remove this file and {slides} {} that use it? You can undo this change.", if slides == 1 { "slide" } else { "slides" }) }
        } else { "References to this file will follow its new name.".into() };
        content.label(cx, ids!(note)).set_text(cx, &note);
        self.file_prompt = Some(FilePrompt { path, delete, revision: session.revision });
        prompt.open_at(cx, anchor);
    }

    fn accept_file_change(&mut self, cx: &mut Cx, ui: &WidgetRef) -> Result<(), String> {
        let Some(prompt) = &self.file_prompt else { return Ok(()) };
        self.file_change_ready(&prompt.path, prompt.delete)?;
        if self.session.as_ref().is_none_or(|session| session.revision != prompt.revision) {
            return Err("The deck changed while this prompt was open. Cancel and choose the file again.".into());
        }
        let path = prompt.path.clone();
        if prompt.delete {
            self.pending.push_back(Operation::DeleteFile { path });
            cx.redraw_all();
        } else {
            let to = ui.popover(cx, ids!(deck_file_prompt)).content().text_input(cx, ids!(name)).text().trim().to_string();
            makepad_asset_data::tree::validate_path(&to)?;
            if path != to {
                if self.session.as_ref().and_then(|session| session.tree.as_ref()).is_some_and(|tree| tree.files.contains_key(&to)) {
                    return Err(format!("A file named `{to}` already exists"));
                }
                self.queue_file(cx, Change::Rename { from: path, to });
            }
        }
        self.close_file_menu(cx, ui);
        Ok(())
    }

    fn queue_file(&mut self, cx: &mut Cx, change: Change) {
        if let Change::Rename { to, .. } = &change { self.wanted_file = Some(to.clone()); }
        self.pending.push_back(Operation::Document(change));
        cx.redraw_all();
    }

    pub(super) fn file_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        for action in actions.filter_widget_actions_cast::<SegmentAction>(ui.widget(cx, ids!(deck_side_tabs)).widget_uid()) {
            if let SegmentAction::Selected(index) = action {
                self.side_structure = index == 1;
                self.panels_seen = None;
                ui.widget(cx, ids!(deck_structure)).set_visible(cx, self.side_structure);
                ui.widget(cx, ids!(deck_presenter)).set_visible(cx, !self.side_structure);
            }
        }
        for action in actions.filter_widget_actions_cast::<StructureAction>(ui.widget(cx, ids!(deck_structure_tree)).widget_uid()) {
            if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
                match action {
                    StructureAction::Select(key) => view.select_element(cx, Some(key)),
                    StructureAction::Fold(key) => view.fold_element(cx, key),
                    _ => {}
                }
            }
        }
        let files = ui.widget(cx, ids!(deck_files)).widget_uid();
        for action in actions.filter_widget_actions_cast::<StructureAction>(files) {
            match action {
                StructureAction::Select(path) => {
                    let path = path.as_str();
                    if path.ends_with('/') {
                        if !self.folded.remove(path) { self.folded.insert(path.into()); }
                        self.files_seen = None;
                    } else {
                        if let Err(error) = self.reveal_file(cx, ui, path) { self.load_error = error; }
                    }
                }
                StructureAction::Fold(path) => {
                    if !self.folded.remove(&path) { self.folded.insert(path); }
                    self.files_seen = None;
                }
                _ => {}
            }
        }
        for action in actions.filter_widget_actions(files).filter_map(|action| action.action.downcast_ref::<StructureContextAction>()) {
            self.file_context_menu(cx, ui, action.path.clone(), action.abs);
        }
        if let Some(id) = makepad_widgets::menu::menu_picked(actions, live_id!(deck_file_menu)) {
            if let Some((path, anchor)) = self.file_menu.take() {
                match id {
                    id if id == live_id!(deck_new_slide) => {
                        let after = self.session.as_ref().and_then(|session| session.doc.slides.iter().position(|slide| slide.graphic.as_deref() == Some(path.as_str())))
                            .or_else(|| ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current()));
                        let index = after.map_or_else(|| self.doc.as_ref().map_or(0, |doc| doc.slides.len()), |index| index + 1);
                        match self.submit_op(cx, DeckOp::AddSlide { after, name: self.fresh_slide_name(), cut: NewCut::Blank }) {
                            Ok(()) => self.select = Some(index),
                            Err(error) => self.load_error = error,
                        }
                    }
                    id if id == live_id!(deck_rename_file) => self.prompt_file_change(cx, ui, path, anchor, false),
                    id if id == live_id!(deck_delete_file) => self.prompt_file_change(cx, ui, path, anchor, true),
                    id if id == live_id!(deck_reveal_file) => {
                        if let Err(error) = self.reveal_file(cx, ui, &path) { self.load_error = error; }
                    }
                    _ => {}
                }
            }
        }
        let prompt = ui.popover(cx, ids!(deck_file_prompt));
        let content = prompt.content();
        if prompt.dismissed(actions) || content.button(cx, ids!(cancel)).clicked(actions) {
            self.close_file_menu(cx, ui);
        } else if prompt.is_open() && (content.button(cx, ids!(accept)).clicked(actions)
            || content.text_input(cx, ids!(name)).returned(actions).is_some()) {
            if let Err(error) = self.accept_file_change(cx, ui) { content.label(cx, ids!(note)).set_text(cx, &error); }
        }
        if ui.menu_layer(cx, ids!(menus)).open_owner() != Some(live_id!(deck_file_menu)) {
            self.file_menu = None;
        }
    }

    pub(super) fn pump_source(&mut self, cx: &mut Cx, ui: &WidgetRef, current: Option<usize>) {
        let Some(session) = &self.session else { return };
        let Some(tree) = session.tree.clone() else { return };
        // The deck shown (a tree may hold several): its file is the deck's source.
        let deck = session.path.clone();
        let revision = session.revision;
        if self.source_seen != Some(revision) {
            for (path, pane) in &mut self.file_sources {
                if let Ok(text) = tree.text(path) { pane.sync_detached(text, "Deck"); }
            }
            self.file_sources.retain(|path, pane| tree.files.contains_key(path) || pane.has_draft());
        }
        if self.wanted_file.as_ref().is_some_and(|path| tree.files.contains_key(path)) {
            let path = self.wanted_file.take().unwrap();
            let _ = self.open_file(cx, ui, &path);
        }
        if !tree.files.contains_key(&self.active_file) {
            let _ = self.open_file(cx, ui, &deck);
        }
        self.paint_files(cx, ui);
        let Some(file) = tree.files.get(&self.active_file) else { return };
        let text_file = if self.source_seen == Some(revision) { self.source.is_some() } else { !matches!(file.kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio) && file.text().is_ok() };
        if !text_file {
            ui.widget(cx, ids!(deck_source)).set_visible(cx, false);
            ui.widget(cx, ids!(deck_binary)).set_visible(cx, true);
            if self.source_seen != Some(revision) {
                self.source_seen = Some(revision);
                ui.label(cx, ids!(deck_binary)).set_text(cx, &format!("{}\n{} bytes · imported media", file.path, file.bytes.len()));
            }
            return;
        }
        ui.widget(cx, ids!(deck_source)).set_visible(cx, true);
        ui.widget(cx, ids!(deck_binary)).set_visible(cx, false);
        if self.source.is_none() {
            let kind = if self.active_file == deck { DocKind::Deck } else if file.kind == AssetKind::Cut { DocKind::Cut } else { DocKind::Motion };
            // The deck's code view has no change-history bar: a change the AI
            // or a tool wrote shows as one chip.
            self.source = Some(SourcePane::new(kind, live_id!(deck_source), &self.active_file).compact().live());
        }
        self.pump_live_write(cx, ui, &tree);
        let Some(pane) = self.source.as_mut() else { return };
        pane.tree = Some(tree.clone());
        if self.source_seen != Some(revision) {
            self.source_seen = Some(revision);
            let Ok(text) = file.text() else { self.load_error = format!("{}: not UTF-8 text", file.path); return; };
            pane.sync(cx, ui, text, &self.author.take().unwrap_or_else(|| "Deck".into()));
            self.revealed = None;
        }
        // The edit typed now goes at once (the one before has landed or
        // failed): no pause, no check before it.
        if self.live_write.is_none() {
            if let Some((text, expected)) = self.source.as_mut().and_then(SourcePane::take_edit) {
                self.live_write = Some(LiveWrite { path: self.active_file.clone(), text: text.clone(), running: false, result: None });
                self.queue_file(cx, Change::Write { path: self.active_file.clone(), text, expected });
                makepad_widgets::makepad_platform::thread::SignalToUI::set_ui_signal();
            }
        }
        let Some(pane) = self.source.as_mut() else { return };
        if let Some(index) = current {
            if self.active_file == deck && self.revealed != Some((index, revision)) {
                if let Some(slide) = self.session.as_ref().and_then(|session| session.doc.slides.get(index)) {
                    if slide.graphic.is_none() {
                        let Ok(text) = file.text() else { return };
                        let line = |at: usize| text[..at.min(text.len())].bytes().filter(|b| *b == b'\n').count();
                        pane.reveal(cx, ui, line(slide.span.0), line(slide.span.1));
                    }
                    self.revealed = Some((index, revision));
                }
            }
        }
        let palette = makepad_look::palette::get(cx);
        pane.paint(cx, ui, &palette);
    }

    /// The code view's edit that finished: landed, it is the document (the
    /// pane takes it on the revision); refused, its problems go on its lines
    /// (at once on the first line, then where the checker finds them) and
    /// the document, and the picture, stay the last that loaded.
    fn pump_live_write(&mut self, cx: &mut Cx, ui: &WidgetRef, tree: &Arc<TreeDraft>) {
        if let Some(checker) = &self.checker {
            while let Some(checked) = checker.try_recv() {
                let Some((serial, path)) = self.diagnosing.take() else { continue };
                if serial != checked.serial { self.diagnosing = Some((serial, path)); continue; }
                if path == self.active_file {
                    if let Some(pane) = self.source.as_mut() { pane.diagnosed(cx, ui, &checked.source, checked.problems); }
                }
            }
        }
        let Some(live) = self.live_write.take_if(|live| live.result.is_some()) else { return };
        let active = live.path == self.active_file;
        match live.result {
            Some(Ok(())) => {
                // A write that changed nothing brings no new revision: the pane hears it here.
                if let (true, Some(pane), Ok(text)) = (active, self.source.as_mut(), tree.text(&live.path)) { pane.sync(cx, ui, text, "You · source"); }
                else if let (Some(pane), Ok(text)) = (self.file_sources.get_mut(&live.path), tree.text(&live.path)) { pane.sync_detached(text, "You · source"); }
            }
            Some(Err(message)) => {
                let first = vec![Problem { line: 1, col: 1, message }];
                if active {
                    if let Some(pane) = self.source.as_mut() { pane.edit_failed(cx, ui, &live.text, first); }
                    if self.checker.is_none() { self.checker = Some(Checker::new(cx)); }
                    if let Some(checker) = &self.checker {
                        let check = Check { serial: self.serial + 1, kind: DocKind::Motion, source: live.text, modules: Vec::new(), activity: false, film: None, tree: Some((tree.clone(), live.path.clone())), diagnose: true };
                        if checker.try_send(check).is_ok() {
                            self.serial += 1;
                            self.diagnosing = Some((self.serial, live.path));
                        }
                    }
                } else if let Some(pane) = self.file_sources.get_mut(&live.path) {
                    pane.edit_failed_detached(&live.text);
                }
            }
            None => {}
        }
    }

    fn paint_files(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(session) = &self.session else { return };
        let Some(tree) = &session.tree else { return };
        let key = (session.revision, self.active_file.clone());
        if self.files_seen.as_ref() == Some(&key) { return; }
        self.files_seen = Some(key);
        let deck = &session.path;
        let mut rows = vec![TreeRow { path: deck.clone(), depth: 0, label: session.doc.title.clone(), detail: deck.clone(), kind: NodeKind::Scene, on: true, open: None }];
        let mut slide_files = std::collections::HashSet::new();
        for (index, slide) in session.doc.slides.iter().enumerate() {
            let Some(path) = &slide.graphic else { continue };
            slide_files.insert(path.clone());
            let name = path.rsplit('/').next().unwrap_or(path);
            rows.push(TreeRow { path: path.clone(), depth: 1, label: format!("{:02} {name}", index + 1), detail: slide.name.clone(), kind: NodeKind::Layer, on: true, open: None });
        }
        let mut rest = crate::document_tree::file_rows(tree, session.saved_tree.as_deref(), &self.folded);
        rest.retain(|row| row.path != *deck && !slide_files.contains(&row.path) && row.path != "slides/");
        rest.sort_by_key(|row| (!row.path.starts_with("shared/"), row.path.clone()));
        rows.extend(rest);
        if let Some(mut widget) = ui.widget(cx, ids!(deck_files)).borrow_mut::<StructureTree>() { widget.set_model(cx, rows, Vec::new(), Some(self.active_file.clone())); }
    }
}
