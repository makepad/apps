//! Route a person's file drag to Publish, the board under the pointer, or the town chat.
use super::*;
use makepad_create::store_worker::BoardMediaTarget;
use std::path::Path;

/// Where a window-wide file drop goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropRoute { Publish, Board { board: Id }, Chat, Ignore }

pub fn drop_route(publish_open: bool, open_board: Option<Id>, over_board: bool, writable: bool) -> DropRoute {
    if publish_open { return DropRoute::Publish }
    if !writable { return DropRoute::Ignore }
    match open_board {
        Some(board) if over_board => DropRoute::Board { board },
        _ => DropRoute::Chat,
    }
}

/// The person's own files in a drag. A drag that Town started itself carries an internal id.
fn dropped_files(items: &[DragItem]) -> impl Iterator<Item = &Path> + Clone {
    items.iter().filter_map(|item| match item {
        DragItem::FilePath { path, internal_id: None } => Some(Path::new(path.as_str())),
        _ => None,
    })
}

/// The hint's words. Only the name: size would be file I/O on the UI thread.
fn hint_line(count: usize, name: Option<&str>) -> String {
    match count {
        0 => String::new(),
        1 => name.map_or_else(|| "Drop to post".into(), |n| format!("Drop to post {n}")),
        n => format!("Drop to post {n} files"),
    }
}

impl App {
    /// Publish keeps its drop only while its page is open.
    pub(super) fn publish_takes_drops(&self) -> bool {
        self.view.page == Page::Publish && self.view.publish.as_ref().is_some_and(|p| p.enabled() && !p.sending())
    }

    fn drop_route(&self, granted: bool, files: usize, open_board: Option<Id>, over_board: bool) -> DropRoute {
        if files == 0 { return DropRoute::Ignore }
        let active = self.model.as_ref().is_some_and(|m| m.state.active_member(&m.me()));
        drop_route(self.publish_takes_drops(), open_board, over_board, granted && active && !self.closing_store)
    }

    /// Show the hint on the chat that is displayed, or hide it. Nothing is redrawn when unchanged.
    pub(super) fn show_drop_hint(&mut self, cx: &mut Cx, line: &str) {
        let hint = self.view.conversation(cx, &self.ui).label(cx, ids!(drop_hint));
        let shown = !line.is_empty();
        if hint.visible() != shown { hint.set_visible(cx, shown); }
        if shown && hint.text() != line { hint.set_text(cx, line); }
    }

