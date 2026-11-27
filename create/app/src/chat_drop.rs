//! A picture or video dropped anywhere on the window posts into the open town's chat.
//! While the drag is over the window the chat shows how it will post, above the input.
use super::*;
use std::path::PathBuf;

/// Where a window-wide file drop goes.
#[derive(Debug, PartialEq)]
enum DropRoute { Publish, Chat, Ignore }

/// The person's own files in a drag. A drag that Town started itself carries an internal id.
fn dropped_files(items: &[DragItem]) -> Vec<PathBuf> {
    items.iter().filter_map(|item| match item {
        DragItem::FilePath { path, internal_id: None } => Some(PathBuf::from(path)),
        _ => None,
    }).collect()
}

/// The hint's words. Only the name: size would be file I/O on the UI thread.
fn hint_line(files: &[PathBuf]) -> String {
    let name = |path: &PathBuf| path.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty());
    match files {
        [] => String::new(),
        [one] => name(one).map_or_else(|| "Drop to post".into(), |n| format!("Drop to post {n}")),
        many => format!("Drop to post {} files", many.len()),
    }
}

impl App {
    /// Publish keeps its drop only while its page is open.
    pub(super) fn publish_takes_drops(&self) -> bool {
        self.view.page == Page::Publish && self.view.publish.as_ref().is_some_and(|p| p.enabled() && !p.sending())
    }

    fn drop_route(&self, granted: bool, files: &[PathBuf]) -> DropRoute {
        if files.is_empty() { return DropRoute::Ignore }
        if self.publish_takes_drops() { return DropRoute::Publish }
        let active = self.model.as_ref().is_some_and(|m| m.state.active_member(&m.me()));
        if granted && active && !self.closing_store { DropRoute::Chat } else { DropRoute::Ignore }
    }

    /// Show the hint on the chat that is displayed, or hide it. Nothing is redrawn when unchanged.
    pub(super) fn show_drop_hint(&mut self, cx: &mut Cx, line: &str) {
        let hint = self.view.conversation(cx, &self.ui).label(cx, ids!(drop_hint));
        let shown = !line.is_empty();
        if hint.visible() != shown { hint.set_visible(cx, shown); }
        if shown && hint.text() != line { hint.set_text(cx, line); }
    }

    /// Queue each file. The store worker reads, sniffs and stores it: nothing is read here.
    pub(super) fn post_dropped_files(&mut self, files: Vec<PathBuf>) {
        let Some(model) = &mut self.model else { return };
        let town = model.open_id();
        let mut problems = Vec::new();
        for path in files {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Err(why) = model.post_dropped(town, path) { problems.push(format!("{name}: {why}")); }
        }
        if !problems.is_empty() {
            if let Some(before) = self.recorder.problem.take() { problems.insert(0, before); }
            self.recorder.problem = Some(problems.join(" · "));
        }
    }

