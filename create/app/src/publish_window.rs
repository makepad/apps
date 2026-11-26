use super::*;
use makepad_create::publish::session::{LocalPublish, Selection};

impl App {
    pub(super) fn open_publish(&mut self, cx: &mut Cx) {
        let Some(model) = &self.model else { return };
        let mut state = LocalPublish::open(&model.home.dir);
        state.load_entries(&cx.task_pool());
        self.ui.drop_down(cx, ids!(publish_target)).set_labels(cx, state.targets());
        self.view.publish = Some(state);
        self.publish_clock(cx);
    }

    fn publish_clock(&mut self, cx: &mut Cx) {
        let pending = self.view.publish.as_ref().is_some_and(LocalPublish::pending);
        if pending && self.publish_timer.is_empty() {
            self.publish_timer = cx.start_interval(0.12);
        } else if !pending && !self.publish_timer.is_empty() {
            cx.stop_timer(std::mem::take(&mut self.publish_timer));
        }
    }

    pub(super) fn show_publish(&self, cx: &mut Cx) {
        let Some(state) = &self.view.publish else { return };
        self.ui.button(cx, ids!(open_publish)).set_visible(cx, state.enabled());
        let error = self.ui.label(cx, ids!(publish_config_error));
        error.set_visible(cx, state.config_problem.is_some());
        if let Some(problem) = &state.config_problem { error.set_text(cx, problem); }
        if self.view.page != Page::Publish { return }
        let busy = state.sending();
        let target = self.ui.drop_down(cx, ids!(publish_target));
        target.set_selected_item(cx, state.draft.target);
        self.ui.widget(cx, ids!(publish_target)).set_disabled(cx, busy);
        for (path, text) in [(ids!(publish_title), &state.draft.title), (ids!(publish_text), &state.draft.text)] {
            let input = self.ui.text_input(cx, path);
            if input.text() != *text { input.set_text(cx, text); }
            input.set_is_read_only(cx, busy);
        }
        self.ui.button(cx, ids!(publish_confirm)).set_enabled(cx, state.can_confirm());
        self.ui.button(cx, ids!(publish_clear)).set_enabled(cx, !state.pending());
        self.ui.button(cx, ids!(publish_add)).set_enabled(cx, !busy);
        self.ui.label(cx, ids!(publish_count)).set_text(cx, &format!("MEDIA  ·  {} selected", state.draft.ids.len()));
        self.ui.widget(cx, ids!(publish_empty)).set_visible(cx, state.entries.is_empty());
        let status = self.ui.label(cx, ids!(publish_status));
        status.set_text(cx, &state.status);
        status.set_visible(cx, !state.status.is_empty());
        let url = self.ui.text_input(cx, ids!(publish_url));
        url.set_visible(cx, state.url.is_some());
        if let Some(link) = &state.url { url.set_text(cx, link); }
        self.ui.widget(cx, ids!(publish_page)).redraw(cx);
    }

    pub(super) fn confirm_publish(&mut self, cx: &mut Cx) {
        let Some(state) = &mut self.view.publish else { return };
        if let Err(problem) = state.confirm(&cx.task_pool()) { state.status = problem; }
        self.publish_clock(cx);
        self.show_publish(cx);
    }

    fn import_media(&mut self, cx: &mut Cx, files: Vec<Selection>) {
        let Some(state) = &mut self.view.publish else { return };
        if !state.enabled() || state.sending() || files.is_empty() { return }
        state.import(&cx.task_pool(), files);
        self.view.page = Page::Publish;
        self.publish_clock(cx);
        self.update_page(cx);
    }

    pub(super) fn publish_event(&mut self, cx: &mut Cx, event: &Event) {
        if self.publish_timer.is_event(event).is_some() {
            if self.view.publish.as_mut().is_some_and(LocalPublish::poll) { self.show_publish(cx); }
            self.publish_clock(cx);
        }
        if !self.view.publish.as_ref().is_some_and(|p| p.enabled() && !p.sending()) || cx.injected_by().is_some() { return }
        let items = match event {
            Event::Drag(e) => &e.items,
            Event::Drop(e) => &e.items,
            _ => return,
        };
        if !items.iter().any(|v| matches!(v, DragItem::FilePath { internal_id: None, .. } | DragItem::VirtualFile(_))) { return }
        let area = self.ui.widget(cx, ids!(main_window.body)).area();
        match event.drag_hits(cx, area) {
            DragHit::Drag(drag) => {
                if let Ok(mut response) = drag.response.try_lock() { *response = DragResponse::Copy; }
            }
            DragHit::Drop(drop) => self.import_media(cx, drop.items.iter().filter_map(|v| match v {
                DragItem::FilePath { path, internal_id: None } => Some(Selection::Path(path.into())),
                DragItem::VirtualFile(file) => Some(Selection::Bytes(file.name.clone(), file.bytes.clone())),
                _ => None,
            }).collect()),
            _ => {}
        }
    }

    pub(super) fn publish_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            match action.downcast_ref::<FileDialogAction>() {
                Some(FileDialogAction::FileSelected { id, paths }) if *id == live_id!(publish_files) =>
                    self.import_media(cx, paths.iter().cloned().map(Selection::Path).collect()),
                Some(FileDialogAction::FileLoaded { id, files }) if *id == live_id!(publish_files) =>
                    self.import_media(cx, files.iter().map(|f| Selection::Bytes(f.name.clone(), f.bytes.clone())).collect()),
                _ => {}
            }
        }
        let Some(state) = &mut self.view.publish else { return };
        if !state.enabled() { return }
        if self.view.page == Page::Share && self.ui.button(cx, ids!(open_publish)).clicked(actions) {
            self.view.page = Page::Publish;
            self.update_page(cx);
            return
        }
        if self.view.page != Page::Publish { return }
        if self.ui.button(cx, ids!(publish_back)).clicked(actions) {
            self.view.page = Page::Share;
            self.update_page(cx);
            return
        }
        if state.sending() { return }
        if self.ui.button(cx, ids!(publish_add)).clicked(actions) {
            cx.open_select_file_dialog(FileDialog::new().set_id(live_id!(publish_files)).set_title("Add media".into()).set_multiple(true));
        }
        let mut changed = false;
        if let Some(title) = self.ui.text_input(cx, ids!(publish_title)).changed(actions) { state.draft.title = title; changed = true; }
        if let Some(text) = self.ui.text_input(cx, ids!(publish_text)).changed(actions) { state.draft.text = text; changed = true; }
        if let Some(i) = self.ui.drop_down(cx, ids!(publish_target)).selected(actions) { state.draft.target = i; changed = true; }
        let list = self.ui.portal_list(cx, ids!(publish_media.list));
        for (i, row) in list.items_with_actions(actions) {
            if row.button(cx, ids!(select_file)).clicked(actions) {
                if let Some(entry) = state.entries.get(i) {
                    let ids = &mut state.draft.ids;
                    if ids.contains(&entry.id) { ids.retain(|id| id != &entry.id); } else { ids.push(entry.id.clone()); }
                    changed = true;
                }
            }
        }
        if !state.pending() && self.ui.button(cx, ids!(publish_clear)).clicked(actions) { state.clear(); changed = true; }
        if changed { self.show_publish(cx); }
    }
}
