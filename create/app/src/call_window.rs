//! The call and its live items, driven by the app's UI thread.
use crate::{App, Live, Page};
use makepad_call::driver::VoiceSource;
use makepad_create::call::{opening_uids, CallAction, CallEnv, CallTarget, Fact, Phase, Press, ShareRole};
use makepad_create::lists::{paint_peer_badge, ActiveTownCall, Pane, SeatPalette, SharePopout};
use makepad_widgets::{makepad_platform::thread::{Class, ThreadOptions}, *};
use makepad_create::{model::{Action, Model, Origin}, transcript::{self, Heard}};
use makepad_town_core::{event::{SessionId, SessionMode, TownId}, key::PublicKey};
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

    pub(crate) fn record_call_fact(&mut self, fact: Fact) -> bool {
        let Some(model) = &mut self.model else { return false };
        let recipients = |town| {
            let mine = model.seat_in_town(town, &model.me());
            let mut to: Vec<_> = model.active_town_peers(town).into_iter()
                .filter_map(|peer| model.seat_in_town(town, &peer))
                .filter(|seat| Some(*seat) != mine).collect();
            to.sort_unstable();
            to.dedup();
            to
        };
        let mut flushed = false;
        let action = match fact {
            Fact::ShareOpen { session, ref title, town } => {
                let to = recipients(town);
                if to.is_empty() { return false }
                Action::SessionOpen { town: Some(town), session, mode: SessionMode::Share { title: title.clone() }, to, town_wide: true }
            }
            Fact::ShareJoin { session, sharer, town } => Action::SessionJoin { peer: Some(sharer), town: Some(town), session },
            Fact::Open { session, video, town, target } => {
                let mut to = recipients(town);
                if let CallTarget::Seat(seat) = target { to.retain(|&s| s == seat); }
                if to.is_empty() {
                    model.observe_voice_call(None, None, None);
                    return false
                }
                Action::SessionOpen { town: Some(town), session, mode: SessionMode::Call { video }, to, town_wide: target == CallTarget::Town }
            }
            Fact::Join { session, from, town } => Action::SessionJoin { peer: Some(from), town: Some(town), session },
            Fact::Leave { session, secs, why } => {
                flushed = self.flush_speech(session);
                Action::SessionLeave { session, secs, why }
            }
            Fact::Decline { session, why, from, town } => Action::SessionDecline { peer: Some(from), town: Some(town), session, why },
        };
        let wrote = self.write_call_action(action);
        let voice = match fact {
            Fact::Open { session, town, .. } => Some((session, None, town, wrote)),
            Fact::Join { session, from, town } => Some((session, Some(from), town, true)),
            _ => None,
        };
        if let Some((session, peer, town, active)) = voice {
            if let Some(model) = &mut self.model {
                model.observe_voice_call(active.then_some(session), peer, active.then_some(town));
            }
            if wrote { self.bind_transcript_town(session, peer, Some(town)); }
            else { self.call.set_transcript_town(session, None); }
        }
        flushed || wrote
    }

    pub(crate) fn record_call_facts(&mut self, cx: &mut Cx) {
        self.call.revoke_ended_shares(&mut self.app_links);
        self.publish_share_frame(cx);
        let mut changed = false;
        for session in self.call.take_muted_sessions() { changed |= self.flush_speech(session); }
        for fact in self.call.take_facts() { changed |= self.record_call_fact(fact); }
        let ringing = self.call.ringing_session();
        let from = self.call.ringing_from();
        let town = self.call.ringing_call_town();
        if let Some(m) = &mut self.model { m.observe_ring(ringing, from, town); }
        if let Some(session) = ringing { self.bind_transcript_town(session, from, town); }
        if changed { self.refresh(cx); }
    }

    // Snapshot the same link destination as observe_ring/Open. Changing the visible
    // town later must never change the audience named by the consent control.
    fn bind_transcript_town(&mut self, session: SessionId, peer: Option<PublicKey>, wire_town: Option<TownId>) {
        if self.call.has_transcript_town(session) { return }
        let label = self.model.as_ref().and_then(|m| {
            let town = m.transcript_town(session, peer, wire_town)?;
            m.town_name(town).map(str::to_owned).or_else(|| {
                m.towns().into_iter().find(|row| row.id == town).map(|row| row.title)
            })
        });
        self.call.set_transcript_town(session, label);
    }

    pub(crate) fn sync_live_items(&mut self, cx: &mut Cx) {
        if let Some(model) = &self.model {
            let calling_peers = self.call.calling_peers(model.open_id());
            let next = makepad_create::view::LiveRoster::from_call(model.open_id(), model.me(),
                &self.view.call, &calling_peers, self.call.ringing_from());
            if next != self.view.live_roster {
                self.view.live_roster = next;
                self.update_presence(cx);
            }
        }
        // A remembered connection can prove its key again without sending its display name.
        if self.view.call.view.peer_name.trim().is_empty() {
            let fallback_peer = self.call.ringing_from()
                .or_else(|| self.call.voice_peers().into_iter().next())
                .or_else(|| self.call.open_town().and_then(|(_, peers)| peers.first().copied()))
                .or(self.view.call.share.watching_sharer)
                .or_else(|| self.view.call.share.offers.iter().find_map(|o| match o.role {
                    ShareRole::Viewer { sharer } => Some(sharer),
                    ShareRole::Sharer { .. } => None,
                }));
            if let (Some(model), Some(peer)) = (&self.model, fallback_peer) {
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

    pub(crate) fn sync_call_peer_badge(&mut self, cx: &mut Cx, force: bool) {
        let view = &self.view.call.view;
        if view.phase == Phase::Idle {
            self.call_badge_painted = None;
            return
        }
        if self.view.seat_palette == SeatPalette::default() {
            self.view.seat_palette = SeatPalette::from_cx(cx);
        }
        let peer = view.participants.first().map(|p| p.peer)
            .or_else(|| self.call.ringing_from())
            .or_else(|| view.town.and_then(|t| self.call.calling_peers(t).into_iter().next()))
            .or_else(|| self.call.open_town().and_then(|(_, peers)| peers.first().copied()));
        let tone = peer.filter(|_| view.town.is_none_or(|t| self.model.as_ref().map(Model::open_id) == Some(t)))
            .and_then(|p| self.view.members.seat_tone_for_device(&p));
        if !force && self.call_badge_painted == Some(tone) { return }
        let badge = self.ui.widget(cx, ids!(live_call.content.people.peer_badge));
        paint_peer_badge(cx, &badge, tone, &self.view.seat_palette);
        self.call_badge_painted = Some(tone);
    }

    pub(crate) fn sync_active_town_call(&mut self, cx: &mut Cx) -> bool {
        let next = self.model.as_ref().filter(|_| self.view.call.view.phase == Phase::Idle).and_then(|model| {
            let town = model.open_id();
            let (session, participants) = model.active_town_call(town)?;
            let scope = model.call_join_scope(town, session)?;
            Some(ActiveTownCall { town, session, scope, participants, can_join: self.call.can_join_call(town, scope) })
        });
        if !self.view.set_active_town_call(next) { return false }
        for path in [ids!(project_call), ids!(home_join_call), ids!(project_page), ids!(home_page)] {
            self.ui.widget(cx, path).redraw(cx);
        }
        true
    }

    pub(crate) fn show_call(&mut self, cx: &mut Cx) {
        let previous = std::mem::replace(&mut self.view.call, self.call.shown());
        self.sync_active_town_call(cx);
        self.sync_live_items(cx);
        self.sync_call_peer_badge(cx, false);
        self.call.sync_timer(cx);
        let shown = &self.view.call;
        let project_changed = previous.view.phase != shown.view.phase || previous.linked != shown.linked
            || previous.can_call != shown.can_call || previous.can_share != shown.can_share || previous.view.peer_name != shown.view.peer_name;
        let open_town = self.call.open_town();
        let open_town_changed = self.carriers.shown_open_town.as_ref()
            .map(|(town, peers)| (*town, peers.as_slice())) != open_town;
        if open_town_changed {
            self.carriers.shown_open_town = open_town.map(|(town, peers)| (town, peers.to_vec()));
        }
        if project_changed {
            self.ui.widget(cx, ids!(project_call)).redraw(cx);
            self.ui.widget(cx, ids!(project_page)).redraw(cx);
            self.ui.widget(cx, ids!(home_page)).redraw(cx);
        }
        if (project_changed || open_town_changed) && self.home_visible() && self.view.selected_member.is_some() {
            self.update_member_card(cx);
            self.ui.widget(cx, ids!(member_card)).redraw(cx);
        }
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
        let mut uids = opening_uids(cx, &self.ui);
        if self.home_visible() && self.view.selected_member.is_some()
            && self.removing.is_none() && self.confirm_cascade.is_none() {
            let watch = self.ui.button(cx, ids!(member_watch));
            if !watch.is_empty() && watch.visible() { uids.push(watch.widget_uid()); }
            let call_btn = self.ui.button(cx, ids!(member_call));
            if !call_btn.is_empty() && call_btn.visible() { uids.push(call_btn.widget_uid()); }
        }
        uids
    }

    pub(crate) fn call_opening_presses(&mut self, cx: &mut Cx, action: CallAction) {
        // The synthetic host dispatches these microphone openings after the same consent check.
        let changed = self.call.call_opening_presses(cx, action)
            || (matches!(action, CallAction::Press(Press::Answer | Press::Call | Press::CallSeat(_) | Press::JoinCall(..))) && self.call.call_actions(cx, action));
        self.record_call_facts(cx);
        if changed { self.show_call(cx); }
    }

    pub(crate) fn consented_member_call(&mut self, cx: &mut Cx, event: &Event, actions: &mut ActionsBuf, opening: &[WidgetUid]) {
        use crate::connect_window::{card_pressed, HomeStep};
        use crate::TranscriptInput;

        if TranscriptInput::from_event(event, false) != Some(TranscriptInput::Choose) || !self.home_visible() { return }
        let button = self.ui.button(cx, ids!(member_call));
        let uid = button.widget_uid();
        if !opening.contains(&uid) { return }
        let home_step = self.home_step;
        let HomeStep::Card { town, member } = home_step else { return };
        if !card_pressed(button, actions, &home_step) { return }
        let Some(seat) = self.view.members.seats.iter().find(|s| s.id == member).cloned() else { return };
        if !self.can_call_member_seat(town, &seat) || !makepad_create::consent::granted(cx, self.call.consent_home.verified()) { return }
        actions.retain(|a| a.as_widget_action().is_none_or(|w| w.widget_uid != uid));
        let makepad_create::view::MemberId::Seat(seat_id) = member else { return };
        self.call_opening_presses(cx, CallAction::Press(Press::CallSeat(seat_id)));
    }

    pub(crate) fn call_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let mut changed = false;
        for widget in actions.iter().filter_map(|a| a.as_widget_action()) {
            let Some(&action) = widget.action.downcast_ref::<CallAction>() else { continue };
            if matches!(action, CallAction::Press(Press::Answer | Press::Call | Press::CallSeat(_) | Press::JoinCall(..))) { continue }
            changed |= self.call.call_actions(cx, action);
            self.record_call_facts(cx);
        }
        if changed { self.show_call(cx); }
    }
}

#[test]
fn call_roster_peer_badge_matches_seat_colour() {
    use crate::connect_window::tests::{append, attach_call_seat};
    use makepad_create::call::{CallRowWidgetRefExt, Message, RingExt};
    use makepad_town_core::{event::{Content, Event as TownEvent, Member}, home::Home, key::DeviceKey,
        link::VERSION, log::Log, share::{hello::Hello, Message as SyncMessage}};

    let ada = DeviceKey::generate().unwrap();
    let peer = ada.public();
    let dir = std::env::temp_dir().join(format!("town-call-badge-{}", makepad_town_core::hex(&peer)));
    let home = Home::open(&dir).unwrap();
    let (me, _) = DeviceKey::load_or_create(&home.key_path()).unwrap();
    let (solo, genesis) = TownEvent::found(&me, 2, 0, 0, [1; 16]).unwrap();
    Log::found(&home.town_log_path(&solo), genesis).unwrap();
    let (town, genesis) = TownEvent::found(&me, 2, 0, 0, [2; 16]).unwrap();
    let mut log = Log::found(&home.town_log_path(&town), genesis).unwrap();
    let seat = append(&mut log, &me, Content::Member(Member::Invite { subject: peer, role: 0 }), None);
    append(&mut log, &ada, Content::Member(Member::Accept { entry: seat }), None);
    drop(log);
    home.set_open_town(&town).unwrap();
    let mut model = Model::open_home(home).unwrap().0;
    model.pin_linked(&peer, VERSION, "Ada").unwrap();
    model.link_proved(&peer, VERSION, "Ada", None).unwrap();
    model.receive_from(peer, &SyncMessage::Hello(Hello::local(std::iter::empty())).encode()).unwrap();

    let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); makepad_create::script_mod(vm); });
    let cx = &mut *pool;
    let (mut app, ports) = attach_call_seat(cx, model, &[(peer, "Ada")]);
    app.ui = cx.with_vm(|vm| {
        let root = script_eval!(vm, { use mod.widgets.* View{live_call := LiveCallPage{}} });
        WidgetRef::script_from_value(vm, root)
    });
    let ring = Message::Ring { call_id: [3; 16], secret: [0; 32], video: false,
        ext: RingExt { town: Some(town), ..Default::default() } };
    ports[&peer].messages_in.try_send(ring.encode()).unwrap();
    app.call.poll(cx);
    app.call.press(cx, Press::Answer);
    app.show_call(cx);
    assert_eq!(app.view.call.view.phase, Phase::Live);
    let tone = app.view.members.seat_tone_for_device(&peer).expect("Ada has a seat in the open town");
    let page = app.ui.call_row(cx, ids!(live_call));
    page.show(cx, &app.view.call.view, &Default::default(), app.view.call.talking, app.view.members.you_name());
    let badge = page.widget(cx, ids!(peer_badge));
    let mut target = ui_checks::UiCheckTarget::new(cx);
    let mut check_badge = |cx: &mut Cx, app: &App, fill: Vec4f, ink: Vec4f| {
        target.draw(cx, &app.ui, dvec2(480., 240.));
        let mut color = [f32::NAN; 4];
        badge.borrow::<View>().unwrap().draw_bg.get_instance(cx, id!(color), &mut color);
        assert_eq!(color, [fill.x, fill.y, fill.z, fill.w], "call badge fill");
        assert_eq!(badge.label(cx, ids!(initial)).borrow().unwrap().draw_text.color, ink, "call badge initial");
        let ring = badge.widget(cx, ids!(ring));
        let Area::Instance(inst) = ring.area() else { panic!("live voice ring must have a drawn instance") };
        let call = cx.draw_lists[inst.draw_list_id].draw_items[inst.draw_item_id].draw_call().unwrap();
        let offset = cx.draw_shaders[call.draw_shader_id.index].mapping.dyn_uniforms.find(id!(ink)).unwrap().offset;
        assert_eq!(&call.dyn_uniforms[offset..offset + 4], &[ink.x, ink.y, ink.z, ink.w], "drawn voice ring ink");
    };
    let palette = SeatPalette::from_cx(cx);
    check_badge(cx, &app, palette.seats[tone.0 as usize], palette.ink_light);

    // Change towns while the same caller is live and still in the previous member snapshot.
    app.model.as_mut().unwrap().open_town(solo).unwrap();
    app.sync_call_peer_badge(cx, false);
    let (fill, ink) = cx.with_vm(|vm| {
        let peer = script_eval!(vm, {mod.palette.peer});
        let onpeer = script_eval!(vm, {mod.palette.onpeer});
        (Vec4f::script_from_value(vm, peer), Vec4f::script_from_value(vm, onpeer))
    });
    check_badge(cx, &app, fill, ink);
    app.call.shutdown(cx);
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}