    pub(super) fn chat_drop_event(&mut self, cx: &mut Cx, event: &Event) {
        let items = match event {
            Event::DragEnd => { self.show_drop_hint(cx, ""); return }
            Event::Drag(e) => &e.items,
            Event::Drop(e) => &e.items,
            _ => return,
        };
        let files = dropped_files(items);
        let granted = makepad_create::consent::granted(cx, self.call.consent_home.verified());
        if self.drop_route(granted, &files) != DropRoute::Chat {
            if matches!(event, Event::Drop(_)) { self.show_drop_hint(cx, ""); }
            return
        }
        let area = self.ui.widget(cx, ids!(main_window.body)).area();
        match event.drag_hits(cx, area) {
            DragHit::Drag(drag) if drag.state != DragState::Out => {
                if let Ok(mut response) = drag.response.try_lock() { *response = DragResponse::Copy; }
                self.show_drop_hint(cx, &hint_line(&files));
            }
            DragHit::Drag(_) => self.show_drop_hint(cx, ""),
            DragHit::Drop(_) => {
                self.show_drop_hint(cx, "");
                self.post_dropped_files(files);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_create::publish::session::LocalPublish;

    #[test]
    fn a_file_drag_shows_its_hint_and_a_drop_posts_to_the_chat_unless_publish_is_open() {
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            makepad_create::script_mod(vm);
        });
        let cx = &mut *pool;
        let mut app = cx.with_vm(App::script_new);
        app.ui = cx.with_vm(|vm| {
            let root = script_eval!(vm, { use mod.widgets.* View{chat_panel := ChatRail{}} });
            WidgetRef::script_from_value(vm, root)
        });
        let dir = std::env::temp_dir().join(format!("town-drop-{}", makepad_town_core::hex(&makepad_town_core::key::DeviceKey::generate().unwrap().public())));
        let mut model = Model::open(&dir).unwrap().0;
        model.set_person_name("Ana").unwrap();
        let (signal, wake) = std::sync::mpsc::channel();
        model.start_store(cx.task_pool(), move || { let _ = signal.send(()); }).unwrap();
        app.model = Some(model);
        app.refresh(cx);

        // The hint names the file, in whichever chat form is displayed, and a drag end hides it.
        let shot = PathBuf::from("/pictures/shot.png");
        app.view.pane = Pane::Chat;
        for form in [Form::Wide, Form::Narrow] {
            app.view.form = form;
            let hint = app.view.conversation(cx, &app.ui).label(cx, ids!(drop_hint));
            assert!(!hint.visible(), "{form:?}: no hint before a drag");
            app.show_drop_hint(cx, &hint_line(std::slice::from_ref(&shot)));
            assert!(hint.visible() && hint.text() == "Drop to post shot.png", "{form:?}: the hint names the file");
            app.chat_drop_event(cx, &Event::DragEnd);
            assert!(!hint.visible(), "{form:?}: a drag end hides the hint");
        }
        assert_eq!(hint_line(&[shot.clone(), shot.clone()]), "Drop to post 2 files");

        // Only the person's own files count. A drag Town started has an internal id.
        let items = [
            DragItem::FilePath { path: "/pictures/shot.png".into(), internal_id: None },
            DragItem::FilePath { path: "/inside".into(), internal_id: Some(live_id!(town_drag)) },
            DragItem::String { value: "task".into(), internal_id: None },
        ];
        assert_eq!(dropped_files(&items), vec![shot.clone()], "internal drags and strings are not files");

        // Routing: the chat takes a drop when consent is given. Publish keeps it only on its page.
        let files = vec![shot.clone()];
        app.view.page = Page::Home;
        assert_eq!(app.drop_route(true, &files), DropRoute::Chat, "a drop anywhere posts to the chat");
        assert_eq!(app.drop_route(false, &files), DropRoute::Ignore, "a script on a real home cannot post a file");
        assert_eq!(app.drop_route(true, &[]), DropRoute::Ignore, "nothing dragged, nothing to do");
        std::fs::write(dir.join("publish.json"), r#"{"targets":[{"name":"Demo","ssh":"example.invalid","dir":"/posts"}]}"#).unwrap();
        app.view.publish = Some(LocalPublish::open(&dir));
        assert_eq!(app.drop_route(true, &files), DropRoute::Chat, "Publish is configured but its page is closed");
        app.view.page = Page::Publish;
        assert_eq!(app.drop_route(true, &files), DropRoute::Publish, "a drop on the open Publish page still goes to Publish");
        app.view.page = Page::Home;

        // The drop queues the file for the store worker, which posts it to the town and keeps it.
        let file = dir.join("shot.png");
        std::fs::write(&file, b"\x89PNG\r\n\x1a\nnot a whole picture").unwrap();
        let video = dir.join("broken.mp4");
        std::fs::write(&video, b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2").unwrap();
        app.post_dropped_files(vec![video.clone(), file.clone()]);
        assert!(app.recorder.problem.is_none(), "{:?}", app.recorder.problem);
        let model = app.model.as_mut().unwrap();
        assert!(model.clips_pending(), "the drop queues the file at once");
        while model.clips_pending() {
            model.drain_store();
            if model.clips_pending() { wake.recv().unwrap(); }
        }
        let clips: Vec<_> = model.state.timeline.iter().filter_map(|entry| match entry {
            makepad_town_core::fold::Entry::Clip(id) => Some(&model.state.entities[id]),
            _ => None,
        }).collect();
        assert_eq!(clips.len(), 2, "both drops reached the chat");
        let (movie, picture) = (clips[0], clips[1]);
        assert_eq!((movie.project, movie.text(field::TITLE), movie.text(field::MEDIA)), (None, "broken.mp4", "video/mp4"));
        assert_eq!(movie.value(field::POSTER), None, "the broken video posts without a poster");
        assert_eq!((picture.project, picture.text(field::TITLE), picture.text(field::MEDIA)), (None, "shot.png", "image/png"));
        assert!(video.is_file() && file.is_file(), "both original files stay where they were");
        model.stop_store();
        while !model.store_stopped() { model.drain_store(); if !model.store_stopped() { wake.recv().unwrap(); } }
        app.model = None;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
