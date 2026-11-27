use super::{AppLinkEnv, AppLinkEvent, AppLinks};
use makepad_widgets::{makepad_platform::thread::ThreadOptions, *};
use std::sync::Arc;

impl AppLinks {
    /// Polls the links once per event and returns what changed, for the window code to route.
    pub fn app_link_event(&mut self, cx: &mut Cx, event: &Event) -> Vec<AppLinkEvent> {
        if !self.configured() {
            let workers = cx.thread_spawner();
            self.configure(AppLinkEnv {
                spawn: Arc::new(move |name, job| {
                    let options = ThreadOptions {
                        name: Some(name.into()),
                        ..Default::default()
                    };
                    match workers.spawn_worker(options, job) {
                        Ok(thread) => thread.detach(),
                        Err(e) => log!("App link: couldn't start {name}: {e}"),
                    }
                }),
                signal: Arc::new(SignalToUI::set_ui_signal),
                codecs: makepad_call::driver::codec::platform_codecs(),
                registry: None,
            });
        }
        if matches!(event, Event::Shutdown) {
            self.shutdown();
        }
        let events = self.poll();
        for change in &events {
            log!("App link: {change:?}");
        }
        events
    }
}
