//! The call and its live items, driven by the app's UI thread.
use crate::{App, Live, Page};
use makepad_call::driver::VoiceSource;
use makepad_create::call::{opening_uids, CallAction, CallEnv, Fact, Phase, Press};
use makepad_create::lists::{Pane, SharePopout};
use makepad_widgets::{makepad_platform::thread::{Class, ThreadOptions}, *};
use makepad_create::{model::{Action, Origin}, transcript::{self, Heard}};
use makepad_town_core::{event::{SessionId, SessionMode}, key::PublicKey};
use std::sync::{Arc, mpsc::SyncSender};

/// Live-check switch, read once at configuration. The controller substitutes its entire audio host.
fn voice_source() -> VoiceSource {
    if std::env::var_os("MAKEPAD_TOWN_CALL_SYNTHETIC").as_deref() == Some(std::ffi::OsStr::new("1")) {
        VoiceSource::Synthetic { tone_hz: 700.0 }
    } else {
        VoiceSource::Feed
    }
}

impl App {
    pub fn transcript_sender(&mut self) -> SyncSender<Heard> {
        self.transcript.get_or_insert_with(transcript::channel).0.clone()
    }

    fn flush_speech(&mut self, session: SessionId) -> bool {
        let Some(lines) = self.transcript.as_mut().and_then(|(_, sink)| sink.flush(session)) else { return false };
        self.write_call_action(Action::Said { session, lines })
    }

    fn write_call_action(&mut self, action: Action) -> bool {
        let Some(model) = self.model.as_mut() else { return false };
        match model.act(action, Origin::Ui) {
            Ok(_) => true,
            Err(why) => { log!("Town call: {why}"); false }
        }
    }

    pub(crate) fn record_call_facts(&mut self, cx: &mut Cx) {
        self.call.revoke_ended_shares(&mut self.app_links);
        self.publish_share_frame(cx);
        let mut changed = false;
        for session in self.call.take_muted_sessions() { changed |= self.flush_speech(session); }
        for fact in self.call.take_facts() {
            let action = match fact {
                Fact::ShareOpen { session, ref title, to } => {
                    let Some(seat) = self.model.as_ref().and_then(|m| m.call_seat(&to)) else { continue };
                    Action::SessionOpen { town: None, session, mode: SessionMode::Share { title: title.clone() }, to: vec![seat] }
                }
                Fact::ShareJoin { session } => Action::SessionJoin { town: None, session },
                Fact::Open { session, video, to } => {
                    let Some(seat) = self.model.as_ref().and_then(|m| m.call_seat(&to)) else {
                        if let Some(model) = &mut self.model { model.observe_voice_call(None); }
                        log!("Town call: no shared town with the rung device"); continue
                    };
                    Action::SessionOpen { town: None, session, mode: SessionMode::Call { video }, to: vec![seat] }
                }
                Fact::Join { session } => Action::SessionJoin { town: None, session },
                Fact::Leave { session, secs, why } => {
                    changed |= self.flush_speech(session);
                    Action::SessionLeave { session, secs, why }
                }
                Fact::Decline { session, why } => Action::SessionDecline { town: None, session, why },
            };
            let wrote = self.write_call_action(action);
            if let Fact::Open { session, .. } | Fact::Join { session } = fact {
                if let Some(model) = &mut self.model {
                    model.observe_voice_call((wrote || matches!(fact, Fact::Join { .. })).then_some(session));
                }
                let peer = match &fact {
                    Fact::Open { to, .. } => Some(*to),
                    Fact::Join { .. } => self.call.active_peer(),
                    _ => None,
                };
                if wrote { self.bind_transcript_town(session, peer); }
                else { self.call.set_transcript_town(session, None); }
            }
            changed |= wrote;
        }
        let ringing = self.call.ringing_session();
        let active_peer = self.call.active_peer();
        if let Some(m) = &mut self.model { m.observe_ring(ringing, active_peer, None); }
        if let Some(session) = ringing { self.bind_transcript_town(session, active_peer); }
        if changed { self.refresh(cx); }
    }

    // Snapshot the same link destination as observe_ring/Open. Changing the visible
    // town later must never change the audience named by the consent control.
    fn bind_transcript_town(&mut self, session: SessionId, peer: Option<PublicKey>) {
        if self.call.has_transcript_town(session) { return }
        let label = self.model.as_ref().and_then(|m| {
            let town = m.transcript_town(session, peer, None)?;
            m.town_name(town).map(str::to_owned).or_else(|| {
                m.towns().into_iter().find(|row| row.id == town).map(|row| row.title)
            })
        });
        self.call.set_transcript_town(session, label);
    }