    /// Queue each file. The store worker reads, sniffs and stores it: nothing is read here.
    fn post_dropped_files<'a>(&mut self, files: impl Iterator<Item = &'a Path>, board: Option<(Id, (i64, i64))>) {
        let Some(model) = &mut self.model else { return };
        let town = model.open_id();
        let mut problems = Vec::new();
        for (i, path) in files.enumerate() {
            let result = if let Some((board, (x, y))) = board {
                let pos = (i as i64).checked_mul(32 * 64)
                    .and_then(|d| Some((x.checked_add(d)?, y.checked_add(d)?)));
                match pos {
                    Some(pos) => model.drop_on_board(town, BoardMediaTarget { board, pos: Some(pos) }, path.to_owned()),
                    None => Err("The drop is outside the board's coordinate range.".into()),
                }
            } else { model.post_dropped(town, path.to_owned()) };
            if let Err(why) = result {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                problems.push(format!("{name}: {why}"));
            }
        }
        if !problems.is_empty() {
            if let Some(before) = self.recorder.problem.take() { problems.insert(0, before); }
            self.recorder.problem = Some(problems.join(" · "));
        }
    }

    /// The empty board's invitation, shown or not. Nothing is redrawn when unchanged.
    fn show_board_invitation(&self, cx: &mut Cx, shown: bool) {
        let invitation = self.ui.widget(cx, ids!(board_empty));
        if invitation.visible() != shown { invitation.set_visible(cx, shown); }
    }

    /// The board's file ghost goes, and the invitation comes back if it stepped aside for it.
    fn clear_board_file_hover(&self, cx: &mut Cx) {
        let surface = self.ui.widget(cx, ids!(canvas.surface));
        let invite = {
            let Some(mut canvas) = surface.borrow_mut::<BoardCanvas>() else { return };
            if !canvas.clear_file_hover(cx) { return }
            canvas.show_invitation()
        };
        self.show_board_invitation(cx, invite);
    }

    pub(super) fn chat_drop_event(&mut self, cx: &mut Cx, event: &Event) {
        let (items, abs) = match event {
            Event::DragEnd | Event::WindowLostFocus(_) => {
                self.clear_board_file_hover(cx);
                self.show_drop_hint(cx, "");
                return
            }
            Event::Drag(e) => (&e.items, e.abs),
            Event::Drop(e) => (&e.items, e.abs),
            _ => return,
        };
        let files = dropped_files(items);
        let count = files.clone().count();
        let surface = self.ui.widget(cx, ids!(canvas.surface));
        if count == 0 {
            self.clear_board_file_hover(cx);
            self.show_drop_hint(cx, "");
            return
        }
        let name = files.clone().next().and_then(Path::file_name).and_then(|s| s.to_str());
        let open_board = self.view.open_board.filter(|_| self.view.page == Page::Board
            && (!self.view.narrow() || self.view.pane == Pane::Page) && surface.borrow::<BoardCanvas>().is_some_and(|c| !c.presenting()));
        let over_board = open_board.is_some() && surface.area().clipped_rect(cx).contains(abs)
            && !self.ui.widget(cx, ids!(board_toolbar)).area().clipped_rect(cx).contains(abs);
        let granted = makepad_create::consent::granted(cx, self.call.consent_home.verified());
        let route = self.drop_route(granted, count, open_board, over_board);
        if let DropRoute::Board { board } = route {
            self.show_drop_hint(cx, "");
            let area = surface.area();
            let Some(mut canvas) = surface.borrow_mut::<BoardCanvas>() else { return };
            match event.drag_hits(cx, area) {
                DragHit::Drag(d) if d.state != DragState::Out => {
                    if let Ok(mut response) = d.response.try_lock() { *response = DragResponse::Copy; }
                    canvas.file_hover(cx, d.abs, d.modifiers.alt, count, name);
                }
                DragHit::Drop(d) => {
                    canvas.file_hover(cx, d.abs, d.modifiers.alt, count, name);
                    if let Some(pos) = canvas.take_file_drop(cx) {
                        self.post_dropped_files(files, Some((board, pos)));
                    } else { self.recorder.problem = Some("The drop is outside the board's coordinate range.".into()); }
                }
                _ => { canvas.clear_file_hover(cx); }
            }
            let invite = canvas.show_invitation();
            drop(canvas);
            self.show_board_invitation(cx, invite);
            return
        }
        self.clear_board_file_hover(cx);
        if route != DropRoute::Chat {
            self.show_drop_hint(cx, "");
            return
        }
        let area = self.ui.widget(cx, ids!(main_window.body)).area();
        match event.drag_hits(cx, area) {
            DragHit::Drag(drag) if drag.state != DragState::Out => {
                if let Ok(mut response) = drag.response.try_lock() { *response = DragResponse::Copy; }
                self.show_drop_hint(cx, &hint_line(count, name));
            }
            DragHit::Drag(_) => self.show_drop_hint(cx, ""),
            DragHit::Drop(_) => {
                self.show_drop_hint(cx, "");
                self.post_dropped_files(files, None);
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
    fn drop_route_prefers_publish_then_open_board_then_chat() {
        use DropRoute::{Board, Chat, Ignore, Publish};
        let board = [7; 32];
        // Columns in each row: closed board / outside, closed / over, open / outside, open / over.
        for (publish, writable, expected) in [
            (false, false, [Ignore; 4]),
            (false, true, [Chat, Chat, Chat, Board { board }]),
            (true, false, [Publish; 4]),
            (true, true, [Publish; 4]),
        ] {
            for ((open, over), route) in [(None, false), (None, true), (Some(board), false), (Some(board), true)]
                .into_iter().zip(expected) {
                assert_eq!(drop_route(publish, open, over, writable), route,
                    "publish={publish}, board={open:?}, over={over}, writable={writable}");
            }
        }
    }

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
        model.start_store_with_poster_decoder(cx.task_pool(), move || { let _ = signal.send(()); },
            std::sync::Arc::new(|_, _| Err("synthetic broken video".into()))).unwrap();
        app.model = Some(model);
        app.refresh(cx);

        // The hint names the file, in whichever chat form is displayed, and a drag end hides it.
        let shot = PathBuf::from("/pictures/shot.png");
        app.view.pane = Pane::Chat;
        for form in [Form::Wide, Form::Narrow] {
            app.view.form = form;
            let hint = app.view.conversation(cx, &app.ui).label(cx, ids!(drop_hint));
            assert!(!hint.visible(), "{form:?}: no hint before a drag");
            app.show_drop_hint(cx, &hint_line(1, shot.file_name().unwrap().to_str()));
            assert!(hint.visible() && hint.text() == "Drop to post shot.png", "{form:?}: the hint names the file");
            app.chat_drop_event(cx, &Event::DragEnd);
            assert!(!hint.visible(), "{form:?}: a drag end hides the hint");
        }
        assert_eq!(hint_line(2, None), "Drop to post 2 files");

        // Only the person's own files count. A drag Town started has an internal id.
        let items = [
            DragItem::FilePath { path: "/pictures/shot.png".into(), internal_id: None },
            DragItem::FilePath { path: "/inside".into(), internal_id: Some(live_id!(town_drag)) },
            DragItem::String { value: "task".into(), internal_id: None },
        ];
        let files: Vec<_> = dropped_files(&items).collect();
        assert_eq!(files, [shot.as_path()], "internal drags and strings are not files");

        // Routing: the chat takes a drop when consent is given. Publish keeps it only on its page.
        app.view.page = Page::Home;
        assert_eq!(app.drop_route(true, files.len(), None, false), DropRoute::Chat, "a drop anywhere posts to the chat");
        assert_eq!(app.drop_route(false, files.len(), None, false), DropRoute::Ignore, "a script on a real home cannot post a file");
        assert_eq!(app.drop_route(true, 0, None, false), DropRoute::Ignore, "nothing dragged, nothing to do");
        std::fs::write(dir.join("publish.json"), r#"{"targets":[{"name":"Demo","ssh":"example.invalid","dir":"/posts"}]}"#).unwrap();
        app.view.publish = Some(LocalPublish::open(&dir));
        assert_eq!(app.drop_route(true, files.len(), None, false), DropRoute::Chat, "Publish is configured but its page is closed");
        app.view.page = Page::Publish;
        assert_eq!(app.drop_route(true, files.len(), None, false), DropRoute::Publish, "a drop on the open Publish page still goes to Publish");
        app.view.page = Page::Home;

        // The drop queues the file for the store worker, which posts it to the town and keeps it.
        let file = dir.join("shot.png");
        std::fs::write(&file, b"\x89PNG\r\n\x1a\nnot a whole picture").unwrap();
        let video = dir.join("broken.mp4");
        std::fs::write(&video, b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2").unwrap();
        app.post_dropped_files([video.as_path(), file.as_path()].into_iter(), None);
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
