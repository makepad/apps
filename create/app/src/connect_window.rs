//! The connection panel's side of the App: it starts and stops the link worker, hands it the words,
//! and moves sync messages between the worker and the model. The UI thread never waits on the
//! worker; a full queue keeps its commands for the next frame.

use crate::App;
use makepad_create::link_worker::{ConnectView, FromWorker, LinkWorker, ToWorker};
use makepad_create::model::{stop_sharing_consequences, wall_ms, Model, ShareState};
use makepad_work_core::{event::Id, home::day_month, share::{Counts, Offer}};
use makepad_widgets::*;

fn counts_text(c: Counts, conjunction: bool) -> String {
    let mut parts: Vec<_> = [(c.projects, "project"), (c.tasks, "task"), (c.docs, "doc"), (c.messages, "message")]
        .into_iter().filter(|(n, _)| *n != 0)
        .map(|(n, noun)| format!("{n} {noun}{}", if n == 1 { "" } else { "s" })).collect();
    if conjunction && parts.len() > 1 {
        let last = parts.pop().unwrap();
        format!("{} and {last}", parts.join(", "))
    } else { parts.join(", ") }
}

impl App {
    pub(super) fn connect_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let panel = self.ui.popover(cx, ids!(connections));
        if panel.closed(actions) || panel.dismissed(actions) {
            self.show_another_ai = false;
            self.view.connect.confirm_forget = false;
        }
        if self.ui.button(cx, ids!(connect_open)).clicked(actions) {
            panel.toggle(cx);
        }
        if !panel.is_open() { return }
        if let Some(on) = self.ui.check_box(cx, ids!(connect_lan)).changed(actions) {
            self.link_lan_off = !on;
            let words = self.ui.text_input(cx, ids!(connect_peer_words)).text();
            if let Some(same_network) = self.view.connect.restart_for_lan_change(on, &words) {
                self.connect_start(cx, same_network);
                self.update_connect(cx);
                self.update_presence(cx);
                return
            }
        }
        if panel.opened(actions) {
            self.update_connect(cx);
        }
        if self.ui.button(cx, ids!(ai_another)).clicked(actions) {
            self.show_another_ai = true;
        } else if self.ui.button(cx, ids!(connect_checked)).clicked(actions) {
            if let (Some(model), Some(device)) = (self.model.as_mut(), self.view.connect.peer) {
                if let Err(e) = model.set_words_checked(&device) { log!("work: keeping the words check: {e}"); }
            }
            self.update_connect(cx);
            self.update_presence(cx);
        } else if self.share_actions(cx, actions) {
            self.refresh(cx);
        } else if self.model.as_ref().and_then(Model::devices_problem).is_some() {
            if self.ui.button(cx, ids!(connect_devices_retry)).clicked(actions) {
                let _ = self.retry_devices(cx);
            } else if self.ui.button(cx, ids!(connect_devices_forget)).clicked(actions) {
                let _ = self.review_forget_devices(cx);
            } else if self.ui.button(cx, ids!(connect_devices_cancel)).clicked(actions) {
                self.view.connect.confirm_forget = false;
                self.update_devices(cx);
            }
            return;
        } else if self.ui.button(cx, ids!(connect_start)).clicked(actions) {
            self.connect_start(cx, !self.link_lan_off);
        } else if self.ui.button(cx, ids!(connect_go)).clicked(actions) {
            let words = self.ui.text_input(cx, ids!(connect_peer_words)).text();
            let name = self.ui.text_input(cx, ids!(connect_name)).text();
            let name = match name.trim() { "" => "Someone", name => name };
            self.connect_words(cx, words.trim().to_owned(), name.to_owned());
        } else if self.ui.button(cx, ids!(connect_stop)).clicked(actions) {
            let idle = !self.view.connect.open && self.view.connect.linked.is_none();
            if idle {
                self.connect_resume(cx);
            } else if self.view.connect.peer.is_some() {
                self.connect_disconnect(cx);
            } else {
                self.connect_stop(cx);
            }
        } else {
            return;
        }
        self.update_connect(cx);
        self.update_presence(cx);
    }

    pub(super) fn switch_space(&mut self, cx: &mut Cx, room: Id) {
        if self.flush_doc(cx).is_err() { return }
        if let Some(model) = &mut self.model {
            if let Err(e) = model.open_space(room) { log!("work: opening a space: {e}"); }
        }
        self.refresh(cx);
    }

    fn share_actions(&mut self, cx: &mut Cx, actions: &Actions) -> bool {
        let Some(model) = &mut self.model else { return false };
        let result = if self.ui.button(cx, ids!(share_this)).clicked(actions) {
            model.offer_space(false)
        } else if self.ui.button(cx, ids!(share_new)).clicked(actions) {
            model.offer_space(true)
        } else if self.ui.button(cx, ids!(share_cancel)).clicked(actions) {
            model.cancel_offer(); Ok(())
        } else if self.ui.button(cx, ids!(share_join)).clicked(actions) {
            model.answer_offer(true); Ok(())
        } else if self.ui.button(cx, ids!(share_decline)).clicked(actions) {
            model.answer_offer(false); Ok(())
        } else if self.ui.button(cx, ids!(share_open)).clicked(actions) {
            if let ShareState::Shared(room) = model.share_state() { self.switch_space(cx, room); }
            Ok(())
        } else if self.ui.button(cx, ids!(shared_stop)).clicked(actions) {
            self.view.confirm_stop = Some(model.open_id()); Ok(())
        } else if self.ui.button(cx, ids!(shared_cancel)).clicked(actions) {
            self.view.confirm_stop = None; Ok(())
        } else if self.ui.button(cx, ids!(shared_confirm)).clicked(actions)
            && self.view.confirm_stop.take() == Some(model.open_id()) {
            model.stop_sharing()
        } else { return false };
        self.view.share_error = result.err();
        true
    }

    fn update_sharing(&self, cx: &mut Cx) {
        let Some(model) = &self.model else { return };
        let peer = model.linked_name();
        let state = model.share_state();
        let linked = self.view.connect.linked.is_some();
        let incoming = matches!(state, ShareState::Incoming(_));
        let show = |cx: &mut Cx, path, yes| self.ui.widget(cx, path).set_visible(cx, yes);
        let line = |cx: &mut Cx, path, text: &str| self.ui.label(cx, path).set_text(cx, text);
        let note = self.view.share_error.as_deref().or(model.share_note());
        for (path, visible) in [(ids!(share_note), linked), (ids!(share_offline_note), !linked)] {
            show(cx, path, visible && note.is_some());
            if visible { if let Some(note) = note { line(cx, path, note); } }
        }
        show(cx, ids!(share_idle), state == ShareState::Idle);
        show(cx, ids!(share_cancel), matches!(state, ShareState::Offered(_)));
        show(cx, ids!(share_incoming), incoming);
        show(cx, ids!(share_open), matches!(state, ShareState::Shared(id) if id != model.open_id()));
        if linked {
            let status = match state {
                ShareState::Idle => {
                    let reason = model.share_problem();
                    // A reason replaces the button: a disabled button still looks pressable.
                    show(cx, ids!(share_this), reason.is_none());
                    show(cx, ids!(share_reason), reason.is_some());
                    match &reason {
                        Some(reason) => line(cx, ids!(share_reason), reason),
                        None => self.ui.button(cx, ids!(share_this)).set_text(cx, &format!("Share this space with {peer}")),
                    }
                    self.ui.button(cx, ids!(share_new)).set_text(cx, &format!("Start a new space with {peer}"));
                    show(cx, ids!(share_history), reason.is_none());
                    if reason.is_none() {
                        if let Offer::This { counts, .. } = model.this_offer() {
                            let counts = counts_text(counts, false);
                            let history = if counts.is_empty() { format!("{peer} sees all of it, from its start. It's empty so far.") }
                                else { format!("{peer} sees all of it, from its start: {counts}.") };
                            line(cx, ids!(share_history), &history);
                        }
                    }
                    format!("Nothing of yours reaches {peer} until you share it. Each of you keeps your own space.")
                }
                ShareState::Offered(Offer::This { .. }) => format!("Waiting for {peer} to answer: you offered this space, with its history."),
                ShareState::Offered(Offer::New) => format!("Waiting for {peer} to answer: you offered a new, empty space."),
                ShareState::Incoming(offer) => {
                    line(cx, ids!(share_private), &format!("Nothing from your own space goes to {peer}."));
                    show(cx, ids!(share_counts), matches!(offer, Offer::This { .. }));
                    match offer {
                        Offer::This { counts, since } => {
                            let list = counts_text(counts, true);
                            let history = if list.is_empty() { "It's empty so far.".into() }
                                else { format!("It holds {list}, from {} on.", day_month(since)) };
                            line(cx, ids!(share_counts), &history);
                            format!("{peer} offers to share a space with you.")
                        }
                        Offer::New => format!("{peer} offers to start a new, empty space with you."),
                    }
                }
                ShareState::Joining => "Joining the space…".into(),
                ShareState::Shared(id) => {
                    let label = self.view.space_ids.iter().position(|i| *i == id)
                        .and_then(|i| self.view.space_labels.get(i)).map(String::as_str).unwrap_or("");
                    format!("You share \"{label}\" with {peer}. It stays in step while you're linked.")
                }
            };
            line(cx, ids!(share_status), &status);
        }
        let other = model.shared_with();
        show(cx, ids!(shared_space), other.is_some());
        if let Some((name, founder)) = other {
            let confirming = self.view.confirm_stop == Some(model.open_id());
            show(cx, ids!(shared_confirmation), confirming);
            show(cx, ids!(shared_stop), !confirming);
            line(cx, ids!(shared_line), &format!("This space is shared with {name}."));
            self.ui.button(cx, ids!(shared_stop)).set_text(cx, if founder { "Stop sharing…" } else { "Leave…" });
            if confirming {
                let (question, consequences) = if founder {
                    (format!("Stop sharing this space with {name}?"), stop_sharing_consequences(name))
                } else {
                    ("Leave this space?".into(), format!("You keep everything in it up to now, to read. {name} keeps everything that reached {name}. From now on, {name}'s changes stop reaching you, and yours stop reaching {name}."))
                };
                line(cx, ids!(shared_question), &question);
                line(cx, ids!(shared_consequences), &consequences);
                self.ui.button(cx, ids!(shared_confirm)).set_text(cx, if founder { "Stop sharing" } else { "Leave" });
            }
        }
    }

    pub(super) fn update_devices(&self, cx: &mut Cx) {
        let problem = self.model.as_ref().and_then(Model::devices_problem);
        self.ui.widget(cx, ids!(connect_devices)).set_visible(cx, problem.is_some());
        // Linking is off: Start and the same-network setting go, rather than a Start that does nothing.
        self.ui.widget(cx, ids!(connect_start)).set_visible(cx, problem.is_none());
        self.ui.widget(cx, ids!(connect_lan)).set_visible(cx, problem.is_none());
        if let Some(reason) = problem {
            let warning = format!("Work couldn't read its list of known devices ({reason}).");
            self.ui.label(cx, ids!(connect_devices_warning)).set_text(cx, &warning);
            let confirm = self.view.connect.confirm_forget;
            self.ui.widget(cx, ids!(connect_devices_actions)).set_visible(cx, !confirm);
            self.ui.widget(cx, ids!(connect_devices_confirmation)).set_visible(cx, confirm);
        }
    }

    pub(super) fn retry_devices(&mut self, cx: &mut Cx) -> Result<(), String> {
        let result = self.model.as_mut().ok_or("Work is not ready.")?.devices_retry();
        if result.is_err() && self.link.is_some() {
            self.connect_stop(cx);
        }
        self.finish_devices_action(cx, &result);
        result
    }

    pub(super) fn review_forget_devices(&mut self, cx: &mut Cx) -> Result<(), String> {
        if self.model.as_ref().and_then(Model::devices_problem).is_none() {
            return Err("The list of known devices is readable.".into());
        }
        self.view.connect.confirm_forget = true;
        self.view.connect.devices_error = None;
        self.update_page(cx);
        Ok(())
    }

    pub(super) fn confirm_forget_devices(&mut self, cx: &mut Cx) {
        if !self.ui.popover(cx, ids!(connections)).is_open() || !self.view.connect.confirm_forget { return }
        let Some(model) = &mut self.model else { return };
        let result = model.devices_forget();
        self.finish_devices_action(cx, &result);
    }

    fn finish_devices_action(&mut self, cx: &mut Cx, result: &Result<(), String>) {
        self.view.connect.confirm_forget = false;
        self.view.connect.devices_error = result.as_ref().err().cloned();
        self.read_workspace();
        self.update_presence(cx);
        self.update_connect(cx);
    }

    pub(super) fn update_ai_commands(&self, cx: &mut Cx) {
        if !self.ui.popover(cx, ids!(connections)).is_open() { return }
        let commands = &self.view.workspace.connect;
        let show_commands = !self.presence.listening() || self.show_another_ai;
        self.ui.widget(cx, ids!(ai_commands)).set_visible(cx, show_commands && self.model.is_some());
        self.ui.widget(cx, ids!(ai_another)).set_visible(cx, !show_commands);
        self.ui.text_input(cx, ids!(ai_terminal)).set_text(cx, &commands.terminal);
        self.ui.text_input(cx, ids!(ai_mcp)).set_text(cx, &commands.mcp);
    }

    pub(super) fn update_connect(&self, cx: &mut Cx) {
        if !self.ui.popover(cx, ids!(connections)).is_open() { return }
        self.update_devices(cx);
        self.update_ai_commands(cx);
        self.update_sharing(cx);
        let v = &self.view.connect;
        let linked = v.linked.is_some();
        let idle = !v.open && !linked;
        // A device the person disconnected stays quiet until they connect again.
        let paused = if idle { self.model.as_ref().and_then(Model::paused_device) } else { None };
        let show_form = v.open && !linked && v.own_words.is_some();
        let error = if idle {
            v.devices_error.as_deref().or(v.failed.as_deref())
        } else if show_form {
            v.bad_words.as_deref().or(v.blocked).or(v.failed.as_deref())
        } else if !linked {
            v.refused.as_deref()
        } else {
            None
        };
        for (path, visible) in [
            (ids!(connect_intro), idle),
            (ids!(connect_form), show_form),
            (ids!(connect_note), show_form && v.note.is_some()),
            (ids!(connect_privacy), show_form),
            (ids!(connect_same), show_form && v.same_network),
            (ids!(connect_linked), linked),
            (ids!(connect_status), !idle || paused.is_some()),
            (ids!(connect_error), error.is_some()),
            (ids!(connect_stop), !idle || paused.is_some()),
        ] {
            self.ui.widget(cx, path).set_visible(cx, visible);
        }
        self.ui.check_box(cx, ids!(connect_lan)).set_active(cx, !self.link_lan_off, Animate::No);
        self.ui.button(cx, ids!(connect_start)).set_text(cx, if v.failed.is_some() { "Start again" } else { "Start" });
        let status = match paused {
            Some((_, name)) => format!("Work isn't reaching {name} again: you disconnected."),
            None => v.status_text(),
        };
        self.ui.label(cx, ids!(connect_status)).set_text(cx, &status);
        let stop = match paused {
            Some((_, name)) => format!("Reconnect with {name}"),
            None if linked || v.peer.is_some() => "Disconnect".to_owned(),
            None => "Cancel".to_owned(),
        };
        self.ui.button(cx, ids!(connect_stop)).set_text(cx, &stop);
        self.ui.text_input(cx, ids!(connect_own_words)).set_text(cx, v.own_words.as_deref().unwrap_or(""));
        self.ui.label(cx, ids!(connect_note)).set_text(cx, v.note.unwrap_or(""));
        self.ui.label(cx, ids!(connect_privacy)).set_text(cx, v.words_privacy());
        self.ui.label(cx, ids!(connect_same)).set_text(cx, ConnectView::SAME_NETWORK);
        self.ui.label(cx, ids!(connect_error)).set_text(cx, error.unwrap_or(""));
        self.ui.widget(cx, ids!(connect_checked)).set_visible(cx, linked && !self.words_checked());
        if let Some(peer) = &v.linked {
            let name = self.model.as_ref().map_or("the other device", Model::linked_name);
            self.ui.label(cx, ids!(connect_linked_title)).set_text(cx, &format!("Linked with {name}"));
            self.ui.text_input(cx, ids!(connect_own_device)).set_text(cx, &peer.own_words);
            self.ui.text_input(cx, ids!(connect_peer_device)).set_text(cx, &peer.peer_words);
        }
        self.ui.popover(cx, ids!(connections)).redraw(cx);
    }

    /// The linked device's words check, kept with its pin.
    pub(crate) fn words_checked(&self) -> bool {
        match (&self.model, &self.view.connect.peer) {
            (Some(model), Some(device)) => model.words_checked(device),
            _ => false,
        }
    }

    /// Starts a first contact: the worker finds our address and makes our words.
    fn connect_start(&mut self, cx: &mut Cx, same_network: bool) {
        let Some(model) = &self.model else { return };
        if model.devices_problem().is_some() { return }
        self.link = None;
        self.link_queue.clear();
        self.view.connect = ConnectView { open: true, ..ConnectView::default() };
        match LinkWorker::start(model.device_key(), same_network, SignalToUI::set_ui_signal) {
            Ok(worker) => self.link = Some(worker),
            Err(e) => (self.view.connect.open, self.view.connect.failed) = (false, Some(format!("couldn't start: {e}"))),
        }
        self.ui.redraw(cx);
    }

    /// The other person's words, as typed, and the name they will see.
    pub(crate) fn connect_words(&mut self, cx: &mut Cx, words: String, name: String) {
        self.view.connect.bad_words = None;
        self.view.connect.peer_words_submitted |= !words.is_empty();
        self.link_queue.push_back(ToWorker::PeerWords { words, name });
        self.link_event(cx);
    }

    /// Starts reaching the device this one was last linked with, with no words, if there is one
    /// and the person hasn't disconnected it. The model syncs with one peer, so this is one
    /// device; a second waits until the model can hold two links.
    pub(crate) fn reconnect_start(&mut self, cx: &mut Cx) {
        if self.link.is_some() { return }
        let Some(model) = &self.model else { return };
        if model.devices_problem().is_some() { return }
        let Some(known) = model.reconnect_known() else { return };
        let device = known.pin.device;
        match LinkWorker::reconnect(model.device_key(), known, SignalToUI::set_ui_signal) {
            Ok(worker) => {
                self.view.connect = ConnectView { open: true, peer: Some(device), reaching_since: Some(wall_ms() / 1000), ..ConnectView::default() };
                self.link = Some(worker);
            }
            Err(e) => log!("work: couldn't start reaching the last linked device: {e}"),
        }
        self.update_presence(cx);
        self.update_connect(cx);
    }

    /// The person connects again: reconnecting to the device they disconnected resumes.
    fn connect_resume(&mut self, cx: &mut Cx) {
        let Some(model) = &mut self.model else { return };
        let Some((device, _)) = model.paused_device() else { return };
        if let Err(e) = model.set_paused(&device, false) {
            log!("work: couldn't resume reaching the device: {e}");
            return;
        }
        self.reconnect_start(cx);
    }

    /// Disconnect: ends the link and pauses reconnecting to that device until the person connects
    /// again. The pause is on the device's record, so a restart keeps it.
    fn connect_disconnect(&mut self, cx: &mut Cx) {
        let peer = self.view.connect.peer;
        if let (Some(model), Some(peer)) = (&mut self.model, peer) {
            if let Err(e) = model.set_paused(&peer, true) { log!("work: couldn't remember the disconnect: {e}"); }
        }
        self.connect_stop(cx);
    }

    /// Ends the first contact or the link.
    pub(crate) fn connect_stop(&mut self, cx: &mut Cx) {
        self.link = None;
        self.link_queue.clear();
        self.unread_link = None;
        if let Some(model) = &mut self.model {
            model.sync_stop();
        }
        self.view.connect = ConnectView::default();
        self.update_presence(cx);
    }

    /// Takes what the worker said, and hands it what is due: queued commands and sync messages.
    pub(crate) fn link_event(&mut self, cx: &mut Cx) {
        if self.link.is_none() { return }
        let (mut changed, mut refresh) = (false, false);
        let mut peer_changed = false;
        while let Some(event) = self.unread_link.take().map(FromWorker::Message)
            .or_else(|| self.link.as_ref().and_then(LinkWorker::try_recv)) {
            if let FromWorker::Message(bytes) = &event {
                if self.document.as_ref().is_some_and(|d| d.dirty()) && self.model.as_ref().is_some_and(|m| m.before_room_change(bytes))
                    && self.flush_doc(cx).is_err() {
                    if let FromWorker::Message(bytes) = event { self.unread_link = Some(bytes); }
                    break
                }
            }
            peer_changed |= matches!(event, FromWorker::Linked { .. } | FromWorker::Failed(_) | FromWorker::Lost | FromWorker::KeyChanged
                | FromWorker::Reconnecting { .. } | FromWorker::Refused(_));
            self.view.connect.apply(&event);
            changed |= !matches!(event, FromWorker::Message(_));
            let Some(model) = &mut self.model else { continue };
            let previous = model.share_state();
            match event {
                FromWorker::Linked { peer_device, version, peer_name, reconnected, .. } => {
                    match model.pin_linked(&peer_device, version, &peer_name) {
                        Ok(()) => refresh = true,
                        Err(e) => log!("work: couldn't remember the linked device ({e})"),
                    }
                    // Linking again by words is the person connecting again.
                    if !reconnected {
                        if let Err(e) = model.set_paused(&peer_device, false) { log!("work: couldn't lift the pause ({e})"); }
                    }
                    model.sync_start(&peer_device);
                }
                FromWorker::Path { peer_device, path } => {
                    if let Err(e) = model.remember_path(&peer_device, path) { log!("work: couldn't remember where the device was reached ({e})"); }
                }
                FromWorker::Lost | FromWorker::KeyChanged => { self.link_queue.clear(); model.sync_stop(); }
                FromWorker::Message(message) => match model.sync_receive(&message) {
                    Ok(brought) => refresh |= brought,
                    Err(e) => log!("work: a sync message from the peer was refused: {e}"),
                },
                FromWorker::Failed(_) => { self.link_queue.clear(); model.sync_stop(); },
                _ => {}
            }
            if model.share_state() != previous { changed = true; }
            if peer_changed || model.share_state() != previous { self.view.share_error = None; }
        }
        if let Some(model) = &mut self.model {
            self.link_queue.extend(model.sync_outgoing().into_iter().map(ToWorker::Send));
        }
        let worker = self.link.as_ref().unwrap();
        while let Some(command) = self.link_queue.pop_front() {
            if let Err(command) = worker.try_send(command) {
                self.link_queue.push_front(command);
                log!("work: the link's queue is full; the rest goes on the next frame");
                let _ = cx.new_next_frame();
                break;
            }
        }
        if !self.view.connect.open {
            // The worker stopped; what it said stays on the screen.
            self.link = None;
        }
        if refresh {
            self.refresh(cx);
        } else if changed {
            self.update_presence(cx);
            self.update_connect(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_copy_counts_people_can_check_and_names_without_pronouns() {
        let c = Counts { projects: 2, tasks: 1, docs: 0, messages: 3 };
        assert_eq!(counts_text(c, false), "2 projects, 1 task, 3 messages", "the outgoing history skips zeros and uses the singular");
        assert_eq!(counts_text(c, true), "2 projects, 1 task and 3 messages", "the incoming history reads as a list");
        assert!(counts_text(Counts::default(), true).is_empty(), "empty rooms use the separate empty message");
        // Check the actual Rust format strings and Splash copy, before substituting anyone's name.
        for source in [include_str!("connect_window.rs"), include_str!("ui/connect.rs"), include_str!("ui/chat.rs"),
            include_str!("ui/navigation.rs"), include_str!("model.rs")] {
            for line in source.split("#[cfg(test)]").next().unwrap().lines() {
                for literal in line.split('"').skip(1).step_by(2) {
                    for word in literal.split(|c: char| !c.is_alphabetic()) {
                        assert!(!["her", "his", "he", "she", "him"].contains(&word.to_ascii_lowercase().as_str()),
                            "share copy assigns a pronoun in {literal}");
                    }
                }
            }
        }
    }
}