    pub(crate) fn sync_live_items(&mut self, cx: &mut Cx) {
        self.view.share_peer = self.call.peer_epoch().map(|(key, _)| key);
        // A remembered connection can prove its key again without sending its display name.
        if self.view.call.view.peer_name.trim().is_empty() {
            if let (Some(model), Some((peer, _))) = (&self.model, self.call.peer_epoch()) {
                Arc::make_mut(&mut self.view.call.view).peer_name = model.author_label(&peer, makepad_town_core::event::Actor::Human);
            }
        }
        let ended = self.view.call.share.ended.clone();
        if let SharePopout::Open(session) = self.view.share_popout {
            if !self.view.share_in_window() {
                self.close_share_window(cx);
                let page = Page::Live(Live::Share(session));
                if ended.iter().any(|e| e.session == session) && !self.closing_store
                    && (self.view.page != page || (self.view.narrow() && self.view.pane != Pane::Page)) {
                    self.open_page(cx, page);
                }
            }
        }
        self.view.open_ended_share = self.view.share_page().filter(|s| ended.iter().any(|e| e.session == *s));
        self.view.sync_live_share(cx, &self.ui);
        self.view.sync_live_rows(cx, &self.ui);
        self.sync_share_preview(cx);
        if let Page::Live(item) = self.view.page {
            if self.view.has_live(item) { return }
            match item {
                Live::Call => self.return_from_call(),
                Live::Share(_) => self.view.page = Page::Home,
            }
            self.update_page(cx);
        }
    }

    pub(crate) fn show_call(&mut self, cx: &mut Cx) {
        let previous = std::mem::replace(&mut self.view.call, self.call.shown());
        self.sync_live_items(cx);
        self.call.sync_timer(cx);
        let shown = &self.view.call;
        let project_changed = previous.view.phase != shown.view.phase || previous.linked != shown.linked
            || previous.view.peer_name != shown.view.peer_name;
        if project_changed { self.ui.widget(cx, ids!(project_call)).redraw(cx); }
        if self.view.page == Page::Live(Live::Call) { self.ui.widget(cx, ids!(live_call)).redraw(cx); }
    }

    pub(crate) fn call_event(&mut self, cx: &mut Cx, event: &Event) {
        let mut changed = false;
        if !self.call.configured() {
            let Some(model) = &self.model else { return };
            let workers = cx.thread_spawner();
            let jobs = cx.task_pool();
            self.call.configure(CallEnv {
                files: model.home.files(),
                devices_path: model.home.call_devices_path(), signal: Arc::new(SignalToUI::set_ui_signal),
                voice: voice_source(),
                spawn: Arc::new(move |name, job| {
                    let opts = ThreadOptions { name: Some(name.into()), ..Default::default() };
                    // A refused worker never reports done, and the call would still answer and open
                    // the microphone without it. Until the driver can report a refusal, stopping is safer.
                    workers.spawn_worker(opts, job).expect("start a Town call worker").detach();
                }),
                jobs: Arc::new(move |name, job| {
                    match jobs.submit_named(Class::Background, name, job) {
                        Ok(task) => task.detach(),
                        Err(err) => log!("Town call: couldn't schedule {name}: {err}"),
                    }
                }),
            });
            let sender = self.transcript_sender();
            self.call.set_transcript_sender(sender);
            changed = true;
        }
        match event {
            Event::AudioDevices(list) => changed |= self.call.audio_devices(cx, &list.descs),
            Event::Shutdown => { self.call.shutdown(cx); }
            _ => {}
        }
        changed |= self.call.poll(cx);
        self.record_call_facts(cx);
        let now = self.transcript_clock.get_or_insert_with(std::time::Instant::now).elapsed().as_millis() as u64;
        let batches = self.transcript.get_or_insert_with(transcript::channel).1.poll(now);
        let mut wrote = false;
        for (session, lines) in batches { wrote |= self.write_call_action(Action::Said { session, lines }); }
        if wrote { self.refresh(cx); }
        if changed { self.show_call(cx); }
    }

    pub(crate) fn call_opening_buttons(&self, cx: &Cx, input: Option<crate::TranscriptInput>) -> Vec<WidgetUid> {
        if input != Some(crate::TranscriptInput::Choose) || !self.call.linked() && self.call.view().phase == Phase::Idle { return Vec::new() }
        opening_uids(cx, &self.ui)
    }

    pub(crate) fn call_opening_presses(&mut self, cx: &mut Cx, action: CallAction) {
        // The synthetic audio host accepts Answer through call_actions. Dispatch it here too,
        // with the same local release and button UID as the physical host.
        let changed = self.call.call_opening_presses(cx, action)
            || (action == CallAction::Press(Press::Answer) && self.call.call_actions(cx, action));
        self.record_call_facts(cx);
        if changed { self.show_call(cx); }
    }

    pub(crate) fn call_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let mut changed = false;
        for widget in actions.iter().filter_map(|a| a.as_widget_action()) {
            let Some(&action) = widget.action.downcast_ref::<CallAction>() else { continue };
            if action == CallAction::Press(Press::Answer) { continue }
            changed |= self.call.call_actions(cx, action);
            self.record_call_facts(cx);
        }
        if changed { self.show_call(cx); }
    }
}
