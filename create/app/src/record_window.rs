//! The window accepts capture commands only from the local press handler.
use super::App;
use makepad_create::{app_link::AppLinkEvent, call::Press, consent, lists::Page, record::{RecordPress, capture::{CaptureBar, shortcut}}};
use makepad_widgets::*;

impl App {
    pub fn recorder_settling(&self) -> bool { self.recorder.settling(self.model.as_ref()) }

    // The picker already checked consent on the input event. A link may finish opening
    // on a later worker wake, after that input's provenance is no longer available.
    pub(crate) fn record_press(&mut self, press: RecordPress) {
        let result = self.model.as_ref().ok_or_else(|| "No town is open.".to_owned())
            .and_then(|m| self.recorder.capture_press(&mut self.app_links, press, m));
        if let Err(why) = result { self.recorder.problem = Some(why); }
    }

    pub(crate) fn capture_key(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if self.closing_store || self.model.is_none() { return false; }
        if matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Escape) && self.call.close_capture_picker() {
            self.show_capture(cx);
            return true;
        }
        let Some(key) = shortcut(event) else { return false; };
        let press = self.call.capture_key_press(key);
        if !self.capture_input(cx, press) { return false; }
        if self.view.page != Page::Tasks && self.call.shown().share.capture.picking { self.open_page(cx, Page::Tasks); }
        // The shortcut consumes its originating event before widget dispatch.
        self.app_link_event(cx, event);
        true
    }

    pub(crate) fn capture_input(&mut self, cx: &mut Cx, press: Press) -> bool {
        let granted = press == Press::StopRecording || consent::granted(cx, self.call.consent_home.verified());
        if self.model.is_none() || !self.call.capture_input(cx, press, granted) { return false; }
        self.show_capture(cx);
        true
    }

    pub(crate) fn show_capture(&mut self, cx: &mut Cx) {
        self.view.call = self.call.shown();
        let capture = &self.view.call.share.capture;
        capture.show_recording(cx, &self.ui);
        self.ui.button(cx, ids!(capture.clip.chip.control)).set_enabled(cx, capture.recording.is_none());
        if let Some(mut bar) = self.ui.widget(cx, ids!(capture_status)).borrow_mut::<CaptureBar>() {
            bar.show(cx, capture);
        }
    }

    pub fn record_link_events(&mut self, cx: &mut Cx, events: &[AppLinkEvent]) {
        let awaiting_capture = self.closing_store && self.recorder.settling(None);
        if let Some(model) = &mut self.model {
            let window = self.ui.window(cx, ids!(main_window));
            self.recorder.capture_event(cx, &window, &mut self.app_links, model);
            self.recorder.record_link_events(&mut self.app_links, events, model);
        }
        let retry = self.recorder.discard_unused_posters(|path| {
            let path = path.to_owned();
            match cx.task_pool().submit_named(makepad_widgets::makepad_platform::thread::Class::Background,
                "remove unused recording poster", move || {
                    if let Err(e) = std::fs::remove_file(path) {
                        if e.kind() != std::io::ErrorKind::NotFound { log!("Removing unused recording poster: {e}"); }
                    }
                }) {
                Ok(task) => { task.detach(); true }
                Err(_) => false,
            }
        });
        // The close loop drained the store before this event. A failed final grab
        // releases its hold without submitting an import that would wake the UI.
        if retry || (awaiting_capture && !self.recorder.settling(None)) { cx.new_next_frame(); }
        if self.call.sync_recording_row(cx, &mut self.recorder, std::time::Instant::now()) { self.show_capture(cx); }
    }
}
